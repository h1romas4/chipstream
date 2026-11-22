//! ES5505 and ES5506 register write tracking.
//!
//! Tracks values written by the host, not changes caused by sample playback,
//! envelope processing, interrupts, or external inputs.

use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::StateEvent;

/// Physical chip variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Es550xVariant {
    /// ES5505, with a 16-bit host bus.
    Es5505,
    /// ES5506, with an 8-bit host bus and a 32-bit write latch.
    Es5506,
}

/// Register write state for one ES5505 or ES5506.
///
/// Registers passed to [`ChipState`] are physical register indices (0–15),
/// not byte offsets. Reads use the currently selected page and return the last
/// written value, after applying writable-bit masks. They are not hardware
/// reads and do not simulate playback. Use the bus methods to preserve partial
/// writes and the ES5506 latch.
#[derive(Debug, Clone)]
pub struct Es5506State {
    variant: Es550xVariant,
    page: u8,
    latch: u32,
    registers: SparseStorage<u16, u32>,
}

impl Es5506State {
    /// Create an ES5506 tracker. The clock is accepted for API consistency.
    pub fn new(clock: f32) -> Self {
        Self::with_variant(clock, Es550xVariant::Es5506)
    }

    /// Create a tracker for a physical chip variant.
    pub fn with_variant(_clock: f32, variant: Es550xVariant) -> Self {
        Self {
            variant,
            page: 0,
            latch: 0,
            registers: SparseStorage::default(),
        }
    }

    /// Return the configured physical chip variant.
    pub fn variant(&self) -> Es550xVariant {
        self.variant
    }

    /// Return the currently selected page.
    pub fn page(&self) -> u8 {
        self.page
    }

    /// Write one byte to the ES5506 host bus.
    ///
    /// The low byte of a 32-bit register commits the shared write latch.
    /// Offsets outside the chip's 64-byte host window are ignored.
    ///
    /// # Panics
    /// Panics if this tracker is not configured for ES5506.
    pub fn write_es5506_bus(&mut self, offset: u8, value: u8) {
        assert_eq!(self.variant, Es550xVariant::Es5506);
        if offset >= 0x40 {
            return;
        }
        let shift = 24 - 8 * (offset & 3);
        self.latch = (self.latch & !(0xff_u32 << shift)) | (u32::from(value) << shift);
        if offset & 3 == 3 {
            let value = self.latch;
            self.latch = 0;
            self.write_register(offset / 4, value, u32::MAX);
        }
    }

    /// Write to the ES5505's 16-bit host bus. Set mask bits for written lanes.
    /// Read-only, unused, and out-of-range registers are ignored.
    ///
    /// # Panics
    /// Panics if this tracker is not configured for ES5505.
    pub fn write_es5505_bus(&mut self, register: u8, value: u16, mask: u16) {
        assert_eq!(self.variant, Es550xVariant::Es5505);
        self.write_register(register, u32::from(value), u32::from(mask));
    }

    fn location_and_mask(&self, register: u8) -> Option<(u16, u32)> {
        if register >= 16 {
            return None;
        }
        let bank = self.page >> 5;
        let voice = self.page & 0x1f;
        let (key_page, mask) = match (self.variant, bank, register) {
            (_, _, 15) => (0x80, 0x7f),
            (Es550xVariant::Es5506, 0 | 1, 0) => (voice, 0xffff),
            (Es550xVariant::Es5506, 0, r @ 1..=10) => {
                let masks = [
                    0x1ffff, 0xffff, 0xff00, 0xffff, 0xff00, 0x1ff, 0xffff, 0xff01, 0xffff, 0xff01,
                ];
                (voice, masks[usize::from(r - 1)])
            }
            (Es550xVariant::Es5506, 0, 11 | 12) => (0x80, 0x1f),
            (Es550xVariant::Es5506, 1, r @ 1..=9) => {
                let masks = [
                    0xfffff800, 0xffffff80, 0xffffffff, 0x3ffff, 0x3ffff, 0x3ffff, 0x3ffff,
                    0x3ffff, 0x3ffff,
                ];
                (0x20 | voice, masks[usize::from(r - 1)])
            }
            (Es550xVariant::Es5506, 1, 10..=12) => (0x81, 0x7f),
            (Es550xVariant::Es5505, 0 | 1, 0) => (voice, 0x0fff),
            (Es550xVariant::Es5505, 0, r @ 1..=11) => {
                let masks = [
                    0xfffe, 0x1fff, 0xffe0, 0x1fff, 0xffe0, 0xfff0, 0xfff0, 0xff00, 0xff00, 0x1fff,
                    0xffff,
                ];
                (voice, masks[usize::from(r - 1)])
            }
            (Es550xVariant::Es5505, 1, 1..=6) => (0x20 | voice, 0xffff),
            (Es550xVariant::Es5505, _, 13) => (0x80, 0x1f),
            (Es550xVariant::Es5505, 2 | 3, 8) => (0x82, 0xf807),
            _ => return None,
        };
        Some(((u16::from(key_page) << 8) | u16::from(register), mask))
    }

    fn write_register(&mut self, register: u8, value: u32, mask: u32) {
        let Some((key, writable)) = self.location_and_mask(register) else {
            return;
        };
        let mask = mask & writable;
        if mask == 0 {
            return;
        }
        let previous = self.registers.read(key).unwrap_or(0);
        let value = (previous & !mask) | (value & mask);
        self.registers.write(key, value);
        if register == 15 {
            self.page = value as u8;
        }
    }
}

impl Default for Es5506State {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl ChipState for Es5506State {
    type Register = u8;
    type Value = u32;

    fn on_register_write(&mut self, register: u8, value: u32) -> Option<Vec<StateEvent>> {
        self.write_register(register, value, u32::MAX);
        None
    }

    fn read_register(&self, register: u8) -> Option<u32> {
        self.location_and_mask(register)
            .and_then(|(key, _)| self.registers.read(key))
    }

    fn reset(&mut self) {
        self.page = 0;
        self.latch = 0;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_5506(state: &mut Es5506State, register: u8, value: u32) {
        for (i, byte) in value.to_be_bytes().into_iter().enumerate() {
            state.write_es5506_bus(register * 4 + i as u8, byte);
        }
    }

    #[test]
    fn es5506_latch_pages_and_aliases() {
        let mut state = Es5506State::new(0.0);
        state.write_es5506_bus(4, 0x12);
        state.write_es5506_bus(5, 0x34);
        state.write_es5506_bus(6, 0x56);
        assert_eq!(state.read_register(1), None);
        state.write_es5506_bus(7, 0x79);
        assert_eq!(state.read_register(1), Some(0x5679 & 0x1ffff));

        write_5506(&mut state, 0, 0x1234);
        write_5506(&mut state, 11, 0x1d);
        write_5506(&mut state, 15, 0x20);
        assert_eq!(state.read_register(0), Some(0x1234));
        write_5506(&mut state, 1, 0x1234_5678);
        assert_eq!(state.read_register(1), Some(0x1234_5000));
        write_5506(&mut state, 15, 1);
        assert_eq!(state.read_register(1), None);
        assert_eq!(state.read_register(11), Some(0x1d));
        write_5506(&mut state, 15, 0);
        assert_eq!(state.read_register(1), Some(0x5679));

        state.write_es5506_bus(4, 0xff);
        state.reset();
        assert_eq!(state.page(), 0);
        state.write_es5506_bus(7, 0x34);
        assert_eq!(state.read_register(1), Some(0x34));
    }

    #[test]
    fn es5505_masked_writes_and_global_registers() {
        let mut state = Es5506State::with_variant(0.0, Es550xVariant::Es5505);
        state.write_es5505_bus(1, 0xab00, 0xff00);
        state.write_es5505_bus(1, 0x00cd, 0x00ff);
        assert_eq!(state.read_register(1), Some(0xabcc));
        state.write_es5505_bus(0, 0x0abc, 0xffff);
        state.write_es5505_bus(13, 0xff, 0xffff);
        state.write_es5505_bus(15, 0x20, 0xffff);
        assert_eq!(state.read_register(0), Some(0x0abc));
        assert_eq!(state.read_register(13), Some(0x1f));
        state.write_es5505_bus(1, 0x1234, 0xffff);
        assert_eq!(state.read_register(1), Some(0x1234));
        state.write_es5505_bus(15, 0, 0xffff);
        assert_eq!(state.read_register(1), Some(0xabcc));
        state.reset();
        assert_eq!(state.variant(), Es550xVariant::Es5505);
        assert_eq!(state.read_register(1), None);
    }
}
