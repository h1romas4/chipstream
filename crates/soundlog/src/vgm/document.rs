//! VGM document and builder utilities
//!
//! This module defines the in-memory representation for a complete VGM
//! file (`VgmDocument`) and a builder (`VgmBuilder`) to construct documents
//! programmatically. It also provides conversion traits to/from bytes and
//! helpers to serialize the document to a valid VGM file.
//!
//! Responsibilities:
//! - `VgmDocument` holds the VGM header, an ordered command stream,
//!   optional GD3 metadata and optional extra header.
//! - `VgmBuilder` incrementally assembles a document, computes derived
//!   header fields (e.g. `total_samples`, `loop_offset`) and finalizes
//!   the document for serialization.
//! - Conversions: `TryFrom<&[u8]> for VgmDocument` (parsing) and `From<VgmDocument> for Vec<u8>`
//!   (serialization) are provided.
//!
//! Notes:
//! - The builder and serialization logic preserve header offset semantics
//!   used across the crate (including `data_offset` fallbacks and stored
//!   `extra_header_offset` semantics).
//! - Most items are crate-visible and intended for use inside `soundlog`.
use crate::chip;
use crate::meta::Gd3;
use crate::vgm::command::Instance;
use crate::vgm::command::VgmCommand;
use crate::vgm::detail;
use crate::vgm::header::{VgmExtraHeader, VgmHeader};
use crate::vgm::parser;
use std::convert::TryFrom;

#[derive(Debug, Clone, PartialEq, Default)]
/// A complete VGM document, consisting of a header, an ordered command
/// stream, and optional GD3 metadata and an optional extra header.
///
/// Construct `VgmDocument` instances using `VgmBuilder`. Once assembled,
/// call `VgmDocument::to_bytes()` to obtain the serialized VGM file
/// bytes suitable for writing to disk.
pub struct VgmDocument {
    pub header: VgmHeader,
    pub extra_header: Option<VgmExtraHeader>,
    pub commands: Vec<VgmCommand>,
    pub gd3: Option<Gd3>,
}

/// Builder for assembling a `VgmDocument`.
///
/// Use this builder to incrementally set header fields, register chip
/// clock frequencies, append commands, and specify a loop point. Methods
/// return `&mut Self` when appropriate to allow chaining. Call
/// `finalize()` to compute derived header fields (for example
/// `total_samples` and `loop_offset`) and obtain the completed
/// `VgmDocument`.
pub struct VgmBuilder {
    document: VgmDocument,
    loop_index: Option<usize>,
}

/// Implementation of `VgmBuilder` methods.
///
/// This `impl` block provides constructors and fluent APIs for building
/// `VgmDocument` instances: adding commands, registering chips, and finalizing
/// the assembled document for serialization.
impl VgmBuilder {
    /// Create a new `VgmBuilder` with a default, empty `VgmDocument`.
    ///
    /// The returned builder is ready to have header fields and commands
    /// appended via the other builder methods.
    pub fn new() -> Self {
        VgmBuilder {
            document: VgmDocument::default(),
            loop_index: None,
        }
    }

    /// Register a chip in the VGM header with its master clock frequency.
    ///
    /// `c` is convertible to `chip::Chip`. `instance` selects which instance
    /// (primary/secondary) the clock applies to. `master_clock` is the chip's
    /// base clock frequency in Hz. For secondary instances the high bit is set
    /// on the stored clock field as per the VGM header convention.
    pub fn register_chip<C, I>(&mut self, c: C, instance: I, master_clock: u32)
    where
        C: Into<chip::Chip>,
        I: Into<Instance>,
    {
        let ch: chip::Chip = c.into();
        let instance: Instance = instance.into();

        self.document
            .header
            .set_chip_clock(ch, instance, master_clock);
    }

    /// Set the loop point by `VgmDocument` index.
    ///
    /// `document_index` is an index into `doc.commands` indicating the command at which
    /// playback should loop. The header's `loop_offset` will be computed in
    /// `finalize()` as a relative offset from 0x1C.
    ///
    /// This method is used when you want to specify a command index directly
    /// into the `VgmDocument`'s `commands` vector. In most cases prefer
    /// `set_loop_offset`, which interprets its argument relative to the first
    /// non-`DataBlock` command so callers do not need to account for
    /// DataBlock relocation performed during finalization.
    ///
    /// Also note that if `document_index` points to a `DataBlock`, the actual
    /// `loop_offset` value computed during `finalize()` may not correctly
    /// correspond to the intended playback position.
    pub fn set_loop_index(&mut self, document_index: usize) -> &mut Self {
        self.loop_index = Some(document_index);
        self
    }

    /// Set the loop point by `VgmDocument` index.
    ///
    /// `index` is an index into `doc.commands` indicating the command at which
    /// playback should loop. The header's `loop_offset` will be computed in
    /// `finalize()` as a relative offset from 0x1C.
    ///
    /// NOTE: The builder treats the first non-`DataBlock` command in the
    /// document as offset 0. `index` is therefore interpreted as an offset
    /// relative to that first non-`DataBlock` command (i.e. passing 0
    /// targets the first non-`DataBlock` command). This avoids callers needing
    /// to account for DataBlock entries that are relocated to the start of the
    /// finalized document.
    pub fn set_loop_offset(&mut self, index: usize) -> &mut Self {
        // Find the index of the first command that is not a DataBlock. If none
        // exist, fall back to index 0 so the requested offset is applied from
        // the start of the command stream.
        let base = self
            .document
            .commands
            .iter()
            .position(|c| !matches!(c, VgmCommand::DataBlock(_)))
            .unwrap_or(0);

        self.loop_index = Some(base.saturating_add(index));
        self
    }

    /// Set the VGM version.
    ///
    /// This should be set before calling `finalize()` to ensure correct
    /// header size calculations.
    pub fn set_version(&mut self, version: u32) -> &mut Self {
        self.document.header.version = version;
        self
    }

    /// Set the sample rate.
    pub fn set_sample_rate(&mut self, sample_rate: u32) -> &mut Self {
        self.document.header.sample_rate = sample_rate;
        self
    }

    /// Append a VGM command to the builder.
    ///
    /// Accepts any type convertible into `VgmCommand` (via `Into`).
    /// Returns `&mut Self` to allow method chaining.
    pub fn add_vgm_command<C>(&mut self, command: C) -> &mut Self
    where
        C: Into<VgmCommand>,
    {
        self.document.commands.push(command.into());
        self
    }

    /// Append a chip write produced by a chip-specific spec.
    ///
    /// `instance` selects the chip instance (`ChipId::Primary` or `ChipId::Secondary`).
    /// `c` must implement `ChipWriteSpec`; the spec will push the appropriate
    /// `VgmCommand` into the builder's command stream. Returns `&mut Self`.
    pub fn add_chip_write<C, I>(&mut self, instance: I, spec: C) -> &mut Self
    where
        I: Into<Instance>,
        (Instance, C): Into<VgmCommand>,
    {
        self.document.commands.push((instance.into(), spec).into());
        self
    }

    /// Attach a `DataBlock` described by a typed detail into the builder.
    ///
    /// Generic convenience helper that accepts any type convertible into
    /// `DataBlockType` (for example `UncompressedStream`) and appends the
    /// constructed on-disk `DataBlock` into the document under construction.
    ///
    /// Note:
    ///
    /// - If you pass ownership (preferred) the `DataBlock` detail is moved and no clone occurs:
    /// - If you pass a reference (`&T`) the library will use the `From<&T>` implementation
    ///   which clones the detail. This is convenient but will duplicate payload data:
    ///
    /// Example:
    ///
    /// ```rust
    /// use soundlog::vgm::detail::UncompressedStream;
    /// use soundlog::vgm::command::StreamChipType;
    /// use soundlog::VgmBuilder;
    ///
    /// let mut builder = VgmBuilder::new();
    /// builder.attach_data_block(UncompressedStream {
    ///     chip_type: StreamChipType::Ym2612Pcm,
    ///     data: vec![0x01, 0x02],
    /// });
    /// ```
    pub fn attach_data_block<D>(&mut self, data_block_detail: D) -> &mut Self
    where
        D: Into<detail::DataBlockType>,
    {
        let dbt: detail::DataBlockType = data_block_detail.into();
        let block = detail::build_data_block(&dbt);
        self.document.commands.push(VgmCommand::DataBlock(block));
        self
    }

    /// Set GD3 metadata for the document under construction.
    ///
    /// This stores the provided `Gd3` into the builder's internal
    /// `VgmDocument` so it will be present on the finalized document.
    pub fn set_gd3(&mut self, gd3: Gd3) -> &mut Self {
        self.document.gd3 = Some(gd3);
        self
    }

    /// Set the extra-header for the document under construction.
    ///
    /// This stores the provided `VgmExtraHeader` into the builder's internal
    /// `VgmDocument` so it will be included when the document is serialized.
    /// The `extra_header_offset` and `data_offset` fields in the header are
    /// reset to 0 so that `finalize()` will recalculate them based on the
    /// actual header size. The extra header's internal offset/size fields
    /// are also reset to allow automatic recalculation during serialization.
    pub fn set_extra_header(&mut self, mut extra: VgmExtraHeader) -> &mut Self {
        // Reset extra header internal fields so to_bytes() recalculates them
        extra.header_size = 0;
        extra.chip_clock_offset = 0;
        extra.chip_vol_offset = 0;

        self.document.extra_header = Some(extra);
        // Reset extra_header_offset and data_offset so finalize() will recalculate them
        self.document.header.extra_header_offset = 0;
        self.document.header.data_offset = 0;
        self
    }

    /// Finalize the builder and return the assembled `VgmDocument`.
    ///
    /// This computes derived header fields (for example `total_samples` and
    /// `loop_offset`) by scanning accumulated commands. If a loop index has
    /// been set via `set_loop_offset()`, the corresponding command's byte
    /// offset is computed and stored (relative to 0x1C) in the header.
    ///
    /// Additionally, when finalizing the builder the implementation will
    /// ensure the document contains an explicit `EndOfData` command: if the
    /// command stream does not already include one, `finalize()` appends an
    /// `EndOfData` to the end of `commands`. Note that `VgmDocument::to_bytes()`
    /// itself intentionally does not auto-append `EndOfData` — the builder is a
    /// convenience layer that guarantees a finalized document is properly
    /// terminated for common programmatic construction flows.
    ///
    /// Note: DataBlock commands (`VgmCommand::DataBlock`, opcode 0x67) are
    /// relocated to the start of the command stream during finalization so
    /// that DataBlock entries appear at the beginning of the serialized VGM.
    /// DecompressionTable DataBlocks (those with `data_type == 0x7F`)
    /// are promoted ahead of other DataBlocks and thus placed at the very
    /// start of the serialized document.
    ///
    /// The method returns the complete document ready for serialization via
    /// `VgmDocument::to_bytes()`.
    pub fn finalize(mut self) -> VgmDocument {
        // Ensure the document always contains an explicit EndOfData when finalizing.
        if !self
            .document
            .commands
            .iter()
            .any(|c| matches!(c, VgmCommand::EndOfData(_)))
        {
            self.document
                .commands
                .push(VgmCommand::EndOfData(crate::vgm::command::EndOfData {}));
        }

        // Phase 1 (B): Extract DataBlocks that occur at-or-after loop_index,
        // adjust loop_index accordingly, but do NOT yet reinsert them at the front.
        // This extraction is now performed by a dedicated private helper.
        self.relocate_data_block();

        // compute total samples
        let total_sample = self.document.total_samples(0);
        self.document.header.total_samples = total_sample;

        // compute data_offset the same way as VgmDocument::to_bytes
        let data_offset: u32 = match self.document.header.data_offset {
            0 => {
                let version_header_size =
                    crate::vgm::header::VgmHeader::fallback_header_size_for_version(
                        self.document.header.version,
                    );
                // For very old versions where header size < 0x34, use 0x40 as minimum data start
                // This ensures data_offset is always valid (>= 0x0C for 0x40 data start)
                if version_header_size < 0x34 {
                    0x0C // 0x34 + 0x0C = 0x40 (64 bytes, minimum VGM data start)
                } else {
                    (version_header_size as u32).wrapping_sub(0x34)
                }
            }
            v => v,
        };

        // handle extra header offset
        if self.document.extra_header.is_some() && self.document.header.extra_header_offset == 0 {
            let header_len = self.document.header.to_bytes(0, data_offset).len() as u32;
            let extra_offset = header_len.wrapping_sub(0xBC_u32);
            self.document.header.extra_header_offset = extra_offset;

            if let Some(eh) = &self.document.extra_header {
                let extra_bytes_len = eh.to_bytes().len() as u32;
                let new_header_len = header_len.wrapping_add(extra_bytes_len);
                let new_data_offset = new_header_len.wrapping_sub(0x34_u32);
                self.document.header.data_offset = new_data_offset;
            }
        } else if self.document.header.data_offset == 0 {
            self.document.header.data_offset = data_offset;
        }

        // handle loop offset
        if let Some(index) = self.loop_index
            && index < self.document.commands.len()
        {
            let offsets = self.document.sourcemap();
            if index < offsets.len() {
                let (cmd_offset, _cmd_len) = offsets[index];
                let computed_loop_offset = cmd_offset.wrapping_sub(0x1C);
                self.document.header.loop_offset = computed_loop_offset as u32;
                self.document.header.loop_samples = self.document.total_samples(index);
            }
        }

        self.document
    }

    // Relocate DataBlock in `VgmDocument`.
    //
    // Behavior:
    // - If a valid `loop_index` exists, remove (move out of `document.commands`)
    //   every `VgmCommand::DataBlock(_)` whose original index is >= loop_index.
    // - Adjust `loop_index` by adding the number of removed DataBlocks so that
    //   after a future prepend/aggregation the loop index will point to the same
    //   logical command.
    // - Do not specify the positions of DataBlock entries via `loop_index`.
    //
    // Returns the number of DataBlocks that were removed.
    fn relocate_data_block(&mut self) {
        // Count DataBlocks after loop_index
        let move_count = if let Some(loop_index) = self.loop_index {
            self.document
                .commands
                .iter()
                .enumerate()
                .skip(loop_index)
                .filter(|(_i, cmd)| matches!(cmd, VgmCommand::DataBlock(_)))
                .count()
        } else {
            0
        };

        if let Some(loop_index) = self.loop_index {
            self.loop_index = Some(loop_index + move_count);
        }

        // Collect all DataBlock commands.
        let mut data_blocks = Vec::new();
        let mut i = 0;
        while i < self.document.commands.len() {
            if matches!(self.document.commands[i], VgmCommand::DataBlock(_)) {
                data_blocks.push(self.document.commands.remove(i));
            } else {
                i += 1;
            }
        }

        // Prepend DecompressionTable blocks (data_type == 0x7F /* TODO: */) tables
        // to the front of data_blocks
        let mut decompression_tables: Vec<VgmCommand> = Vec::new();
        let mut i = 0;
        while i < data_blocks.len() {
            match &data_blocks[i] {
                VgmCommand::DataBlock(db) if db.data_type == 0x7F => {
                    decompression_tables.push(data_blocks.remove(i));
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        data_blocks.splice(0..0, decompression_tables);

        // Remove DataBlock commands from the document, keeping other commands.
        self.document
            .commands
            .retain(|cmd| !matches!(cmd, VgmCommand::DataBlock(_)));

        // Prepend the collected DataBlocks to the front of the command list.
        self.document.commands.splice(0..0, data_blocks);
    }
}

/// Conversion from `VgmDocument` to `VgmBuilder`.
impl From<VgmDocument> for VgmBuilder {
    fn from(document: VgmDocument) -> Self {
        VgmBuilder {
            document,
            loop_index: None,
        }
    }
}

/// Default implementation for `VgmBuilder`.
impl Default for VgmBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Attempt to convert a raw VGM byte slice into a `VgmDocument`.
///
/// This is a fallible conversion that delegates to `parser::parse_vgm` and
/// returns a `crate::binutil::ParseError` on failure.
///
/// Use `VgmDocument::try_from(bytes)` or `parser::parse_vgm(bytes)` when
/// you need to handle parse errors explicitly.
impl TryFrom<&[u8]> for VgmDocument {
    type Error = crate::binutil::ParseError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        parser::parse_vgm(bytes)
    }
}

/// Convert a `VgmDocument` into its serialized VGM bytes.
impl From<VgmDocument> for Vec<u8> {
    fn from(document: VgmDocument) -> Vec<u8> {
        document.to_bytes()
    }
}

/// Convert a borrowed `VgmDocument` into serialized bytes.
impl From<&VgmDocument> for Vec<u8> {
    fn from(document: &VgmDocument) -> Vec<u8> {
        document.to_bytes()
    }
}

impl VgmDocument {
    /// Return an iterator over `VgmCommand` references.
    pub fn iter(&self) -> std::slice::Iter<'_, VgmCommand> {
        self.commands.iter()
    }

    /// Return a mutable iterator over `VgmCommand` references.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, VgmCommand> {
        self.commands.iter_mut()
    }

    /// Calculates the command index corresponding to the `loop_offset` in the header.
    ///
    /// Returns `Some(index)` if the header has a non-zero loop offset and a matching
    /// command can be found, or `None` if there is no loop or the offset is invalid.
    ///
    /// This is the inverse operation of `VgmBuilder::set_loop_offset()`.
    pub fn loop_command_index(&self) -> Option<usize> {
        if self.header.loop_offset == 0 {
            return None;
        }

        let data_offset = if self.header.data_offset == 0 {
            use crate::vgm::header::VgmHeader;
            (VgmHeader::fallback_header_size_for_version(self.header.version) - 0x34) as u32
        } else {
            self.header.data_offset
        };

        // Calculate actual header length from data_offset
        // data_offset is relative to 0x34, so actual header length is 0x34 + data_offset
        let header_len = 0x34_u32.wrapping_add(data_offset);
        let loop_abs_offset = 0x1C_u32.wrapping_add(self.header.loop_offset);
        let loop_command_offset = loop_abs_offset.wrapping_sub(header_len);

        let offsets = self.command_offsets_and_lengths();
        for (index, (cmd_offset, _len)) in offsets.iter().enumerate() {
            if *cmd_offset as u32 == loop_command_offset {
                return Some(index);
            }
        }
        None
    }
}

/// Consume the document and iterate its commands by value.
impl IntoIterator for VgmDocument {
    type Item = VgmCommand;
    type IntoIter = std::vec::IntoIter<VgmCommand>;

    fn into_iter(self) -> Self::IntoIter {
        self.commands.into_iter()
    }
}

/// Iterate over commands by reference: `for c in &doc { ... }`.
impl<'a> IntoIterator for &'a VgmDocument {
    type Item = &'a VgmCommand;
    type IntoIter = std::slice::Iter<'a, VgmCommand>;

    fn into_iter(self) -> Self::IntoIter {
        self.commands.iter()
    }
}

/// Iterate over commands by mutable reference: `for c in &mut doc { ... }`.
impl<'a> IntoIterator for &'a mut VgmDocument {
    type Item = &'a mut VgmCommand;
    type IntoIter = std::slice::IterMut<'a, VgmCommand>;

    fn into_iter(self) -> Self::IntoIter {
        self.commands.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vgm::command::{EndOfData, VgmCommand};

    #[test]
    fn test_finalize_appends_end_of_data_when_missing() {
        let builder = VgmBuilder::new();
        let doc = builder.finalize();
        assert!(
            doc.commands
                .iter()
                .any(|c| matches!(c, VgmCommand::EndOfData(_))),
            "finalize() should append EndOfData when missing"
        );
    }

    #[test]
    fn test_finalize_does_not_duplicate_end_of_data() {
        let mut builder = VgmBuilder::new();
        // Insert an explicit EndOfData before finalizing
        builder
            .document
            .commands
            .push(VgmCommand::EndOfData(EndOfData {}));
        let doc = builder.finalize();
        let count = doc
            .commands
            .iter()
            .filter(|c| matches!(c, VgmCommand::EndOfData(_)))
            .count();
        assert_eq!(
            count, 1,
            "finalize() must not duplicate an existing EndOfData"
        );
    }
}
