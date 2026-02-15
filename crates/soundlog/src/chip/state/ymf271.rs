//! YMF271 (OPX) chip state implementation.
//!
//! This module provides state tracking for the Yamaha YMF271 FM synthesis chip,
//! commonly known as OPX, with 12 FM channels and PCM capabilities.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};

/// YMF271 has 12 FM channels (slots)
/// (PCM wavetable capabilities are not tracked separately)
const YMF271_CHANNELS: usize = 12;

/// YMF271 recommended storage
pub type Ymf271Storage = SparseStorage<u8, u8>;

/// YMF271 register state tracker
///
/// Tracks all 12 FM channels (called "slots" in YMF271 documentation) and their
/// register state, detecting key on/off events and extracting tone information.
///
/// # Register Layout
///
/// YMF271 has a complex register structure with:
/// - 12 slots (channels) numbered 0-11
/// - 4 groups (0-3) with 3 slots each
/// - Registers per slot accessed via slot number and register offset
///
/// Key registers:
/// - Register 0: Key On (bit 0) + External enable/output
/// - Register 12: Block (octave)
/// - Register 13: F-number high byte
/// - Register 14: F-number low byte
///
/// Slots are organized as:
/// - Group 0: Slots 0, 12, 24, 36
/// - Group 1: Slots 1, 13, 25, 37
/// - Group 2: Slots 2, 14, 26, 38
/// - Group 3: Slots 3, 15, 27, 39
///   (Only first 12 slots are FM channels)
#[derive(Debug, Clone)]
pub struct Ymf271State {
    /// Channel states for 12 FM channels
    channels: [ChannelState; YMF271_CHANNELS],
    /// Master clock frequency in Hz (used for frequency calculation)
    master_clock_hz: f64,
    /// Current selected slot for register writes
    selected_slot: u8,
    /// Global register storage for all written registers
    registers: Ymf271Storage,
}

impl Ymf271State {
    /// Create a new YMF271 state tracker
    ///
    /// # Arguments
    ///
    /// * `master_clock_hz` - Master clock frequency in Hz
    ///
    /// Common values:
    /// - 16,934,400 Hz (standard OPX)
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::chip::state::Ymf271State;
    ///
    /// let state = Ymf271State::new(16_934_400.0);
    /// ```
    pub fn new(master_clock_hz: f64) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            master_clock_hz,
            selected_slot: 0,
            registers: Ymf271Storage::default(),
        }
    }

    /// Get a reference to a channel's state
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel index (0-11)
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
    /// * `channel` - Channel index (0-11)
    ///
    /// # Returns
    ///
    /// Some(&mut ChannelState) if channel index is valid, None otherwise
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(channel as usize)
    }

    /// Set the selected slot for register writes
    ///
    /// # Arguments
    ///
    /// * `slot` - Slot number (0-47, but only 0-11 are FM channels)
    pub fn set_selected_slot(&mut self, slot: u8) {
        self.selected_slot = slot;
    }

    /// Get the selected slot
    ///
    /// # Returns
    ///
    /// Current selected slot number
    pub fn selected_slot(&self) -> u8 {
        self.selected_slot
    }

    /// Calculate frequency in Hz from block and F-number
    ///
    /// YMF271 frequency formula (simplified):
    /// freq = (fnum * master_clock / 2) / (2^(20 - block))
    ///
    /// # Arguments
    ///
    /// * `fnum` - 12-bit F-number value
    /// * `block` - 4-bit block (octave) value
    ///
    /// # Returns
    ///
    /// Frequency in Hz
    fn calculate_frequency(&self, fnum: u16, block: u8) -> f64 {
        if fnum == 0 {
            return 0.0;
        }

        let block_shift = 20_i32 - (block & 0x0F) as i32;
        let divisor = 2_f64.powi(block_shift);
        (fnum as f64 * self.master_clock_hz / 2.0) / divisor
    }

    /// Extract tone from slot registers
    ///
    /// YMF271 register layout per slot:
    /// - Register 12: Block (bits 3-0)
    /// - Register 13: F-number high byte (bits 7-4)
    /// - Register 14: F-number low byte (bits 7-0)
    ///
    /// # Arguments
    ///
    /// * `slot` - Slot index (0-11)
    ///
    /// # Returns
    ///
    /// Some(ToneInfo) if registers have been written, None otherwise
    fn extract_tone(&self, slot: usize) -> Option<ToneInfo> {
        if slot >= YMF271_CHANNELS {
            return None;
        }

        // Read from global register storage
        // Read block register (register 12)
        let block_reg = 12;
        let block_data = self.registers.read(block_reg)?;
        let block = block_data & 0x0F;

        // Read F-number high (register 13)
        let fnum_hi_reg = 13;
        let fnum_hi = self.registers.read(fnum_hi_reg)?;

        // Read F-number low (register 14)
        let fnum_lo_reg = 14;
        let fnum_lo = self.registers.read(fnum_lo_reg)?;

        // Combine into 12-bit F-number
        // F-number is 12 bits: 4 bits from high + 8 bits from low
        let fnum = ((fnum_hi as u16 & 0x0F) << 8) | (fnum_lo as u16);

        if fnum == 0 {
            return None;
        }

        let freq_hz = self.calculate_frequency(fnum, block);

        Some(ToneInfo::new(fnum, block, Some(freq_hz)))
    }

    /// Handle key on/off register write (register 0)
    ///
    /// Register 0 format:
    /// - Bit 7: External enable
    /// - Bits 6-3: External output
    /// - Bit 0: Key On (1=on, 0=off)
    ///
    /// # Arguments
    ///
    /// * `slot` - Slot index (0-11)
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if key state changed, None otherwise
    fn handle_key_on_register(&mut self, slot: usize, value: u8) -> Option<Vec<StateEvent>> {
        if slot >= YMF271_CHANNELS {
            return None;
        }

        let key_on = (value & 0x01) != 0;
        let new_key_state = if key_on { KeyState::On } else { KeyState::Off };

        let old_key_state = self.channels[slot].key_state;
        self.channels[slot].key_state = new_key_state;

        match (old_key_state, new_key_state) {
            (KeyState::Off, KeyState::On) => {
                if let Some(tone) = self.extract_tone(slot) {
                    self.channels[slot].tone = Some(tone);
                    Some(vec![StateEvent::KeyOn {
                        channel: slot as u8,
                        tone,
                    }])
                } else {
                    None
                }
            }
            (KeyState::On, KeyState::Off) => Some(vec![StateEvent::KeyOff {
                channel: slot as u8,
            }]),
            _ => None,
        }
    }

    /// Handle frequency register writes (registers 12, 13, 14)
    /// Handle frequency register write
    ///
    /// # Arguments
    ///
    /// * `slot` - Slot index (0-47)
    ///
    /// # Returns
    ///
    /// Some(vec![StateEvent::ToneChange) if tone changed while key is on, None otherwise
    fn handle_frequency_register(&mut self, slot: usize) -> Option<Vec<StateEvent>> {
        if slot >= YMF271_CHANNELS {
            return None;
        }

        // If key is on and frequency changed, emit ToneChange
        if self.channels[slot].key_state == KeyState::On
            && let Some(tone) = self.extract_tone(slot)
        {
            let tone_changed = self.channels[slot]
                .tone
                .as_ref()
                .map(|old_tone| old_tone.fnum != tone.fnum || old_tone.block != tone.block)
                .unwrap_or(true);

            if tone_changed {
                self.channels[slot].tone = Some(tone);
                return Some(vec![StateEvent::ToneChange {
                    channel: slot as u8,
                    tone,
                }]);
            }
        }

        None
    }

    /// Handle register write to the selected slot
    ///
    /// # Arguments
    ///
    /// * `register` - Register offset (0-31)
    /// * `value` - Value written
    ///
    /// # Returns
    ///
    /// Some(StateEvent) if state changed, None otherwise
    fn handle_slot_register_write(&mut self, register: u8, value: u8) -> Option<Vec<StateEvent>> {
        let slot = (self.selected_slot % 48) as usize;

        // Only track first 12 slots (FM channels)
        if slot >= YMF271_CHANNELS {
            return None;
        }

        match register {
            // Register 0: Key On + External enable/output
            0 => self.handle_key_on_register(slot, value),

            // Register 12: Block (octave)
            12 => self.handle_frequency_register(slot),

            // Register 13: F-number high
            13 => self.handle_frequency_register(slot),

            // Register 14: F-number low
            14 => self.handle_frequency_register(slot),

            // Other registers - store but don't generate events
            _ => None,
        }
    }
}

impl ChipState for Ymf271State {
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

        // YMF271 uses a two-step register access:
        // 1. Write slot number to register select (we treat high bit as indicator)
        // 2. Write data to the register offset
        //
        // For simplification, we use:
        // - Registers 0x00-0x1F: Slot register writes (after slot selection)
        // - Register 0x80+: Slot selection (bit 7 set indicates slot select)

        if register >= 0x80 {
            // Slot selection
            self.set_selected_slot(value);
            None
        } else {
            // Register write to selected slot
            self.handle_slot_register_write(register, value)
        }
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.clear();
        }
        self.selected_slot = 0;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        YMF271_CHANNELS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ymf271_key_on() {
        let mut state = Ymf271State::new(16_934_400.0);

        // Select slot 0
        state.on_register_write(0x80, 0x00);

        // Set block
        state.on_register_write(12, 0x04); // Block 4

        // Set F-number
        state.on_register_write(13, 0x02); // F-num high
        state.on_register_write(14, 0x00); // F-num low

        // Key on
        let event = state.on_register_write(0, 0x01);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { channel, tone } = &events[0] {
            assert_eq!(*channel, 0);
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        } else {
            panic!("Expected KeyOn event");
        }
    }

    #[test]
    fn test_ymf271_key_off() {
        let mut state = Ymf271State::new(16_934_400.0);

        // Set up and key on slot 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(12, 0x04);
        state.on_register_write(13, 0x02);
        state.on_register_write(14, 0x00);
        state.on_register_write(0, 0x01);

        // Key off
        let event = state.on_register_write(0, 0x00);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn test_ymf271_tone_change() {
        let mut state = Ymf271State::new(16_934_400.0);

        // Set up and key on slot 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(12, 0x04);
        state.on_register_write(13, 0x02);
        state.on_register_write(14, 0x00);
        state.on_register_write(0, 0x01);

        // Change frequency while key is on
        let event = state.on_register_write(14, 0x80);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], StateEvent::ToneChange { .. }));
    }

    #[test]
    fn test_ymf271_multiple_channels() {
        let mut state = Ymf271State::new(16_934_400.0);

        // Slot 0
        state.on_register_write(0x80, 0x00);
        state.on_register_write(12, 0x04);
        state.on_register_write(13, 0x02);
        state.on_register_write(14, 0x00);
        state.on_register_write(0, 0x01);

        // Slot 5
        state.on_register_write(0x80, 0x05);
        state.on_register_write(12, 0x05);
        state.on_register_write(13, 0x03);
        state.on_register_write(14, 0x00);
        state.on_register_write(0, 0x01);

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::On);
        assert_eq!(state.channel(5).unwrap().key_state, KeyState::On);
    }

    #[test]
    fn test_ymf271_channel_count() {
        let state = Ymf271State::new(16_934_400.0);
        assert_eq!(state.channel_count(), 12);
    }

    #[test]
    fn test_ymf271_reset() {
        let mut state = Ymf271State::new(16_934_400.0);

        state.on_register_write(0x80, 0x00);
        state.on_register_write(12, 0x04);
        state.on_register_write(13, 0x02);
        state.on_register_write(14, 0x00);
        state.on_register_write(0, 0x01);

        state.reset();

        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
        assert_eq!(state.selected_slot(), 0);
    }

    #[test]
    fn test_ymf271_block_extraction() {
        let mut state = Ymf271State::new(16_934_400.0);

        state.on_register_write(0x80, 0x00);
        state.on_register_write(12, 0x07); // Block 7
        state.on_register_write(13, 0x0F);
        state.on_register_write(14, 0xFF);
        let event = state.on_register_write(0, 0x01);

        assert!(event.is_some());
        let events = event.as_ref().unwrap();
        assert_eq!(events.len(), 1);
        if let StateEvent::KeyOn { tone, .. } = &events[0] {
            assert_eq!(tone.block, 7);
            assert_eq!(tone.fnum, 0xFFF);
        } else {
            panic!("Expected KeyOn event");
        }
    }
}
