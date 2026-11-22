//! WonderSwan APU state implementation.
//!
//! The WonderSwan has 4 PCM channels with independent frequency and volume control.
//! Each channel can play back wave samples with programmable pitch.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// Number of channels in WonderSwan
const WONDERSWAN_CHANNELS: usize = 4;

/// Register storage for WonderSwan channels
///
/// The WonderSwan protocol supports two write forms:
/// - 16-bit memory offset writes (mm ll dd)
/// - 8-bit register writes (aa dd)
///
/// We store both forms independently: a small array for registers and a
/// sparse map for waveform memory.
#[derive(Debug, Clone, Default)]
struct WonderSwanStorage {
    reg: ArrayStorage<u8, 256>,
    mem: SparseStorage<u16, u8>,
}

impl WonderSwanStorage {
    /// Write using 8-bit register addressing (aa)
    fn write_reg(&mut self, register: u8, value: u8) {
        self.reg.write(register, value);
    }

    /// Write using 16-bit memory offset addressing (mmll)
    fn write_mem(&mut self, offset: u16, value: u8) {
        self.mem.write(offset, value);
    }

    /// Read a logical 8-bit register value.
    fn read(&self, register: u8) -> Option<u8> {
        self.reg.read(register)
    }

    fn clear(&mut self) {
        self.reg.clear();
        self.mem.clear();
    }
}

/// WonderSwan state tracker
///
/// Tracks register state and key on/off events for all 4 channels.
/// Each channel has frequency control (11-bit) and stereo volume control.
#[derive(Debug, Clone)]
pub struct WonderSwanState {
    /// Channel states for 4 PCM channels
    channels: [ChannelState; WONDERSWAN_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f32,
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
    /// let state = WonderSwanState::new(3_072_000.0f32);
    /// ```
    pub fn new(master_clock_hz: f32) -> Self {
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

    fn update_channel(&mut self, channel: usize) -> Option<StateEvent> {
        let control = self.registers.read(0x90).unwrap_or(0);
        let tone_mode =
            !(channel == 1 && control & 0x20 != 0 || channel == 3 && control & 0x80 != 0);
        let on = control & (1 << channel) != 0 && tone_mode && self.has_volume(channel);
        let was_on = self.channels[channel].key_state == KeyState::On;
        if on == was_on {
            return None;
        }
        self.channels[channel].key_state = if on { KeyState::On } else { KeyState::Off };
        Some(if on {
            StateEvent::KeyOn {
                channel: channel as u8,
                tone: self.extract_tone(channel),
            }
        } else {
            StateEvent::KeyOff {
                channel: channel as u8,
            }
        })
    }

    fn update_channels(&mut self) -> Option<Vec<StateEvent>> {
        let events: Vec<_> = (0..WONDERSWAN_CHANNELS)
            .filter_map(|channel| self.update_channel(channel))
            .collect();
        (!events.is_empty()).then_some(events)
    }

    /// Check if a channel has non-zero volume
    fn has_volume(&self, channel: usize) -> bool {
        let register = 0x88 + channel as u8;
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
        // A 32-sample waveform advances once per (2048 - freq) master clocks.
        // period = 2048 - freq
        let period = 2048 - freq;
        let freq_hz = if period > 0 {
            Some(self.master_clock_hz / (32.0f32 * period as f32))
        } else {
            None
        };

        ToneInfo::new(freq, 0, freq_hz)
    }

    /// Map a VGM register offset and value into the internal WonderSwan
    /// register space used by `WonderSwanState`.
    pub fn map_vgm_to_wonderswan_register(register: u8, value: u8) -> (u8, u8) {
        let mapped_register = register.wrapping_add(0x80);
        (mapped_register, value)
    }

    /// Handle a write after the storage has been updated. `register` is the
    /// logical 8-bit register (low byte of any 16-bit offset) and `value` is
    /// the byte written.
    fn handle_write(&mut self, register: u8, _value: u8) -> Option<Vec<StateEvent>> {
        match register {
            // Frequency registers (0x80-0x87)
            0x80..=0x87 => {
                let offset = register - 0x80;
                let channel = offset >> 1;
                self.handle_frequency_register(channel)
            }
            // Volume registers (0x88-0x8B)
            0x88..=0x8b => self
                .update_channel(usize::from(register - 0x88))
                .map(|event| vec![event]),
            // Audio control register (0x90)
            0x90 => self.update_channels(),
            _ => None,
        }
    }

    /// Handle waveform (state only)
    pub fn on_waveform_write(&mut self, offset: u16, value: u8) -> Option<Vec<StateEvent>> {
        // waveform
        self.registers.write_mem(offset, value);
        None
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
        self.registers.write_reg(register, value);
        self.handle_write(register, value)
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
        let state = WonderSwanState::new(3_072_000.0f32);
        assert_eq!(state.channel_count(), 4);
    }

    #[test]
    fn test_wonderswan_key_on() {
        let mut state = WonderSwanState::new(3_072_000.0f32);

        // Set frequency for channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);

        state.on_register_write(0x90, 1);
        let event = state.on_register_write(0x88, 0x88);
        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_wonderswan_key_on_via_control() {
        let mut state = WonderSwanState::new(3_072_000.0f32);

        // Set frequency for channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);

        // Set volume for channel 0
        state.on_register_write(0x88, 0x88);

        // Enable channel 0 via control register
        let event = state.on_register_write(0x90, 0x01);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOn { channel: 0, .. }));
    }

    #[test]
    fn test_wonderswan_key_off() {
        let mut state = WonderSwanState::new(3_072_000.0f32);

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
        let mut state = WonderSwanState::new(3_072_000.0f32);

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
        let mut state = WonderSwanState::new(3_072_000.0f32);

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
    fn control_uses_each_channels_own_volume() {
        let mut state = WonderSwanState::new(3_072_000.0);
        state.on_register_write(0x8a, 0x66);
        state.on_register_write(0x90, 0);
        let events = state.on_register_write(0x90, 4).unwrap();
        assert!(matches!(events[0], StateEvent::KeyOn { channel: 2, .. }));
    }

    #[test]
    fn test_wonderswan_reset() {
        let mut state = WonderSwanState::new(3_072_000.0f32);

        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);
        state.on_register_write(0x90, 0x01);

        state.reset();

        assert_eq!(state.channels[0].key_state, KeyState::Off);
    }

    #[test]
    fn test_wonderswan_volume_key_off() {
        let mut state = WonderSwanState::new(3_072_000.0f32);

        // Enable channel 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(0x81, 0x04);
        state.on_register_write(0x88, 0x88);
        state.on_register_write(0x90, 1);

        // Disable by setting volume to 0
        let event = state.on_register_write(0x88, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn simultaneous_channels_and_voice_noise_modes() {
        let mut state = WonderSwanState::new(3_072_000.0);
        for register in 0x88..=0x8b {
            assert!(state.on_register_write(register, 0xff).is_none());
        }
        let events = state.on_register_write(0x90, 0x0f).unwrap();
        assert_eq!(events.len(), 4);
        let events = state.on_register_write(0x90, 0xaf).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StateEvent::KeyOff { channel: 1 }));
        assert!(matches!(events[1], StateEvent::KeyOff { channel: 3 }));
        let events = state.on_register_write(0x90, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StateEvent::KeyOff { channel: 0 }));
        assert!(matches!(events[1], StateEvent::KeyOff { channel: 2 }));
    }
}
