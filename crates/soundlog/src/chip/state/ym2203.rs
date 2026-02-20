//! YM2203 (OPN) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YM2203 FM synthesis chip,
//! which has 3 FM channels and 3 PSG (SSG) channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{self as fnumber, ChipTypeSpec};

/// YM2203 has 3 FM channels + 3 PSG channels = 6 total channels
const YM2203_CHANNELS: usize = 6;
const YM2203_FM_CHANNELS: usize = 3;

/// YM2203 recommended storage
pub type Ym2203Storage = SparseStorage<u8, u8>;

/// YM2203 register state tracker
///
/// Tracks 3 FM channels and 3 PSG channels, detecting key on/off events
/// and extracting tone information.
///
/// # Register Layout (FM part)
///
/// - 0xA0-0xA2: F-Number low 8 bits for FM channels 0-2
/// - 0xA4-0xA6: Key On (bit 5-4 for slot) + Block (bits 2-0 for block bits 2-0) + F-Number high 3 bits
/// - 0x28: Key On/Off register (special handling)
///
/// # Register Layout (PSG part)
///
/// - 0x00-0x05: Tone period registers (similar to AY-3-8910)
/// - 0x07: Mixer/Enable register
/// - 0x08-0x0A: Volume registers
#[derive(Debug, Clone)]
pub struct Ym2203State {
    /// Channel states for 6 channels (3 FM + 3 PSG)
    channels: [ChannelState; YM2203_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Ym2203Storage,
}

impl Ym2203State {
    /// Create a new YM2203 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 4,000,000 Hz (standard)
    /// - 3,993,600 Hz (some systems)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ym2203State;
    ///
    /// let state = Ym2203State::new(4_000_000.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: Ym2203Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-2: FM, 3-5: PSG)
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
    /// * `channel` - Channel index (0-2: FM, 3-5: PSG)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Extract fnum and block from register state for an FM channel
    ///
    /// YM2203 FM register layout:
    /// - Register 0xA0-0xA2: F-number low 8 bits
    /// - Register 0xA4-0xA6: Block (bits 5-3) + F-number high 3 bits (bits 2-0)
    ///
    /// # Arguments
    ///
    /// * `channel` - FM channel index (0-2)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if both fnum and block registers have been written, None otherwise
    fn extract_fm_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= YM2203_FM_CHANNELS {
            return None;
        }

        // Read from global register storage
        let fnum_low_reg = 0xA0 + channel as u8;
        let block_fnum_high_reg = 0xA4 + channel as u8;

        let fnum_low = self.registers.read(fnum_low_reg)?;
        let block_fnum_high = self.registers.read(block_fnum_high_reg)?;

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
    /// - Bits 0-1: Channel selection (0-2 for FM channels)
    /// - Bits 4-7: Slot/operator mask (which operators to key on)
    ///
    /// # Arguments
    ///
    /// * `value` - Value written to register 0x28
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_key_on_off(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let channel = (value & 0x03) as usize;

        if channel >= YM2203_FM_CHANNELS {
            return None;
        }

        // Slot mask determines key on/off (bits 4-7)
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
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_fm_frequency_register(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        let channel = match register {
            0xA0..=0xA2 => (register - 0xA0) as usize,
            0xA4..=0xA6 => (register - 0xA4) as usize,
            _ => return None,
        };

        if channel >= YM2203_FM_CHANNELS {
            return None;
        }

        if self.channels[channel].key_state == KeyState::On
            && let Some(tone) = self.extract_fm_tone(channel)
        {
            self.channels[channel].tone = Some(tone);
            return Some(vec![StateEvent::ToneChange {
                channel: channel as u8,
                tone,
            }]);
        }

        None
    }

    /// Extract tone from PSG registers
    ///
    /// PSG uses 12-bit period values stored in two registers
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
        // PSG registers: 0x00/0x01 (ch0), 0x02/0x03 (ch1), 0x04/0x05 (ch2)
        let fine_reg = (psg_channel * 2) as u8;
        let coarse_reg = fine_reg + 1;

        let fine = self.registers.read(fine_reg)?;
        let coarse = self.registers.read(coarse_reg)?;

        // 12-bit period
        let period = (fine as u16) | ((coarse as u16 & 0x0F) << 8);

        if period == 0 {
            return None;
        }

        // PSG frequency = master_clock / 2 / (16 * period)
        // The YM2203 SSG section receives the chip master clock pre-divided by 2
        // before the AY-compatible tone counters, identical to YM2608/YM2610B.
        let freq_hz = self.master_clock_hz / 2.0f32 / (16.0f32 * period as f32);

        // For PSG, we use period as "fnum" and 0 as block
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
                // Tone period registers
                let psg_channel = (register / 2) as usize;
                let channel = YM2203_FM_CHANNELS + psg_channel;

                // If enabled and tone changed, emit ToneChange
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
                // Mixer/Enable register
                // Bits 0-2: Tone enable (0=enabled, 1=disabled)
                // Bits 3-5: Noise enable (0=enabled, 1=disabled)
                let mut events = Vec::new();

                for psg_channel in 0..3 {
                    let channel = YM2203_FM_CHANNELS + psg_channel;
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

impl ChipState for Ym2203State {
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

        // Key On/Off register (0x28) - FM only
        if register == 0x28 {
            return self.handle_key_on_off(value);
        }

        // FM F-number and Block registers
        if matches!(register, 0xA0..=0xA2 | 0xA4..=0xA6) {
            return self.handle_fm_frequency_register(register);
        }

        // PSG registers
        if register <= 0x0A {
            return self.handle_psg_register(register, value);
        }

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
        YM2203_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ym2203_fm_key_on() {
        let mut state = Ym2203State::new(4_000_000.0f32);

        // Write fnum and block for FM channel 0
        state.on_register_write(0xA4, 0x22); // block=4, fnum_high=2
        state.on_register_write(0xA0, 0x6D); // fnum_low=0x6D

        // Key on channel 0
        let event = state.on_register_write(0x28, 0xF0); // ch=0, all slots

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
    }

    #[test]
    fn test_ym2203_fm_key_off() {
        let mut state = Ym2203State::new(4_000_000.0f32);

        // Set up and key on
        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        // Key off
        let event = state.on_register_write(0x28, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_ym2203_psg_tone() {
        let mut state = Ym2203State::new(4_000_000.0f32);

        // Write PSG channel 0 period
        state.on_register_write(0x00, 0xCD); // fine
        state.on_register_write(0x01, 0x02); // coarse

        // Enable PSG channel 0 (disable tone mute)
        let event = state.on_register_write(0x07, 0b11111110); // ch0 enabled

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 3); // First PSG channel
            assert_eq!(tone.fnum, 0x2CD);
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ym2203_channel_count() {
        let state = Ym2203State::new(4_000_000.0f32);
        assert_eq!(state.channel_count(), 6);
    }

    #[test]
    fn test_ym2203_reset() {
        let mut state = Ym2203State::new(4_000_000.0f32);

        state.on_register_write(0xA4, 0x22);
        state.on_register_write(0xA0, 0x6D);
        state.on_register_write(0x28, 0xF0);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }
}
