//! YM2413 (OPLL) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YM2413 FM synthesis chip,
//! commonly found in MSX computers and Sega Master System (FM Sound Unit).

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{self as fnumber, ChipTypeSpec};

/// YM2413 has 9 FM channels
const YM2413_CHANNELS: usize = 9;

/// YM2413 recommended storage
pub type Ym2413Storage = SparseStorage<u8, u8>;

/// YM2413 register state tracker
///
/// Tracks all 9 channels and their register state, detecting key on/off
/// events and extracting tone information (fnum, block).
///
/// # Register Layout
///
/// YM2413 has a simple register interface:
/// - 0x10-0x18: F-Number (low 8 bits) for channels 0-8
/// - 0x20-0x28: Key-On bit + Block + F-Number high bit for channels 0-8
///   - Bit 4: Key On (1=on, 0=off)
///   - Bits 3-1: Block (octave)
///   - Bit 0: F-Number bit 8 (MSB)
#[derive(Debug, Clone)]
pub struct Ym2413State {
    /// Channel states for 9 FM channels
    channels: [ChannelState; YM2413_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Ym2413Storage,
}

impl Ym2413State {
    /// Create a new YM2413 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - NTSC systems: 3,579,545 Hz
    /// - PAL systems: 3,546,893 Hz
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ym2413State;
    ///
    /// // NTSC system
    /// let state = Ym2413State::new(3_579_545.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: Ym2413Storage::default(),
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
    /// YM2413 register layout:
    /// - Register 0x10-0x18: F-Number low 8 bits
    /// - Register 0x20-0x28: Key-On (bit 4) + Block (bits 3-1) + F-Number bit 8 (bit 0)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-8)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if both registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= YM2413_CHANNELS {
            return None;
        }

        // Read from global register storage
        let fnum_low_reg = 0x10 + channel as u8;
        let block_fnum_high_reg = 0x20 + channel as u8;

        let fnum_low = self.registers.read(fnum_low_reg)?;
        let block_fnum_high = self.registers.read(block_fnum_high_reg)?;

        // Extract fnum (9 bits total: 8 low + 1 high)
        let fnum = (fnum_low as u16) | ((block_fnum_high & 0x01) as u16) << 8;

        // Extract block (3 bits, bits 3-1 of block_fnum_high register)
        let block = (block_fnum_high >> 1) & 0x07;

        // Calculate actual frequency using Opl3Spec
        // YM2413 (OPLL) uses OPL-family frequency calculation
        let freq_hz =
            fnumber::Opl3Spec::fnum_block_to_freq(fnum as u32, block, self.master_clock_hz).ok();

        Some(ToneInfo::new(fnum, block, freq_hz))
    }

    /// Handle key on/off and frequency register write (0x20-0x28)
    ///
    /// Register 0x20-0x28 format (one register per channel):
    /// - Bit 4: Key On (1=on, 0=off)
    /// - Bits 3-1: Block (octave, 0-7)
    /// - Bit 0: F-Number bit 8 (MSB of 9-bit f-number)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0x20-0x28)
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed or tone changed, None otherwise
    fn handle_block_fnum_key(&mut self, register: u8, value: u8) -> Option<Vec<StateEvent>> {
        let channel = (register - 0x20) as usize;

        if channel >= YM2413_CHANNELS {
            return None;
        }

        // Store the register value

        // Extract key on bit (bit 4)
        let new_key_state = if (value & 0x10) != 0 {
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
            _ => None, // No state change
        }
    }

    /// Handle F-Number low byte register write (0x10-0x18)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0x10-0x18)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_fnum_low(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        let channel = (register - 0x10) as usize;

        if channel >= YM2413_CHANNELS {
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

impl ChipState for Ym2413State {
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

        // Block + F-Number high + Key On registers (0x20-0x28)
        if matches!(register, 0x20..=0x28) {
            return self.handle_block_fnum_key(register, value);
        }

        // F-Number low registers (0x10-0x18)
        if matches!(register, 0x10..=0x18) {
            return self.handle_fnum_low(register);
        }

        // Other registers - store but don't generate events
        // (e.g., 0x00-0x07: instrument selection, 0x30-0x38: volume/sustain)
        None
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        YM2413_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ym2413_key_on_channel_0() {
        let mut state = Ym2413State::new(3_579_545.0f32);

        // Write fnum low for channel 0
        state.on_register_write(0x10, 0x6D); // fnum_low=0x6D

        // Write block + fnum high + key on
        let event = state.on_register_write(0x20, 0x18); // key_on=1, block=4, fnum_high=0

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x06D); // 9-bit fnum
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ym2413_key_off() {
        let mut state = Ym2413State::new(3_579_545.0f32);

        // Set up and key on
        state.on_register_write(0x10, 0x6D);
        state.on_register_write(0x20, 0x18);

        // Key off (clear bit 4)
        let event = state.on_register_write(0x20, 0x08);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));

        let ch = state.channel(0).unwrap();
        assert_eq!(ch.key_state, KeyState::Off);
    }

    #[test]
    fn test_ym2413_tone_change() {
        let mut state = Ym2413State::new(3_579_545.0f32);

        // Set up and key on
        state.on_register_write(0x10, 0x6D);
        state.on_register_write(0x20, 0x18);

        // Change fnum while key is on
        let event = state.on_register_write(0x10, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::ToneChange { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x080); // Updated fnum
            assert_eq!(tone.block, 4); // Block unchanged
        } else {
            panic!("Expected ToneChange event");
        }
    }

    #[test]
    fn test_ym2413_no_event_when_key_off() {
        let mut state = Ym2413State::new(3_579_545.0f32);

        state.on_register_write(0x10, 0x6D);
        // Don't key on, just set block/fnum
        state.on_register_write(0x20, 0x08); // key_on=0

        // Change fnum while key is off (should not generate event)
        let event = state.on_register_write(0x10, 0x80);
        assert!(event.is_none());
    }

    #[test]
    fn test_ym2413_channel_count() {
        let state = Ym2413State::new(3_579_545.0f32);
        assert_eq!(state.channel_count(), 9);
    }

    #[test]
    fn test_ym2413_reset() {
        let mut state = Ym2413State::new(3_579_545.0f32);

        state.on_register_write(0x10, 0x6D);
        state.on_register_write(0x20, 0x18);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }

    #[test]
    fn test_ym2413_multiple_channels() {
        let mut state = Ym2413State::new(3_579_545.0f32);

        // Channel 0
        state.on_register_write(0x10, 0x6D);
        state.on_register_write(0x20, 0x18);

        // Channel 5
        state.on_register_write(0x15, 0x80);
        state.on_register_write(0x25, 0x16);

        let ch0 = state.channel(0).unwrap();
        let ch5 = state.channel(5).unwrap();

        assert_eq!(ch0.key_state, KeyState::On);
        assert_eq!(ch5.key_state, KeyState::On);
        assert_eq!(ch0.tone.unwrap().fnum, 0x06D);
        assert_eq!(ch5.tone.unwrap().fnum, 0x080);
    }
}
