//! POKEY (Atari 8-bit) chip state implementation.
//!
//! This module provides state tracking for the POKEY sound chip,
//! used in Atari 8-bit computers and arcade systems. POKEY has 4 audio channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use std::array;

/// POKEY has 4 audio channels
const POKEY_CHANNELS: usize = 4;

/// POKEY recommended storage (uses array storage for small register set)
pub type PokeyStorage = ArrayStorage<u8, 16>;

/// POKEY register state tracker
///
/// Tracks all 4 audio channels and their register state, detecting key on/off
/// events and extracting tone information. Frequencies describe pure-tone
/// timer periods; noise, filters and two-tone modulation can alter the output.
/// A joined pair's lower channel has no single frequency and reports `None`.
///
/// # Register Layout
///
/// POKEY has 16 register addresses:
/// - 0x00 (AUDF1): Channel 0 frequency
/// - 0x01 (AUDC1): Channel 0 control (volume + enable)
/// - 0x02 (AUDF2): Channel 1 frequency
/// - 0x03 (AUDC2): Channel 1 control
/// - 0x04 (AUDF3): Channel 2 frequency
/// - 0x05 (AUDC3): Channel 2 control
/// - 0x06 (AUDF4): Channel 3 frequency
/// - 0x07 (AUDC4): Channel 3 control
/// - 0x08 (AUDCTL): Audio control register
/// - 0x09 (STIMER): Start timers
/// - 0x0A (SKREST): Reset keyboard scan
/// - 0x0B (POTGO): Start pot scan
/// - 0x0D (SEROUT): Serial port output
/// - 0x0E (IRQEN): IRQ enable
/// - 0x0F (SKCTL): Serial port control
///
/// AUDCTL format (audio control register):
///
/// Bit 7: Use 9-bit polynomial counter (shortened from default 17-bit)
/// - 1 = 9-bit poly (shorter, more metallic/clicky noise)
/// - 0 = Standard 17-bit poly (longer, whiter noise)
///
/// Bit 6: Clock Channel 1 with 1.79 MHz (instead of base clock)
/// - 1 = Channel 1 uses 1.79 MHz clock (high frequency/precision)
/// - 0 = Channel 1 uses base clock (64 kHz or 15 kHz)
///
/// Bit 5: Clock Channel 3 with 1.79 MHz (instead of base clock)
/// - 1 = Channel 3 uses 1.79 MHz clock
/// - 0 = Channel 3 uses base clock
///
/// Bit 4: Join channels 1 & 2 for 16-bit resolution
/// - 1 = Channel 2 clocked by Channel 1 output → 16-bit combined (Ch1+Ch2)
/// - 0 = Channels 1 and 2 independent (8-bit each)
///
/// Bit 3: Join channels 3 & 4 for 16-bit resolution
///
/// - 1 = Channel 4 clocked by Channel 3 output → 16-bit combined (Ch3+Ch4)
/// - 0 = Channels 3 and 4 independent (8-bit each)
///
/// Bit 2: Enable high-pass filter on Channel 1
///
/// - 1 = High-pass filter enabled on Ch1 (clocked by Ch3 output)
/// - 0 = No high-pass filter on Ch1
///
/// Bit 1: Enable high-pass filter on Channel 2
///
/// - 1 = High-pass filter enabled on Ch2 (clocked by Ch4 output)
/// - 0 = No high-pass filter on Ch2
///
/// Bit 0: Select 15 kHz base clock instead of 64 kHz
///
/// - 1 = Base clock = 15 kHz (affects channels not using 1.79 MHz)
/// - 0 = Base clock = 64 kHz
///
/// Note: Channels with 1.79 MHz clock (bits 5 or 6 set) ignore this bit
#[derive(Debug, Clone)]
pub struct PokeyState {
    /// Channel states for 4 channels
    channels: [ChannelState; POKEY_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: PokeyStorage,
}

impl PokeyState {
    /// Create a new POKEY state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 1,789,790 Hz (Atari 800, exact NTSC)
    /// - 1,773,447 Hz (PAL)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::PokeyState;
    ///
    /// // NTSC Atari 8-bit
    /// let state = PokeyState::new(1_789_790.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: PokeyStorage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
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

    /// Return the pure-tone timer frequency; joined low channels have
    /// irregular periods and cannot be represented by one frequency.
    fn calculate_frequency(&self, audf: u16, channel: usize) -> Option<f32> {
        let audctl = self.registers.read(0x08).unwrap_or(0);
        let pair = channel / 2;
        let joined = audctl & [0x10, 0x08][pair] != 0;
        let high_clock = audctl & [0x40, 0x20][pair] != 0;
        if joined && channel.is_multiple_of(2) {
            return None;
        }
        let period = if high_clock && (channel.is_multiple_of(2) || joined) {
            f32::from(audf) + if joined { 7.0 } else { 4.0 }
        } else {
            (f32::from(audf) + 1.0) * if audctl & 1 != 0 { 114.0 } else { 28.0 }
        };
        Some(self.master_clock_hz / (2.0 * period))
    }

    /// Extract tone from channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= POKEY_CHANNELS {
            return None;
        }

        // Read from global register storage
        // AUDF register: 0x00, 0x02, 0x04, 0x06 (frequency)
        let audf_reg = (channel * 2) as u8;
        let mut audf = u16::from(self.registers.read(audf_reg)?);
        let audctl = self.registers.read(0x08).unwrap_or(0);
        if channel % 2 == 1 && audctl & [0x10, 0x08][channel / 2] != 0 {
            audf = (audf << 8) | u16::from(self.registers.read(audf_reg - 2).unwrap_or(0));
        }
        Some(ToneInfo::new(
            audf,
            0,
            self.calculate_frequency(audf, channel),
        ))
    }

    /// Check if channel is enabled based on volume
    ///
    /// # Arguments
    ///
    /// * `audc` - AUDC register value
    ///
    /// # Returns
    ///
    /// true if channel has non-zero volume, false otherwise
    fn is_channel_enabled(&self, audc: u8) -> bool {
        // Volume is in bits 3-0
        (audc & 0x0F) != 0
    }

    /// Handle frequency register write (AUDF)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while channel is on, None otherwise
    fn handle_frequency_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= POKEY_CHANNELS {
            return None;
        }

        // If channel is on and frequency changed, emit ToneChange
        if self.channels[channel].key_state == KeyState::On
            && let Some(tone) = self.extract_tone(channel)
            && self.channels[channel].tone != Some(tone)
        {
            self.channels[channel].tone = Some(tone);
            return Some(vec![StateEvent::ToneChange {
                channel: channel as u8,
                tone,
            }]);
        }

        None
    }

    /// Handle control register write (AUDC)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    /// * `value` - Control value (volume + distortion)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_control_register(&mut self, channel: usize, value: u8) -> Option<Vec<StateEvent>> {
        if channel >= POKEY_CHANNELS {
            return None;
        }

        let enabled = self.is_channel_enabled(value);
        let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

        let old_key_state = self.channels[channel].key_state;
        self.channels[channel].key_state = new_key_state;

        match (old_key_state, new_key_state) {
            (KeyState::Off, KeyState::On) => {
                if let Some(tone) = self.extract_tone(channel) {
                    self.channels[channel].tone = Some(tone);
                    Some(vec![StateEvent::KeyOn {
                        channel: channel as u8,
                        tone,
                    }])
                } else {
                    None
                }
            }
            (KeyState::On, KeyState::Off) => Some(vec![StateEvent::KeyOff {
                channel: channel as u8,
            }]),
            _ => None,
        }
    }

    /// Refresh active channels affected by timer configuration changes.
    fn update_tones(
        &mut self,
        channels: impl IntoIterator<Item = usize>,
    ) -> Option<Vec<StateEvent>> {
        let events: Vec<_> = channels
            .into_iter()
            .flat_map(|channel| {
                self.handle_frequency_register(channel)
                    .into_iter()
                    .flatten()
            })
            .collect();
        (!events.is_empty()).then_some(events)
    }
}

impl ChipState for PokeyState {
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
            // AUDF1 (0x00) - Channel 0 frequency
            0x00 => self.update_tones([0, 1]),

            // AUDC1 (0x01) - Channel 0 control
            0x01 => self.handle_control_register(0, value),

            // AUDF2 (0x02) - Channel 1 frequency
            0x02 => self.handle_frequency_register(1),

            // AUDC2 (0x03) - Channel 1 control
            0x03 => self.handle_control_register(1, value),

            // AUDF3 (0x04) - Channel 2 frequency
            0x04 => self.update_tones([2, 3]),

            // AUDC3 (0x05) - Channel 2 control
            0x05 => self.handle_control_register(2, value),

            // AUDF4 (0x06) - Channel 3 frequency
            0x06 => self.handle_frequency_register(3),

            // AUDC4 (0x07) - Channel 3 control
            0x07 => self.handle_control_register(3, value),

            // AUDCTL (0x08) - Audio control register
            0x08 => self.update_tones(0..POKEY_CHANNELS),

            // Other registers (STIMER, SKREST, POTGO, SEROUT, IRQEN, SKCTL)
            // These don't affect audio state
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
        POKEY_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pokey_channel_enable() {
        let mut state = PokeyState::new(1_789_790.0f32);

        // Set frequency for channel 0
        state.on_register_write(0x00, 0x10); // AUDF1

        // Enable channel 0 with volume 8
        let event = state.on_register_write(0x01, 0x08); // AUDC1

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x10);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_pokey_channel_disable() {
        let mut state = PokeyState::new(1_789_790.0f32);

        // Set up and enable channel 0
        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        // Disable channel 0 (volume = 0)
        let event = state.on_register_write(0x01, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_pokey_tone_change() {
        let mut state = PokeyState::new(1_789_790.0f32);

        // Set up and enable channel 0
        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        // Change frequency while enabled
        let event = state.on_register_write(0x00, 0x20);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::ToneChange { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x20);
        } else {
            panic!("Expected ToneChange event");
        }
    }

    #[test]
    fn test_pokey_multiple_channels() {
        let mut state = PokeyState::new(1_789_790.0f32);

        // Enable channel 0
        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        // Enable channel 2
        state.on_register_write(0x04, 0x20);
        state.on_register_write(0x05, 0x0A);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(2).unwrap().key_state, KeyState::On);
    }

    #[test]
    fn test_pokey_channel_count() {
        let state = PokeyState::new(1_789_790.0f32);
        assert_eq!(state.channel_count(), 4);
    }

    #[test]
    fn test_pokey_reset() {
        let mut state = PokeyState::new(1_789_790.0f32);

        state.on_register_write(0x00, 0x10);
        state.on_register_write(0x01, 0x08);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }

    #[test]
    fn test_pokey_volume_controls_enable() {
        let mut state = PokeyState::new(1_789_790.0f32);

        // Set frequency
        state.on_register_write(0x00, 0x10);

        // Enable with different volumes
        state.on_register_write(0x01, 0x01); // Volume 1
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);

        state.on_register_write(0x01, 0x0F); // Volume 15
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);

        state.on_register_write(0x01, 0x00); // Volume 0 = off
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
    }

    #[test]
    fn clock_and_join_changes_update_active_tones() {
        let mut state = PokeyState::new(1_789_790.0);
        state.on_register_write(0, 5);
        state.on_register_write(1, 0xaf);
        state.on_register_write(2, 1);
        state.on_register_write(3, 0xaf);

        for (audctl, channel, fnum, divisor) in [
            (0x01, 0, 5, 114.0 * 6.0),
            (0x40, 0, 5, 9.0),
            (0x10, 1, 0x105, 28.0 * 262.0),
            (0x50, 1, 0x105, 268.0),
        ] {
            let events = state.on_register_write(8, audctl).unwrap();
            let tone = events
                .iter()
                .find_map(|event| match event {
                    StateEvent::ToneChange { channel: c, tone } if *c == channel => Some(tone),
                    _ => None,
                })
                .unwrap();
            assert_eq!(tone.fnum, fnum);
            assert_eq!(tone.freq_hz, Some(1_789_790.0 / (2.0 * divisor)));
        }
        assert_eq!(state.channel(0).unwrap().tone.unwrap().freq_hz, None);
        let events = state.on_register_write(0, 6).unwrap();
        assert!(events.iter().any(|event| matches!(
            event, StateEvent::ToneChange { channel: 1, tone } if tone.fnum == 0x106
        )));
    }
}
