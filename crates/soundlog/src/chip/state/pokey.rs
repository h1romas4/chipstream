//! POKEY (Atari 8-bit) chip state implementation.
//!
//! This module provides state tracking for the POKEY sound chip,
//! used in Atari 8-bit computers and arcade systems. POKEY has 4 audio channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// POKEY has 4 audio channels
const POKEY_CHANNELS: usize = 4;

/// POKEY recommended storage (uses array storage for small register set)
pub type PokeyStorage = ArrayStorage<u8, 16>;

/// POKEY register state tracker
///
/// Tracks all 4 audio channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// POKEY has 8 write registers per chip:
/// - 0x00 (AUDF1): Channel 0 frequency
/// - 0x01 (AUDC1): Channel 0 control (volume + enable)
/// - 0x02 (AUDF2): Channel 1 frequency
/// - 0x03 (AUDC2): Channel 1 control
/// - 0x04 (AUDF3): Channel 2 frequency
/// - 0x05 (AUDC3): Channel 2 control
/// - 0x06 (AUDF4): Channel 3 frequency
/// - 0x07 (AUDC4): Channel 3 control
/// - 0x08 (AUDCTL): Audio control register
/// - 0x09 (STIMER): Start timers
/// - 0x0A (SKREST): Reset keyboard scan
/// - 0x0B (POTGO): Start pot scan
/// - 0x0D (SEROUT): Serial port output
/// - 0x0E (IRQEN): IRQ enable
/// - 0x0F (SKCTL): Serial port control
///
/// AUDCx format (control register):
/// - Bits 3-0: Volume (0-15)
/// - Bits 7-4: Distortion/noise control
///   - If volume is 0, channel is off
///   - If volume is non-zero, channel is on
#[derive(Debug, Clone)]
pub struct PokeyState {
    /// Channel states for 4 channels
    channels: [ChannelState; POKEY_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f64,
    /// Global register storage for all written registers
    registers: PokeyStorage,
}

impl PokeyState {
    /// Create a new POKEY state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 1,789,790 Hz (Atari 800, exact NTSC)
    /// - 1,773,447 Hz (PAL)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::PokeyState;
    ///
    /// // NTSC Atari 8-bit
    /// let state = PokeyState::new(1_789_790.0);
    /// ```
    pub fn new(master_clock_hz: f64) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: PokeyStorage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
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
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Calculate frequency in Hz from POKEY frequency value
    ///
    /// POKEY frequency formula depends on AUDCTL settings, but basic formula is:
    /// freq = master_clock / (2 * (AUDF + 1))
    ///
    /// # Arguments
    ///
    /// * `audf` - 8-bit frequency value (0-255)
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn calculate_frequency(&self, audf: u8) -> f64 {
        if audf == 0 {
            // AUDF=0 is technically valid, represents highest frequency
            self.master_clock_hz / 2.0
        } else {
            // Basic POKEY formula: freq = clock / (2 * (AUDF + 1))
            // Note: Actual formula is more complex with AUDCTL settings
            self.master_clock_hz / (2.0 * (audf as f64 + 1.0))
        }
    }

    /// Extract tone from channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= POKEY_CHANNELS {
            return None;
        }

        // Read from global register storage
        // AUDF register: 0x00, 0x02, 0x04, 0x06 (frequency)
        let audf_reg = (channel * 2) as u8;
        let audf = self.registers.read(audf_reg)?;

        let freq_hz = self.calculate_frequency(audf);

        Some(ToneInfo::new(audf as u16, 0, Some(freq_hz)))
    }

    /// Check if channel is enabled based on volume
    ///
    /// # Arguments
    ///
    /// * `audc` - AUDC register value
    ///
    /// # Returns
    ///
    /// true if channel has non-zero volume, false otherwise
    fn is_channel_enabled(&self, audc: u8) -> bool {
        // Volume is in bits 3-0
        (audc & 0x0F) != 0
    }

    /// Handle frequency register write (AUDF)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while channel is on, None otherwise
    fn handle_frequency_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= POKEY_CHANNELS {
            return None;
        }

        // If channel is on and frequency changed, emit ToneChange
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

    /// Handle control register write (AUDC)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    /// * `value` - Control value (volume + distortion)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_control_register(&mut self, channel: usize, value: u8) -> Option<Vec<StateEvent>> {
        if channel >= POKEY_CHANNELS {
            return None;
        }

        let enabled = self.is_channel_enabled(value);
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
}

impl ChipState for PokeyState {
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
            // AUDF1 (0x00) - Channel 0 frequency
            0x00 => self.handle_frequency_register(0),

            // AUDC1 (0x01) - Channel 0 control
            0x01 => self.handle_control_register(0, value),

            // AUDF2 (0x02) - Channel 1 frequency
            0x02 => self.handle_frequency_register(1),

            // AUDC2 (0x03) - Channel 1 control
            0x03 => self.handle_control_register(1, value),

            // AUDF3 (0x04) - Channel 2 frequency
            0x04 => self.handle_frequency_register(2),

            // AUDC3 (0x05) - Channel 2 control
            0x05 => self.handle_control_register(2, value),

            // AUDF4 (0x06) - Channel 3 frequency
            0x06 => self.handle_frequency_register(3),

            // AUDC4 (0x07) - Channel 3 control
            0x07 => self.handle_control_register(3, value),

            // AUDCTL (0x08) - Audio control register
            // This affects frequency calculation but we use simplified formula
            0x08 => None,

            // Other registers (STIMER, SKREST, POTGO, SEROUT, IRQEN, SKCTL)
            // These don't affect audio state
            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        POKEY_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pokey_channel_enable() {
        let mut state = PokeyState::new(1_789_790.0);

        // Set frequency for channel 0
        state.on_register_write(0x00, 0x10); // AUDF1

        // Enable channel 0 with volume 8
        let event = state.on_register_write(0x01, 0x08); // AUDC1

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x10);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_pokey_channel_disable() {
        let mut state = PokeyState::new(1_789_790.0);

        // Set up and enable channel 0
        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        // Disable channel 0 (volume = 0)
        let event = state.on_register_write(0x01, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_pokey_tone_change() {
        let mut state = PokeyState::new(1_789_790.0);

        // Set up and enable channel 0
        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        // Change frequency while enabled
        let event = state.on_register_write(0x00, 0x20);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::ToneChange { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x20);
        } else {
            panic!("Expected ToneChange event");
        }
    }

    #[test]
    fn test_pokey_multiple_channels() {
        let mut state = PokeyState::new(1_789_790.0);

        // Enable channel 0
        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        // Enable channel 2
        state.on_register_write(0x04, 0x20);
        state.on_register_write(0x05, 0x0A);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(2).unwrap().key_state, KeyState::On);
    }

    #[test]
    fn test_pokey_channel_count() {
        let state = PokeyState::new(1_789_790.0);
        assert_eq!(state.channel_count(), 4);
    }

    #[test]
    fn test_pokey_reset() {
        let mut state = PokeyState::new(1_789_790.0);

        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }

    #[test]
    fn test_pokey_volume_controls_enable() {
        let mut state = PokeyState::new(1_789_790.0);

        // Set frequency
        state.on_register_write(0x00, 0x10);

        // Enable with different volumes
        state.on_register_write(0x01, 0x01); // Volume 1
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);

        state.on_register_write(0x01, 0x0F); // Volume 15
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);

        state.on_register_write(0x01, 0x00); // Volume 0 = off
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
    }
}
