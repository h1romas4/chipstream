//! Mikey (Atari Lynx) chip state implementation.
//!
//! This module provides state tracking for the Mikey sound chip,
//! used in the Atari Lynx handheld console. Mikey has 4 audio channels.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use std::array;

/// Mikey has 4 audio channels
const MIKEY_CHANNELS: usize = 4;

/// Mikey recommended sparse register storage.
pub type MikeyStorage = SparseStorage<u8, u8>;

/// Mikey register state tracker
///
/// Tracks all 4 audio channels and their register state, detecting key on/off
/// events and extracting tone information.
///
/// # Register Layout
///
/// Mikey has 8 registers per channel (channels at 0x20, 0x28, 0x30, 0x38):
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
/// - 0x50: Per-channel stereo output disable bits
///
/// A channel is enabled when:
/// - Volume is non-zero
/// - Count enable is set (Control1 bit 3)
/// - At least one output is enabled (register 0x50)
#[derive(Debug, Clone)]
pub struct MikeyState {
    /// Channel states for 4 audio channels
    channels: [ChannelState; MIKEY_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
    /// Per-channel stereo output disable bits.
    stereo: u8,
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
            channels: array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            stereo: 0,
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

    /// Calculate timer frequency in Hz from the backup and clock divisor.
    ///
    /// `freq = master_clock / (16 * 2^timer_clock * (backup + 1))`.
    /// Linked clocks do not have a directly calculable frequency.
    ///
    /// # Arguments
    ///
    /// * `counter` - 8-bit backup value
    /// * `timer_clock` - 3-bit timer clock divisor
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn calculate_frequency(&self, counter: u8, timer_clock: u8) -> f32 {
        // Timer clock divisor: 0=1, 1=2, 2=4, 3=8, 4=16, 5=32, 6=64, 7=linked
        let divisor = 16 << timer_clock;

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
    /// Tone information for a valid channel.
    fn extract_tone(&self, channel: usize) -> Option<ToneInfo> {
        if channel >= MIKEY_CHANNELS {
            return None;
        }

        // Read from global register storage
        let base_reg = 0x20 + (channel * 8) as u8;

        // The backup register determines the reloaded timer period.
        let counter_reg = base_reg + 4;
        let counter = self.registers.read(counter_reg).unwrap_or(0);

        // Control1 register (offset+5)
        let control1_reg = base_reg + 5;
        let control1 = self.registers.read(control1_reg).unwrap_or(0);

        // Extract timer clock divisor (bits 2-0)
        let timer_clock = control1 & 0x07;

        let freq_hz = (timer_clock != 7).then(|| self.calculate_frequency(counter, timer_clock));

        // Use counter as fnum, timer_clock as block for identification
        Some(ToneInfo::new(counter as u16, timer_clock, freq_hz))
    }

    /// Check if channel is enabled
    ///
    /// A channel is enabled if:
    /// 1. At least one stereo output is enabled (0x50)
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
        let mask = 0x11 << channel;
        if self.stereo & mask == mask {
            return false;
        }

        // Read from global register storage
        let base_reg = 0x20 + (channel * 8) as u8;

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
            (KeyState::On, KeyState::On) => {
                let tone = self.extract_tone(channel)?;
                if self.channels[channel].tone == Some(tone) {
                    return None;
                }
                self.channels[channel].tone = Some(tone);
                Some(vec![StateEvent::ToneChange {
                    channel: channel as u8,
                    tone,
                }])
            }
            _ => None,
        }
    }

    /// Handle backup register write (offset+4).
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-3)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while enabled, None otherwise
    fn handle_backup_register(&mut self, channel: usize) -> Option<Vec<StateEvent>> {
        if channel >= MIKEY_CHANNELS {
            return None;
        }

        // If the channel is enabled, emit a tone change.
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

    /// Handle stereo disable register write (0x50).
    ///
    /// # Arguments
    ///
    /// * `value` - Per-channel left and right disable bits.
    ///
    /// # Returns
    ///
    /// Events for all channels whose key state changed.
    fn handle_stereo_register(&mut self, value: u8) -> Option<Vec<StateEvent>> {
        let old_stereo = self.stereo;
        self.stereo = value;
        let mut events = Vec::new();

        // Update all channels when the stereo mask changes.
        if old_stereo != value {
            for channel in 0..MIKEY_CHANNELS {
                let enabled = self.is_channel_enabled(channel);
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
        }

        (!events.is_empty()).then_some(events)
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
            // Channel registers (0x20-0x3F for channels 0-3).
            0x20..=0x3F => {
                let channel = ((register - 0x20) / 8) as usize;
                let offset = register % 8;

                match offset {
                    0 => self.handle_volume_register(channel),
                    5 => self.handle_control1_register(channel),
                    4 => self.handle_backup_register(channel),
                    // Other registers (feedback, output, shifter, backup, control2)
                    _ => None,
                }
            }

            // Stereo attenuation registers (0x40-0x43) - Lynx II only
            0x40..=0x43 => None,

            // Attenuation enable (0x44) - Lynx II only
            0x44 => None,

            // Stereo output disable (0x50).
            0x50 => self.handle_stereo_register(value),

            _ => None,
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.stereo = 0;
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

        // Set backup for channel 0
        state.on_register_write(0x24, 0x40); // Backup

        // Set volume
        state.on_register_write(0x20, 0x7F); // Volume

        // Enable count (Control1 bit 3)
        let event = state.on_register_write(0x25, 0x08);

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
        state.on_register_write(0x24, 0x40);
        state.on_register_write(0x20, 0x7F);
        state.on_register_write(0x25, 0x08);

        // Disable by setting volume to 0
        let event = state.on_register_write(0x20, 0x00);

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
        state.on_register_write(0x24, 0x40);
        state.on_register_write(0x20, 0x7F);
        state.on_register_write(0x25, 0x08);

        // Change backup while enabled
        let event = state.on_register_write(0x24, 0x80);

        assert!(event.is_some());
        if let Some(ref events) = event {
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
        }
    }

    #[test]
    fn test_mikey_stereo_disable() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x24, 0x40);
        state.on_register_write(0x20, 0x7F);
        state.on_register_write(0x25, 0x08);

        // Disable both outputs.
        let event = state.on_register_write(0x50, 0x11);

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

        assert_eq!(state.stereo, 0);
    }

    #[test]
    fn test_mikey_multiple_channels() {
        let mut state = MikeyState::new(16_000_000.0f32);

        // Enable channel 0
        state.on_register_write(0x24, 0x40);
        state.on_register_write(0x20, 0x7F);
        state.on_register_write(0x25, 0x08);

        // Enable channel 2 (base register 0x30).
        state.on_register_write(0x34, 0x20);
        state.on_register_write(0x30, 0x60);
        state.on_register_write(0x35, 0x08);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(2).unwrap().key_state, KeyState::On);
        let events = state.on_register_write(0x50, 0x55).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn timer_registers_stereo_and_clock_division() {
        let mut state = MikeyState::new(16_000_000.0);
        assert!(state.on_register_write(0x00, 127).is_none());
        assert!(state.on_register_write(0x05, 8).is_none());
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);

        state.on_register_write(0x20, 1);
        state.on_register_write(0x24, 9);
        let event = state.on_register_write(0x25, 9).unwrap();
        assert!(
            matches!(event[0], StateEvent::KeyOn { tone, .. } if tone.freq_hz == Some(50_000.0))
        );
        assert!(state.on_register_write(0x50, 0x01).is_none());
        assert!(matches!(
            state.on_register_write(0x50, 0x11).unwrap()[0],
            StateEvent::KeyOff { channel: 0 }
        ));
        assert!(matches!(
            state.on_register_write(0x50, 0).unwrap()[0],
            StateEvent::KeyOn { channel: 0, .. }
        ));
        let event = state.on_register_write(0x25, 0x0f).unwrap();
        assert!(matches!(event[0], StateEvent::ToneChange { tone, .. } if tone.freq_hz.is_none()));
    }
}
