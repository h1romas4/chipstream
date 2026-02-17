//! VSU (Virtual Boy) chip state implementation.
//!
//! This module provides state tracking for the Virtual Boy VSU (Virtual Sound Unit),
//! which has 6 audio channels (5 wavetable + 1 noise).

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// VSU has 6 audio channels (5 wavetable + 1 noise)
const VSU_CHANNELS: usize = 6;

/// VSU recommended storage
pub type VsuStorage = SparseStorage<u16, u8>;

/// VSU register state tracker
///
/// Tracks all 6 audio channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// VSU has registers organized per channel at base addresses:
/// - Channel 0: 0x400 + 0x00
/// - Channel 1: 0x400 + 0x40
/// - Channel 2: 0x400 + 0x80
/// - Channel 3: 0x400 + 0xC0
/// - Channel 4: 0x400 + 0x100
/// - Channel 5: 0x400 + 0x140
///
/// Registers per channel (relative offsets):
/// - 0x00 (SxINT): Sound interval/enable (bit 7 = enable)
/// - 0x04 (SxLRV): Left/Right volume (4 bits each)
/// - 0x08 (SxFQL): Frequency low byte
/// - 0x0C (SxFQH): Frequency high byte (bits 2-0)
/// - 0x10 (SxEV0): Envelope 0
/// - 0x14 (SxEV1): Envelope 1
/// - 0x18 (SxRAM): Wave RAM address
///
/// A channel is enabled when:
/// - SxINT bit 7 is set
/// - SxLRV is non-zero (volume > 0)
#[derive(Debug, Clone)]
pub struct VsuState {
    /// Channel states for 6 channels
    channels: [ChannelState; VSU_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Global register storage for all written registers
    registers: VsuStorage,
}

impl VsuState {
    /// Create a new VSU state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 5,000,000 Hz (Virtual Boy)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::VsuState;
    ///
    /// let state = VsuState::new(5_000_000.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            registers: VsuStorage::default(),
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

    /// Calculate frequency in Hz from VSU frequency value
    ///
    /// VSU frequency formula (simplified):
    /// freq = master_clock / (2048 - frequency_value)
    ///
    /// # Arguments
    ///
    /// * `freq_value` - 11-bit frequency value (0-2047)
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn calculate_frequency(&self, freq_value: u16) -> f32 {
        if freq_value >= 2048 {
            return 0.0f32;
        }

        let divisor = 2048 - freq_value as i32;
        if divisor <= 0 {
            return 0.0f32;
        }

        self.master_clock_hz / (divisor as f32)
    }

    /// Extract tone from channel registers
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= VSU_CHANNELS {
            return None;
        }

        // Read from global register storage
        let base_addr = 0x400 + (channel as u16 * 0x40);

        // Frequency low register (offset 0x08)
        let fql_addr = base_addr + 0x08;
        let freq_low = self.registers.read(fql_addr)?;

        // Frequency high register (offset 0x0C, bits 2-0)
        let fqh_addr = base_addr + 0x0C;
        let freq_high = self.registers.read(fqh_addr)?;

        // Combine into 11-bit frequency
        let freq_value = (freq_low as u16) | ((freq_high as u16 & 0x07) << 8);

        if freq_value == 0 {
            return None;
        }

        let freq_hz = self.calculate_frequency(freq_value);

        Some(ToneInfo::new(freq_value, 0, Some(freq_hz)))
    }

    /// Check if channel is enabled
    ///
    /// A channel is enabled if:
    /// 1. SxINT bit 7 is set (interval enable)
    /// 2. SxLRV is non-zero (volume > 0)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// true if channel is enabled, false otherwise
    fn is_channel_enabled(&self, channel: usize) -> bool {
        if channel >= VSU_CHANNELS {
            return false;
        }

        // Read from global register storage
        let base_addr = 0x400 + (channel as u16 * 0x40);

        // Check interval register (offset 0x00, bit 7)
        let int_addr = base_addr;
        let interval = self.registers.read(int_addr).unwrap_or(0);
        if (interval & 0x80) == 0 {
            return false;
        }

        // Check volume register (offset 0x04)
        let lrv_addr = base_addr + 0x04;
        let volume = self.registers.read(lrv_addr).unwrap_or(0);

        volume != 0
    }

    /// Handle interval register write (SxINT, offset 0x00)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_interval_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= VSU_CHANNELS {
            return None;
        }

        let enabled = self.is_channel_enabled(channel);
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

    /// Handle volume register write (SxLRV, offset 0x04)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_volume_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= VSU_CHANNELS {
            return None;
        }

        let enabled = self.is_channel_enabled(channel);
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

    /// Handle frequency register write (SxFQL/SxFQH, offsets 0x08/0x0C)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-5)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while enabled, None otherwise
    fn handle_frequency_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= VSU_CHANNELS {
            return None;
        }

        // If channel is enabled and frequency changed, emit ToneChange
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

impl ChipState for VsuState {
    type Register = u16;
    type Value = u8;

    fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
        self.registers.read(register)
    }

    fn on_register_write(
        &mut self,
        register: Self::Register,
        value: Self::Value,
    ) -> Option<Vec<StateEvent>> {
        // VSU uses addresses in the 0x400-0x57F range
        // However, VGM files may use relative offsets (0x00-0x17F)
        // Convert to absolute address if needed
        let address = if register < 0x400 {
            0x400 + register
        } else {
            register
        };

        // Store all register writes in global storage with absolute address
        self.registers.write(address, value);

        // Check if address is in valid range
        if !(0x400..0x580).contains(&address) {
            return None;
        }

        // Calculate channel and offset from the address
        let channel = ((address - 0x400) / 0x40) as usize;
        let offset = ((address - 0x400) % 0x40) / 4;

        if channel >= VSU_CHANNELS {
            return None;
        }

        match offset {
            0 => self.handle_interval_register(channel),
            1 => self.handle_volume_register(channel),
            2 => self.handle_frequency_register(channel),
            3 => self.handle_frequency_register(channel),
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
        VSU_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vsu_channel_enable() {
        let mut state = VsuState::new(5_000_000.0f32);

        // Channel 0: register 0x400 (interval), 0x404 (volume), 0x408 (freq low), 0x40C (freq high)
        state.on_register_write(0x408, 0x00); // Freq low
        state.on_register_write(0x40C, 0x04); // Freq high

        // Set volume
        state.on_register_write(0x404, 0x88); // L=8, R=8

        // Enable interval
        let event = state.on_register_write(0x400, 0x80); // Interval enable

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_vsu_channel_disable() {
        let mut state = VsuState::new(5_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x408, 0x00);
        state.on_register_write(0x40C, 0x04);
        state.on_register_write(0x404, 0x88);
        state.on_register_write(0x400, 0x80);

        // Disable by clearing interval bit
        let event = state.on_register_write(0x400, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_vsu_tone_change() {
        let mut state = VsuState::new(5_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x408, 0x00);
        state.on_register_write(0x40C, 0x04);
        state.on_register_write(0x404, 0x88);
        state.on_register_write(0x400, 0x80);

        // Change frequency while playing
        let event = state.on_register_write(0x408, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_vsu_multiple_channels() {
        let mut state = VsuState::new(5_000_000.0f32);

        // Enable channel 0 (register base 0x400)
        state.on_register_write(0x408, 0x00);
        state.on_register_write(0x40C, 0x04);
        state.on_register_write(0x404, 0x88);
        state.on_register_write(0x400, 0x80);

        // Enable channel 2 (register base 0x480)
        state.on_register_write(0x488, 0x00);
        state.on_register_write(0x48C, 0x02);
        state.on_register_write(0x484, 0x66);
        state.on_register_write(0x480, 0x80);

        assert_eq!(state.channels[0].key_state, KeyState::On);
        assert_eq!(state.channels[2].key_state, KeyState::On);
    }

    #[test]
    fn test_vsu_channel_count() {
        let state = VsuState::new(5_000_000.0f32);
        assert_eq!(state.channel_count(), 6);
    }

    #[test]
    fn test_vsu_reset() {
        let mut state = VsuState::new(5_000_000.0f32);

        state.on_register_write(0x408, 0x00);
        state.on_register_write(0x40C, 0x04);
        state.on_register_write(0x404, 0x88);
        state.on_register_write(0x400, 0x80);

        state.reset();

        assert_eq!(state.channels[0].key_state, KeyState::Off);
    }

    #[test]
    fn test_vsu_volume_disable() {
        let mut state = VsuState::new(5_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x408, 0x00);
        state.on_register_write(0x40C, 0x04);
        state.on_register_write(0x404, 0x88);
        state.on_register_write(0x400, 0x80);

        // Disable by setting volume to 0
        let event = state.on_register_write(0x404, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }
}
