//! WonderSwan APU state implementation.
//!
//! The WonderSwan has 4 PCM channels with independent frequency and volume control.
//! Each channel can play back wave samples with programmable pitch.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// Number of channels in WonderSwan
const WONDERSWAN_CHANNELS: usize = 4;

/// Register storage for WonderSwan channels
type WonderSwanStorage = ArrayStorage<u8, 256>;

/// WonderSwan state tracker
///
/// Tracks register state and key on/off events for all 4 channels.
/// Each channel has frequency control (11-bit) and stereo volume control.
#[derive(Debug, Clone)]
pub struct WonderSwanState {
    /// Channel states for 4 PCM channels
    channels: [ChannelState; WONDERSWAN_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f64,
    /// Global register storage for all written registers
    registers: WonderSwanStorage,
}

impl WonderSwanState {
    /// Create a new WonderSwan state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 3,072,000 Hz (WonderSwan/WonderSwan Color)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::WonderSwanState;
    ///
    /// let state = WonderSwanState::new(3_072_000.0);
    /// ```
    pub fn new(master_clock_hz: f64) -> Self {
        Self {
            channels: [
                ChannelState::new(),
                ChannelState::new(),
                ChannelState::new(),
                ChannelState::new(),
            ],
            master_clock_hz,
            registers: WonderSwanStorage::default(),
        }
    }

    /// Handle frequency register write (0x80-0x87)
    fn handle_frequency_register(&mut self, channel: u8) -> Option<Vec<StateEvent>> {
        // If channel is on, emit tone change event
        if self.channels[channel as usize].key_state == KeyState::On {
            let tone = self.extract_tone(channel as usize);
            Some(vec![StateEvent::ToneChange { channel, tone }])
        } else {
            None
        }
    }

    /// Handle volume register write (0x88-0x8B)
    fn handle_volume_register(&mut self, channel: u8, value: u8) -> Option<Vec<StateEvent>> {
        // Volume is stored in the register we just wrote
        // For channel 0/2: bits 0-3=right, bits 4-7=left
        // For channel 1/3: bits 8-11=right, bits 12-15=left (but we get only low byte here)
        // We need to check the stored value
        let vol_left = (value >> 4) & 0x0F;
        let vol_right = value & 0x0F;

        let has_volume = vol_left > 0 || vol_right > 0;

        if has_volume && self.channels[channel as usize].key_state == KeyState::Off {
            // Key on
            self.channels[channel as usize].key_state = KeyState::On;
            let tone = self.extract_tone(channel as usize);
            Some(vec![StateEvent::KeyOn { channel, tone }])
        } else if !has_volume && self.channels[channel as usize].key_state == KeyState::On {
            // Key off
            self.channels[channel as usize].key_state = KeyState::Off;
            Some(vec![StateEvent::KeyOff { channel }])
        } else {
            None
        }
    }

    /// Handle audio control register write (0x90)
    fn handle_audio_control(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        // Check each channel's enable bit
        for channel in 0..WONDERSWAN_CHANNELS {
            let on = (value & (1 << channel)) != 0;

            if on && self.channels[channel].key_state == KeyState::Off {
                // Check if channel has volume
                let has_volume = self.has_volume(channel);
                if has_volume {
                    self.channels[channel].key_state = KeyState::On;
                    let tone = self.extract_tone(channel);
                    return Some(vec![StateEvent::KeyOn {
                        channel: channel as u8,
                        tone,
                    }]);
                }
            } else if !on && self.channels[channel].key_state == KeyState::On {
                self.channels[channel].key_state = KeyState::Off;
                return Some(vec![StateEvent::KeyOff {
                    channel: channel as u8,
                }]);
            }
        }

        None
    }

    /// Check if a channel has non-zero volume
    fn has_volume(&self, channel: usize) -> bool {
        let register = 0x88 + ((channel >> 1) as u8);
        // Read from global register storage
        if let Some(vol_reg) = self.registers.read(register) {
            let vol_left = (vol_reg >> 4) & 0x0F;
            let vol_right = vol_reg & 0x0F;
            vol_left > 0 || vol_right > 0
        } else {
            false
        }
    }

    /// Extract tone information from a channel's registers
    fn extract_tone(&self, channel: usize) -> ToneInfo {
        // Read from global register storage
        // Frequency is an 11-bit value in registers 0x80-0x87
        // Channel 0: 0x80, Channel 1: 0x82, Channel 2: 0x84, Channel 3: 0x86
        let freq_reg = 0x80 + (channel as u8 * 2);
        let freq_low = self.registers.read(freq_reg).unwrap_or(0) as u16;
        let freq_high = self.registers.read(freq_reg + 1).unwrap_or(0) as u16;
        let freq = freq_low | ((freq_high & 0x07) << 8);

        // Calculate frequency from freq value
        // WonderSwan formula: actual_freq = master_clock / (128 * (2048 - freq))
        // period = 2048 - freq
        let period = 2048 - freq;
        let freq_hz = if period > 0 {
            Some(self.master_clock_hz / (128.0 * period as f64))
        } else {
            None
        };

        ToneInfo {
            fnum: freq,
            block: 0, // WonderSwan doesn't use block/octave
            freq_hz,
        }
    }
}

impl ChipState for WonderSwanState {
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
            // Frequency registers (0x80-0x87)
            0x80..=0x87 => {
                let offset = register - 0x80;
                let channel = offset >> 1;
                self.handle_frequency_register(channel)
            }
            // Volume registers (0x88-0x8B)
            0x88 => {
                // Channel 0 volume (low byte)
                self.handle_volume_register(0, value)
            }
            0x89 => {
                // Channel 1 volume (high byte)
                self.handle_volume_register(1, value)
            }
            0x8A => {
                // Channel 2 volume (low byte)
                self.handle_volume_register(2, value)
            }
            0x8B => {
                // Channel 3 volume (high byte)
                self.handle_volume_register(3, value)
            }
            // Audio control register (0x90)
            0x90 => self.handle_audio_control(value),
            _ => {
                // Other registers - store but don't generate events
                None
            }
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        WONDERSWAN_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wonderswan_channel_count() {
        let state = WonderSwanState::new(3_072_000.0);
        assert_eq!(state.channel_count(), 4);
    }

    #[test]
    fn test_wonderswan_key_on() {
        let mut state = WonderSwanState::new(3_072_000.0);

        // Set frequency for channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);

        // Set volume for channel 0 (left=8, right=8)
        let event = state.on_register_write(0x88, 0x88);

        // Volume change should trigger key on
        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_wonderswan_key_on_via_control() {
        let mut state = WonderSwanState::new(3_072_000.0);

        // Set frequency for channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);

        // Set volume for channel 0
        state.on_register_write(0x88, 0x88);

        // Reset state for this test
        state.channels[0].key_state = KeyState::Off;

        // Enable channel 0 via control register
        let event = state.on_register_write(0x90, 0x01);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_wonderswan_key_off() {
        let mut state = WonderSwanState::new(3_072_000.0);

        // Set frequency and volume, enable channel
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);
        state.on_register_write(0x90, 0x01);

        // Disable channel 0 via control register
        let event = state.on_register_write(0x90, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_wonderswan_tone_change() {
        let mut state = WonderSwanState::new(3_072_000.0);

        // Set frequency and volume, enable channel
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);
        state.on_register_write(0x90, 0x01);

        // Change frequency while playing
        let event = state.on_register_write(0x80, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_wonderswan_multiple_channels() {
        let mut state = WonderSwanState::new(3_072_000.0);

        // Enable channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);
        state.on_register_write(0x90, 0x01);

        // Enable channel 2
        state.on_register_write(0x84, 0x00);
        state.on_register_write(0x85, 0x02);
        state.on_register_write(0x8A, 0x66);
        state.on_register_write(0x90, 0x05); // Enable ch0 and ch2

        assert_eq!(state.channels[0].key_state, KeyState::On);
        assert_eq!(state.channels[2].key_state, KeyState::On);
    }

    #[test]
    fn test_wonderswan_reset() {
        let mut state = WonderSwanState::new(3_072_000.0);

        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);
        state.on_register_write(0x90, 0x01);

        state.reset();

        assert_eq!(state.channels[0].key_state, KeyState::Off);
    }

    #[test]
    fn test_wonderswan_volume_key_off() {
        let mut state = WonderSwanState::new(3_072_000.0);

        // Enable channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);

        // Disable by setting volume to 0
        let event = state.on_register_write(0x88, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }
}
