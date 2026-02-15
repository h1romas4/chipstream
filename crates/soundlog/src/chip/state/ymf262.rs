//! YMF262 (OPL3) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YMF262 FM synthesis chip,
//! commonly known as OPL3, with 18 FM channels across 2 ports.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{self as fnumber, ChipTypeSpec};

/// YMF262 has 18 FM channels (9 per port)
const YMF262_CHANNELS: usize = 18;

/// YMF262 global register storage (uses u16 to encode port in upper byte)
///
/// Port 0 registers: 0x0000-0x00FF
/// Port 1 registers: 0x0100-0x01FF
pub type Ymf262Storage = SparseStorage<u16, u8>;

/// YMF262 register state tracker
///
/// Tracks all 18 FM channels (9 channels × 2 ports) and their register state,
/// detecting key on/off events and extracting tone information.
///
/// # Port Handling
///
/// YMF262 has two ports:
/// - Port 0: Controls channels 0-8
/// - Port 1: Controls channels 9-17
///
/// # Register Layout
///
/// - 0xA0-0xA8: F-Number low 8 bits for channels 0-8 (per port)
/// - 0xB0-0xB8: Key On (bit 5) + Block (bits 4-2) + F-Number high 2 bits (bits 1-0)
/// - 0xBD: Rhythm mode control (percussion)
/// - 0x40-0x55: Key Scale Level / Total Level (volume)
/// - 0x105 (port 1, reg 0x05): OPL3 mode enable
#[derive(Debug, Clone)]
pub struct Ymf262State {
    /// Channel states for 18 FM channels
    channels: [ChannelState; YMF262_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f64,
    /// Current port (0 or 1) for multi-byte register writes
    current_port: u8,
    /// OPL3 mode flag (register 0x105 bit 0)
    opl3_mode: bool,
    /// Global register storage for all written registers
    /// Uses u16 address space to encode port (port 0: 0x00-0xFF, port 1: 0x100-0x1FF)
    registers: Ymf262Storage,
}

impl Ymf262State {
    /// Create a new YMF262 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 14,318,180 Hz (standard)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ymf262State;
    ///
    /// let state = Ymf262State::new(14_318_180.0);
    /// ```
    pub fn new(master_clock_hz: f64) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            current_port: 0,
            opl3_mode: false,
            registers: Ymf262Storage::default(),
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

    /// Check if OPL3 mode is enabled
    ///
    /// # Returns
    ///
    /// true if OPL3 mode is enabled, false otherwise
    pub fn is_opl3_mode(&self) -> bool {
        self.opl3_mode
    }

    /// Extract fnum and block from register state for a channel
    ///
    /// YMF262 register layout:
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
        if channel >= YMF262_CHANNELS {
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

        // Calculate actual frequency using Opl3Spec
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

        if channel >= YMF262_CHANNELS {
            return None;
        }

        // Register value already stored in global storage by on_register_write

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
    /// * `value` - Value written (F-Number bits 7-0)
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

        if channel >= YMF262_CHANNELS {
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

    /// Handle OPL3 mode enable register (0x05 on port 1)
    ///
    /// # Arguments
    ///
    /// * `value` - Value written (bit 0 = OPL3 mode)
    fn handle_opl3_mode(&mut self, value: u8) {
        self.opl3_mode = (value & 0x01) != 0;
    }
}

impl ChipState for Ymf262State {
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

        // OPL3 mode enable (register 0x05 on port 1)
        if self.current_port == 1 && register == 0x05 {
            self.handle_opl3_mode(value);
            return None;
        }

        // Block + F-Number high + Key On registers (0xB0-0xB8)
        if matches!(register, 0xB0..=0xB8) {
            return self.handle_block_fnum_key(register, value);
        }

        // F-Number low registers (0xA0-0xA8)
        if matches!(register, 0xA0..=0xA8) {
            return self.handle_fnum_low(register, value);
        }

        // Other registers - store but don't generate events
        None
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.current_port = 0;
        self.opl3_mode = false;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        YMF262_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ymf262_key_on_port0() {
        let mut state = Ymf262State::new(14_318_180.0);

        state.set_port(0);
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
    fn test_ymf262_key_on_port1() {
        let mut state = Ymf262State::new(14_318_180.0);

        state.set_port(1);
        state.on_register_write(0xA0, 0x80);
        let event = state.on_register_write(0xB0, 0x28);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 9);
            assert_eq!(tone.fnum, 0x080);
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ymf262_dual_port() {
        let mut state = Ymf262State::new(14_318_180.0);

        // Write to channel 0 on port 0
        state.set_port(0);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0xB0, 0x30);

        // Write to channel 9 on port 1
        state.set_port(1);
        state.on_register_write(0xA0, 0x80);
        state.on_register_write(0xB0, 0x28);

        let ch0 = state.channel(0).unwrap();
        let ch9 = state.channel(9).unwrap();

        assert_eq!(ch0.tone.unwrap().fnum, 0x06D);
        assert_eq!(ch9.tone.unwrap().fnum, 0x080);
    }

    #[test]
    fn test_ymf262_opl3_mode() {
        let mut state = Ymf262State::new(14_318_180.0);

        assert!(!state.is_opl3_mode());

        state.set_port(1);
        state.on_register_write(0x05, 0x01);

        assert!(state.is_opl3_mode());
    }

    #[test]
    fn test_ymf262_channel_count() {
        let state = Ymf262State::new(14_318_180.0);
        assert_eq!(state.channel_count(), 18);
    }

    #[test]
    fn test_ymf262_reset() {
        let mut state = Ymf262State::new(14_318_180.0);

        state.set_port(0);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0xB0, 0x30);

        state.set_port(1);
        state.on_register_write(0x05, 0x01);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert_eq!(state.current_port(), 0);
        assert!(!state.is_opl3_mode());
    }
}
