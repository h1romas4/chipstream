//! YM2610B (OPNB) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YM2610B FM synthesis chip,
//! which has 6 FM channels, 3 PSG (SSG) channels, and ADPCM capabilities.
//! YM2610B is an enhanced version of YM2610 used in Neo Geo systems.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{self as fnumber, ChipTypeSpec};

/// YM2610B has 6 FM channels + 3 PSG channels = 9 total channels
/// (ADPCM channels are not tracked for tone)
const YM2610B_CHANNELS: usize = 9;
const YM2610B_FM_CHANNELS: usize = 6;

/// YM2610B recommended storage
pub type Ym2610bStorage = SparseStorage<u16, u8>;

/// YM2610B register state tracker
///
/// Tracks 6 FM channels and 3 PSG channels, detecting key on/off events
/// and extracting tone information.
///
/// # Port Handling
///
/// YM2610B has two ports:
/// - Port 0: Controls FM channels 0-2 and PSG
/// - Port 1: Controls FM channels 3-5
///
/// # Register Layout (FM part)
///
/// - 0xA0-0xA2: F-Number low 8 bits for channels 0-2 (per port)
/// - 0xA4-0xA6: Block (bits 5-3) + F-Number high 3 bits (bits 2-0)
/// - 0x28: Key On/Off register (port independent)
///
/// # Register Layout (PSG part)
///
/// - 0x00-0x05: Tone period registers
/// - 0x07: Mixer/Enable register
/// - 0x08-0x0A: Volume registers
#[derive(Debug, Clone)]
pub struct Ym2610bState {
    /// Channel states for 6 FM + 3 PSG channels
    channels: [ChannelState; YM2610B_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f64,
    /// Current port (0 or 1) for multi-byte register writes
    current_port: u8,
    /// Global register storage for all written registers
    /// Uses u16 address space to encode port (port 0: 0x00-0xFF, port 1: 0x100-0x1FF)
    registers: Ym2610bStorage,
}

impl Ym2610bState {
    /// Create a new YM2610B state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 8,000,000 Hz (Neo Geo standard)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ym2610bState;
    ///
    /// // Neo Geo
    /// let state = Ym2610bState::new(8_000_000.0);
    /// ```
    pub fn new(master_clock_hz: f64) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            current_port: 0,
            registers: Ym2610bStorage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5: FM, 6-8: PSG)
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
    /// * `channel` - Channel index (0-5: FM, 6-8: PSG)
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

    /// Extract fnum and block from register state for an FM channel
    ///
    /// YM2610B FM register layout:
    /// - Register 0xA0-0xA2: F-number low 8 bits
    /// - Register 0xA4-0xA6: Block (bits 5-3) + F-number high 3 bits (bits 2-0)
    ///
    /// # Arguments
    ///
    /// * `channel` - FM channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if both fnum and block registers have been written, None otherwise
    fn extract_fm_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= YM2610B_FM_CHANNELS {
            return None;
        }

        // Determine which port and local channel based on channel number
        // Channels 0-2 are on port 0, channels 3-5 are on port 1
        let port = (channel / 3) as u8;
        let local_ch = channel % 3;

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

        // Calculate actual frequency using OpnSpec
        let freq_hz =
            fnumber::OpnSpec::fnum_block_to_freq(fnum as u32, block, self.master_clock_hz).ok();

        Some(ToneInfo::new(fnum, block, freq_hz))
    }

    /// Handle key on/off register write (0x28)
    ///
    /// Register 0x28 format:
    /// - Bits 0-2: Channel selection (0-2 for port 0, 4-6 for port 1)
    /// - Bits 4-7: Slot/operator mask
    ///
    /// # Arguments
    ///
    /// * `value` - Value written to register 0x28
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_key_on_off(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let ch_bits = value & 0x07;
        let channel = match ch_bits {
            0..=2 => ch_bits as usize,
            4..=6 => (ch_bits - 4 + 3) as usize,
            _ => return None,
        };

        if channel >= YM2610B_FM_CHANNELS {
            return None;
        }

        let slot_mask = (value >> 4) & 0x0F;
        let new_key_state = if slot_mask != 0 {
            KeyState::On
        } else {
            KeyState::Off
        };

        let old_key_state = self.channels[channel].key_state;
        self.channels[channel].key_state = new_key_state;

        match (old_key_state, new_key_state) {
            (KeyState::Off, KeyState::On) => {
                let tone = self.extract_fm_tone(channel)?;
                self.channels[channel].tone = Some(tone);
                Some(vec![StateEvent::KeyOn {
                    channel: channel as u8,
                    tone,
                }])
            }
            (KeyState::On, KeyState::Off) => Some(vec![StateEvent::KeyOff {
                channel: channel as u8,
            }]),
            _ => None,
        }
    }

    /// Handle FM frequency register writes
    ///
    /// # Arguments
    ///
    /// * `register` - Register address
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_fm_frequency_register(
        &mut self,
        register: u8,
        _value: u8,
    ) -> Option<Vec<StateEvent>> {
        let channel_offset = self.current_port as usize * 3;

        let channel = match register {
            0xA0..=0xA2 => Some(channel_offset + (register - 0xA0) as usize),
            0xA4..=0xA6 => Some(channel_offset + (register - 0xA4) as usize),
            _ => None,
        };

        if let Some(ch) = channel
            && ch < YM2610B_FM_CHANNELS
        {
            // If key is on and tone registers changed, emit ToneChange event
            // Read from global register storage (already written by on_register_write)
            if self.channels[ch].key_state == KeyState::On
                && let Some(tone) = self.extract_fm_tone(ch)
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

    /// Extract tone from PSG registers
    ///
    /// # Arguments
    ///
    /// * `psg_channel` - PSG channel index (0-2)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_psg_tone(&self, psg_channel: usize) -> Option<ToneInfo> {
        if psg_channel >= 3 {
            return None;
        }

        // Read from global register storage
        let fine_reg = (psg_channel * 2) as u8;
        let coarse_reg = fine_reg + 1;

        let fine = self.registers.read(fine_reg as u16)?;
        let coarse = self.registers.read(coarse_reg as u16)?;

        let period = (fine as u16) | ((coarse as u16 & 0x0F) << 8);

        if period == 0 {
            return None;
        }

        // PSG frequency = master_clock / 2 / (16 * period)
        let freq_hz = self.master_clock_hz / 2.0 / (16.0 * period as f64);

        Some(ToneInfo::new(period, 0, Some(freq_hz)))
    }

    /// Handle PSG register writes
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0x00-0x0A)
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if tone or key state changed, None otherwise
    fn handle_psg_register(&mut self, register: u8, value: u8) -> Option<Vec<StateEvent>> {
        match register {
            0x00..=0x05 => {
                let psg_channel = (register / 2) as usize;
                let channel = YM2610B_FM_CHANNELS + psg_channel;

                if self.channels[channel].key_state == KeyState::On
                    && let Some(tone) = self.extract_psg_tone(psg_channel)
                {
                    self.channels[channel].tone = Some(tone);
                    return Some(vec![StateEvent::ToneChange {
                        channel: channel as u8,
                        tone,
                    }]);
                }
                None
            }
            0x07 => {
                let mut events = Vec::new();

                for psg_channel in 0..3 {
                    let channel = YM2610B_FM_CHANNELS + psg_channel;
                    let tone_disabled = (value & (1 << psg_channel)) != 0;
                    let new_key_state = if tone_disabled {
                        KeyState::Off
                    } else {
                        KeyState::On
                    };

                    let old_key_state = self.channels[channel].key_state;
                    self.channels[channel].key_state = new_key_state;

                    match (old_key_state, new_key_state) {
                        (KeyState::Off, KeyState::On) => {
                            if let Some(tone) = self.extract_psg_tone(psg_channel) {
                                self.channels[channel].tone = Some(tone);
                                events.push(StateEvent::KeyOn {
                                    channel: channel as u8,
                                    tone,
                                });
                            }
                        }
                        (KeyState::On, KeyState::Off) => {
                            events.push(StateEvent::KeyOff {
                                channel: channel as u8,
                            });
                        }
                        _ => {}
                    }
                }

                // Return all events
                if events.is_empty() {
                    None
                } else {
                    Some(events)
                }
            }
            0x08..=0x0A => {
                // Volume registers - store but don't generate events
                None
            }
            _ => None,
        }
    }
}

impl ChipState for Ym2610bState {
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

        // FM F-number and Block registers - port dependent
        if matches!(register, 0xA0..=0xA2 | 0xA4..=0xA6) {
            return self.handle_fm_frequency_register(register, value);
        }

        // PSG registers (only on port 0)
        if self.current_port == 0 && register <= 0x0A {
            return self.handle_psg_register(register, value);
        }

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
        YM2610B_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ym2610b_fm_key_on_port0() {
        let mut state = Ym2610bState::new(8_000_000.0);

        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);

        let event = state.on_register_write(0x28, 0xF0);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_ym2610b_fm_key_on_port1() {
        let mut state = Ym2610bState::new(8_000_000.0);

        state.set_port(1);
        state.on_register_write(0xA4, 0x1A);
        state.on_register_write(0xA0, 0x80);

        let event = state.on_register_write(0x28, 0xF4);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 3, .. }));
    }

    #[test]
    fn test_ym2610b_psg_tone() {
        let mut state = Ym2610bState::new(8_000_000.0);

        state.set_port(0);
        state.on_register_write(0x00, 0xCD);
        state.on_register_write(0x01, 0x02);

        let event = state.on_register_write(0x07, 0b11111110);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 6, .. }));
    }

    #[test]
    fn test_ym2610b_channel_count() {
        let state = Ym2610bState::new(8_000_000.0);
        assert_eq!(state.channel_count(), 9);
    }

    #[test]
    fn test_ym2610b_reset() {
        let mut state = Ym2610bState::new(8_000_000.0);

        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert_eq!(state.current_port(), 0);
    }

    #[test]
    fn test_ym2610b_fm_tone_change() {
        let mut state = Ym2610bState::new(8_000_000.0);

        state.set_port(0);
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        // Change frequency while key is on
        let event = state.on_register_write(0xA0, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_ym2610b_psg_tone_change() {
        let mut state = Ym2610bState::new(8_000_000.0);

        state.set_port(0);
        state.on_register_write(0x00, 0xCD);
        state.on_register_write(0x01, 0x02);
        state.on_register_write(0x07, 0b11111110);

        // Change PSG frequency while enabled
        let event = state.on_register_write(0x00, 0x50);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StateEvent::ToneChange { channel: 6, .. }
        ));
    }
}
