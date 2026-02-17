//! Register storage backends.
//!
//! This module provides the `RegisterStorage` trait and multiple implementations
//! optimized for different chip architectures and usage patterns.
//!
//! # Generic Type Support
//!
//! All storage implementations support different register address and value types
//! through associated types. This allows you to use:
//! - `u8`, `u16`, or `u32` for register addresses
//! - `u8`, `u16`, or `u32` for register values
//!
//! # Examples
//!
//! ```
//! use soundlog::chip::state::{SparseStorage, ArrayStorage, RegisterStorage};
//!
//! // Standard u8 register addresses and u8 values
//! let mut storage8 = SparseStorage::<u8, u8>::default();
//! storage8.write(0xFF, 0x42);
//!
//! // u16 register addresses with u32 values
//! let mut storage_wide = SparseStorage::<u16, u32>::default();
//! storage_wide.write(0x1234, 0xDEADBEEF);
//!
//! // Array storage with u16 values
//! let mut array_storage = ArrayStorage::<u16, 256>::default();
//! array_storage.write(0x20, 0xABCD);
//! ```

use std::fmt::Debug;
use std::hash::Hash;

/// Trait for register storage backend
///
/// This trait abstracts the storage mechanism for chip registers,
/// allowing different implementations optimized for different chips.
///
/// The register address and value types are specified as associated types,
/// allowing each storage implementation to choose the appropriate types.
pub trait RegisterStorage: Default + Clone + Debug {
    /// Register address type (e.g., u8, u16, u32)
    type Register: Copy + Eq + Hash + Debug + Default;

    /// Register value type (e.g., u8, u16, u32)
    type Value: Copy + Debug + Default;

    /// Write a value to a register
    ///
    /// # Arguments
    ///
    /// * `register` - Register address
    /// * `value` - Value to write
    fn write(&mut self, register: Self::Register, value: Self::Value);

    /// Read a value from a register
    ///
    /// # Arguments
    ///
    /// * `register` - Register address
    ///
    /// # Returns
    ///
    /// Some(value) if the register has been written, None otherwise
    fn read(&self, register: Self::Register) -> Option<Self::Value>;

    /// Clear all register values
    fn clear(&mut self);

    /// Get the number of registers that have been written
    ///
    /// # Returns
    ///
    /// The count of registers that have non-None values
    fn len(&self) -> usize;

    /// Check if no registers have been written
    ///
    /// # Returns
    ///
    /// true if len() == 0, false otherwise
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Sparse storage using HashMap (flexible, good for chips with large/sparse register space)
///
/// Best for:
/// - Chips with large register address space (e.g., 0x00-0xFF)
/// - Registers that are sparsely populated
/// - Unknown access patterns
///
/// # Type Parameters
///
/// * `R` - Register address type (e.g., u8, u16, u32)
/// * `V` - Register value type (e.g., u8, u16, u32)
///
/// # Memory Usage
///
/// Memory usage scales with the number of unique registers written,
/// not the total register address space.
///
/// Note: The maximum number of entries this storage can hold depends on the
/// width of the register type `R`. For example:
/// - `u8` register addresses → at most 256 distinct entries
/// - `u16` register addresses → at most 65,536 distinct entries
///
/// While these are finite limits, storing all possible addresses for a wide
/// register type (e.g. `u16`) can still consume a significant amount of memory
/// (approximately `sizeof(R) + sizeof(V)` plus HashMap overhead per entry).
/// If you expect a large number of distinct register writes from untrusted or
/// malformed input, prefer `ArrayStorage` or `CompactStorage` when appropriate,
/// or ensure the caller enforces sensible limits.
///
/// # Examples
///
/// ```
/// use soundlog::chip::state::{SparseStorage, RegisterStorage};
///
/// // u8 register addresses and values (default)
/// let mut storage = SparseStorage::<u8, u8>::default();
/// storage.write(0xFF, 0x42);
/// assert_eq!(storage.read(0xFF), Some(0x42));
/// assert_eq!(storage.read(0x00), None);
///
/// // u16 register addresses with u32 values
/// let mut storage16 = SparseStorage::<u16, u32>::default();
/// storage16.write(0x1234, 0xDEADBEEF);
/// assert_eq!(storage16.read(0x1234), Some(0xDEADBEEF));
/// ```
#[derive(Debug, Clone, Default)]
pub struct SparseStorage<R = u8, V = u8>
where
    R: Copy + Eq + Hash + Debug + Default,
    V: Copy + Debug + Default,
{
    registers: std::collections::HashMap<R, V>,
}

impl<R, V> RegisterStorage for SparseStorage<R, V>
where
    R: Copy + Eq + Hash + Debug + Default,
    V: Copy + Debug + Default,
{
    type Register = R;
    type Value = V;

    fn write(&mut self, register: Self::Register, value: Self::Value) {
        self.registers.insert(register, value);
    }

    fn read(&self, register: Self::Register) -> Option<Self::Value> {
        self.registers.get(&register).copied()
    }

    fn clear(&mut self) {
        self.registers.clear();
    }

    fn len(&self) -> usize {
        self.registers.len()
    }
}

/// Fixed-size array storage (fast, good for chips with contiguous register space)
///
/// Best for:
/// - Chips with small, contiguous register space
/// - Predictable register access patterns
/// - Performance-critical applications
///
/// # Type Parameters
///
/// * `V` - Register value type (e.g., u8, u16, u32)
/// * `N` - Size of the register array (must match chip's register address range)
///
/// # Limitations
///
/// Register addresses must be u8 and fit within the array bounds.
/// This implementation is designed for chips with contiguous, small register spaces.
///
/// # Memory Usage
///
/// Always uses N * `size_of::<Option<V>>()` bytes of memory regardless of how many registers are written.
///
/// # Examples
///
/// ```
/// use soundlog::chip::state::{ArrayStorage, RegisterStorage};
///
/// // u8 addresses with u8 values
/// let mut storage = ArrayStorage::<u8, 256>::default();
/// storage.write(0x10, 0x99);
/// assert_eq!(storage.read(0x10), Some(0x99));
/// assert_eq!(storage.read(0x11), None);
///
/// // u8 addresses with u16 values
/// let mut storage16 = ArrayStorage::<u16, 128>::default();
/// storage16.write(0x20, 0xABCD);
/// assert_eq!(storage16.read(0x20), Some(0xABCD));
/// ```
#[derive(Debug, Clone)]
pub struct ArrayStorage<V = u8, const N: usize = 256>
where
    V: Copy + Debug + Default,
{
    registers: [Option<V>; N],
}

impl<V, const N: usize> Default for ArrayStorage<V, N>
where
    V: Copy + Debug + Default,
{
    fn default() -> Self {
        Self {
            registers: [None; N],
        }
    }
}

impl<V, const N: usize> RegisterStorage for ArrayStorage<V, N>
where
    V: Copy + Debug + Default,
{
    type Register = u8;
    type Value = V;

    fn write(&mut self, register: Self::Register, value: Self::Value) {
        let idx = register as usize;
        if idx < N {
            self.registers[idx] = Some(value);
        }
    }

    fn read(&self, register: Self::Register) -> Option<Self::Value> {
        let idx = register as usize;
        if idx < N { self.registers[idx] } else { None }
    }

    fn clear(&mut self) {
        self.registers = [None; N];
    }

    fn len(&self) -> usize {
        self.registers.iter().filter(|v| v.is_some()).count()
    }
}

/// Compact storage using bitfield and sparse array (memory-efficient)
///
/// Best for:
/// - Memory-constrained environments
/// - Many chip instances tracked simultaneously
/// - Known subset of important registers
/// - u8 register addresses with any value type
///
/// # Type Parameters
///
/// * `V` - Register value type (e.g., u8, u16, u32)
///
/// # Limitations
///
/// This implementation only supports u8 register addresses (0-255) due to the bitfield design.
/// For larger address spaces, use `SparseStorage` instead.
///
/// # Memory Usage
///
/// Uses a 32-byte bitfield to track which registers have been written,
/// plus a Vec containing only the written values. This is more memory-efficient
/// than HashMap for small numbers of written registers.
///
/// # Implementation Details
///
/// Uses a bitfield (32 bytes = 256 bits) to track which of the 256 possible
/// register addresses have been written, and stores only non-default values
/// in a sparse Vec.
///
/// # Examples
///
/// ```
/// use soundlog::chip::state::{CompactStorage, RegisterStorage};
///
/// // u8 addresses with u8 values (default)
/// let mut storage = CompactStorage::<u8>::default();
/// storage.write(0x28, 0xAA);
/// assert_eq!(storage.read(0x28), Some(0xAA));
/// storage.clear();
/// assert_eq!(storage.len(), 0);
///
/// // u8 addresses with u16 values
/// let mut storage16 = CompactStorage::<u16>::default();
/// storage16.write(0x50, 0x1234);
/// assert_eq!(storage16.read(0x50), Some(0x1234));
/// ```
#[derive(Debug, Clone, Default)]
pub struct CompactStorage<V = u8>
where
    V: Copy + Debug + Default,
{
    /// Bitfield indicating which registers have been written (32 bytes = 256 bits)
    written_mask: [u8; 32],
    /// Sparse storage for actually written values (register, value) pairs
    values: Vec<(u8, V)>,
}

impl<V> CompactStorage<V>
where
    V: Copy + Debug + Default,
{
    /// Check if a register has been written
    pub(crate) fn is_written(&self, register: u8) -> bool {
        let byte_idx = (register / 8) as usize;
        let bit_idx = register % 8;
        (self.written_mask[byte_idx] & (1 << bit_idx)) != 0
    }

    /// Mark a register as written
    fn mark_written(&mut self, register: u8) {
        let byte_idx = (register / 8) as usize;
        let bit_idx = register % 8;
        self.written_mask[byte_idx] |= 1 << bit_idx;
    }

    /// Mark a register as unwritten
    #[allow(dead_code)]
    fn mark_unwritten(&mut self, register: u8) {
        let byte_idx = (register / 8) as usize;
        let bit_idx = register % 8;
        self.written_mask[byte_idx] &= !(1 << bit_idx);
    }
}

impl<V> RegisterStorage for CompactStorage<V>
where
    V: Copy + Debug + Default,
{
    type Register = u8;
    type Value = V;

    fn write(&mut self, register: Self::Register, value: Self::Value) {
        if let Some(entry) = self.values.iter_mut().find(|(r, _)| *r == register) {
            // Update existing value
            entry.1 = value;
        } else {
            // Add new value
            self.values.push((register, value));
            self.mark_written(register);
        }
    }

    fn read(&self, register: Self::Register) -> Option<Self::Value> {
        if self.is_written(register) {
            self.values
                .iter()
                .find(|(r, _)| *r == register)
                .map(|(_, v)| *v)
        } else {
            None
        }
    }

    fn clear(&mut self) {
        self.written_mask = [0; 32];
        self.values.clear();
    }

    fn len(&self) -> usize {
        self.values.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sparse_storage_u8() {
        let mut storage = SparseStorage::<u8, u8>::default();
        assert_eq!(storage.len(), 0);
        assert!(storage.is_empty());

        storage.write(0xFF, 0x42);
        assert_eq!(storage.read(0xFF), Some(0x42));
        assert_eq!(storage.read(0x00), None);
        assert_eq!(storage.len(), 1);
        assert!(!storage.is_empty());

        storage.write(0x10, 0x99);
        assert_eq!(storage.read(0x10), Some(0x99));
        assert_eq!(storage.len(), 2);

        storage.clear();
        assert_eq!(storage.len(), 0);
        assert_eq!(storage.read(0xFF), None);
    }

    #[test]
    fn test_sparse_storage_u16_u32() {
        let mut storage = SparseStorage::<u16, u32>::default();
        assert_eq!(storage.len(), 0);

        storage.write(0x1234, 0xDEADBEEF);
        assert_eq!(storage.read(0x1234), Some(0xDEADBEEF));
        assert_eq!(storage.len(), 1);

        storage.write(0xFFFF, 0xCAFEBABE);
        assert_eq!(storage.read(0xFFFF), Some(0xCAFEBABE));
        assert_eq!(storage.len(), 2);

        storage.clear();
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_array_storage_u8() {
        let mut storage = ArrayStorage::<u8, 256>::default();
        assert_eq!(storage.len(), 0);

        storage.write(0x10, 0x99);
        assert_eq!(storage.read(0x10), Some(0x99));
        assert_eq!(storage.read(0x11), None);
        assert_eq!(storage.len(), 1);

        storage.write(0xFF, 0xAA);
        assert_eq!(storage.read(0xFF), Some(0xAA));
        assert_eq!(storage.len(), 2);

        // Test out of bounds (should be ignored)
        let mut small_storage = ArrayStorage::<u8, 16>::default();
        small_storage.write(0xFF, 0x42);
        assert_eq!(small_storage.read(0xFF), None);

        storage.clear();
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_array_storage_u16() {
        let mut storage = ArrayStorage::<u16, 128>::default();
        assert_eq!(storage.len(), 0);

        storage.write(0x20, 0xABCD);
        assert_eq!(storage.read(0x20), Some(0xABCD));
        assert_eq!(storage.len(), 1);

        storage.write(0x7F, 0x1234);
        assert_eq!(storage.read(0x7F), Some(0x1234));
        assert_eq!(storage.len(), 2);

        storage.clear();
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_compact_storage_u8() {
        let mut storage = CompactStorage::<u8>::default();
        assert_eq!(storage.len(), 0);

        storage.write(0x28, 0xAA);
        assert_eq!(storage.read(0x28), Some(0xAA));
        assert_eq!(storage.len(), 1);
        assert!(storage.is_written(0x28));
        assert!(!storage.is_written(0x29));

        // Update existing register
        storage.write(0x28, 0xBB);
        assert_eq!(storage.read(0x28), Some(0xBB));
        assert_eq!(storage.len(), 1);

        storage.write(0xA0, 0x10);
        assert_eq!(storage.read(0xA0), Some(0x10));
        assert_eq!(storage.len(), 2);

        storage.clear();
        assert_eq!(storage.len(), 0);
        assert!(!storage.is_written(0x28));
    }

    #[test]
    fn test_compact_storage_u16() {
        let mut storage = CompactStorage::<u16>::default();
        assert_eq!(storage.len(), 0);

        storage.write(0x50, 0x1234);
        assert_eq!(storage.read(0x50), Some(0x1234));
        assert_eq!(storage.len(), 1);

        storage.write(0xA0, 0xABCD);
        assert_eq!(storage.read(0xA0), Some(0xABCD));
        assert_eq!(storage.len(), 2);

        storage.clear();
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_storage_trait_consistency() {
        // Test that all storage types behave consistently
        fn test_storage<S: RegisterStorage<Register = u8, Value = u8>>() {
            let mut storage = S::default();

            // Initial state
            assert_eq!(storage.len(), 0);
            assert!(storage.is_empty());
            assert_eq!(storage.read(0x00), None);

            // Write and read
            storage.write(0x42, 0x99);
            assert_eq!(storage.read(0x42), Some(0x99));
            assert_eq!(storage.len(), 1);
            assert!(!storage.is_empty());

            // Overwrite
            storage.write(0x42, 0xAA);
            assert_eq!(storage.read(0x42), Some(0xAA));
            assert_eq!(storage.len(), 1);

            // Clear
            storage.clear();
            assert_eq!(storage.len(), 0);
            assert_eq!(storage.read(0x42), None);
        }

        test_storage::<SparseStorage<u8, u8>>();
        test_storage::<ArrayStorage<u8, 256>>();
        test_storage::<CompactStorage<u8>>();
    }
}
