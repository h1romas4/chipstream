//! SAA1099 (Philips) chip state implementation.
//!
//! This module provides state tracking for the Philips SAA1099 sound chip,
//! used in SAM Coupé and some PC sound cards. SAA1099 has 6 audio channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// SAA1099 has 6 audio channels
const SAA1099_CHANNELS: usize = 6;

/// SAA1099 recommended storage (uses array storage with 256 entries to accommodate control writes)
/// SAA1099 uses two-stage addressing where register >= 0x80 selects a register,
/// and register < 0x80 writes data to the selected register.
/// We store both control writes and data writes.
pub type Saa1099Storage = ArrayStorage<u8, 256>;

/// SAA1099 register state tracker
///
/// Tracks all 6 audio channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// SAA1099 has a two-stage register access:
/// 1. Write register address to control port
/// 2. Write data to data port
///
/// Registers:
/// - 0x00-0x05: Amplitude (4-bit left, 4-bit right)
/// - 0x08-0x0D: Frequency (8-bit frequency value)
/// - 0x10-0x12: Octave (3-bit octave for two channels each)
/// - 0x14: Frequency enable (6 bits, one per channel)
/// - 0x15: Noise enable (6 bits, one per channel)
/// - 0x16: Noise parameters
/// - 0x18-0x19: Envelope generators
/// - 0x1C: All channels enable + sync/reset
///
/// Frequency calculation:
/// freq_hz = master_clock / ((511 - frequency) * 2^(8 - octave))
#[derive(Debug, Clone)]
pub struct Saa1099State {
    /// Channel states for 6 tone channels
    channels: [ChannelState; SAA1099_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Saa1099Storage,
    /// All channels enable flag (register 0x1C bit 0)
    all_channels_enable: bool,
}

impl Saa1099State {
    /// Create a new SAA1099 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 8,000,000 Hz (SAM Coupé)
    /// - 7,159,090 Hz (some PC cards)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Saa1099State;
    ///
    /// // SAM Coupé
    /// let state = Saa1099State::new(8_000_000.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: Saa1099Storage::default(),
            all_channels_enable: false,
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(&ChannelState) if channel index is valid, None otherwise
    pub fn channel(&self, channel: u8) -> Option<&ChannelState> {
        self.channels.get(channel as usize)
    }

    /// Get a mutable reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Calculate frequency in Hz from SAA1099 frequency and octave values
    ///
    /// SAA1099 frequency formula:
    /// freq_hz = master_clock / ((511 - frequency) * 2^(8 - octave))
    ///
    /// # Arguments
    ///
    /// * `frequency` - 8-bit frequency value (0-255)
    /// * `octave` - 3-bit octave value (0-7)
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn calculate_frequency(&self, frequency: u8, octave: u8) -> f32 {
        let divisor = (511 - frequency as i32).max(1) as f32;
        let octave_shift = 2_f32.powi(8 - (octave & 0x07) as i32);

        self.master_clock_hz / (divisor * octave_shift)
    }

    /// Extract tone from channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= SAA1099_CHANNELS {
            return None;
        }

        // Read from global register storage
        // Frequency register: 0x08 + channel
        let freq_reg = 0x08 + channel as u8;
        let frequency = self.registers.read(freq_reg)?;

        // Octave register: 0x10 + (channel / 2)
        // Each octave register holds octave for 2 channels
        let octave_reg = 0x10 + (channel / 2) as u8;
        let octave_data = self.registers.read(octave_reg).unwrap_or(0);

        // Extract octave: low 3 bits for even channel, high 3 bits for odd channel
        let octave = if channel.is_multiple_of(2) {
            octave_data & 0x07
        } else {
            (octave_data >> 4) & 0x07
        };

        let freq_hz = self.calculate_frequency(frequency, octave);

        Some(ToneInfo::new(frequency as u16, octave, Some(freq_hz)))
    }

    /// Check if channel is enabled
    ///
    /// A channel is enabled if:
    /// 1. All channels enable is on (0x1C bit 0)
    /// 2. Frequency enable is on for this channel (0x14)
    /// 3. Amplitude is non-zero (0x00-0x05)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// true if channel is enabled, false otherwise
    fn is_channel_enabled(&self, channel: usize) -> bool {
        if !self.all_channels_enable {
            return false;
        }

        // Read from global register storage
        // Check frequency enable (0x14)
        let freq_enable_data = self.registers.read(0x14).unwrap_or(0);
        let freq_enabled = (freq_enable_data & (1 << channel)) != 0;

        if !freq_enabled {
            return false;
        }

        // Check amplitude (0x00 + channel)
        let amp_reg = channel as u8;
        let amplitude = self.registers.read(amp_reg).unwrap_or(0);

        // Both left and right amplitude are zero = channel off
        (amplitude & 0x0F) != 0 || (amplitude & 0xF0) != 0
    }

    /// Handle amplitude register write (0x00-0x05)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_amplitude_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= SAA1099_CHANNELS {
            return None;
        }

        let enabled = self.is_channel_enabled(channel);
        let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

        let old_key_state = self.channels[channel].key_state;
        self.channels[channel].key_state = new_key_state;

        match (old_key_state, new_key_state) {
            (KeyState::Off, KeyState::On) => {
                if let Some(tone) = self.extract_tone(channel) {
                    self.channels[channel].tone = Some(tone);
                    Some(vec![StateEvent::KeyOn {
                        channel: channel as u8,
                        tone,
                    }])
                } else {
                    None
                }
            }
            (KeyState::On, KeyState::Off) => Some(vec![StateEvent::KeyOff {
                channel: channel as u8,
            }]),
            _ => None,
        }
    }

    /// Handle frequency register write (0x08-0x0D)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    /// * `value` - Frequency value (8-bit)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while enabled, None otherwise
    fn handle_frequency_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= SAA1099_CHANNELS {
            return None;
        }

        // If channel is enabled and frequency changed, emit ToneChange
        if self.channels[channel].key_state == KeyState::On
            && let Some(tone) = self.extract_tone(channel)
        {
            self.channels[channel].tone = Some(tone);
            return Some(vec![StateEvent::ToneChange {
                channel: channel as u8,
                tone,
            }]);
        }

        None
    }

    /// Handle octave register write (0x10-0x12)
    ///
    /// Each octave register controls 2 channels
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0x10-0x12)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) for first channel that changed, None otherwise
    fn handle_octave_register(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        let base_channel = ((register - 0x10) * 2) as usize;

        if base_channel >= SAA1099_CHANNELS {
            return None;
        }

        // Check if tone changed for enabled channels
        for offset in 0..2 {
            let channel = base_channel + offset;
            if channel >= SAA1099_CHANNELS {
                break;
            }

            if self.channels[channel].key_state == KeyState::On
                && let Some(tone) = self.extract_tone(channel)
            {
                self.channels[channel].tone = Some(tone);
                return Some(vec![StateEvent::ToneChange {
                    channel: channel as u8,
                    tone,
                }]);
            }
        }

        None
    }

    /// Handle frequency enable register write (0x14)
    ///
    /// # Arguments
    ///
    ///
    /// # Returns
    ///
    /// Some(StateEvent) for first channel that changed, None otherwise
    fn handle_frequency_enable_register(&mut self) -> Option<Vec<StateEvent>> {
        // Check each channel for state change
        for channel in 0..SAA1099_CHANNELS {
            let enabled = self.is_channel_enabled(channel);
            let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

            let old_key_state = self.channels[channel].key_state;
            self.channels[channel].key_state = new_key_state;

            match (old_key_state, new_key_state) {
                (KeyState::Off, KeyState::On) => {
                    if let Some(tone) = self.extract_tone(channel) {
                        self.channels[channel].tone = Some(tone);
                        return Some(vec![StateEvent::KeyOn {
                            channel: channel as u8,
                            tone,
                        }]);
                    }
                }
                (KeyState::On, KeyState::Off) => {
                    return Some(vec![StateEvent::KeyOff {
                        channel: channel as u8,
                    }]);
                }
                _ => {}
            }
        }

        None
    }

    /// Handle all channels enable register write (0x1C)
    ///
    /// # Arguments
    ///
    /// * `value` - Control value (bit 0 = all enable, bit 1 = sync/reset)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) for first channel that changed, None otherwise
    fn handle_all_enable_register(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let new_all_enable = (value & 0x01) != 0;
        let old_all_enable = self.all_channels_enable;
        self.all_channels_enable = new_all_enable;

        // If all enable changed, update all channels
        if old_all_enable != new_all_enable {
            for channel in 0..SAA1099_CHANNELS {
                let enabled = self.is_channel_enabled(channel);
                let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

                let old_key_state = self.channels[channel].key_state;
                self.channels[channel].key_state = new_key_state;

                match (old_key_state, new_key_state) {
                    (KeyState::Off, KeyState::On) => {
                        if let Some(tone) = self.extract_tone(channel) {
                            self.channels[channel].tone = Some(tone);
                            return Some(vec![StateEvent::KeyOn {
                                channel: channel as u8,
                                tone,
                            }]);
                        }
                    }
                    (KeyState::On, KeyState::Off) => {
                        return Some(vec![StateEvent::KeyOff {
                            channel: channel as u8,
                        }]);
                    }
                    _ => {}
                }
            }
        }

        None
    }
}

impl ChipState for Saa1099State {
    type Register = u8;
    type Value = u8;

    fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
        self.registers.read(register)
    }

    fn on_register_write(
        &mut self,
        register: Self::Register,
        value: Self::Value,
    ) -> Option<Vec<StateEvent>> {
        // Store all register writes in global storage
        self.registers.write(register, value);

        match register {
            // Amplitude registers (0x00-0x05)
            0x00..=0x05 => self.handle_amplitude_register(register as usize),

            // Frequency registers (0x08-0x0D)
            0x08..=0x0D => {
                let channel = (register - 0x08) as usize;
                self.handle_frequency_register(channel)
            }

            // Octave registers (0x10-0x12)
            0x10..=0x12 => self.handle_octave_register(register),

            // Frequency enable (0x14)
            0x14 => self.handle_frequency_enable_register(),

            // Noise enable (0x15) - don't generate events
            0x15 => None,

            // Noise parameters (0x16) - don't generate events
            0x16 => None,

            // Envelope generators (0x18-0x19) - don't generate events
            0x18..=0x19 => None,

            // All channels enable (0x1C) - handle inline
            0x1C => self.handle_all_enable_register(value),

            // Default: no event generation for other registers
            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.all_channels_enable = false;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        SAA1099_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_saa1099_channel_enable() {
        let mut state = Saa1099State::new(8_000_000.0f32);

        // Set frequency for channel 0
        state.on_register_write(0x08, 0x20); // Frequency register for ch0 = 0x20

        // Set octave for channel 0
        state.on_register_write(0x10, 0x03); // Octave register

        // Enable all channels
        state.on_register_write(0x1C, 0x01); // Enable all

        // Enable frequency for channel 0
        state.on_register_write(0x14, 0x01); // Enable ch 0

        // Set amplitude (this should trigger KeyOn)
        let event = state.on_register_write(0x00, 0x88); // Volume 8/8

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_saa1099_channel_disable() {
        let mut state = Saa1099State::new(8_000_000.0f32);

        // Set up and enable channel 0
        state.on_register_write(0x08, 0x20); // freq
        state.on_register_write(0x10, 0x03); // octave
        state.on_register_write(0x1C, 0x01); // all enable
        state.on_register_write(0x14, 0x01); // freq enable ch0
        state.on_register_write(0x00, 0x88); // amplitude non-zero -> KeyOn

        // Disable by clearing frequency enable for channel 0
        let event = state.on_register_write(0x14, 0x00); // disable channel 0

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_saa1099_tone_change() {
        let mut state = Saa1099State::new(8_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x08, 0x20);
        state.on_register_write(0x10, 0x03);
        state.on_register_write(0x1C, 0x01);
        state.on_register_write(0x14, 0x01);
        state.on_register_write(0x00, 0x88);

        // Change frequency while enabled
        let event = state.on_register_write(0x08, 0x40);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_saa1099_channel_count() {
        let state = Saa1099State::new(8_000_000.0f32);
        assert_eq!(state.channel_count(), 6);
    }

    #[test]
    fn test_saa1099_reset() {
        let mut state = Saa1099State::new(8_000_000.0f32);

        state.on_register_write(0x1C, 0x01);

        state.reset();

        assert!(!state.all_channels_enable);
        assert!(state.read_register(0x1C).is_none());
    }
}
