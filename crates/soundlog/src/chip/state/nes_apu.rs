//! NES APU chip state implementation.
//!
//! This module provides state tracking for the Nintendo Entertainment System (NES)
//! Audio Processing Unit (APU), which has 5 channels:
//! - 2 pulse wave channels
//! - 1 triangle wave channel
//! - 1 noise channel
//! - 1 DMC (Delta Modulation Channel) for samples
//!
//! Note: FDS (Famicom Disk System) expansion audio is not currently supported.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// NES APU has 5 channels
const NES_APU_CHANNELS: usize = 5;

/// NES APU recommended storage
pub type NesApuStorage = SparseStorage<u8, u8>;

/// NES APU base clock frequency
/// N2A03 clock: 21,477,270 / 12 = 1,789,772.5 Hz
/// Clock / 16 for tone calculation
const NES_CLK_BASE: f32 = 111_860.78_f32;

/// NES APU register state tracker
///
/// Tracks all 5 channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// Channel 0 (Pulse 1):
/// - 0x00 (WRA0): Duty, envelope, volume
/// - 0x01 (WRA1): Sweep unit
/// - 0x02 (WRA2): Timer low 8 bits
/// - 0x03 (WRA3): Length counter load, timer high 3 bits
///
/// Channel 1 (Pulse 2):
/// - 0x04 (WRB0): Duty, envelope, volume
/// - 0x05 (WRB1): Sweep unit
/// - 0x06 (WRB2): Timer low 8 bits
/// - 0x07 (WRB3): Length counter load, timer high 3 bits
///
/// Channel 2 (Triangle):
/// - 0x08 (WRC0): Linear counter
/// - 0x0A (WRC2): Timer low 8 bits
/// - 0x0B (WRC3): Length counter load, timer high 3 bits
///
/// Channel 3 (Noise):
/// - 0x0C (WRD0): Envelope, volume
/// - 0x0E (WRD2): Period
/// - 0x0F (WRD3): Length counter load
///
/// Channel 4 (DMC):
/// - 0x10 (WRE0): IRQ, loop, frequency
/// - 0x11 (WRE1): Direct load
/// - 0x12 (WRE2): Sample address
/// - 0x13 (WRE3): Sample length
///
/// - 0x15 (SMASK): Status / channel enable
/// - 0x17 (IRQCTRL): Frame counter
#[derive(Debug, Clone)]
pub struct NesApuState {
    /// Channel states for 5 channels
    channels: [ChannelState; NES_APU_CHANNELS],
    /// Global register storage for all written registers
    registers: NesApuStorage,
}

impl NesApuState {
    /// Create a new NES APU state tracker
    ///
    /// The clock parameter is accepted for API consistency but not used.
    ///
    /// # Arguments
    ///
    /// * `_clock` - Clock frequency in Hz (unused, accepted for API consistency)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::NesApuState;
    ///
    /// let state = NesApuState::new(1_789_773.0f32);
    /// ```
    pub fn new(_clock: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            registers: NesApuStorage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-1: Pulse, 2: Triangle, 3: Noise, 4: DMC)
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

    /// Calculate frequency in Hz from NES timer value
    ///
    /// # Arguments
    ///
    /// * `timer` - 11-bit timer value
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn hz_nes(timer: u32) -> f32 {
        NES_CLK_BASE / (timer + 1) as f32
    }

    /// Calculate frequency in Hz from NES noise period
    ///
    /// # Arguments
    ///
    /// * `period` - 4-bit period value
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn hz_nes_noise(period: u8) -> f32 {
        const NOISE_PERIODS: [u16; 16] = [
            4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
        ];

        let timer = NOISE_PERIODS[period as usize & 0x0F];
        NES_CLK_BASE / timer as f32
    }

    /// Extract tone from pulse/triangle channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-2)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= 3 {
            return None;
        }

        // Read from global register storage
        // Register layout for pulse and triangle channels
        let timer_low_reg = if channel == 2 {
            0x0A // Triangle uses 0x0A
        } else {
            0x02 + (channel as u8 * 4) // Pulse channels use 0x02, 0x06
        };

        let timer_high_reg = if channel == 2 {
            0x0B // Triangle uses 0x0B
        } else {
            0x03 + (channel as u8 * 4) // Pulse channels use 0x03, 0x07
        };

        let timer_low = self.registers.read(timer_low_reg)?;
        let timer_high = self.registers.read(timer_high_reg)?;

        // 11-bit timer: low 8 bits + high 3 bits
        let timer = (timer_low as u16) | ((timer_high as u16 & 0x07) << 8);

        let freq_hz = if channel == 2 {
            // Triangle channel frequency is half of timer frequency
            Self::hz_nes(timer as u32) / 2.0f32
        } else {
            Self::hz_nes(timer as u32)
        };

        // Use timer as fnum, 0 as block
        Some(ToneInfo::new(timer, 0, Some(freq_hz)))
    }

    /// Extract tone from noise channel registers
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if register has been written, None otherwise
    fn extract_noise_tone(&self) -> Option<ToneInfo> {
        // Read from global register storage
        let period_reg = 0x0E; // WRD2
        let period = self.registers.read(period_reg)?;

        // Period is bits 3-0
        let period_value = period & 0x0F;

        let freq_hz = Self::hz_nes_noise(period_value);

        // Use period as fnum, 0 as block
        Some(ToneInfo::new(period_value as u16, 0, Some(freq_hz)))
    }

    /// Handle timer register writes for pulse/triangle channels
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-2)
    /// * `register` - Register address
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if tone or key state changed, None otherwise
    fn handle_timer_register(&mut self, channel: usize, register: u8) -> Option<Vec<StateEvent>> {
        if channel >= 3 {
            return None;
        }

        // Check if this is a high byte write (triggers length counter reload)
        let is_high_byte = match channel {
            0 => register == 0x03, // WRA3
            1 => register == 0x07, // WRB3
            2 => register == 0x0B, // WRC3
            _ => false,
        };

        // Length counter reload can trigger key on for enabled channels
        if is_high_byte
            && self.channels[channel].key_state == KeyState::On
            && let Some(tone) = self.extract_tone(channel)
        {
            self.channels[channel].tone = Some(tone);
            return Some(vec![StateEvent::KeyOn {
                channel: channel as u8,
                tone,
            }]);
        }

        // If key is on and tone changed, emit ToneChange
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

    /// Handle noise period register writes (WRE2)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if tone changed while enabled, None otherwise
    fn handle_noise_period(&mut self) -> Option<Vec<StateEvent>> {
        let channel = 3;

        if self.channels[channel].key_state == KeyState::On
            && let Some(tone) = self.extract_noise_tone()
        {
            self.channels[channel].tone = Some(tone);
            return Some(vec![StateEvent::ToneChange {
                channel: channel as u8,
                tone,
            }]);
        }

        None
    }

    /// Handle status/enable register write (0x15)
    ///
    /// Register 0x15 format:
    /// - Bit 0: Pulse 1 enable
    /// - Bit 1: Pulse 2 enable
    /// - Bit 2: Triangle enable
    /// - Bit 3: Noise enable
    /// - Bit 4: DMC enable
    ///
    /// # Arguments
    ///
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) for the first channel that changed state, None otherwise
    fn handle_status_register(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let mut events = Vec::new();

        for channel in 0..5 {
            let enabled = (value & (1 << channel)) != 0;
            let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

            let old_key_state = self.channels[channel].key_state;
            self.channels[channel].key_state = new_key_state;

            match (old_key_state, new_key_state) {
                (KeyState::Off, KeyState::On) => {
                    let tone = if channel < 3 {
                        self.extract_tone(channel)
                    } else if channel == 3 {
                        self.extract_noise_tone()
                    } else {
                        None // DMC has no tone
                    };

                    if let Some(tone) = tone {
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
}

impl Default for NesApuState {
    fn default() -> Self {
        Self::new(0.0f32)
    }
}

impl ChipState for NesApuState {
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
            // Pulse 1 registers
            0x00 => {
                // WRA0: Duty, envelope, volume
                None
            }
            0x01 => {
                // WRA1: Sweep
                None
            }
            0x02 | 0x03 => {
                // WRA2, WRA3: Timer
                self.handle_timer_register(0, register)
            }

            // Pulse 2 registers
            0x04 => {
                // WRB0: Duty, envelope, volume
                None
            }
            0x05 => {
                // WRB1: Sweep
                None
            }
            0x06 | 0x07 => {
                // WRB2, WRB3: Timer
                self.handle_timer_register(1, register)
            }

            // Triangle registers
            0x08 => {
                // WRC0: Linear counter
                None
            }
            0x0A | 0x0B => {
                // WRC2, WRC3: Timer
                self.handle_timer_register(2, register)
            }

            // Noise registers
            0x0C => {
                // WRD0: Envelope, volume
                None
            }
            0x0E => {
                // WRD2: Period
                self.handle_noise_period()
            }
            0x0F => {
                // WRD3: Length counter load
                None
            }

            // DMC registers
            0x10..=0x13 => {
                // WRE0-WRE3: DMC control, direct load, sample address, sample length
                // DMC doesn't have tone information
                None
            }

            // Status/Enable register
            0x15 => self.handle_status_register(value),

            // Frame counter
            0x17 => {
                // IRQCTRL - doesn't affect tone
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
        NES_APU_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nes_apu_pulse_enable() {
        let mut state = NesApuState::new(0.0f32);

        // Set timer for pulse channel 0
        state.on_register_write(0x02, 0x6D); // Timer low
        state.on_register_write(0x03, 0x02); // Timer high

        // Enable pulse channel 0
        let event = state.on_register_write(0x15, 0x01);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x26D);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_nes_apu_pulse_disable() {
        let mut state = NesApuState::new(0.0f32);

        // Enable and set up pulse channel
        state.on_register_write(0x02, 0x6D);
        state.on_register_write(0x03, 0x02);
        state.on_register_write(0x15, 0x01);

        // Disable pulse channel 0
        let event = state.on_register_write(0x15, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_nes_apu_triangle() {
        let mut state = NesApuState::new(0.0f32);

        // Set triangle timer
        state.on_register_write(0x0A, 0x80); // Timer low
        state.on_register_write(0x0B, 0x01); // Timer high

        // Enable triangle channel
        let event = state.on_register_write(0x15, 0x04);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 2);
            assert_eq!(tone.fnum, 0x180);
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_nes_apu_noise() {
        let mut state = NesApuState::new(0.0f32);

        // Set noise period
        state.on_register_write(0x0E, 0x05);

        // Enable noise channel
        let event = state.on_register_write(0x15, 0x08);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 3);
            assert_eq!(tone.fnum, 0x05);
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_nes_apu_tone_change() {
        let mut state = NesApuState::new(0.0f32);

        // Enable and set up pulse channel
        state.on_register_write(0x02, 0x6D);
        state.on_register_write(0x03, 0x02);
        state.on_register_write(0x15, 0x01);

        // Change timer
        let event = state.on_register_write(0x02, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::ToneChange { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x280);
        } else {
            panic!("Expected ToneChange event");
        }
    }

    #[test]
    fn test_nes_apu_channel_count() {
        let state = NesApuState::new(0.0f32);
        assert_eq!(state.channel_count(), 5);
    }

    #[test]
    fn test_nes_apu_reset() {
        let mut state = NesApuState::new(0.0f32);

        state.on_register_write(0x02, 0x6D);
        state.on_register_write(0x03, 0x02);
        state.on_register_write(0x15, 0x01);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert!(state.channel(0).unwrap().tone.is_none());
    }
}
