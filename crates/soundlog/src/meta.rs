//! Gd3 metadata parsing and serialization utilities.
//!
//! This module provides the `Gd3` struct and functions to parse and
//! serialize the Gd3 metadata chunk used in VGM files. The Gd3 chunk
//! format consists of a four-byte identifier (`"Gd3 "`), a 32-bit
//! little-endian version field, a 32-bit little-endian length field, and
//! a sequence of UTF-16LE nul-terminated strings representing various
//! metadata fields (track name, game name, author, etc.).
//!
//! Use [`Gd3::parse`] to parse a full Gd3 chunk starting at offset 0.
//! The parser returns a `Gd3` on success or a `crate::ParseError`
//! on failure. `Gd3::to_bytes()` serializes the struct back into the raw
//! Gd3 chunk bytes, preserving the raw `version` value stored in the
//! parsed chunk.
use crate::ParseError;
use crate::binutil::{read_slice, read_u16_le_at, read_u32_le_at};
use std::convert::TryFrom;

/// Resource limits for parsing a GD3 chunk.
///
/// `None` disables the limit; zero permits no bytes.
#[derive(Debug, Clone, Copy)]
pub struct Gd3ParseLimits {
    /// Maximum chunk size, including the 12-byte header. Default: 16 KiB.
    pub max_gd3_bytes: Option<usize>,
}

impl Default for Gd3ParseLimits {
    fn default() -> Self {
        Self {
            max_gd3_bytes: Some(16 * 1024),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// GD3 metadata associated with a VGM file.
pub struct Gd3 {
    /// English track title.
    pub track_name_en: Option<String>,
    /// Original-language track title.
    pub track_name_origin: Option<String>,
    /// English game title.
    pub game_name_en: Option<String>,
    /// Original-language game title.
    pub game_name_origin: Option<String>,
    /// English system name.
    pub system_name_en: Option<String>,
    /// Original-language system name.
    pub system_name_origin: Option<String>,
    /// English author or composer name.
    pub author_name_en: Option<String>,
    /// Original-language author or composer name.
    pub author_name_origin: Option<String>,
    /// Release date text.
    pub release_date: Option<String>,
    /// Name of the VGM creator or ripper.
    pub creator: Option<String>,
    /// Additional notes.
    pub notes: Option<String>,
    /// The Gd3 chunk version (as the raw u32 the chunk stores, e.g. 0x00000100 for 1.00)
    pub version: u32,
}

// Note: raw-bytes preservation was removed. The parser now parses GD3 fields
// and the serializer constructs canonical GD3 bytes from those fields.
impl Default for Gd3 {
    fn default() -> Self {
        Gd3 {
            track_name_en: None,
            track_name_origin: None,
            game_name_en: None,
            game_name_origin: None,
            system_name_en: None,
            system_name_origin: None,
            author_name_en: None,
            author_name_origin: None,
            release_date: None,
            creator: None,
            notes: None,
            version: 0x00000100, // default to 1.00
        }
    }
}

impl Gd3 {
    /// Parse a complete GD3 chunk with the default resource limit.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with_limits(bytes, Gd3ParseLimits::default())
    }

    /// Parse a complete GD3 chunk with a caller-selected resource limit.
    pub fn parse_with_limits(bytes: &[u8], limits: Gd3ParseLimits) -> Result<Self, ParseError> {
        parse_gd3(bytes, limits)
    }

    /// Serialize the metadata as a complete GD3 chunk.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();

        out.extend_from_slice(b"Gd3 ");
        out.extend_from_slice(&self.version.to_le_bytes()); // use stored version
        out.extend_from_slice(&0_u32.to_le_bytes());

        let fields: [&Option<String>; 11] = [
            &self.track_name_en,
            &self.track_name_origin,
            &self.game_name_en,
            &self.game_name_origin,
            &self.system_name_en,
            &self.system_name_origin,
            &self.author_name_en,
            &self.author_name_origin,
            &self.release_date,
            &self.creator,
            &self.notes,
        ];

        let mut data: Vec<u8> = Vec::new();
        for f in &fields {
            if let Some(s) = f {
                for code in s.encode_utf16() {
                    data.extend_from_slice(&code.to_le_bytes());
                }
            }
            data.extend_from_slice(&0_u16.to_le_bytes());
        }

        let len = data.len() as u32;
        out.extend_from_slice(&data);

        let len_bytes = len.to_le_bytes();
        out[8..12].copy_from_slice(&len_bytes);

        out
    }
}

/// Parse a Gd3 block from bytes (full Gd3 chunk starting at offset 0).
/// Returns a populated `Gd3` or a `ParseError` on failure.
pub(crate) fn parse_gd3(bytes: &[u8], limits: Gd3ParseLimits) -> Result<Gd3, ParseError> {
    // need at least 12 bytes: ident(4) + version(4) + length(4)
    if bytes.len() < 12 {
        return Err(ParseError::HeaderTooShort("gd3".into()));
    }

    let ident = read_slice(bytes, 0, 4)?;
    if ident != b"Gd3 " {
        let mut id: [u8; 4] = [0; 4];
        id.copy_from_slice(ident);
        return Err(ParseError::InvalidIdent(id));
    }

    let version = read_u32_le_at(bytes, 4)?;
    let data_len = read_u32_le_at(bytes, 8)? as usize;

    let data_off = 0x0C_usize;
    let total = data_off
        .checked_add(data_len)
        .ok_or_else(|| ParseError::DataInconsistency("GD3 chunk size overflow".into()))?;
    if let Some(limit) = limits.max_gd3_bytes
        && total > limit
    {
        return Err(ParseError::DataInconsistency(format!(
            "GD3 bytes limit exceeded (size {total}, maximum {limit})"
        )));
    }
    if bytes.len() < total {
        return Err(ParseError::OffsetOutOfRange {
            offset: data_off,
            needed: 1,
            available: bytes.len(),
            context: Some("meta:data_offset".into()),
        });
    }

    let data = read_slice(bytes, data_off, data_len)?;

    // There are 11 UTF-16LE nul-terminated fields.
    // Be tolerant of truncated data: if the UTF-16 stream ends mid-code-unit,
    // treat the current and remaining fields as empty rather than error.
    let mut fields: Vec<Option<String>> = Vec::with_capacity(11);
    let mut i = 0_usize;
    let mut truncated = false;
    for _ in 0..11 {
        let mut codes: Vec<u16> = Vec::new();
        loop {
            // Need two bytes for a UTF-16 code unit. If not available, mark
            // truncation and stop collecting codes for this and subsequent fields.
            if i + 1 >= data.len() {
                truncated = true;
                break;
            }
            let code = read_u16_le_at(data, i)?;
            i += 2;
            if code == 0 {
                break;
            }
            codes.push(code);
        }

        if truncated {
            // Current field incomplete — treat as empty and fill remaining fields as None.
            fields.push(None);
            let remaining = 11usize.saturating_sub(fields.len());
            for _ in 0..remaining {
                fields.push(None);
            }
            break;
        }

        if codes.is_empty() {
            fields.push(None);
        } else {
            match String::from_utf16(&codes) {
                Ok(s) => fields.push(Some(s)),
                Err(source) => {
                    return Err(ParseError::InvalidUtf16 {
                        field: "GD3",
                        source,
                    });
                }
            }
        }
    }

    // Map into Gd3 struct
    Ok(Gd3 {
        track_name_en: fields[0].clone(),
        track_name_origin: fields[1].clone(),
        game_name_en: fields[2].clone(),
        game_name_origin: fields[3].clone(),
        system_name_en: fields[4].clone(),
        system_name_origin: fields[5].clone(),
        author_name_en: fields[6].clone(),
        author_name_origin: fields[7].clone(),
        release_date: fields[8].clone(),
        creator: fields[9].clone(),
        notes: fields[10].clone(),
        version,
    })
}

/// Attempt to convert a raw Gd3 byte slice into a `Gd3` value.
///
/// Uses [`Gd3ParseLimits::default`]. For custom limits use [`Gd3::parse_with_limits`].
impl TryFrom<&[u8]> for Gd3 {
    type Error = ParseError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::parse(bytes)
    }
}
