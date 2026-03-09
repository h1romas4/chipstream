#![allow(clippy::needless_return)]
//! PCM chip state implementations.
//!
//! This module provides state trackers for PCM-based chips that don't
//! have traditional tone information (fnum/block). These chips typically play
//! back sampled audio data rather than generating tones.
//!
//! Each chip has its own newtype wrapper to provide type safety and potential
//! for chip-specific extensions in the future.

use super::chip_state::ChipState;
use super::storage::{ArrayStorage, RegisterStorage, SparseStorage};
use crate::chip::event::StateEvent;

macro_rules! impl_pcm_chip_u8_u8 {
    (
        $(#[$meta:meta])*
        $name:ident, $channels:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name {
            /// Register storage
            registers: ArrayStorage<u8, 256>,
            /// Number of channels
            channel_count: usize,
        }

        impl $name {
            /// Create a new chip state tracker
            ///
            /// The clock parameter is accepted for API consistency but not used.
            ///
            /// # Arguments
            ///
            /// * `_clock` - Clock frequency in Hz (unused, accepted for API consistency)
            pub fn new(_clock: f32) -> Self {
                Self {
                    registers: ArrayStorage::default(),
                    channel_count: $channels,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(0.0f32)
            }
        }

        impl ChipState for $name {
            type Register = u8;
            type Value = u8;

            fn on_register_write(
                &mut self,
                register: Self::Register,
                value: Self::Value,
            ) -> Option<Vec<StateEvent>> {
                // Store the register value
                self.registers.write(register, value);
                // PCM chips don't generate tone-related events
                None
            }

            fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
                self.registers.read(register)
            }

            fn reset(&mut self) {
                self.registers.clear();
            }

            fn channel_count(&self) -> usize {
                self.channel_count
            }
        }
    };
}

macro_rules! impl_pcm_chip_u16_u8 {
    (
        $(#[$meta:meta])*
        $name:ident, $channels:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name {
            /// Register storage
            registers: SparseStorage<u16, u8>,
            /// Number of channels
            channel_count: usize,
        }

        impl $name {
            /// Create a new chip state tracker
            ///
            /// The clock parameter is accepted for API consistency but not used.
            ///
            /// # Arguments
            ///
            /// * `_clock` - Clock frequency in Hz (unused, accepted for API consistency)
            pub fn new(_clock: f32) -> Self {
                Self {
                    registers: SparseStorage::default(),
                    channel_count: $channels,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(0.0f32)
            }
        }

        impl ChipState for $name {
            type Register = u16;
            type Value = u8;

            fn on_register_write(
                &mut self,
                register: Self::Register,
                value: Self::Value,
            ) -> Option<Vec<StateEvent>> {
                // Store the register value
                self.registers.write(register, value);
                // PCM chips don't generate tone-related events
                None
            }

            fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
                self.registers.read(register)
            }

            fn reset(&mut self) {
                self.registers.clear();
            }

            fn channel_count(&self) -> usize {
                self.channel_count
            }
        }
    };
}

macro_rules! impl_pcm_chip_u8_u16 {
    (
        $(#[$meta:meta])*
        $name:ident, $channels:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name {
            /// Register storage
            registers: ArrayStorage<u16, 256>,
            /// Number of channels
            channel_count: usize,
        }

        impl $name {
            /// Create a new chip state tracker
            ///
            /// The clock parameter is accepted for API consistency but not used.
            ///
            /// # Arguments
            ///
            /// * `_clock` - Clock frequency in Hz (unused, accepted for API consistency)
            pub fn new(_clock: f32) -> Self {
                Self {
                    registers: ArrayStorage::default(),
                    channel_count: $channels,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(0.0f32)
            }
        }

        impl ChipState for $name {
            type Register = u8;
            type Value = u16;

            fn on_register_write(
                &mut self,
                register: Self::Register,
                value: Self::Value,
            ) -> Option<Vec<StateEvent>> {
                // Store the register value
                self.registers.write(register, value);
                // PCM chips don't generate tone-related events
                None
            }

            fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
                self.registers.read(register)
            }

            fn reset(&mut self) {
                self.registers.clear();
            }

            fn channel_count(&self) -> usize {
                self.channel_count
            }
        }
    };
}

macro_rules! impl_pcm_chip_u16_u16 {
    (
        $(#[$meta:meta])*
        $name:ident, $channels:expr
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name {
            /// Register storage
            registers: SparseStorage<u16, u16>,
            /// Number of channels
            channel_count: usize,
        }

        impl $name {
            /// Create a new chip state tracker
            ///
            /// The clock parameter is accepted for API consistency but not used.
            ///
            /// # Arguments
            ///
            /// * `_clock` - Clock frequency in Hz (unused, accepted for API consistency)
            pub fn new(_clock: f32) -> Self {
                Self {
                    registers: SparseStorage::default(),
                    channel_count: $channels,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(0.0f32)
            }
        }

        impl ChipState for $name {
            type Register = u16;
            type Value = u16;

            fn on_register_write(
                &mut self,
                register: Self::Register,
                value: Self::Value,
            ) -> Option<Vec<StateEvent>> {
                // Store the register value
                self.registers.write(register, value);
                // PCM chips don't generate tone-related events
                None
            }

            fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
                self.registers.read(register)
            }

            fn reset(&mut self) {
                self.registers.clear();
            }

            fn channel_count(&self) -> usize {
                self.channel_count
            }
        }
    };
}

// Sega PCM (offset: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// Sega PCM state (16 channels)
    SegaPcmState,
    16
);

// RF5C68 (offset: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// RF5C68 state (8 channels)
    Rf5c68State,
    8
);

// RF5C164 (offset: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// RF5C164 state (8 channels)
    Rf5c164State,
    8
);

// YMZ280B (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// YMZ280B state (8 channels)
    Ymz280bState,
    8
);

// MultiPCM (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// MultiPCM state (28 channels)
    MultiPcmState,
    28
);

// uPD7759 (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// uPD7759 state (1 channel)
    Upd7759State,
    1
);

// OKIM6258 (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// OKIM6258 state (1 channel)
    Okim6258State,
    1
);

// OKIM6295 (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// OKIM6295 state (4 channels)
    Okim6295State,
    4
);

// K054539 (register: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// K054539 state (8 channels)
    K054539State,
    8
);

// C140 (register: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// C140 state (24 channels)
    C140State,
    24
);

// C352 (register: u16, value: u16)
impl_pcm_chip_u16_u16!(
    /// C352 state (32 channels)
    C352State,
    32
);

// K053260 (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// K053260 state (4 channels)
    K053260State,
    4
);

// QSound (register: u8, value: u16)
impl_pcm_chip_u8_u16!(
    /// QSound state (16 channels)
    QsoundState,
    16
);

// SCSP (offset: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// SCSP state (32 channels)
    ScspState,
    32
);

// ES5503 (register: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// ES5503 state (32 channels)
    Es5503State,
    32
);

// ES5506 has two variants: u8 and u16 value types
// We'll use the u16 variant as the default state
impl_pcm_chip_u8_u16!(
    /// ES5506 state (32 channels)
    Es5506State,
    32
);

// X1-010 (offset: u16, value: u8)
impl_pcm_chip_u16_u8!(
    /// X1-010 state (16 channels)
    X1010State,
    16
);

// GA20 (register: u8, value: u8)
impl_pcm_chip_u8_u8!(
    /// GA20 state (4 channels)
    Ga20State,
    4
);

// PWM (register: u8, value: u32)
// PWM uses lower 24 bits of a 32-bit value; track as u32 in storage.
#[derive(Debug, Clone)]
pub struct PwmState {
    /// Register storage (u8 register -> u32 value)
    registers: ArrayStorage<u32, 256>,
    /// Number of channels (PWM generally treated as a single channel for tracking)
    channel_count: usize,
}

impl PwmState {
    /// Create a new PWM state tracker
    ///
    /// The clock parameter is accepted for API consistency but not used.
    ///
    /// # Arguments
    ///
    /// * `_clock` - Clock frequency in Hz (unused, accepted for API consistency)
    pub fn new(_clock: f32) -> Self {
        Self {
            registers: ArrayStorage::default(),
            channel_count: 1,
        }
    }
}

impl Default for PwmState {
    fn default() -> Self {
        Self::new(0.0f32)
    }
}

impl ChipState for PwmState {
    type Register = u8;
    type Value = u32;

    fn on_register_write(
        &mut self,
        register: Self::Register,
        value: Self::Value,
    ) -> Option<Vec<StateEvent>> {
        // Only lower 24 bits are used by PWM spec; mask to be explicit.
        let masked = value & 0x00FF_FFFF;
        self.registers.write(register, masked);
        // PWM writes do not generate tone/key events in this tracker.
        None
    }

    fn read_register(&self, register: Self::Register) -> Option<Self::Value> {
        self.registers.read(register)
    }

    fn reset(&mut self) {
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        self.channel_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sega_pcm_register_storage() {
        let mut state = SegaPcmState::new(0.0f32);

        // Initially no registers stored
        assert_eq!(state.read_register(0x0000), None);

        // Write a register
        let event = state.on_register_write(0x0010, 0x42);
        assert!(event.is_none()); // PCM chips don't generate events

        // Read it back
        assert_eq!(state.read_register(0x0010), Some(0x42));

        // Write another
        state.on_register_write(0x0020, 0x99);
        assert_eq!(state.read_register(0x0020), Some(0x99));

        // First register still there
        assert_eq!(state.read_register(0x0010), Some(0x42));
    }

    #[test]
    fn test_pcm_reset() {
        let mut state = Rf5c68State::new(0.0f32);

        state.on_register_write(0x0010, 0x42);
        state.on_register_write(0x0020, 0x99);

        assert_eq!(state.read_register(0x0010), Some(0x42));

        state.reset();

        // Registers cleared after reset
        assert_eq!(state.read_register(0x0010), None);
        assert_eq!(state.read_register(0x0020), None);

        // Channel count unchanged
        assert_eq!(state.channel_count(), 8);
    }

    #[test]
    fn test_channel_counts() {
        assert_eq!(SegaPcmState::new(0.0f32).channel_count(), 16);
        assert_eq!(Rf5c68State::new(0.0f32).channel_count(), 8);
        assert_eq!(Rf5c164State::new(0.0f32).channel_count(), 8);
        assert_eq!(Ymz280bState::new(0.0f32).channel_count(), 8);
        assert_eq!(MultiPcmState::new(0.0f32).channel_count(), 28);
        assert_eq!(Upd7759State::new(0.0f32).channel_count(), 1);
        assert_eq!(Okim6258State::new(0.0f32).channel_count(), 1);
        assert_eq!(Okim6295State::new(0.0f32).channel_count(), 4);
        assert_eq!(K054539State::new(0.0f32).channel_count(), 8);
        assert_eq!(C140State::new(0.0f32).channel_count(), 24);
        assert_eq!(C352State::new(0.0f32).channel_count(), 32);
        assert_eq!(K053260State::new(0.0f32).channel_count(), 4);
        assert_eq!(QsoundState::new(0.0f32).channel_count(), 16);
        assert_eq!(ScspState::new(0.0f32).channel_count(), 32);
        assert_eq!(Es5503State::new(0.0f32).channel_count(), 32);
        assert_eq!(Es5506State::new(0.0f32).channel_count(), 32);
        assert_eq!(X1010State::new(0.0f32).channel_count(), 16);
        assert_eq!(Ga20State::new(0.0f32).channel_count(), 4);
    }

    #[test]
    fn test_multiple_chips_independent() {
        let mut sega = SegaPcmState::new(0.0f32);
        let mut okim = Okim6295State::new(0.0f32);

        sega.on_register_write(0x0010, 0xAA);
        okim.on_register_write(0x10, 0xBB);

        assert_eq!(sega.read_register(0x0010), Some(0xAA));
        assert_eq!(okim.read_register(0x10), Some(0xBB));
    }

    #[test]
    fn test_qsound_u16_value() {
        let mut state = QsoundState::new(0.0f32);

        // QSound has u8 register but u16 value
        state.on_register_write(0x10, 0xBEEF);
        assert_eq!(state.read_register(0x10), Some(0xBEEF));
    }

    #[test]
    fn test_c352_u16_u16() {
        let mut state = C352State::new(0.0f32);

        // C352 has u16 register and u16 value
        state.on_register_write(0x0100, 0x1234);
        assert_eq!(state.read_register(0x0100), Some(0x1234));
    }

    #[test]
    fn test_es5506_u16_value() {
        let mut state = Es5506State::new(0.0f32);

        // ES5506 state uses u8 register but u16 value
        state.on_register_write(0x1A, 0xBEEF);
        assert_eq!(state.read_register(0x1A), Some(0xBEEF));
    }

    #[test]
    fn test_k054539_u16_register() {
        let mut state = K054539State::new(0.0f32);

        // K054539 has u16 register
        state.on_register_write(0x0200, 0x42);
        assert_eq!(state.read_register(0x0200), Some(0x42));
    }

    // New comprehensive test: verify Default and reset behavior across all PCM chips.
    #[test]
    fn test_default_and_reset_for_all_pcm_chips() {
        // SegaPcmState (u16 register -> u8 value)
        {
            let mut s = SegaPcmState::default();
            assert_eq!(
                SegaPcmState::default().channel_count(),
                SegaPcmState::new(0.0f32).channel_count()
            );
            assert_eq!(s.read_register(0x0010u16), None);
            s.on_register_write(0x0010u16, 0x42u8);
            assert_eq!(s.read_register(0x0010u16), Some(0x42u8));
            s.reset();
            assert_eq!(s.read_register(0x0010u16), None);
        }

        // Rf5c68State (u16 -> u8)
        {
            let mut s = Rf5c68State::default();
            assert_eq!(
                Rf5c68State::default().channel_count(),
                Rf5c68State::new(0.0f32).channel_count()
            );
            s.on_register_write(0x0010u16, 0x11u8);
            assert_eq!(s.read_register(0x0010u16), Some(0x11u8));
            s.reset();
            assert_eq!(s.read_register(0x0010u16), None);
        }

        // Rf5c164State (u16 -> u8)
        {
            let mut s = Rf5c164State::default();
            s.on_register_write(0x0010u16, 0x22u8);
            assert_eq!(s.read_register(0x0010u16), Some(0x22u8));
            s.reset();
            assert_eq!(s.read_register(0x0010u16), None);
        }

        // Ymz280bState (u8 -> u8)
        {
            let mut s = Ymz280bState::default();
            s.on_register_write(0x10u8, 0x33u8);
            assert_eq!(s.read_register(0x10u8), Some(0x33u8));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }

        // MultiPcmState (u8 -> u8)
        {
            let mut s = MultiPcmState::default();
            s.on_register_write(0x10u8, 0x44u8);
            assert_eq!(s.read_register(0x10u8), Some(0x44u8));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }

        // Upd7759State (u8 -> u8)
        {
            let mut s = Upd7759State::default();
            s.on_register_write(0x01u8, 0x55u8);
            assert_eq!(s.read_register(0x01u8), Some(0x55u8));
            s.reset();
            assert_eq!(s.read_register(0x01u8), None);
        }

        // Okim6258State (u8 -> u8)
        {
            let mut s = Okim6258State::default();
            s.on_register_write(0x10u8, 0x66u8);
            assert_eq!(s.read_register(0x10u8), Some(0x66u8));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }

        // Okim6295State (u8 -> u8)
        {
            let mut s = Okim6295State::default();
            s.on_register_write(0x10u8, 0x77u8);
            assert_eq!(s.read_register(0x10u8), Some(0x77u8));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }

        // K054539State (u16 -> u8)
        {
            let mut s = K054539State::default();
            s.on_register_write(0x0200u16, 0x88u8);
            assert_eq!(s.read_register(0x0200u16), Some(0x88u8));
            s.reset();
            assert_eq!(s.read_register(0x0200u16), None);
        }

        // C140State (u16 -> u8)
        {
            let mut s = C140State::default();
            s.on_register_write(0x0030u16, 0x99u8);
            assert_eq!(s.read_register(0x0030u16), Some(0x99u8));
            s.reset();
            assert_eq!(s.read_register(0x0030u16), None);
        }

        // C352State (u16 -> u16)
        {
            let mut s = C352State::default();
            s.on_register_write(0x0100u16, 0x1234u16);
            assert_eq!(s.read_register(0x0100u16), Some(0x1234u16));
            s.reset();
            assert_eq!(s.read_register(0x0100u16), None);
        }

        // K053260State (u8 -> u8)
        {
            let mut s = K053260State::default();
            s.on_register_write(0x10u8, 0xAAu8);
            assert_eq!(s.read_register(0x10u8), Some(0xAAu8));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }

        // QsoundState (u8 -> u16)
        {
            let mut s = QsoundState::default();
            s.on_register_write(0x10u8, 0xBEEFu16);
            assert_eq!(s.read_register(0x10u8), Some(0xBEEFu16));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }

        // ScspState (u16 -> u8)
        {
            let mut s = ScspState::default();
            s.on_register_write(0x0020u16, 0x12u8);
            assert_eq!(s.read_register(0x0020u16), Some(0x12u8));
            s.reset();
            assert_eq!(s.read_register(0x0020u16), None);
        }

        // Es5503State (u16 -> u8)
        {
            let mut s = Es5503State::default();
            s.on_register_write(0x0035u16, 0x13u8);
            assert_eq!(s.read_register(0x0035u16), Some(0x13u8));
            s.reset();
            assert_eq!(s.read_register(0x0035u16), None);
        }

        // Es5506State (u8 -> u16)
        {
            let mut s = Es5506State::default();
            s.on_register_write(0x1Au8, 0xCAFEu16);
            assert_eq!(s.read_register(0x1Au8), Some(0xCAFEu16));
            s.reset();
            assert_eq!(s.read_register(0x1Au8), None);
        }

        // X1010State (u16 -> u8)
        {
            let mut s = X1010State::default();
            s.on_register_write(0x0010u16, 0x21u8);
            assert_eq!(s.read_register(0x0010u16), Some(0x21u8));
            s.reset();
            assert_eq!(s.read_register(0x0010u16), None);
        }

        // Ga20State (u8 -> u8)
        {
            let mut s = Ga20State::default();
            s.on_register_write(0x05u8, 0x31u8);
            assert_eq!(s.read_register(0x05u8), Some(0x31u8));
            s.reset();
            assert_eq!(s.read_register(0x05u8), None);
        }

        // PwmState (u8 -> u32 masked to 24 bits)
        {
            let mut s = PwmState::default();
            s.on_register_write(0x10u8, 0x00FF_FFFFu32);
            assert_eq!(s.read_register(0x10u8), Some(0x00FF_FFFFu32 & 0x00FF_FFFF));
            s.reset();
            assert_eq!(s.read_register(0x10u8), None);
        }
    }
}
