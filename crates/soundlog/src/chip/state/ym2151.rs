//! YM2151 (OPM) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YM2151 FM synthesis chip,
//! commonly found in arcade systems and some home computers.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use std::array;

/// YM2151 has 8 FM channels
const YM2151_CHANNELS: usize = 8;
/// YM2151 channel storage (256 register space)
///
/// YM2151 has a 256-register address space. ArrayStorage provides
/// fast access with reasonable memory usage (256 bytes).
pub type Ym2151Storage = ArrayStorage<u8, 256>;

/// YM2151 register state tracker
///
/// Tracks all 8 channels and their register state, detecting key on/off
/// events and extracting tone information (fnum, block).
///
/// # Register Layout
///
/// YM2151 has a single port with 256 registers:
/// - 0x08: Key On register (controls which channel and operators)
/// - 0x28-0x2F: Key Code (KC) - contains block and note information
/// - 0x30-0x37: Key Fraction (KF) - fine frequency tuning
#[derive(Debug, Clone)]
pub struct Ym2151State {
    /// Channel states for 8 FM channels
    channels: [ChannelState; YM2151_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Ym2151Storage,
}

impl Ym2151State {
    /// Create a new YM2151 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - Arcade systems: 3,579,545 Hz (NTSC colorburst)
    /// - Some systems: 4,000,000 Hz
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ym2151State;
    ///
    /// // Arcade system
    /// let state = Ym2151State::new(3_579_545.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: Ym2151Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-7)
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
    /// * `channel` - Channel index (0-7)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Extract fnum and block from register state for a channel
    ///
    /// YM2151 register layout:
    /// - Register 0x28-0x2F: KC (Key Code) - bits 6-4: octave/block, bits 3-0: note code
    /// - Register 0x30-0x37: KF (Key Fraction) - bits 7-2: fine frequency
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-7)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= YM2151_CHANNELS {
            return None;
        }

        // Read from global register storage
        // YM2151 register addresses
        // 0x28 + channel: KC (Key Code)
        // 0x30 + channel: KF (Key Fraction)
        let kc_reg = 0x28 + channel as u8;
        let kf_reg = 0x30 + channel as u8;

        let kc = self.registers.read(kc_reg)?;
        let kf = self.registers.read(kf_reg).unwrap_or(0);

        // Extract block, note and KF fraction and compute fnum/block for ToneInfo
        let block = (kc >> 4) & 0x07;
        let note_code = kc & 0x0F;
        let kf_fraction = (kf >> 2) & 0x3F;
        let fnum = (note_code as u32) * 64 + kf_fraction as u32;

        // Calculate actual frequency directly from KC/KF (note ratio + octave + KF fine tuning)
        let freq_hz = Some(Self::kc_kf_to_freq(kc, kf, self.master_clock_hz));

        Some(ToneInfo::new(fnum as u16, block, freq_hz))
    }

    /// Convert KC/KF to frequency using a YM2151 phase-increment mapping.
    ///
    /// `(KC - (KC >> 2)) * 64 + KF[7:2]` gives 768 steps per octave. The
    /// phase-increment curve is approximated by its 1299 base value and an
    /// exponential curve. The phase accumulator has `1024 * 2^16` units per
    /// cycle, with the base value referenced to octave 2.
    fn kc_kf_to_freq(kc: u8, kf: u8, master_clock_hz: f32) -> f32 {
        let key_code = u32::from(kc & 0x7F);
        let key_position = (key_code - (key_code >> 2)) * 64 + u32::from((kf >> 2) & 0x3F);
        let key_position = key_position.min(8 * 768 - 1);
        let octave = (key_position / 768) as i32;
        let fraction = (key_position % 768) as f32;

        let phase_increment = 1299.0f32 * 2.0f32.powf(fraction / 768.0);
        let octave_scale = 2.0f32.powi(octave - 2);
        phase_increment * octave_scale * master_clock_hz / (1024.0 * 65_536.0)
    }

    /// Handle key on/off register write (0x08)
    ///
    /// Register 0x08 format:
    /// - Bits 2-0: Channel (0-7)
    /// - Bits 6-3: Operator mask (M1, C1, M2, C2)
    ///   - If any operator is enabled: key on
    ///   - If all operators are disabled: key off
    ///
    /// # Arguments
    ///
    /// * `value` - Value written to register 0x08
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_key_on_off(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        // Extract channel from bits 2-0
        let channel = (value & 0x07) as usize;

        if channel >= YM2151_CHANNELS {
            return None;
        }

        // Operator mask is in bits 6-3
        let op_mask = (value >> 3) & 0x0F;
        let new_key_state = if op_mask != 0 {
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
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_frequency_register(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        // Determine which channel this register affects
        let channel = match register {
            0x28..=0x2F => Some((register - 0x28) as usize), // KC registers
            0x30..=0x37 => Some((register - 0x30) as usize), // KF registers
            _ => None,
        };

        if let Some(ch) = channel
            && ch < YM2151_CHANNELS
        {
            // Store the register value

            // If key is on and tone registers changed, emit ToneChange event
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

impl ChipState for Ym2151State {
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

        // Key On/Off register (0x08)
        if register == 0x08 {
            return self.handle_key_on_off(value);
        }

        // KC (Key Code) and KF (Key Fraction) registers
        if matches!(register, 0x28..=0x37) {
            return self.handle_frequency_register(register);
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
        YM2151_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ym2151_key_on_channel_0() {
        let mut state = Ym2151State::new(3_579_545.0f32);

        // Write KC and KF for channel 0
        state.on_register_write(0x28, 0x4C); // KC: note=A
        state.on_register_write(0x30, 0x00); // KF: no fraction

        // Key on channel 0, all operators
        let event = state.on_register_write(0x08, 0x78); // ch=0, all ops

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ym2151_key_off() {
        let mut state = Ym2151State::new(3_579_545.0f32);

        // Set up and key on
        state.on_register_write(0x28, 0x4C);
        state.on_register_write(0x08, 0x78);

        // Key off
        let event = state.on_register_write(0x08, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_ym2151_tone_change() {
        let mut state = Ym2151State::new(3_579_545.0f32);

        // Set up and key on
        state.on_register_write(0x28, 0x4C);
        state.on_register_write(0x08, 0x78);

        // Change tone while key is on
        let event = state.on_register_write(0x28, 0x50); // Change KC

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_ym2151_channel_count() {
        let state = Ym2151State::new(3_579_545.0f32);
        assert_eq!(state.channel_count(), 8);
    }

    #[test]
    fn test_ym2151_reset() {
        let mut state = Ym2151State::new(3_579_545.0f32);

        state.on_register_write(0x28, 0x4C);
        state.on_register_write(0x08, 0x78);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
    }

    #[test]
    fn test_kc_kf_4a_matches_440_hz() {
        let master_clock: f32 = 3_579_545.0f32;

        let freq = Ym2151State::kc_kf_to_freq(0x4A, 0x00, master_clock);
        let diff = (freq - 440.0f32).abs();
        assert!(
            diff <= 1.0f32,
            "Expected ≈440 Hz for KC=0x4A KF=0x00, got {} Hz (diff {})",
            freq,
            diff
        );
    }

    #[test]
    fn test_keycode_gaps_alias_the_following_code() {
        for (gap, following) in [(0x43, 0x44), (0x47, 0x48), (0x4B, 0x4C), (0x4F, 0x50)] {
            assert_eq!(
                Ym2151State::kc_kf_to_freq(gap, 0, 3_579_545.0),
                Ym2151State::kc_kf_to_freq(following, 0, 3_579_545.0),
                "KC mapping should alias 0x{gap:02X} to 0x{following:02X}"
            );
        }
    }

    #[test]
    fn test_kf_and_clock_scale_frequency() {
        let base = Ym2151State::kc_kf_to_freq(0x4A, 0, 3_579_545.0);
        let high_kf = Ym2151State::kc_kf_to_freq(0x4A, 0xFC, 3_579_545.0);
        let higher_clock = Ym2151State::kc_kf_to_freq(0x4A, 0, 4_000_000.0);

        assert!(high_kf > base);
        assert!((higher_clock / base - 4_000_000.0 / 3_579_545.0).abs() < 0.0001);
    }
}
