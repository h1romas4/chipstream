//! HuC6280 (PC Engine/TurboGrafx-16) chip state implementation.
//!
//! This module provides state tracking for the Hudson HuC6280 sound chip,
//! found in the PC Engine/TurboGrafx-16, which has 6 wavetable synthesis channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// HuC6280 has 6 wavetable channels
const HUC6280_CHANNELS: usize = 6;

/// HuC6280 recommended storage
pub type Huc6280Storage = SparseStorage<u8, u8>;

/// HuC6280 register state tracker
///
/// Tracks all 6 wavetable channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// Channel Select (0x00):
/// - Bits 0-2: Channel select (0-5)
///
/// Global Volume (0x01):
/// - Bits 0-3: Master volume (left)
/// - Bits 4-7: Master volume (right)
///
/// Per-Channel Registers (selected via 0x00):
/// - 0x02: Frequency low (8 bits)
/// - 0x03: Frequency high (4 bits)
/// - 0x04: Channel on/off and DDA mode
///   - Bit 7: Channel enable (1=on, 0=off)
///   - Bit 6: DDA mode
/// - 0x05: Channel volume (left/right)
/// - 0x06: Wave RAM data port
/// - 0x07: Noise enable and frequency
///
/// LFO (0x08-0x09):
/// - 0x08: LFO frequency
/// - 0x09: LFO control
#[derive(Debug, Clone)]
pub struct Huc6280State {
    /// Channel states for 6 wavetable channels
    channels: [ChannelState; HUC6280_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f64,
    /// Currently selected channel for register access
    selected_channel: u8,
    /// Global register storage for all written registers
    registers: Huc6280Storage,
}

impl Huc6280State {
    /// Create a new HuC6280 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 3,579,545 Hz (NTSC)
    /// - 3,546,893 Hz (PAL)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Huc6280State;
    ///
    /// let state = Huc6280State::new(3_579_545.0);
    /// ```
    pub fn new(master_clock_hz: f64) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            selected_channel: 0,
            registers: Huc6280Storage::default(),
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

    /// Get the currently selected channel
    ///
    /// # Returns
    ///
    /// Selected channel index (0-5)
    pub fn selected_channel(&self) -> u8 {
        self.selected_channel
    }

    /// Calculate frequency in Hz from HuC6280 frequency value
    ///
    /// # Arguments
    ///
    /// * `freq` - 12-bit frequency value
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn hz_huc6280(&self, freq: u16) -> f64 {
        if freq == 0 {
            0.0
        } else {
            // HuC6280 frequency formula: master_clock / (32 * 32 * (4096 - freq))
            // The PSG runs at master_clock / 32, and each step is 32 samples
            self.master_clock_hz / (32.0 * 32.0 * (4096 - freq as i32) as f64)
        }
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
        if channel >= HUC6280_CHANNELS {
            return None;
        }

        // Read from global register storage
        // Frequency registers: 0x02 (low) and 0x03 (high)
        let freq_low = self.registers.read(0x02)?;
        let freq_high = self.registers.read(0x03)?;

        // 12-bit frequency
        let freq = (freq_low as u16) | ((freq_high as u16 & 0x0F) << 8);

        if freq == 0 {
            return None;
        }

        let freq_hz = self.hz_huc6280(freq);

        Some(ToneInfo::new(freq, 0, Some(freq_hz)))
    }

    /// Handle channel select register write (0x00)
    ///
    /// # Arguments
    ///
    /// * `value` - Value written (bits 0-2 = channel select)
    fn handle_channel_select(&mut self, value: u8) {
        self.selected_channel = value & 0x07;
        if self.selected_channel as usize >= HUC6280_CHANNELS {
            self.selected_channel = 0;
        }
    }

    /// Handle frequency register writes (0x02-0x03)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while enabled, None otherwise
    fn handle_frequency_register(&mut self) -> Option<Vec<StateEvent>> {
        let channel = self.selected_channel as usize;

        if channel >= HUC6280_CHANNELS {
            return None;
        }

        // If channel is enabled and tone changed, emit ToneChange
        if self.channels[channel].key_state == KeyState::On
            && let Some(tone) = self.extract_tone(channel)
        {
            let tone_changed = self.channels[channel]
                .tone
                .as_ref()
                .map(|old_tone| old_tone.fnum != tone.fnum)
                .unwrap_or(true);

            if tone_changed {
                self.channels[channel].tone = Some(tone);
                return Some(vec![StateEvent::ToneChange {
                    channel: channel as u8,
                    tone,
                }]);
            }
        }

        None
    }

    /// Handle channel enable register write (0x04)
    ///
    /// Register 0x04 format:
    /// - Bit 7: Channel enable (1=on, 0=off)
    /// - Bit 6: DDA mode (direct D/A)
    /// - Bits 0-4: Volume
    ///
    /// # Arguments
    ///
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_channel_enable(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let channel = self.selected_channel as usize;

        if channel >= HUC6280_CHANNELS {
            return None;
        }

        let enabled = (value & 0x80) != 0;
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

    /// Handle volume register write (0x05)
    ///
    /// # Arguments
    ///
    /// # Returns
    ///
    /// None (volume changes don't generate events)
    fn handle_volume_register(&mut self) -> Option<Vec<StateEvent>> {
        let channel = self.selected_channel as usize;

        if channel >= HUC6280_CHANNELS {
            return None;
        }

        None
    }
}

impl ChipState for Huc6280State {
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
            // Channel select (0x00)
            0x00 => {
                self.handle_channel_select(value);
                None
            }

            // Global volume (0x01)
            0x01 => None,

            // Frequency low (0x02)
            0x02 => self.handle_frequency_register(),

            // Frequency high (0x03)
            0x03 => self.handle_frequency_register(),

            // Channel enable (0x04)
            0x04 => self.handle_channel_enable(value),

            // Channel volume (0x05)
            0x05 => self.handle_volume_register(),

            // Wave RAM data (0x06)
            0x06 => {
                // Waveform data - store but don't generate events
                None
            }

            // Noise control (0x07)
            0x07 => None,

            // LFO frequency (0x08)
            0x08 => None,

            // LFO control (0x09)
            0x09 => None,

            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.selected_channel = 0;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        HUC6280_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_huc6280_enable_channel() {
        let mut state = Huc6280State::new(3_579_545.0);

        // Select channel 0
        state.on_register_write(0x00, 0x00);

        // Set frequency
        state.on_register_write(0x02, 0x00); // Freq low
        state.on_register_write(0x03, 0x08); // Freq high (0x800)

        // Enable channel
        let event = state.on_register_write(0x04, 0x80);

        assert!(event.is_some());

        if let Some(ref events) = event
            && events.len() == 1
            && let StateEvent::KeyOn { channel, tone } = &events[0]
        {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x800);
            assert!(tone.freq_hz.is_some());
        }
    }

    #[test]
    fn test_huc6280_disable_channel() {
        let mut state = Huc6280State::new(3_579_545.0);

        // Set up and enable channel 0
        state.on_register_write(0x00, 0x00);
        state.on_register_write(0x02, 0x00);
        state.on_register_write(0x03, 0x08);
        state.on_register_write(0x04, 0x80);

        // Disable channel
        let event = state.on_register_write(0x04, 0x00);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::KeyOff { .. }));
        }
    }

    #[test]
    fn test_huc6280_tone_change() {
        let mut state = Huc6280State::new(3_579_545.0);

        // Set up and enable channel 0
        state.on_register_write(0x00, 0x00);
        state.on_register_write(0x02, 0x00);
        state.on_register_write(0x03, 0x08);
        state.on_register_write(0x04, 0x80);

        // Change frequency while enabled
        let event = state.on_register_write(0x02, 0xFF);

        assert!(event.is_some());

        if let Some(ref events) = event
            && events.len() == 1
            && let StateEvent::ToneChange { channel, tone } = &events[0]
        {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x8FF);
        }
    }

    #[test]
    fn test_huc6280_multiple_channels() {
        let mut state = Huc6280State::new(3_579_545.0);

        // Set up channel 0
        state.on_register_write(0x00, 0x00);
        state.on_register_write(0x02, 0x00);
        state.on_register_write(0x03, 0x08);
        state.on_register_write(0x04, 0x80);

        // Set up channel 2
        state.on_register_write(0x00, 0x02);
        state.on_register_write(0x02, 0x00);
        state.on_register_write(0x03, 0x04);
        state.on_register_write(0x04, 0x80);

        let ch0 = state.channel(0).unwrap();
        let ch2 = state.channel(2).unwrap();

        assert_eq!(ch0.key_state, KeyState::On);
        assert_eq!(ch2.key_state, KeyState::On);
        assert_eq!(ch0.tone.unwrap().fnum, 0x800);
        assert_eq!(ch2.tone.unwrap().fnum, 0x400);
    }

    #[test]
    fn test_huc6280_channel_count() {
        let state = Huc6280State::new(3_579_545.0);
        assert_eq!(state.channel_count(), 6);
    }

    #[test]
    fn test_huc6280_reset() {
        let mut state = Huc6280State::new(3_579_545.0);

        state.on_register_write(0x00, 0x00);
        state.on_register_write(0x02, 0x00);
        state.on_register_write(0x03, 0x08);
        state.on_register_write(0x04, 0x80);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
        assert_eq!(state.selected_channel(), 0);
    }

    #[test]
    fn test_huc6280_channel_select() {
        let mut state = Huc6280State::new(3_579_545.0);

        // Select channel 3
        state.on_register_write(0x00, 0x03);
        assert_eq!(state.selected_channel(), 3);

        // Select channel 5
        state.on_register_write(0x00, 0x05);
        assert_eq!(state.selected_channel(), 5);
    }
}
