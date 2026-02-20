//! YM2612 (OPN2) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YM2612 FM synthesis chip,
//! commonly found in Sega Genesis/Mega Drive systems.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{ChipTypeSpec, OpnaSpec};

/// YM2612 channel storage (256 register space, but sparse usage)
///
/// YM2612 has a 256-register address space split between two ports,
/// but typically only a small subset is used for tone generation.
/// SparseStorage provides good balance of flexibility and performance.
pub type Ym2612Storage = SparseStorage<u16, u8>;

/// YM2612 has 6 FM channels
const YM2612_CHANNELS: usize = 6;

/// YM2612 register state tracker
///
/// Tracks all 6 channels and their register state, detecting key on/off
/// events and extracting tone information (fnum, block).
///
/// # Port Handling
///
/// YM2612 has two ports that must be tracked separately:
/// - Port 0: Controls channels 0-2
/// - Port 1: Controls channels 3-5
///
/// Most register writes are port-specific, meaning the same register address
/// affects different channels depending on which port is selected.
#[derive(Debug, Clone)]
pub struct Ym2612State {
    /// Channel states for 6 FM channels
    channels: [ChannelState; YM2612_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Current port (0 or 1) for multi-byte register writes
    current_port: u8,
    /// Global register storage for all written registers
    /// Uses u16 address space to encode port (port 0: 0x00-0xFF, port 1: 0x100-0x1FF)
    registers: Ym2612Storage,
}

impl Ym2612State {
    /// Create a new YM2612 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - NTSC Genesis/Mega Drive: 7,670,454 Hz
    /// - PAL Genesis/Mega Drive: 7,600,489 Hz
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ym2612State;
    ///
    /// // NTSC Genesis
    /// let state = Ym2612State::new(7_670_454.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            current_port: 0,
            registers: Ym2612Storage::default(),
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
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ym2612State;
    /// use soundlog::chip::event::KeyState;
    ///
    /// let state = Ym2612State::new(7_670_454.0f32);
    /// let ch0 = state.channel(0).unwrap();
    /// assert_eq!(ch0.key_state, KeyState::Off);
    /// ```
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

    /// Set the current port for register writes
    ///
    /// YM2612 has two ports (0 and 1) that affect which channels are addressed.
    /// This is typically set by the VGM command that includes port information.
    ///
    /// # Arguments
    ///
    /// * `port` - Port number (0 or 1, other values are masked)
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
    /// YM2612 register layout:
    /// - Port 0: Channels 0-2
    /// - Port 1: Channels 3-5
    /// - Register A0-A2: F-number low 8 bits (per channel)
    /// - Register A4-A6: Block (3 bits) + F-number high 3 bits (per channel)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if both fnum and block registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= YM2612_CHANNELS {
            return None;
        }

        // Determine which port and local channel based on channel number
        // Channels 0-2 are on port 0
        // Channels 3-5 are on port 1
        let port = (channel / 3) as u8;
        let local_ch = channel % 3;

        // YM2612 register addresses for fnum/block
        // 0xA0-0xA2: F-number low 8 bits
        // 0xA4-0xA6: Block (bits 5-3) + F-number high 3 bits (bits 2-0)
        let fnum_low_reg = 0xA0 + local_ch as u8;
        let block_fnum_high_reg = 0xA4 + local_ch as u8;

        // Read from global register storage with port encoding
        let fnum_low_addr = ((port as u16) << 8) | fnum_low_reg as u16;
        let block_fnum_high_addr = ((port as u16) << 8) | block_fnum_high_reg as u16;

        let fnum_low = self.registers.read(fnum_low_addr)?;
        let block_fnum_high = self.registers.read(block_fnum_high_addr)?;

        // Extract fnum (11 bits total: 8 low + 3 high)
        let fnum = (fnum_low as u16) | ((block_fnum_high & 0x07) as u16) << 8;

        // Extract block (3 bits, bits 5-3 of block_fnum_high register)
        let block = (block_fnum_high >> 3) & 0x07;

        // Calculate actual frequency using Opn2Spec (prescaler=1.0, matches vgm2wav/Nuked-OPN2)
        let freq_hz = OpnaSpec::fnum_block_to_freq(fnum as u32, block, self.master_clock_hz).ok();

        Some(ToneInfo::new(fnum, block, freq_hz))
    }

    /// Handle key on/off register write (0x28)
    ///
    /// Register 0x28 format:
    /// - Bits 0-2: Channel selection (special encoding)
    ///   - 0, 1, 2: Channels 0-2 (port 0)
    ///   - 4, 5, 6: Channels 3-5 (port 1)
    /// - Bits 4-7: Slot/operator mask (which operators to key on)
    ///   - Bit 4: Operator 1
    ///   - Bit 5: Operator 2
    ///   - Bit 6: Operator 3
    ///   - Bit 7: Operator 4
    ///
    /// # Arguments
    ///
    /// * `value` - Value written to register 0x28
    ///
    /// # Returns
    ///
    /// Some(Vec<StateEvent>) if key state changed, None otherwise
    fn handle_key_on_off(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        // Extract channel from bits 0-2
        // Bit 2 effectively selects the port, bits 0-1 select channel within port
        let ch_bits = value & 0x07;
        let channel = match ch_bits {
            0..=2 => ch_bits as usize,           // Port 0, channels 0-2
            4..=6 => (ch_bits - 4 + 3) as usize, // Port 1, channels 3-5
            _ => return None,                    // Invalid channel (3, 7)
        };

        if channel >= YM2612_CHANNELS {
            return None;
        }

        // Slot mask determines key on/off (bits 4-7)
        // Any slot enabled means key on, all slots disabled means key off
        let slot_mask = (value >> 4) & 0x0F;
        let new_key_state = if slot_mask != 0 {
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
            _ => None, // No state change
        }
    }

    /// Handle frequency register writes
    ///
    /// Checks if the written register affects tone parameters and generates
    /// a ToneChange event if the channel is currently playing.
    ///
    /// # Arguments
    ///
    /// * `register` - Register address
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_frequency_register(&mut self, register: u8, _value: u8) -> Option<Vec<StateEvent>> {
        // Determine which channel this register affects based on current port
        let channel_offset = self.current_port as usize * 3;

        // F-number and Block registers
        let channel = match register {
            0xA0..=0xA2 => Some(channel_offset + (register - 0xA0) as usize),
            0xA4..=0xA6 => Some(channel_offset + (register - 0xA4) as usize),
            _ => None,
        };

        if let Some(ch) = channel
            && ch < YM2612_CHANNELS
        {
            // If key is on and tone registers changed, emit ToneChange event
            // Read from global register storage (already written by on_register_write)
            if self.channels[ch].key_state == KeyState::On
                && let Some(tone) = self.extract_tone(ch)
            {
                self.channels[ch].tone = Some(tone);
                return Some(vec![StateEvent::ToneChange {
                    channel: ch as u8,
                    tone,
                }]);
            }
        }

        None
    }
}

impl ChipState for Ym2612State {
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

        // Key On/Off register (0x28) - port independent
        if register == 0x28 {
            return self.handle_key_on_off(value);
        }

        // F-number and Block registers - port dependent
        if matches!(register, 0xA0..=0xA2 | 0xA4..=0xA6) {
            return self.handle_frequency_register(register, value);
        }

        // Other registers - store but don't generate events
        // These could include algorithm, feedback, operator parameters, etc.
        // We don't track them for tone info, but they're available if needed
        None
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.current_port = 0;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        YM2612_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ym2612_key_on_channel_0() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Set port 0 (channels 0-2)
        state.set_port(0);

        // Write fnum and block for channel 0
        state.on_register_write(0xA4, 0x22); // block=4, fnum_high=2
        state.on_register_write(0xA0, 0x6D); // fnum_low=0x6D

        // Key on channel 0 (all slots)
        let event = state.on_register_write(0x28, 0xF0); // ch=0, slots=all

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x26D);
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }

        // Verify channel state
        let ch = state.channel(0).unwrap();
        assert_eq!(ch.key_state, KeyState::On);
        assert!(ch.tone.is_some());
    }

    #[test]
    fn test_ym2612_key_on_channel_3() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Set port 1 (channels 3-5)
        state.set_port(1);

        // Write fnum and block for channel 3 (register 0xA0 on port 1)
        state.on_register_write(0xA4, 0x1A); // block=3, fnum_high=2
        state.on_register_write(0xA0, 0x80); // fnum_low=0x80

        // Key on channel 3 (ch_bits=4 for channel 3)
        let event = state.on_register_write(0x28, 0xF4);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 3);
            assert_eq!(tone.fnum, 0x280);
            assert_eq!(tone.block, 3);
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ym2612_key_off() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Set up and key on first
        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        // Key off channel 0
        let event = state.on_register_write(0x28, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));

        let ch = state.channel(0).unwrap();
        assert_eq!(ch.key_state, KeyState::Off);
    }

    #[test]
    fn test_ym2612_tone_change() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Set up and key on
        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        // Change tone while key is on
        let event = state.on_register_write(0xA0, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::ToneChange { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x280); // Updated fnum
            assert_eq!(tone.block, 4); // Block unchanged
        } else {
            panic!("Expected ToneChange event");
        }
    }

    #[test]
    fn test_ym2612_no_event_when_key_off() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);

        // Change tone while key is off (should not generate event)
        let event = state.on_register_write(0xA0, 0x80);
        assert!(event.is_none());
    }

    #[test]
    fn test_ym2612_invalid_channel() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Try to key on invalid channel 3 (ch_bits=3 is invalid)
        let event = state.on_register_write(0x28, 0xF3);
        assert!(event.is_none());

        // Try to key on invalid channel 7 (ch_bits=7 is invalid)
        let event = state.on_register_write(0x28, 0xF7);
        assert!(event.is_none());
    }

    #[test]
    fn test_ym2612_port_switching() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Write to channel 0 on port 0
        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);

        // Switch to port 1 and write to channel 3 (same register addresses)
        state.set_port(1);
        state.on_register_write(0xA4, 0x1A);
        state.on_register_write(0xA0, 0x80);

        // Key on both channels and verify they have different tones
        state.on_register_write(0x28, 0xF0); // Channel 0
        state.on_register_write(0x28, 0xF4); // Channel 3

        let ch0 = state.channel(0).unwrap();
        let ch3 = state.channel(3).unwrap();

        assert_eq!(ch0.tone.unwrap().fnum, 0x26D);
        assert_eq!(ch3.tone.unwrap().fnum, 0x280);
    }

    #[test]
    fn test_ym2612_reset() {
        let mut state = Ym2612State::new(7_670_454.0f32);

        // Set up some state
        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);

        // Reset
        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert_eq!(state.current_port(), 0);
        assert!(state.channel(0).unwrap().tone.is_none());
    }

    #[test]
    fn test_ym2612_channel_count() {
        let state = Ym2612State::new(7_670_454.0f32);
        assert_eq!(state.channel_count(), 6);
    }
}
