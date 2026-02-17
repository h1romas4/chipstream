//! SN76489 (PSG) chip state implementation.
//!
//! This module provides state tracking for the SN76489 Programmable Sound Generator,
//! commonly found in Sega Master System, Game Gear, and BBC Micro.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// SN76489 has 4 channels (3 tone + 1 noise)
const SN76489_CHANNELS: usize = 4;

/// Noise channel index
const NOISE_CHANNEL: usize = 3;

/// SN76489 recommended storage
pub type Sn76489Storage = ArrayStorage<u8, 16>;

/// SN76489 register state tracker
///
/// Tracks all 4 channels (3 tone channels + 1 noise channel) and their register state.
///
/// # Register Layout
///
/// SN76489 uses a latch-based register interface:
/// - Latch byte (bit 7 = 1): selects channel and register type
///   - Bits 6-5: Channel (0-2 = tone, 3 = noise)
///   - Bit 4: Type (0 = frequency, 1 = volume/attenuation)
/// - Data byte (bit 7 = 0): provides additional data bits
///
/// Frequency is 10-bit for tone channels.
/// Volume is 4-bit attenuation (0 = max, 15 = silent).
///
/// # TODO: Key on/off detection details
///
/// The SN76489 doesn't have explicit key on/off commands.
/// Key state is inferred from volume changes:
/// - Volume set to non-silent (0-14) after being silent (15) = key on
/// - Volume set to silent (15) after being non-silent = key off
///
/// This heuristic may need refinement based on actual music data patterns.
#[derive(Debug, Clone)]
pub struct Sn76489State {
    /// Channel states for 4 channels
    channels: [ChannelState; SN76489_CHANNELS],
    /// Current latched channel and register type
    current_latch: Option<(u8, bool)>, // (channel, is_volume)
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Sn76489Storage,
}

impl Sn76489State {
    /// Create a new SN76489 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - NTSC systems: 3,579,545 Hz
    /// - PAL systems: 3,546,893 Hz
    /// - BBC Micro: 4,000,000 Hz
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Sn76489State;
    ///
    /// // Sega Master System (NTSC)
    /// let state = Sn76489State::new(3_579_545.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            current_latch: None,
            master_clock_hz,
            registers: Sn76489Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3, where 3 is noise)
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

    /// Extract tone information for a tone channel
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-2 for tone channels)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if frequency has been set, None otherwise
    ///
    /// # TODO: Frequency calculation
    ///
    /// The SN76489 frequency calculation is:
    /// freq_hz = master_clock / (32 * freq_value)
    /// where freq_value is a 10-bit value (0-1023)
    ///
    /// For now, we store the raw freq_value as fnum and use block=0.
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= NOISE_CHANNEL {
            return None; // Noise channel doesn't have traditional tone
        }

        // Read from global register storage
        // Register layout per channel:
        // - register 0: frequency low 4 bits
        // - register 1: frequency high 6 bits
        let base_reg = (channel * 2) as u8;
        let freq_low = self.registers.read(base_reg)?;
        let freq_high = self.registers.read(base_reg + 1).unwrap_or(0);

        // Combine to 10-bit frequency value
        let freq_value = ((freq_high as u16) << 4) | (freq_low as u16);

        if freq_value == 0 {
            return None; // Division by zero protection
        }

        // Calculate actual frequency
        // Formula: f = master_clock / (32 * freq_value)
        let freq_hz = Some(self.master_clock_hz / (32.0f32 * freq_value as f32));

        // Store freq_value as fnum, use block=0 for PSG
        Some(ToneInfo::new(freq_value, 0, freq_hz))
    }

    /// Handle register write
    ///
    /// SN76489 uses a latch-based interface where the first byte (bit 7 = 1)
    /// latches the channel and register type, and subsequent data bytes provide values.
    ///
    /// # Arguments
    ///
    /// * `value` - Value written to the chip
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if a notable event occurred, None otherwise
    fn handle_write(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        if (value & 0x80) != 0 {
            // Latch byte
            let channel = ((value >> 5) & 0x03) as usize;
            let is_volume = (value & 0x10) != 0;
            let data = value & 0x0F;

            self.current_latch = Some((channel as u8, is_volume));

            if is_volume {
                // Volume/attenuation update
                return self.handle_volume_change(channel, data);
            } else {
                // Frequency low 4 bits
                if channel < SN76489_CHANNELS {
                    let base_reg = (channel * 2) as u8;
                    self.registers.write(base_reg, data);
                }
            }
        } else {
            // Data byte (bit 7 = 0)
            if let Some((channel, is_volume)) = self.current_latch {
                let channel = channel as usize;
                if channel < SN76489_CHANNELS {
                    if is_volume {
                        // Additional volume data (unlikely for SN76489)
                        return self.handle_volume_change(channel, value & 0x0F);
                    } else {
                        // Frequency high 6 bits
                        let base_reg = (channel * 2) as u8;
                        self.registers.write(base_reg + 1, value);
                        return self.handle_frequency_change(channel);
                    }
                }
            }
        }

        None
    }

    /// Handle volume/attenuation change
    ///
    /// # TODO: Key on/off heuristic
    ///
    /// Currently uses a simple heuristic:
    /// - Setting volume to non-silent (0-14) when previously silent = key on
    /// - Setting volume to silent (15) when previously non-silent = key off
    ///
    /// This may need adjustment based on actual game music behavior.
    fn handle_volume_change(&mut self, channel: usize, attenuation: u8) -> Option<Vec<StateEvent>> {
        if channel >= SN76489_CHANNELS {
            return None;
        }

        // Volume is stored at register offset 8 + channel
        let vol_reg = (8 + channel) as u8;
        let old_attenuation = self.registers.read(vol_reg).unwrap_or(15);
        self.registers.write(vol_reg, attenuation);

        let old_silent = old_attenuation == 15;
        let new_silent = attenuation == 15;

        match (old_silent, new_silent) {
            (true, false) => {
                // Volume changed from silent to audible = key on
                self.channels[channel].key_state = KeyState::On;
                if channel < NOISE_CHANNEL
                    && let Some(tone) = self.extract_tone(channel)
                {
                    self.channels[channel].tone = Some(tone);
                    return Some(vec![StateEvent::KeyOn {
                        channel: channel as u8,
                        tone,
                    }]);
                }
            }
            (false, true) => {
                // Volume changed from audible to silent = key off
                self.channels[channel].key_state = KeyState::Off;
                return Some(vec![StateEvent::KeyOff {
                    channel: channel as u8,
                }]);
            }
            _ => {}
        }

        None
    }

    /// Handle frequency change
    fn handle_frequency_change(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= NOISE_CHANNEL {
            return None; // Noise channel handled separately
        }

        // Only emit ToneChange if key is on
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

impl ChipState for Sn76489State {
    type Register = u8;
    type Value = u8;

    fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
        self.registers.read(register)
    }

    fn on_register_write(
        &mut self,
        _register: Self::Register,
        value: Self::Value,
    ) -> Option<Vec<StateEvent>> {
        // SN76489 uses a single write port, register parameter is ignored
        // Register writes are handled by handle_write() which writes to the correct addresses
        self.handle_write(value)
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.current_latch = None;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        SN76489_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sn76489_latch_frequency() {
        let mut state = Sn76489State::new(3_579_545.0f32);

        // Latch channel 0, frequency, low 4 bits
        state.on_register_write(0, 0x80 | 0x0D); // ch=0, freq, data=0x0D

        // Data byte: high 6 bits
        state.on_register_write(0, 0x26); // high 6 bits = 0x26

        // Verify registers are stored in global storage
        // Channel 0 uses base_reg = 0*2 = 0, so registers 0 and 1
        assert_eq!(state.read_register(0), Some(0x0D));
        assert_eq!(state.read_register(1), Some(0x26));

        // Verify channel state
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
    }

    #[test]
    fn test_sn76489_volume_key_on() {
        let mut state = Sn76489State::new(3_579_545.0f32);

        // Set frequency first
        state.on_register_write(0, 0x80 | 0x0D);
        state.on_register_write(0, 0x26);

        // Set volume (attenuation=0, max volume) - should trigger key on
        let event = state.on_register_write(0, 0x90); // ch=0, volume, att=0

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { .. }));
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
    }

    #[test]
    fn test_sn76489_volume_key_off() {
        let mut state = Sn76489State::new(3_579_545.0f32);

        // Set up and key on
        state.on_register_write(0, 0x80 | 0x0D);
        state.on_register_write(0, 0x26);
        state.on_register_write(0, 0x90);

        // Set to silent (attenuation=15)
        let event = state.on_register_write(0, 0x90 | 0x0F);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
    }

    #[test]
    fn test_sn76489_frequency_change() {
        let mut state = Sn76489State::new(3_579_545.0f32);

        // Set up and key on
        state.on_register_write(0, 0x80 | 0x0D);
        state.on_register_write(0, 0x26);
        state.on_register_write(0, 0x90);

        // Change frequency while playing (latch + data)
        state.on_register_write(0, 0x80 | 0x05); // Latch low bits
        let event = state.on_register_write(0, 0x20); // Data byte with high bits

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_sn76489_channel_count() {
        let state = Sn76489State::new(3_579_545.0f32);
        assert_eq!(state.channel_count(), 4);
    }

    #[test]
    fn test_sn76489_reset() {
        let mut state = Sn76489State::new(3_579_545.0f32);

        state.on_register_write(0, 0x80 | 0x0D);
        state.on_register_write(0, 0x90);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.current_latch.is_none());
    }

    #[test]
    fn test_sn76489_multiple_channels() {
        let mut state = Sn76489State::new(3_579_545.0f32);

        // Channel 0
        state.on_register_write(0, 0x80 | 0x0D); // ch=0, freq
        state.on_register_write(0, 0x90); // ch=0, vol

        // Channel 1
        state.on_register_write(0, 0xA0 | 0x05); // ch=1, freq
        state.on_register_write(0, 0xB0 | 0x02); // ch=1, vol

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(1).unwrap().key_state, KeyState::On);
    }
}
