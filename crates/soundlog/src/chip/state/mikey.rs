//! Mikey (Atari Lynx) chip state implementation.
//!
//! This module provides state tracking for the Mikey sound chip,
//! used in the Atari Lynx handheld console. Mikey has 4 audio channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// Mikey has 4 audio channels
const MIKEY_CHANNELS: usize = 4;

/// Mikey recommended storage (sparse for 64 registers)
pub type MikeyStorage = SparseStorage<u8, u8>;

/// Mikey register state tracker
///
/// Tracks all 4 audio channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// Mikey has 8 registers per channel (channels at offsets 0x00, 0x08, 0x10, 0x18):
/// - Offset+0: Volume (signed 8-bit)
/// - Offset+1: Feedback (shift register feedback configuration)
/// - Offset+2: Output (output value)
/// - Offset+3: Shifter (shift register low byte)
/// - Offset+4: Backup (backup count value)
/// - Offset+5: Control1 (timer control)
///   - Bit 5: Integrate mode
///   - Bit 4: Reload enable
///   - Bit 3: Count enable
///   - Bits 2-0: Timer clock divisor
/// - Offset+6: Counter (8-bit counter value)
/// - Offset+7: Control2 (additional control)
///
/// Additional registers (Lynx II):
/// - 0x40-0x43: Stereo attenuation per channel
/// - 0x44: Attenuation enable
/// - 0x50: Master enable
///
/// A channel is enabled when:
/// - Volume is non-zero
/// - Count enable is set (Control1 bit 3)
/// - Master enable is set (register 0x50)
#[derive(Debug, Clone)]
pub struct MikeyState {
    /// Channel states for 4 audio channels
    channels: [ChannelState; MIKEY_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Master enable flag (register 0x50)
    master_enable: bool,
    /// Global register storage for all written registers
    registers: MikeyStorage,
}

impl MikeyState {
    /// Create a new Mikey state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 16,000,000 Hz (Atari Lynx)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::MikeyState;
    ///
    /// // Atari Lynx
    /// let state = MikeyState::new(16_000_000.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            master_enable: false,
            registers: MikeyStorage::default(),
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

    /// Calculate frequency in Hz from counter and clock divisor values
    ///
    /// Mikey frequency formula (simplified):
    /// freq = master_clock / (prescaler * (counter + 1))
    ///
    /// Where prescaler depends on timer_clock bits in Control1
    ///
    /// # Arguments
    ///
    /// * `counter` - 8-bit counter value
    /// * `timer_clock` - 3-bit timer clock divisor
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn calculate_frequency(&self, counter: u8, timer_clock: u8) -> f32 {
        // Timer clock divisor: 0=1, 1=2, 2=4, 3=8, 4=16, 5=32, 6=64, 7=linked
        let divisor = if timer_clock < 7 {
            1 << timer_clock
        } else {
            // Linked mode - use default divisor
            1
        };

        if counter == 0 {
            // Counter = 0 gives highest frequency
            self.master_clock_hz / (divisor as f32)
        } else {
            self.master_clock_hz / (divisor as f32 * (counter as f32 + 1.0_f32))
        }
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
        if channel >= MIKEY_CHANNELS {
            return None;
        }

        // Read from global register storage
        let base_reg = (channel * 8) as u8;

        // Counter register (offset+6)
        let counter_reg = base_reg + 6;
        let counter = self.registers.read(counter_reg)?;

        // Control1 register (offset+5)
        let control1_reg = base_reg + 5;
        let control1 = self.registers.read(control1_reg).unwrap_or(0);

        // Extract timer clock divisor (bits 2-0)
        let timer_clock = control1 & 0x07;

        let freq_hz = self.calculate_frequency(counter, timer_clock);

        // Use counter as fnum, timer_clock as block for identification
        Some(ToneInfo::new(counter as u16, timer_clock, Some(freq_hz)))
    }

    /// Check if channel is enabled
    ///
    /// A channel is enabled if:
    /// 1. Master enable is set (0x50)
    /// 2. Volume is non-zero (offset+0)
    /// 3. Count enable is set (Control1 bit 3)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// true if channel is enabled, false otherwise
    fn is_channel_enabled(&self, channel: usize) -> bool {
        if !self.master_enable {
            return false;
        }

        // Read from global register storage
        let base_reg = (channel * 8) as u8;

        // Check volume (offset+0)
        let volume_reg = base_reg;
        let volume = self.registers.read(volume_reg).unwrap_or(0);
        if volume == 0 {
            return false;
        }

        // Check count enable (Control1 bit 3)
        let control1_reg = base_reg + 5;
        let control1 = self.registers.read(control1_reg).unwrap_or(0);
        (control1 & 0x08) != 0
    }

    /// Handle volume register write (offset+0)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    /// * `value` - Volume value (signed 8-bit)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_volume_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= MIKEY_CHANNELS {
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

    /// Handle control1 register write (offset+5)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_control1_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= MIKEY_CHANNELS {
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

    /// Handle counter register write (offset+6)
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while enabled, None otherwise
    fn handle_counter_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= MIKEY_CHANNELS {
            return None;
        }

        // If channel is enabled and counter changed, emit ToneChange
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

    /// Handle master enable register write (0x50)
    ///
    /// # Arguments
    ///
    /// * `value` - Master enable value (bit 0)
    ///
    /// # Returns
    ///
    /// Some(StateEvent) for first channel that changed, None otherwise
    fn handle_master_enable_register(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let new_master_enable = (value & 0x01) != 0;
        let old_master_enable = self.master_enable;
        self.master_enable = new_master_enable;

        // If master enable changed, update all channels
        if old_master_enable != new_master_enable {
            for channel in 0..MIKEY_CHANNELS {
                let enabled = self.is_channel_enabled(channel);
                let new_key_state = if enabled { KeyState::On } else { KeyState::Off };

                let old_key_state = self.channels[channel].key_state;
                self.channels[channel].key_state = new_key_state;

                match (old_key_state, new_key_state) {
                    (KeyState::Off, KeyState::On) => {
                        if let Some(tone) = self.extract_tone(channel) {
                            self.channels[channel].tone = Some(tone);
                            return Some(vec![StateEvent::KeyOn {
                                channel: channel as u8,
                                tone,
                            }]);
                        }
                    }
                    (KeyState::On, KeyState::Off) => {
                        return Some(vec![StateEvent::KeyOff {
                            channel: channel as u8,
                        }]);
                    }
                    _ => {}
                }
            }
        }

        None
    }
}

impl ChipState for MikeyState {
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
            // Channel registers (0x00-0x1F for channels 0-3)
            0x00..=0x1F => {
                let channel = (register / 8) as usize;
                let offset = register % 8;

                match offset {
                    0 => self.handle_volume_register(channel),
                    5 => self.handle_control1_register(channel),
                    6 => self.handle_counter_register(channel),
                    // Other registers (feedback, output, shifter, backup, control2)
                    _ => None,
                }
            }

            // Stereo attenuation registers (0x40-0x43) - Lynx II only
            0x40..=0x43 => None,

            // Attenuation enable (0x44) - Lynx II only
            0x44 => None,

            // Master enable (0x50)
            0x50 => self.handle_master_enable_register(value),

            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.master_enable = false;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        MIKEY_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mikey_channel_enable() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Set counter for channel 0
        state.on_register_write(0x06, 0x40); // Counter

        // Set volume
        state.on_register_write(0x00, 0x7F); // Volume

        // Enable master
        state.on_register_write(0x50, 0x01);

        // Enable count (Control1 bit 3)
        let event = state.on_register_write(0x05, 0x08);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
        }
    }

    #[test]
    fn test_mikey_channel_disable() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x06, 0x40);
        state.on_register_write(0x00, 0x7F);
        state.on_register_write(0x50, 0x01);
        state.on_register_write(0x05, 0x08);

        // Disable by setting volume to 0
        let event = state.on_register_write(0x00, 0x00);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
        }
    }

    #[test]
    fn test_mikey_tone_change() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x06, 0x40);
        state.on_register_write(0x00, 0x7F);
        state.on_register_write(0x50, 0x01);
        state.on_register_write(0x05, 0x08);

        // Change counter while enabled
        let event = state.on_register_write(0x06, 0x80);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
        }
    }

    #[test]
    fn test_mikey_master_disable() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x06, 0x40);
        state.on_register_write(0x00, 0x7F);
        state.on_register_write(0x50, 0x01);
        state.on_register_write(0x05, 0x08);

        // Disable master
        let event = state.on_register_write(0x50, 0x00);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
        }
    }

    #[test]
    fn test_mikey_channel_count() {
        let state = MikeyState::new(16_000_000.0f32);
        assert_eq!(state.channel_count(), 4);
    }

    #[test]
    fn test_mikey_reset() {
        let mut state = MikeyState::new(16_000_000.0f32);

        state.on_register_write(0x50, 0x01);

        state.reset();

        assert!(!state.master_enable);
    }

    #[test]
    fn test_mikey_multiple_channels() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Enable master
        state.on_register_write(0x50, 0x01);

        // Enable channel 0
        state.on_register_write(0x06, 0x40);
        state.on_register_write(0x00, 0x7F);
        state.on_register_write(0x05, 0x08);

        // Enable channel 2 (base register 0x10)
        state.on_register_write(0x16, 0x20);
        state.on_register_write(0x10, 0x60);
        state.on_register_write(0x15, 0x08);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(2).unwrap().key_state, KeyState::On);
    }
}
