//! PDX PCM container parsing and lossless sample-data serialization.
//!
//! PDX stores PCM sample offsets in fixed 96-entry banks followed by encoded
//! sample data. This module validates that table, exposes individual samples,
//! and rebuilds canonical PDX bytes from typed sample data.
//!
//! Both uncompressed and MDX-compatible LZ-compressed payloads are supported.
//! Parsing retains the compression mode so callers can choose a lossless
//! round trip or construct a canonical uncompressed document.

use crate::ParseError;
use crate::mdx::lz;
use std::array;

/// Number of note slots in one PDX bank.
const ENTRIES_PER_BANK: usize = 96;
/// Size in bytes of one PDX sample-table entry (`start` followed by `size`).
const ENTRY_SIZE: usize = 8;
/// Size in bytes of one complete PDX bank table.
const BANK_SIZE: usize = ENTRIES_PER_BANK * ENTRY_SIZE;
/// Maximum number of bank tables accepted by the parser and builder.
const MAX_BANKS: usize = 32;
/// Four-byte marker that prefixes a NanoDriveX-compatible LZ stream.
const LZ_STREAM_MARKER: [u8; 4] = [0x7f, 0xff, 0xff, 0x4c];
/// Maximum decoded size allocated for a compressed PDX payload.
const MAX_DECODED_PDX_SIZE: usize = 64 * 1024 * 1024;

/// A PDX sample table entry.
///
/// The entry describes a byte range in [`PdxDocument::decoded_bytes`]. Both
/// values use the PDX file's decoded, uncompressed address space, even when
/// the original PDX was stored as an LZ stream. A zero-size table entry is
/// represented by `None` in [`PdxBank::entries`] rather than by a
/// `PdxSample`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdxSample {
    /// Byte offset from the beginning of the decoded PDX stream.
    pub start: u32,
    /// Number of encoded sample bytes in the referenced range.
    pub size: u32,
}

/// One PDX bank containing one entry for each of the 96 PDX note slots.
///
/// The array index is the zero-based note number within the bank. `None`
/// means that the slot has no sample data; populated entries point into the
/// decoded bytes held by the containing [`PdxDocument`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdxBank {
    /// Sample entries indexed by zero-based note number.
    pub entries: [Option<PdxSample>; ENTRIES_PER_BANK],
}

/// Builder for creating a PDX document from encoded sample bytes.
///
/// Samples are supplied already encoded for the selected playback format;
/// this builder does not decode or transform their contents. Banks are created
/// on demand by [`Self::set_sample`], while [`Self::finalize`] always emits at
/// least one bank. A finalized document uses canonical table offsets and
/// stores samples consecutively after its bank tables.
pub struct PdxBuilder {
    samples: Vec<[Option<Vec<u8>>; ENTRIES_PER_BANK]>,
    lz_compressed: bool,
}

impl PdxBuilder {
    /// Create a builder with one empty, uncompressed PDX bank.
    pub fn new() -> Self {
        Self {
            samples: vec![array::from_fn(|_| None)],
            lz_compressed: false,
        }
    }

    /// Create a builder from a parsed PDX document.
    ///
    /// Every populated sample is copied from the source document, and the
    /// source compression mode is retained for [`Self::finalize`]. The rebuilt
    /// document uses canonical table and sample offsets, so its bytes may
    /// differ from the source while preserving every sample.
    ///
    /// Returns [`ParseError::DataInconsistency`] if a populated table entry in
    /// `document` does not resolve to a valid sample range.
    pub fn from_document(document: &PdxDocument) -> Result<Self, ParseError> {
        let mut builder = Self {
            samples: (0..document.banks.len())
                .map(|_| array::from_fn(|_| None))
                .collect(),
            lz_compressed: document.is_compressed(),
        };
        if builder.samples.is_empty() {
            builder.samples.push(array::from_fn(|_| None));
        }
        for (bank, data) in document.banks.iter().enumerate() {
            for (note, entry) in data.entries.iter().enumerate() {
                if entry.is_some() {
                    let sample = document.sample_bytes(bank, note).ok_or_else(|| {
                        ParseError::DataInconsistency(format!(
                            "PDX sample entry is outside the data: bank {bank}, note {note}"
                        ))
                    })?;
                    builder.samples[bank][note] = Some(sample.to_vec());
                }
            }
        }
        Ok(builder)
    }

    /// Add or replace one encoded sample in a bank/note slot.
    ///
    /// `bank` is zero-based and must be less than 32; `note` is zero-based and
    /// must be less than 96. Intermediate banks are created automatically.
    /// Empty sample data is rejected because an empty slot is represented by
    /// removing the sample instead. The input vector is moved into the
    /// builder without copying.
    pub fn set_sample(
        &mut self,
        bank: usize,
        note: usize,
        bytes: Vec<u8>,
    ) -> Result<&mut Self, ParseError> {
        if bytes.is_empty() {
            return Err(ParseError::DataInconsistency(
                "PDX sample data must not be empty".into(),
            ));
        }
        let slot = self.slot_mut(bank, note)?;
        *slot = Some(bytes);
        Ok(self)
    }

    /// Remove a sample from a bank/note slot.
    ///
    /// The same zero-based bank and note limits as [`Self::set_sample`] apply.
    /// Removing a slot that was not populated is successful and leaves the
    /// slot empty. Intermediate banks are still created when `bank` is valid.
    pub fn remove_sample(&mut self, bank: usize, note: usize) -> Result<&mut Self, ParseError> {
        let slot = self.slot_mut(bank, note)?;
        *slot = None;
        Ok(self)
    }

    /// Enable or disable NanoDriveX-compatible LZ compression on output.
    ///
    /// This changes only how [`Self::finalize`] serializes the decoded PDX
    /// representation; sample bytes and table contents are unchanged.
    pub fn set_lz_compressed(&mut self, compressed: bool) -> &mut Self {
        self.lz_compressed = compressed;
        self
    }

    /// Finalize the table and sample payloads into a PDX document.
    ///
    /// The returned document contains canonical big-endian table entries,
    /// with each populated sample placed after all bank tables in insertion
    /// order. The builder is consumed, and subsequent changes require a new
    /// builder. Compression is applied according to the last value passed to
    /// [`Self::set_lz_compressed`].
    pub fn finalize(self) -> PdxDocument {
        let table_size = self.samples.len() * BANK_SIZE;
        let mut decoded_bytes = vec![0; table_size];
        let mut sample_data = Vec::new();
        let mut banks = Vec::with_capacity(self.samples.len());
        let mut data_position = table_size;

        for sample_bank in self.samples {
            let mut entries = [None; ENTRIES_PER_BANK];
            for (note, sample_bytes) in sample_bank.into_iter().enumerate() {
                let Some(sample_bytes) = sample_bytes else {
                    continue;
                };
                let start = u32::try_from(data_position).unwrap_or(u32::MAX);
                let size = u32::try_from(sample_bytes.len()).unwrap_or(u32::MAX);
                let entry_offset = banks.len() * BANK_SIZE + note * ENTRY_SIZE;
                decoded_bytes[entry_offset..entry_offset + 4].copy_from_slice(&start.to_be_bytes());
                decoded_bytes[entry_offset + 4..entry_offset + 8]
                    .copy_from_slice(&size.to_be_bytes());
                sample_data.extend_from_slice(&sample_bytes);
                entries[note] = Some(PdxSample { start, size });
                data_position = data_position.saturating_add(sample_bytes.len());
            }
            banks.push(PdxBank { entries });
        }
        decoded_bytes.extend_from_slice(&sample_data);

        PdxDocument {
            banks,
            decoded_bytes,
            compressed: self.lz_compressed,
        }
    }

    fn slot_mut(&mut self, bank: usize, note: usize) -> Result<&mut Option<Vec<u8>>, ParseError> {
        if note >= ENTRIES_PER_BANK || bank >= MAX_BANKS {
            return Err(ParseError::DataInconsistency(format!(
                "PDX sample index out of range: bank {bank}, note {note}"
            )));
        }
        while self.samples.len() <= bank {
            self.samples.push(array::from_fn(|_| None));
        }
        Ok(&mut self.samples[bank][note])
    }
}

impl Default for PdxBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed PDX data retaining its decoded bytes, bank tables, and compression
/// mode.
///
/// The public [`Self::banks`] entries refer to ranges in the private decoded
/// byte buffer. This keeps sample lookup independent of whether the input was
/// compressed. Serialization can reproduce the selected compressed or
/// uncompressed representation, although compressed output is canonical rather
/// than necessarily byte-for-byte identical to the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdxDocument {
    /// Parsed banks in file order, indexed by zero-based bank number.
    pub banks: Vec<PdxBank>,
    decoded_bytes: Vec<u8>,
    compressed: bool,
}

impl PdxDocument {
    /// Parse an uncompressed or NanoDriveX-compatible LZ-compressed PDX file.
    ///
    /// The input slice is copied before parsing. Use [`Self::parse_owned`] to
    /// transfer ownership of an existing `Vec<u8>` and avoid retaining that
    /// original input allocation in addition to the decoded representation.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Self::parse_owned(bytes.to_vec())
    }

    /// Parse an owned PDX buffer without retaining a second copy of the file.
    ///
    /// The document consumes `bytes` and retains only the decoded PDX bytes,
    /// parsed bank tables, and a flag indicating whether the input contained
    /// an LZ stream. Compressed input is re-encoded by [`Self::to_bytes`] when
    /// serialized again.
    pub fn parse_owned(bytes: Vec<u8>) -> Result<Self, ParseError> {
        let (decoded_bytes, compressed) = decode_pdx_body_owned(bytes)?;
        let banks = parse_banks(&decoded_bytes)?;
        Ok(Self {
            banks,
            decoded_bytes,
            compressed,
        })
    }

    /// Serialize the document, preserving its compressed or uncompressed mode.
    ///
    /// Uncompressed documents return their decoded bytes. Compressed documents
    /// are prefixed with the NanoDriveX marker and re-encoded using the MDX/PDX
    /// LZ encoder. The result is canonical and is not required to be
    /// byte-for-byte identical to the input stream.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.compressed {
            let mut bytes = LZ_STREAM_MARKER.to_vec();
            bytes.extend_from_slice(&lz::encode(&self.decoded_bytes));
            bytes
        } else {
            self.decoded_bytes.clone()
        }
    }

    /// Return the decoded, uncompressed PDX bytes.
    ///
    /// The returned slice contains the complete table area followed by the
    /// encoded sample payloads. Its offsets are the address space used by
    /// [`PdxSample::start`] and [`PdxSample::size`]. The slice is borrowed from
    /// the document and remains valid until the document is dropped.
    pub fn decoded_bytes(&self) -> &[u8] {
        &self.decoded_bytes
    }

    /// Return whether the parsed input contained an LZ stream.
    ///
    /// This flag also determines whether [`Self::to_bytes`] emits compressed
    /// or uncompressed output.
    pub const fn is_compressed(&self) -> bool {
        self.compressed
    }

    /// Return a bank entry, or `None` for an empty or out-of-range entry.
    ///
    /// Both coordinates are zero-based. `bank` indexes [`Self::banks`], and
    /// `note` indexes [`PdxBank::entries`]. The returned value is copied from
    /// the table and does not borrow the document.
    pub fn entry(&self, bank: usize, note: usize) -> Option<PdxSample> {
        self.banks.get(bank)?.entries.get(note).copied().flatten()
    }

    /// Return the encoded sample bytes referenced by a bank/note entry.
    ///
    /// Returns `None` when the bank or note is out of range, the slot is empty,
    /// or the table entry does not describe a valid range in
    /// [`Self::decoded_bytes`]. The returned bytes are still in their PDX
    /// storage format; decode them separately according to the MDX PCM format.
    pub fn sample_bytes(&self, bank: usize, note: usize) -> Option<&[u8]> {
        let sample = self.entry(bank, note)?;
        let start = usize::try_from(sample.start).ok()?;
        let size = usize::try_from(sample.size).ok()?;
        let end = start.checked_add(size)?;
        self.decoded_bytes.get(start..end)
    }
}

impl TryFrom<&[u8]> for PdxDocument {
    type Error = ParseError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::parse(bytes)
    }
}

/// Parse the bank table from the given byte slice.
/// Returns a vector of `PdxBank` structures or a `ParseError` if the data is inconsistent.
fn parse_banks(bytes: &[u8]) -> Result<Vec<PdxBank>, ParseError> {
    let mut banks = Vec::new();
    let mut data_start = bytes.len();

    for bank_index in 0..MAX_BANKS {
        let bank_start = bank_index * BANK_SIZE;
        if bank_start >= bytes.len() || bank_start >= data_start {
            break;
        }
        let bank_end = bank_start + BANK_SIZE;
        if bank_end > bytes.len() {
            break;
        }

        let mut entries = [None; ENTRIES_PER_BANK];
        for (note, entry) in entries.iter_mut().enumerate() {
            let offset = bank_start + note * ENTRY_SIZE;
            let start = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let size = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            if size == 0 {
                continue;
            }
            let Some(start_position) = usize::try_from(start).ok() else {
                return Err(ParseError::DataInconsistency(format!(
                    "PDX sample offset is not addressable: bank {bank_index}, note {note}"
                )));
            };
            if start_position < bank_end {
                return Err(ParseError::DataInconsistency(format!(
                    "PDX sample overlaps its table: bank {bank_index}, note {note}"
                )));
            }
            let Some(end_position) = start_position.checked_add(size as usize) else {
                return Err(ParseError::DataInconsistency(format!(
                    "PDX sample range overflows: bank {bank_index}, note {note}"
                )));
            };
            if end_position > bytes.len() {
                return Err(ParseError::DataInconsistency(format!(
                    "PDX sample extends past the file: bank {bank_index}, note {note}"
                )));
            }
            data_start = data_start.min(start_position);
            *entry = Some(PdxSample { start, size });
        }
        banks.push(PdxBank { entries });
    }

    if banks.is_empty() && !bytes.is_empty() && bytes.len() < BANK_SIZE {
        return Err(ParseError::DataInconsistency(
            "PDX data is shorter than one bank table".into(),
        ));
    }
    Ok(banks)
}

/// Decode the body of a PDX file, returning the decoded bytes and a flag indicating whether decoding was performed.
/// If the PDX body does not contain an LZ stream, the original bytes are returned with the flag set to `false`.
fn decode_pdx_body_owned(bytes: Vec<u8>) -> Result<(Vec<u8>, bool), ParseError> {
    let Some(marker_offset) = bytes
        .windows(LZ_STREAM_MARKER.len())
        .position(|window| window == LZ_STREAM_MARKER)
    else {
        return Ok((bytes, false));
    };
    let stream_start = marker_offset
        .checked_add(LZ_STREAM_MARKER.len())
        .ok_or_else(|| ParseError::DataInconsistency("PDX LZ stream offset overflow".into()))?;
    let compressed = bytes.get(stream_start..).ok_or_else(|| {
        ParseError::DataInconsistency("PDX LZ stream has no compressed data".into())
    })?;
    let mut decoded = vec![0; MAX_DECODED_PDX_SIZE];
    let result = lz::decode(compressed, &mut decoded);
    match result.result {
        lz::Result::Ok => {
            decoded.truncate(result.bytes_written);
            decoded.shrink_to_fit();
            Ok((decoded, true))
        }
        error => Err(ParseError::DataInconsistency(format!(
            "PDX LZ decode failed: {error:?}"
        ))),
    }
}
