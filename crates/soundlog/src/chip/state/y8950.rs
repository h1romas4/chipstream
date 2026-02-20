//! Y8950 (MSX-Audio) chip state implementation.
//!
//! This module provides state tracking for the Yamaha Y8950 FM synthesis chip,
//! also known as MSX-Audio, with 9 FM channels and ADPCM support.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{self as fnumber, ChipTypeSpec};

/// Y8950 has 9 FM channels
const Y8950_CHANNELS: usize = 9;

/// Y8950 recommended storage
pub type Y8950Storage = SparseStorage<u8, u8>;

/// Y8950 register state tracker
///
/// Tracks all 9 FM channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// - 0xA0-0xA8: F-Number low 8 bits for channels 0-8
/// - 0xB0-0xB8: Key On (bit 5) + Block (bits 4-2) + F-Number high 2 bits (bits 1-0)
/// - 0xBD: Rhythm mode control (percussion)
/// - 0x40-0x55: Key Scale Level / Total Level (volume)
/// - 0x07-0x12: ADPCM registers (not tracked for tone)
#[derive(Debug, Clone)]
pub struct Y8950State {
    /// Channel states for 9 FM channels
    channels: [ChannelState; Y8950_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Y8950Storage,
}

impl Y8950State {
    /// Create a new Y8950 state tracker
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
    /// use soundlog::chip::state::Y8950State;
    ///
    /// let state = Y8950State::new(3_579_545.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: Y8950Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-8)
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
    /// * `channel` - Channel index (0-8)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Extract fnum and block from register state for a channel
    ///
    /// Y8950 register layout:
    /// - Register 0xA0-0xA8: F-number low 8 bits
    /// - Register 0xB0-0xB8: Key On (bit 5) + Block (bits 4-2) + F-number high 2 bits (bits 1-0)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-8)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if both fnum and block registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= Y8950_CHANNELS {
            return None;
        }

        // Read from global register storage
        let fnum_low_reg = 0xA0 + channel as u8;
        let block_fnum_high_reg = 0xB0 + channel as u8;

        let fnum_low = self.registers.read(fnum_low_reg)?;
        let block_fnum_high = self.registers.read(block_fnum_high_reg)?;

        // Extract fnum (10 bits total: 8 low + 2 high)
        let fnum = (fnum_low as u16) | ((block_fnum_high & 0x03) as u16) << 8;

        // Extract block (3 bits, bits 4-2 of block_fnum_high register)
        let block = (block_fnum_high >> 2) & 0x07;

        // Calculate actual frequency using Opl3Spec
        let freq_hz =
            fnumber::OplSpec::fnum_block_to_freq(fnum as u32, block, self.master_clock_hz).ok();

        Some(ToneInfo::new(fnum, block, freq_hz))
    }

    /// Handle key on/off and frequency register write (0xB0-0xB8)
    ///
    /// Register 0xB0-0xB8 format:
    /// - Bit 5: Key On (1=on, 0=off)
    /// - Bits 4-2: Block (octave, 0-7)
    /// - Bits 1-0: F-Number bits 9-8 (MSB of 10-bit f-number)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0xB0-0xB8)
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed or tone changed, None otherwise
    fn handle_block_fnum_key(&mut self, register: u8, value: u8) -> Option<Vec<StateEvent>> {
        let channel = (register - 0xB0) as usize;

        if channel >= Y8950_CHANNELS {
            return None;
        }

        // Store the register value

        // Extract key on bit (bit 5)
        let new_key_state = if (value & 0x20) != 0 {
            KeyState::On
        } else {
            KeyState::Off
        };

        let old_key_state = self.channels[channel].key_state;
        self.channels[channel].key_state = new_key_state;

        // Generate event based on state transition
        match (old_key_state, new_key_state) {
            (KeyState::Off, KeyState::On) => {
                // Key on: extract and store current tone
                let tone = self.extract_tone(channel)?;
                self.channels[channel].tone = Some(tone);
                Some(vec![StateEvent::KeyOn {
                    channel: channel as u8,
                    tone,
                }])
            }
            (KeyState::On, KeyState::Off) => {
                // Key off
                Some(vec![StateEvent::KeyOff {
                    channel: channel as u8,
                }])
            }
            (KeyState::On, KeyState::On) => {
                // Tone change while key is on
                if let Some(tone) = self.extract_tone(channel) {
                    self.channels[channel].tone = Some(tone);
                    Some(vec![StateEvent::ToneChange {
                        channel: channel as u8,
                        tone,
                    }])
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Handle F-Number low byte register write (0xA0-0xA8)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0xA0-0xA8)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_fnum_low(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        let channel = (register - 0xA0) as usize;

        if channel >= Y8950_CHANNELS {
            return None;
        }

        // If key is on and tone registers changed, emit ToneChange event
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
}

impl ChipState for Y8950State {
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

        // Block + F-Number high + Key On registers (0xB0-0xB8)
        if matches!(register, 0xB0..=0xB8) {
            return self.handle_block_fnum_key(register, value);
        }

        // F-Number low registers (0xA0-0xA8)
        if matches!(register, 0xA0..=0xA8) {
            return self.handle_fnum_low(register);
        }

        // ADPCM registers (0x07-0x12) - store but don't generate events
        // Other registers - store but don't generate events
        None
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        Y8950_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_y8950_key_on() {
        let mut state = Y8950State::new(3_579_545.0f32);

        state.on_register_write(0xA0, 0x6D);
        let event = state.on_register_write(0xB0, 0x30);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x06D);
            assert_eq!(tone.block, 4);
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_y8950_key_off() {
        let mut state = Y8950State::new(3_579_545.0f32);

        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0xB0, 0x30);

        let event = state.on_register_write(0xB0, 0x10);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_y8950_tone_change() {
        let mut state = Y8950State::new(3_579_545.0f32);

        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0xB0, 0x30);

        let event = state.on_register_write(0xA0, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_y8950_channel_count() {
        let state = Y8950State::new(3_579_545.0f32);
        assert_eq!(state.channel_count(), 9);
    }

    #[test]
    fn test_y8950_reset() {
        let mut state = Y8950State::new(3_579_545.0f32);

        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0xB0, 0x30);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }
}
