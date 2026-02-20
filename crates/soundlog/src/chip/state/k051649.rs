//! K051649 (Konami SCC) chip state implementation.
//!
//! This module provides state tracking for the Konami K051649 sound chip,
//! also known as SCC (Sound Creative Chip), which has 5 wavetable synthesis channels.
//!
//! # SCC1 Compatibility
//!
//! The SCC1 chip is functionally identical to the K051649. This module provides
//! type aliases (`Scc1State` and `Scc1Storage`) for compatibility with code that
//! references the SCC1 chip name.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// K051649 has 5 wavetable channels
const K051649_CHANNELS: usize = 5;

/// K051649 recommended storage
pub type K051649Storage = SparseStorage<u8, u8>;

/// K051649 register state tracker
///
/// Tracks all 5 wavetable channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// Wave RAM (0x00-0x7F):
/// - 0x00-0x1F: Channel 0 waveform (32 bytes, 4-bit samples)
/// - 0x20-0x3F: Channel 1 waveform
/// - 0x40-0x5F: Channel 2 waveform
/// - 0x60-0x7F: Channel 3/4 waveform (shared)
///
/// Frequency (0x80-0x89):
/// - 0x80-0x81: Channel 0 frequency (12-bit, little-endian)
/// - 0x82-0x83: Channel 1 frequency
/// - 0x84-0x85: Channel 2 frequency
/// - 0x86-0x87: Channel 3 frequency
/// - 0x88-0x89: Channel 4 frequency
///
/// Volume (0x8A-0x8E):
/// - 0x8A: Channel 0 volume (4-bit)
/// - 0x8B: Channel 1 volume
/// - 0x8C: Channel 2 volume
/// - 0x8D: Channel 3 volume
/// - 0x8E: Channel 4 volume
///
/// Channel Enable (0x8F):
/// - Bit 0: Channel 0 enable
/// - Bit 1: Channel 1 enable
/// - Bit 2: Channel 2 enable
/// - Bit 3: Channel 3 enable
/// - Bit 4: Channel 4 enable
///
/// Test Register (0xE0):
/// - 0xE0: Test register
///
/// Deformation Register (0xE1):
/// - 0xE1: Waveform deformation control
#[derive(Debug, Clone)]
pub struct K051649State {
    /// Channel states for 5 channels
    channels: [ChannelState; K051649_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: K051649Storage,
}

impl K051649State {
    /// Create a new K051649 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 1,789,773 Hz (standard)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::K051649State;
    ///
    /// let state = K051649State::new(1_789_773.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: K051649Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-4)
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
    /// * `channel` - Channel index (0-4)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Calculate frequency in Hz from SCC period value
    ///
    /// # Arguments
    ///
    /// * `period` - 12-bit period value
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn hz_scc(&self, period: u16) -> f32 {
        let period = period as f32;
        let denom = 32.0 * (period + 1.0);
        self.master_clock_hz / denom
    }

    /// Extract tone from channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-4)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= K051649_CHANNELS {
            return None;
        }

        // Read from global register storage
        // Frequency registers (little-endian 12-bit)
        let freq_low_reg = 0x80 + (channel as u8 * 2);
        let freq_high_reg = 0x81 + (channel as u8 * 2);

        let freq_low = self.registers.read(freq_low_reg)?;
        let freq_high = self.registers.read(freq_high_reg)?;

        // 12-bit frequency (little-endian)
        let freq = (freq_low as u16) | ((freq_high as u16 & 0x0F) << 8);

        if freq == 0 {
            return None;
        }

        let freq_hz = self.hz_scc(freq);

        Some(ToneInfo::new(freq, 0, Some(freq_hz)))
    }

    /// Handle frequency register writes (0x80-0x89)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0x80-0x89)
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while enabled, None otherwise
    fn handle_frequency_register(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        let channel = ((register - 0x80) / 2) as usize;

        if channel >= K051649_CHANNELS {
            return None;
        }

        // If channel is enabled and tone changed, emit ToneChange
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

    /// Handle volume register writes (0x8A-0x8E)
    ///
    /// # Arguments
    ///
    /// * `register` - Register address (0x8A-0x8E)
    ///
    /// # Returns
    ///
    /// None (volume changes don't generate events)
    fn handle_volume_register(&mut self, register: u8) -> Option<Vec<StateEvent>> {
        let channel = (register - 0x8A) as usize;

        if channel >= K051649_CHANNELS {
            return None;
        }

        None
    }

    /// Handle channel enable register write (0x8F)
    ///
    /// Register 0x8F format:
    /// - Bit 0: Channel 0 enable
    /// - Bit 1: Channel 1 enable
    /// - Bit 2: Channel 2 enable
    /// - Bit 3: Channel 3 enable
    /// - Bit 4: Channel 4 enable
    ///
    /// # Arguments
    ///
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) for the first channel that changed state, None otherwise
    fn handle_enable_register(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let mut events = Vec::new();

        for channel in 0..K051649_CHANNELS {
            let enabled = (value & (1 << channel)) != 0;
            let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

            let old_key_state = self.channels[channel].key_state;
            self.channels[channel].key_state = new_key_state;

            match (old_key_state, new_key_state) {
                (KeyState::Off, KeyState::On) => {
                    if let Some(tone) = self.extract_tone(channel) {
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

        if events.is_empty() {
            None
        } else {
            Some(events)
        }
    }

    /// Map a VGM SCC1-style (port, register, value) tuple into the K051649 internal
    /// register space. This helper is pure and does not borrow self; it encodes the
    /// canonical VGM -> K051649 register mapping used by the callback stream.
    /// VGM port format:
    ///  0x00 - waveform
    ///  0x01 - frequency
    ///  0x02 - volume
    ///  0x03 - key on/off
    ///  0x04 - waveform (0x00 used to do SCC access, 0x04 SCC+)
    ///  0x05 - test register
    /// Returns (mapped_register, mapped_value).
    pub(crate) fn map_vgm_to_k051649_register(port: u8, register: u8, value: u8) -> (u8, u8) {
        match port {
            // Waveform RAM (0x00 - 0x7F) and SCC+ waveform (0x04) map directly.
            0x00 | 0x04 => (register, value),

            // Frequency registers -> 0x80 - 0x89
            0x01 => (0x80u8.wrapping_add(register), value),

            // Volume registers -> 0x8A - 0x8E
            0x02 => (0x8Au8.wrapping_add(register), value),

            // Key on/off -> channel enable register 0x8F.
            // If 'register' is a small channel index (0..4) treat as index and encode mask,
            // otherwise assume 'value' already conveys the mask.
            0x03 => {
                // Key on/off -> channel enable register 0x8F.
                // Always write the provided `value` to 0x8F (no special-case channel-index encoding).
                (0x8Fu8, value)
            }

            // Test registers -> 0xE0+
            0x05 => (0xE0u8.wrapping_add(register), value),

            // Unknown ports: fall back to raw register/value.
            _ => (register, value),
        }
    }
}

impl ChipState for K051649State {
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
            // Wave RAM (0x00-0x7F) - store but don't generate events
            0x00..=0x7F => {
                // Waveform data for channels 0-3 (ch4 shares with ch3)
                // We could track which channel's waveform changed, but we don't
                // generate events for waveform changes
                None
            }

            // Frequency registers (0x80-0x89)
            0x80..=0x89 => self.handle_frequency_register(register),

            // Volume registers (0x8A-0x8E)
            0x8A..=0x8E => self.handle_volume_register(register),

            // Channel enable register (0x8F)
            0x8F => self.handle_enable_register(value),

            // Test register (0xE0)
            0xE0 => None,

            // Deformation register (0xE1)
            0xE1 => None,

            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        K051649_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_k051649_enable_channel() {
        let mut state = K051649State::new(1_789_773.0f32);

        // Set frequency for channel 0
        state.on_register_write(0x80, 0x00); // Freq low
        state.on_register_write(0x81, 0x08); // Freq high (0x800)

        // Enable channel 0
        let event = state.on_register_write(0x8F, 0x01);

        assert!(event.is_some());

        if let Some(ref events) = event
            && events.len() == 1
            && let StateEvent::KeyOn { channel, tone } = &events[0]
        {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x800);
            assert!(tone.freq_hz.is_some());
        }
    }

    #[test]
    fn test_k051649_disable_channel() {
        let mut state = K051649State::new(1_789_773.0f32);

        // Set up and enable channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x08);
        state.on_register_write(0x8F, 0x01);

        // Disable channel 0
        let event = state.on_register_write(0x8F, 0x00);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
        }
    }

    #[test]
    fn test_k051649_tone_change() {
        let mut state = K051649State::new(1_789_773.0f32);

        // Set up and enable channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x08);
        state.on_register_write(0x8F, 0x01);

        // Change frequency while enabled
        let event = state.on_register_write(0x80, 0xFF);

        assert!(event.is_some());

        if let Some(ref events) = event
            && events.len() == 1
            && let StateEvent::ToneChange { channel, tone } = &events[0]
        {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x8FF);
        }
    }

    #[test]
    fn test_k051649_multiple_channels() {
        let mut state = K051649State::new(1_789_773.0f32);

        // Set up channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x08);

        // Set up channel 2
        state.on_register_write(0x84, 0x00);
        state.on_register_write(0x85, 0x04);

        // Enable both channels
        state.on_register_write(0x8F, 0x05); // Bits 0 and 2

        let ch0 = state.channel(0).unwrap();
        let ch2 = state.channel(2).unwrap();

        assert_eq!(ch0.key_state, KeyState::On);
        assert_eq!(ch2.key_state, KeyState::On);
        assert_eq!(ch0.tone.unwrap().fnum, 0x800);
        assert_eq!(ch2.tone.unwrap().fnum, 0x400);
    }

    #[test]
    fn test_k051649_channel_count() {
        let state = K051649State::new(1_789_773.0f32);
        assert_eq!(state.channel_count(), 5);
    }

    #[test]
    fn test_k051649_reset() {
        let mut state = K051649State::new(1_789_773.0f32);

        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x08);
        state.on_register_write(0x8F, 0x01);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }

    #[test]
    fn test_k051649_zero_frequency() {
        let mut state = K051649State::new(1_789_773.0f32);

        // Set zero frequency
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x00);

        // Enable channel 0
        let event = state.on_register_write(0x8F, 0x01);

        // Zero frequency should not generate KeyOn event
        assert!(
            event.is_none()
                || (event
                    .as_ref()
                    .map(|e| e.len() == 1 && matches!(&e[0], StateEvent::KeyOff { .. }))
                    .unwrap_or(false))
        );
    }

    #[test]
    fn test_hz_scc_period_formula() {
        // Verify the period-based formula produces a frequency near A4 for a known case.
        // Use the requested master clock of 3_579_545 Hz.
        let state = K051649State::new(3_579_545.0f32);

        // Period value (12-bit) to test: 0x0FE
        let period: u16 = 0x0FE;
        let freq_hz = state.hz_scc(period);

        // Expected: approximately A4 (440Hz). Allow a small tolerance.
        let diff = (freq_hz - 440.0f32).abs();
        assert!(
            diff < 2.0,
            "hz_scc({:#X}) produced {:.6} Hz, which differs from 440 Hz by {:.6} Hz",
            period,
            freq_hz,
            diff
        );
    }
}
