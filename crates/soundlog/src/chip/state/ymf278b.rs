//! YMF278B (OPL4) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YMF278B FM synthesis chip,
//! commonly known as OPL4, with 18 FM channels and PCM capabilities.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{self as fnumber, ChipTypeSpec};

/// YMF278B has 18 FM channels
const YMF278B_CHANNELS: usize = 18;

/// YMF278B global register storage (uses u16 to encode port in upper byte)
///
/// Port 0 registers: 0x0000-0x00FF
/// Port 1 registers: 0x0100-0x01FF
pub type Ymf278bStorage = SparseStorage<u16, u8>;

/// YMF278B register state tracker
///
/// Tracks all 18 FM channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Port Handling
///
/// YMF278B has multiple register sets:
/// - FM registers: Similar to OPL3 (YMF262)
/// - PCM registers: Wave table synthesis (not tracked for tone)
///
/// # Register Layout (FM part)
///
/// - 0xA0-0xA8: F-Number low 8 bits for channels 0-8 (per bank)
/// - 0xB0-0xB8: Key On (bit 5) + Block (bits 4-2) + F-Number high 2 bits (bits 1-0)
/// - 0x40-0x55: Key Scale Level / Total Level (volume)
/// - 0x105: OPL4 mode enable
///
/// YMF278B has two banks of 9 FM channels each for a total of 18 channels.
#[derive(Debug, Clone)]
pub struct Ymf278bState {
    /// Channel states for 18 FM channels
    channels: [ChannelState; YMF278B_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Current port (0 or 1) for multi-byte register writes
    current_port: u8,
    /// Global register storage for all written registers
    /// Uses u16 address space to encode port (port 0: 0x00-0xFF, port 1: 0x100-0x1FF)
    registers: Ymf278bStorage,
}

impl Ymf278bState {
    /// Create a new YMF278B state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 33,868,800 Hz (standard OPL4)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ymf278bState;
    ///
    /// let state = Ymf278bState::new(33_868_800.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            current_port: 0,
            registers: Ymf278bStorage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-17)
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
    /// * `channel` - Channel index (0-17)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Set the current port for register writes
    ///
    /// # Arguments
    ///
    /// * `port` - Port number (0 or 1)
    pub fn set_port(&mut self, port: u8) {
        self.current_port = port & 1;
    }

    /// Encode port and register into a single u16 address
    ///
    /// Port 0: 0x0000-0x00FF
    /// Port 1: 0x0100-0x01FF
    fn encode_register_address(&self, register: u8) -> u16 {
        (self.current_port as u16) << 8 | register as u16
    }

    /// Get the current port
    ///
    /// # Returns
    ///
    /// Current port number (0 or 1)
    pub fn current_port(&self) -> u8 {
        self.current_port
    }

    /// Extract fnum and block from register state for a channel
    ///
    /// YMF278B FM register layout (similar to OPL3):
    /// - Register 0xA0-0xA8: F-number low 8 bits
    /// - Register 0xB0-0xB8: Key On (bit 5) + Block (bits 4-2) + F-number high 2 bits (bits 1-0)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-17)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if both fnum and block registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= YMF278B_CHANNELS {
            return None;
        }

        // Determine which port and local channel based on channel number
        // Channels 0-8 are on port 0, channels 9-17 are on port 1
        let port = (channel / 9) as u8;
        let local_ch = channel % 9;

        let fnum_low_reg = 0xA0 + local_ch as u8;
        let block_fnum_high_reg = 0xB0 + local_ch as u8;

        // Read from global register storage with port encoding
        let fnum_low_addr = ((port as u16) << 8) | fnum_low_reg as u16;
        let block_fnum_high_addr = ((port as u16) << 8) | block_fnum_high_reg as u16;

        let fnum_low = self.registers.read(fnum_low_addr)?;
        let block_fnum_high = self.registers.read(block_fnum_high_addr)?;

        // Extract fnum (10 bits total: 8 low + 2 high)
        let fnum = (fnum_low as u16) | ((block_fnum_high & 0x03) as u16) << 8;

        // Extract block (3 bits, bits 4-2 of block_fnum_high register)
        let block = (block_fnum_high >> 2) & 0x07;

        // Calculate actual frequency using Opl3Spec (OPL4 uses same formula)
        let freq_hz =
            fnumber::Opl3Spec::fnum_block_to_freq(fnum as u32, block, self.master_clock_hz).ok();

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
        let local_ch = (register - 0xB0) as usize;

        if local_ch >= 9 {
            return None;
        }

        // Calculate actual channel based on port
        let channel = self.current_port as usize * 9 + local_ch;

        if channel >= YMF278B_CHANNELS {
            return None;
        }

        // Extract key on bit (bit 5)
        let key_on = (value & 0x20) != 0;
        let new_key_state = if key_on { KeyState::On } else { KeyState::Off };

        let old_key_state = self.channels[channel].key_state;
        self.channels[channel].key_state = new_key_state;

        // Generate appropriate event
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
                // Key still on, but tone might have changed
                if let Some(tone) = self.extract_tone(channel) {
                    let tone_changed = self.channels[channel]
                        .tone
                        .as_ref()
                        .map(|old_tone| old_tone.fnum != tone.fnum || old_tone.block != tone.block)
                        .unwrap_or(true);

                    if tone_changed {
                        self.channels[channel].tone = Some(tone);
                        Some(vec![StateEvent::ToneChange {
                            channel: channel as u8,
                            tone,
                        }])
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Handle F-number low register write (0xA0-0xA8)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0xA0-0xA8)
    /// * `value` - Value written (8-bit F-number low)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_fnum_low(&mut self, register: u8, _value: u8) -> Option<Vec<StateEvent>> {
        let local_ch = (register - 0xA0) as usize;

        if local_ch >= 9 {
            return None;
        }

        let channel = self.current_port as usize * 9 + local_ch;

        if channel >= YMF278B_CHANNELS {
            return None;
        }

        // Register value already stored in global storage by on_register_write

        // If key is on and frequency changed, emit ToneChange
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
}

impl ChipState for Ymf278bState {
    type Register = u8;
    type Value = u8;

    fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
        // Use current_port to read from the currently selected port
        let encoded_addr = self.encode_register_address(register);
        self.registers.read(encoded_addr)
    }

    fn on_register_write(
        &mut self,
        register: Self::Register,
        value: Self::Value,
    ) -> Option<Vec<StateEvent>> {
        // Store all register writes in global storage with port encoding
        let encoded_addr = self.encode_register_address(register);
        self.registers.write(encoded_addr, value);

        match register {
            // F-number low registers (0xA0-0xA8)
            0xA0..=0xA8 => self.handle_fnum_low(register, value),

            // Block + F-number high + Key On registers (0xB0-0xB8)
            0xB0..=0xB8 => self.handle_block_fnum_key(register, value),

            // Other FM registers (operators, connection, etc.)
            // We don't track these for tone extraction
            0x20..=0x35 | 0x40..=0x55 | 0x60..=0x75 | 0x80..=0x95 | 0xE0..=0xF5 | 0xC0..=0xC8 => {
                None
            }

            // OPL4 mode enable (0x105) and other control registers
            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.current_port = 0;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        YMF278B_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ymf278b_key_on_port0() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        state.set_port(0);
        state.on_register_write(0xA0, 0x81); // F-num low
        let event = state.on_register_write(0xB0, 0x2D); // Key on + block + fnum high

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.block, 3);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ymf278b_key_on_port1() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        state.set_port(1);
        state.on_register_write(0xA0, 0x81);
        let event = state.on_register_write(0xB0, 0x2D);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 9, .. }));
    }

    #[test]
    fn test_ymf278b_key_off() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        state.set_port(0);
        state.on_register_write(0xA0, 0x81);
        state.on_register_write(0xB0, 0x2D); // Key on

        let event = state.on_register_write(0xB0, 0x0D); // Key off (bit 5 = 0)

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_ymf278b_tone_change() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        state.set_port(0);
        state.on_register_write(0xA0, 0x81);
        state.on_register_write(0xB0, 0x2D);

        // Change frequency while key is on
        let event = state.on_register_write(0xA0, 0xC0);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_ymf278b_multiple_channels() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        // Channel 0 on port 0
        state.set_port(0);
        state.on_register_write(0xA0, 0x81);
        state.on_register_write(0xB0, 0x2D);

        // Channel 9 on port 1
        state.set_port(1);
        state.on_register_write(0xA0, 0x90);
        state.on_register_write(0xB0, 0x3E);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(9).unwrap().key_state, KeyState::On);
    }

    #[test]
    fn test_ymf278b_channel_count() {
        let state = Ymf278bState::new(33_868_800.0f32);
        assert_eq!(state.channel_count(), 18);
    }

    #[test]
    fn test_ymf278b_reset() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        state.set_port(0);
        state.on_register_write(0xA0, 0x81);
        state.on_register_write(0xB0, 0x2D);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert_eq!(state.current_port(), 0);
    }

    #[test]
    fn test_ymf278b_block_extraction() {
        let mut state = Ymf278bState::new(33_868_800.0f32);

        state.set_port(0);
        state.on_register_write(0xA1, 0xFF); // Channel 1, fnum low
        let event = state.on_register_write(0xB1, 0x3C); // Block=7, key on

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { tone, .. } = &events[0] {
            assert_eq!(tone.block, 7);
        } else {
            panic!("Expected KeyOn event");
        }
    }
}
