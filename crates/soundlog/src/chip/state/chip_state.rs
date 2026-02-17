//! Chip state tracking trait.
//!
//! This module provides the `ChipState` trait that chip-specific implementations
//! must implement to provide register state tracking and event generation.

use crate::chip::event::StateEvent;

/// Base trait for chip state tracking
///
/// Implement this trait for each chip type to provide chip-specific
/// register decoding and event generation logic.
pub trait ChipState: Send + Sync {
    /// Register address type (u8 for most chips, u16 for chips with large address spaces like VSU)
    type Register: Copy + From<u8>;

    /// Register value type (u8 for most chips, u16 or u32 for chips with wider registers)
    type Value: Copy + From<u8>;

    /// Update state from a register write
    ///
    /// This method is called for each register write and should update
    /// the internal state accordingly. It returns an optional Vec of StateEvents
    /// if one or more notable events occurred (key on/off, tone change, etc.).
    ///
    /// # Arguments
    ///
    /// * `register` - Register address being written
    /// * `value` - Value being written to the register
    ///
    /// # Returns
    ///
    /// Some(`Vec<StateEvent>`) if one or more notable events occurred, None otherwise
    fn on_register_write(
        &mut self,
        register: Self::Register,
        value: Self::Value,
    ) -> Option<Vec<StateEvent>>;

    /// Read a register value
    ///
    /// This method allows external programs to access stored register values
    /// for emulation or debugging purposes.
    ///
    /// # Arguments
    ///
    /// * `register` - Register address to read
    ///
    /// # Returns
    ///
    /// Some(value) if the register has been written, None otherwise
    fn read_register(&self, register: Self::Register) -> Option<Self::Value>;

    /// Reset all state
    ///
    /// Clears all channel states and returns the chip to its initial state.
    fn reset(&mut self);

    /// Get the number of channels this chip has
    ///
    /// # Returns
    ///
    /// The total number of channels supported by this chip
    fn channel_count(&self) -> usize;
}
