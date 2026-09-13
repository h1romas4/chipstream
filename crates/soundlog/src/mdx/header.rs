//! MDX file header parsing and serialization.
//!
//! This module handles the variable-length title and PDX-name fields, the
//! tone-data offset, and the track offset table at the beginning of an MDX
//! file.
//!
//! Responsibilities:
//! - Parse and serialize the header while preserving original encoded Shift
//!   JIS bytes for lossless round trips.
//! - Resolve tone and track offsets relative to the MDX body base position.
//! - Represent both standard nine-track and extended sixteen-track headers.

use crate::ParseError;
use crate::binutil::read_u16_be_at;
use crate::mdx::encoding::decode_shift_jis;

/// Number of track offsets in a standard MDX header.
const TRACK_COUNT: usize = 9;
/// Number of track offsets in an extended MDX header.
const EXTENDED_TRACK_COUNT: usize = 16;
/// On-disk track offset value used for an absent track.
const UNUSED_TRACK_OFFSET: u16 = 0xffff;

/// The variable-length header and track offset table at the beginning of an
/// MDX file.
///
/// Text fields expose decoded strings and retain their original encoded bytes
/// so that [`to_bytes`][Self::to_bytes] can reproduce the header text without
/// re-encoding it. Offset values are stored relative to [`base_offset`][Self::base_offset].
///
/// The header supports both the standard nine-track form and the extended
/// sixteen-track form. An absent track is represented by `None` in
/// [`track_offsets`][Self::track_offsets] and is serialized as `0xffff`.
///
/// # Examples
///
/// ```
/// use soundlog::mdx::command::MdxRest;
/// use soundlog::mdx::document::MdxBuilder;
/// use soundlog::mdx::header::MdxHeader;
///
/// let mut builder = MdxBuilder::new();
/// builder.add_mdx_command(0, MdxRest::new(8).unwrap());
/// let bytes = builder.finalize().to_bytes();
/// let (header, body_offset) = MdxHeader::parse(&bytes).unwrap();
///
/// assert_eq!(header.track_count(), 9);
/// assert_eq!(body_offset, header.base_offset);
/// assert!(header.track_position(0).is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxHeader {
    /// The title decoded from CP932-compatible Shift JIS.
    ///
    /// The encoded form used for serialization is retained separately in
    /// [`title_raw_bytes`][Self::title_raw_bytes].
    pub title: String,
    /// The original encoded title bytes before the `CR LF 1A` terminator.
    ///
    /// These bytes are serialized as-is by [`to_bytes`][Self::to_bytes].
    pub title_raw_bytes: Vec<u8>,
    /// The optional PDX filename decoded from CP932-compatible Shift JIS.
    ///
    /// The encoded form used for serialization is retained separately in
    /// [`pdx_name_raw_bytes`][Self::pdx_name_raw_bytes].
    pub pdx_name: Option<String>,
    /// The original encoded PDX filename bytes before its NUL terminator.
    ///
    /// `None` represents an empty PDX filename field.
    pub pdx_name_raw_bytes: Option<Vec<u8>>,
    /// The absolute position immediately after the PDX filename terminator.
    ///
    /// Tone and track offsets are resolved relative to this position.
    pub base_offset: usize,
    /// The tone data offset relative to [`base_offset`][Self::base_offset].
    ///
    /// Use [`tone_data_position`][Self::tone_data_position] to obtain the
    /// checked absolute position.
    pub tone_data_offset: u16,
    /// Track offsets relative to [`base_offset`][Self::base_offset].
    ///
    /// `None` represents the on-disk value `0xffff`, which marks an absent
    /// track. The vector length determines whether this is a standard
    /// nine-track or extended sixteen-track header.
    pub track_offsets: Vec<Option<u16>>,
}

impl MdxHeader {
    /// Parses an MDX header and returns it with the first body position.
    ///
    /// The returned position is the absolute byte offset immediately after
    /// the variable-length title and PDX filename fields. The parser detects
    /// the extended track form from the first track's PCM-mode marker and
    /// validates the bytes needed for the header fields.
    pub fn parse(bytes: &[u8]) -> Result<(Self, usize), ParseError> {
        parse_mdx_header(bytes)
    }

    /// Returns the checked absolute position of the tone data.
    ///
    /// Returns `None` if adding `tone_data_offset` to `base_offset` would
    /// overflow `usize`.
    pub fn tone_data_position(&self) -> Option<usize> {
        self.base_offset.checked_add(self.tone_data_offset as usize)
    }

    /// Returns the checked absolute position of a present track.
    ///
    /// `track` is zero-based. Returns `None` when the index is outside the
    /// offset table, the track is absent, or adding the relative offset to
    /// `base_offset` would overflow `usize`.
    pub fn track_position(&self, track: usize) -> Option<usize> {
        self.base_offset
            .checked_add(*self.track_offsets.get(track)?.as_ref()? as usize)
    }

    /// Returns the number of track offset entries in this header.
    ///
    /// This is normally 9 for standard MDX and 16 for extended MDX.
    pub fn track_count(&self) -> usize {
        self.track_offsets.len()
    }

    /// Serializes this header using the canonical MDX header delimiters.
    ///
    /// The title raw bytes are followed by `CR LF 1A`, the optional PDX name
    /// bytes and a NUL terminator, then the tone offset and track offset table.
    /// Absent tracks are serialized as `0xffff`. The decoded `title` and
    /// `pdx_name` fields are not re-encoded; their corresponding raw byte
    /// fields are authoritative for serialization.
    pub fn to_bytes(&self) -> Vec<u8> {
        let pdx_name = self.pdx_name_raw_bytes.as_deref().unwrap_or_default();
        let mut bytes = Vec::with_capacity(
            self.title_raw_bytes.len() + 3 + pdx_name.len() + 1 + 2 + self.track_offsets.len() * 2,
        );
        bytes.extend_from_slice(&self.title_raw_bytes);
        bytes.extend_from_slice(&[0x0d, 0x0a, 0x1a]);
        bytes.extend_from_slice(pdx_name);
        bytes.push(0);
        bytes.extend_from_slice(&self.tone_data_offset.to_be_bytes());
        for offset in &self.track_offsets {
            bytes.extend_from_slice(&offset.unwrap_or(UNUSED_TRACK_OFFSET).to_be_bytes());
        }
        bytes
    }
}

/// Parses the variable-length MDX header and returns the header along with its base offset.
pub fn parse_mdx_header(bytes: &[u8]) -> Result<(MdxHeader, usize), ParseError> {
    let title_end = find_title_terminator(bytes)?;
    let title_raw_bytes = bytes[..title_end].to_vec();
    let pdx_start = title_end + 3;
    let pdx_end = bytes[pdx_start..]
        .iter()
        .position(|&byte| byte == 0)
        .map(|offset| pdx_start + offset)
        .ok_or_else(|| ParseError::Other("MDX PDX filename is not NUL-terminated".into()))?;
    let pdx_name_raw_bytes = if pdx_end == pdx_start {
        None
    } else {
        Some(bytes[pdx_start..pdx_end].to_vec())
    };
    let title = decode_shift_jis(&title_raw_bytes);
    let pdx_name = pdx_name_raw_bytes.as_deref().map(decode_shift_jis);
    let base_offset = pdx_end + 1;
    let tone_data_offset = read_u16_be_at(bytes, base_offset)?;
    let track_table_offset = base_offset + 2;

    let first_track_offset = read_u16_be_at(bytes, track_table_offset)?;
    let first_track_position = base_offset.checked_add(first_track_offset as usize);
    let track_count = if first_track_position
        .and_then(|position| bytes.get(position))
        .is_some_and(|&opcode| opcode == 0xe8)
    {
        EXTENDED_TRACK_COUNT
    } else {
        TRACK_COUNT
    };

    let mut track_offsets = Vec::with_capacity(track_count);
    for track in 0..track_count {
        let offset = read_u16_be_at(bytes, track_table_offset + track * 2)?;
        track_offsets.push((offset != UNUSED_TRACK_OFFSET).then_some(offset));
    }
    Ok((
        MdxHeader {
            title,
            title_raw_bytes,
            pdx_name,
            pdx_name_raw_bytes,
            base_offset,
            tone_data_offset,
            track_offsets,
        },
        base_offset,
    ))
}

/// Finds the end of the MDX title by searching for the CR LF 1A sequence.
/// Returns the index of the first byte of the terminator if found.
fn find_title_terminator(bytes: &[u8]) -> Result<usize, ParseError> {
    bytes
        .windows(3)
        .position(|window| window == [0x0d, 0x0a, 0x1a])
        .ok_or_else(|| ParseError::Other("MDX title is not CR LF 1A-terminated".into()))
}
