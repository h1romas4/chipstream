//! AY-3-8910 PSG chip state implementation.
//!
//! This module provides state tracking for the General Instrument AY-3-8910
//! Programmable Sound Generator, commonly found in ZX Spectrum, MSX, and
//! arcade systems. It has 3 tone channels and 1 noise channel.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use std::array;

/// AY-3-8910 has 3 tone channels
const AY8910_CHANNELS: usize = 3;

/// AY-3-8910 recommended storage
pub type Ay8910Storage = SparseStorage<u8, u8>;

/// AY-3-8910 register state tracker
///
/// Tracks all 3 tone channels and their register state, detecting key on/off
/// events and extracting tone information.
/// A tone is considered active when its mixer bit is enabled, its period is
/// nonzero, and its volume is nonzero or envelope mode is selected. Envelope
/// amplitude and noise-only output are not tracked. Until written, mixer bits
/// are considered disabled and volume and period registers zero.
///
/// # Register Layout
///
/// - 0x00-0x01: Channel A period (fine, coarse)
/// - 0x02-0x03: Channel B period (fine, coarse)
/// - 0x04-0x05: Channel C period (fine, coarse)
/// - 0x06: Noise period
/// - 0x07: Mixer control (tone/noise enable)
/// - 0x08: Channel A volume
/// - 0x09: Channel B volume
/// - 0x0A: Channel C volume
/// - 0x0B-0x0C: Envelope period (fine, coarse)
/// - 0x0D: Envelope shape
/// - 0x0E-0x0F: I/O ports A and B
#[derive(Debug, Clone)]
pub struct Ay8910State {
    /// Channel states for 3 tone channels
    channels: [ChannelState; AY8910_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: Ay8910Storage,
}

impl Ay8910State {
    /// Create a new AY-3-8910 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 1,789,773 Hz (ZX Spectrum, MSX)
    /// - 1,500,000 Hz (some arcade systems)
    /// - 2,000,000 Hz (other arcade systems)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ay8910State;
    ///
    /// // ZX Spectrum / MSX
    /// let state = Ay8910State::new(1_789_773.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: Ay8910Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-2)
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
    /// * `channel` - Channel index (0-2)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Calculate frequency in Hz from AY-3-8910 period value
    ///
    /// # Arguments
    ///
    /// * `period` - 12-bit period value
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn hz_ay(&self, period: u16) -> f32 {
        if period == 0 {
            0.0f32
        } else {
            self.master_clock_hz / 16.0_f32 / period as f32
        }
    }

    /// Extract tone from channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-2)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= AY8910_CHANNELS {
            return None;
        }

        // Read from global register storage
        // Register addresses for period
        let fine_reg = (channel * 2) as u8;
        let coarse_reg = fine_reg + 1;

        let fine = self.registers.read(fine_reg).unwrap_or(0);
        let coarse = self.registers.read(coarse_reg).unwrap_or(0);

        // 12-bit period: fine (8 bits) + coarse (4 bits)
        let period = (fine as u16) | ((coarse as u16 & 0x0F) << 8);

        if period == 0 {
            return None;
        }

        let freq_hz = self.hz_ay(period);

        Some(ToneInfo::new(period, 0, Some(freq_hz)))
    }

    fn update_channel(&mut self, channel: usize) -> Option<StateEvent> {
        let mixer = self.registers.read(0x07).unwrap_or(0xff);
        let volume = self.registers.read(0x08 + channel as u8).unwrap_or(0);
        let tone = if mixer & (1 << channel) == 0 && volume & 0x1f != 0 {
            self.extract_tone(channel)
        } else {
            None
        };
        let state = &mut self.channels[channel];
        let event = match (state.key_state, state.tone, tone) {
            (KeyState::Off, _, Some(tone)) => Some(StateEvent::KeyOn {
                channel: channel as u8,
                tone,
            }),
            (KeyState::On, _, None) => Some(StateEvent::KeyOff {
                channel: channel as u8,
            }),
            (KeyState::On, previous, Some(tone)) if previous != Some(tone) => {
                Some(StateEvent::ToneChange {
                    channel: channel as u8,
                    tone,
                })
            }
            _ => None,
        };
        state.key_state = if tone.is_some() {
            KeyState::On
        } else {
            KeyState::Off
        };
        state.tone = tone;
        event
    }
}

impl ChipState for Ay8910State {
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
            // Channel period registers (0x00-0x05)
            0x00..=0x05 => self
                .update_channel((register / 2) as usize)
                .map(|event| vec![event]),

            // Noise period register (0x06)
            0x06 => {
                // Noise doesn't have tone info, just store
                None
            }

            // Mixer control register (0x07)
            0x07 => {
                let events: Vec<_> = (0..AY8910_CHANNELS)
                    .filter_map(|channel| self.update_channel(channel))
                    .collect();
                (!events.is_empty()).then_some(events)
            }

            // Volume registers (0x08-0x0A)
            0x08..=0x0A => self
                .update_channel((register - 0x08) as usize)
                .map(|event| vec![event]),

            // Envelope period registers (0x0B-0x0C)
            0x0B | 0x0C => {
                // Envelope doesn't affect tone, just store
                None
            }

            // Envelope shape register (0x0D)
            0x0D => {
                // Envelope shape doesn't affect tone
                None
            }

            // I/O port registers (0x0E-0x0F)
            0x0E | 0x0F => {
                // I/O ports don't affect audio
                None
            }

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
        AY8910_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ay8910_tone_enable() {
        let mut state = Ay8910State::new(1_789_773.0f32);

        // Set period for channel A
        state.on_register_write(0x00, 0xCD); // Fine
        state.on_register_write(0x01, 0x02); // Coarse
        state.on_register_write(0x08, 0x0f);

        // Enable channel A tone (disable bit 0)
        let event = state.on_register_write(0x07, 0b11111110);

        assert!(event.is_some());

        if let Some(ref events) = event
            && events.len() == 1
            && let StateEvent::KeyOn { channel, tone } = &events[0]
        {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x2CD);
            assert!(tone.freq_hz.is_some());
        }
    }

    #[test]
    fn test_ay8910_tone_disable() {
        let mut state = Ay8910State::new(1_789_773.0f32);

        // Set up and enable channel A
        state.on_register_write(0x00, 0xCD);
        state.on_register_write(0x01, 0x02);
        state.on_register_write(0x08, 0x0f);
        state.on_register_write(0x07, 0b11111110);

        // Disable channel A tone (set bit 0)
        let event = state.on_register_write(0x07, 0b11111111);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
        }
    }

    #[test]
    fn test_ay8910_tone_change() {
        let mut state = Ay8910State::new(1_789_773.0f32);

        // Set up and enable channel A
        state.on_register_write(0x00, 0xCD);
        state.on_register_write(0x01, 0x02);
        state.on_register_write(0x08, 0x0f);
        state.on_register_write(0x07, 0b11111110);

        // Change period while enabled
        let event = state.on_register_write(0x00, 0x80);

        assert!(event.is_some());

        if let Some(ref events) = event
            && events.len() == 1
            && let StateEvent::ToneChange { channel, tone } = &events[0]
        {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x280);
        }
    }

    #[test]
    fn test_ay8910_multiple_channels() {
        let mut state = Ay8910State::new(1_789_773.0f32);

        // Set up channel A
        state.on_register_write(0x00, 0xCD);
        state.on_register_write(0x01, 0x02);

        // Set up channel B
        state.on_register_write(0x02, 0x80);
        state.on_register_write(0x03, 0x01);
        state.on_register_write(0x08, 0x0f);
        state.on_register_write(0x09, 0x0f);

        // Enable both channels (disable bits 0 and 1)
        state.on_register_write(0x07, 0b11111100);

        let ch_a = state.channel(0).unwrap();
        let ch_b = state.channel(1).unwrap();

        assert_eq!(ch_a.key_state, KeyState::On);
        assert_eq!(ch_b.key_state, KeyState::On);
        assert_eq!(ch_a.tone.unwrap().fnum, 0x2CD);
        assert_eq!(ch_b.tone.unwrap().fnum, 0x180);
    }

    #[test]
    fn test_ay8910_channel_count() {
        let state = Ay8910State::new(1_789_773.0f32);
        assert_eq!(state.channel_count(), 3);
    }

    #[test]
    fn test_ay8910_reset() {
        let mut state = Ay8910State::new(1_789_773.0f32);

        state.on_register_write(0x00, 0xCD);
        state.on_register_write(0x01, 0x02);
        state.on_register_write(0x08, 0x0f);
        state.on_register_write(0x07, 0b11111110);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }

    #[test]
    fn test_ay8910_zero_period() {
        let mut state = Ay8910State::new(1_789_773.0f32);

        // Set zero period
        state.on_register_write(0x00, 0x00);
        state.on_register_write(0x01, 0x00);

        // Enable channel A
        let event = state.on_register_write(0x07, 0b11111110);

        // Zero period should not generate KeyOn event
        assert!(
            event.is_none()
                || (event
                    .as_ref()
                    .map(|e| e.len() == 1 && matches!(&e[0], StateEvent::KeyOff { .. }))
                    .unwrap_or(false))
        );
    }

    #[test]
    fn volume_mixer_envelope_and_period_control_notes() {
        let mut state = Ay8910State::new(2_000_000.0);
        assert!(state.on_register_write(0x00, 125).is_none());
        assert!(state.on_register_write(0x07, 0xfe).is_none());
        let on = state.on_register_write(0x08, 0x0f).unwrap();
        assert!(
            matches!(&on[..], [StateEvent::KeyOn { channel: 0, tone }] if tone.freq_hz == Some(1000.0))
        );
        assert!(state.on_register_write(0x08, 1).is_none());
        assert!(matches!(
            &state.on_register_write(0x08, 0).unwrap()[..],
            [StateEvent::KeyOff { channel: 0 }]
        ));
        assert!(matches!(
            &state.on_register_write(0x08, 0x10).unwrap()[..],
            [StateEvent::KeyOn { channel: 0, .. }]
        ));
        assert!(matches!(
            &state.on_register_write(0x00, 0).unwrap()[..],
            [StateEvent::KeyOff { channel: 0 }]
        ));
        assert!(matches!(
            &state.on_register_write(0x00, 125).unwrap()[..],
            [StateEvent::KeyOn { channel: 0, .. }]
        ));
        assert!(matches!(
            &state.on_register_write(0x07, 0xf7).unwrap()[..],
            [StateEvent::KeyOff { channel: 0 }]
        ));
        assert!(state.on_register_write(0x08, 15).is_none());
    }
}
