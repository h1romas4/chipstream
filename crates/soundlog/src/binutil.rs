//! Utilities used by parsers: parse error type and byte readers/writers.
use std::fmt;

/// Error type returned by the parsing helpers in this module.
#[derive(Debug, Clone)]
pub enum ParseError {
    /// Input ended unexpectedly while the parser was expecting more bytes.
    UnexpectedEof,

    /// An attempted read was outside the available buffer range.
    ///
    /// - `offset` is the index that was attempted to be accessed.
    /// - `needed` is the number of bytes required for the operation.
    /// - `available` is the current buffer length.
    /// - `context` is an optional string describing the logical location
    ///   (for example `"header_size"` or `"gd3_start"`) where the access
    ///   was attempted.
    OffsetOutOfRange {
        offset: usize,
        needed: usize,
        available: usize,
        context: Option<String>,
    },

    /// A four-byte identifier (typically ASCII) did not match an expected value.
    ///
    /// The contained array is the raw 4 bytes that were read.
    InvalidIdent([u8; 4]),

    /// The data uses a version the parser does not support.
    ///
    /// The contained `u32` is the unsupported version number.
    UnsupportedVersion(u32),

    /// A header was shorter than the minimum required length.
    ///
    /// The contained `String` identifies which header or field was too short
    /// (for example: "VGM header", "Gd3 header", or "meta:data_offset").
    HeaderTooShort(String),

    /// A generic error with a human-readable message.
    Other(String),

    /// An opcode byte was not recognized by the parser.
    ///
    /// - `opcode` is the raw opcode byte that was invalid.
    /// - `offset` is the position in the input where the opcode was found.
    UnknownOpcode { opcode: u8, offset: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnexpectedEof => write!(f, "unexpected end of input"),
            ParseError::OffsetOutOfRange {
                offset,
                needed,
                available,
                context,
            } => {
                if let Some(ctx) = context {
                    write!(
                        f,
                        "offset out of range at {}: 0x{:X} (needed {} bytes, available {})",
                        ctx, offset, needed, available
                    )
                } else {
                    write!(
                        f,
                        "offset out of range: 0x{:X} (needed {} bytes, available {})",
                        offset, needed, available
                    )
                }
            }
            ParseError::InvalidIdent(id) => write!(f, "invalid ident: {:?}", id),
            ParseError::UnsupportedVersion(v) => write!(f, "unsupported version: {}", v),
            ParseError::HeaderTooShort(name) => write!(f, "header too short: {}", name),
            ParseError::Other(s) => write!(f, "{}", s),
            ParseError::UnknownOpcode { opcode, offset } => {
                write!(
                    f,
                    "unknown opcode 0x{:02X} at offset 0x{:X}",
                    opcode, offset
                )
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Read a 32-bit little-endian unsigned integer from `bytes` at `off`.
///
/// Returns `Ok(u32)` when the four bytes starting at `off` are available and
/// were successfully interpreted as a little-endian `u32`. Returns
/// `Err(ParseError::OffsetOutOfRange)` when the buffer is too short.
pub fn read_u32_le_at(bytes: &[u8], off: usize) -> Result<u32, ParseError> {
    if bytes.len() < off + 4 {
        return Err(ParseError::OffsetOutOfRange {
            offset: off,
            needed: 4,
            available: bytes.len(),
            context: None,
        });
    }
    let mut tmp: [u8; 4] = [0; 4];
    tmp.copy_from_slice(&bytes[off..off + 4]);
    Ok(u32::from_le_bytes(tmp))
}

/// Read a 16-bit little-endian unsigned integer from `bytes` at `off`.
///
/// Returns `Ok(u16)` when the two bytes starting at `off` are available and
/// were successfully interpreted as a little-endian `u16`. Returns
/// `Err(ParseError::OffsetOutOfRange)` when the buffer is too short.
pub fn read_u16_le_at(bytes: &[u8], off: usize) -> Result<u16, ParseError> {
    if bytes.len() < off + 2 {
        return Err(ParseError::OffsetOutOfRange {
            offset: off,
            needed: 2,
            available: bytes.len(),
            context: None,
        });
    }
    let mut tmp: [u8; 2] = [0; 2];
    tmp.copy_from_slice(&bytes[off..off + 2]);
    Ok(u16::from_le_bytes(tmp))
}

/// Read a single byte from `bytes` at `off`.
///
/// Returns `Ok(u8)` when `off` is a valid index into `bytes`. Returns
/// `Err(ParseError::OffsetOutOfRange)` when `off` is out of bounds.
pub fn read_u8_at(bytes: &[u8], off: usize) -> Result<u8, ParseError> {
    if bytes.len() <= off {
        return Err(ParseError::OffsetOutOfRange {
            offset: off,
            needed: 1,
            available: bytes.len(),
            context: None,
        });
    }
    Ok(bytes[off])
}

/// Return a borrowed slice of length `len` starting at `off` from `bytes`.
///
/// Returns `Ok(&[u8])` that borrows from the input slice when the requested
/// range is within bounds. Returns `Err(ParseError::OffsetOutOfRange)` when the
/// requested range exceeds the available buffer.
pub fn read_slice(bytes: &[u8], off: usize, len: usize) -> Result<&[u8], ParseError> {
    if bytes.len() < off + len {
        return Err(ParseError::OffsetOutOfRange {
            offset: off,
            needed: len,
            // Report the remaining number of bytes from `off` to the end of the buffer.
            available: bytes.len().saturating_sub(off),
            context: Some("read_slice".into()),
        });
    }
    Ok(&bytes[off..off + len])
}

/// Read a 24-bit big-endian unsigned integer from `bytes` at `off`.
///
/// Returns the value as a `u32`. The function expects three bytes at `off`,
/// `off+1` and `off+2` in big-endian order; if they are not available the
/// function returns `Err(ParseError::OffsetOutOfRange)`.
pub fn read_u24_be_at(bytes: &[u8], off: usize) -> Result<u32, ParseError> {
    if bytes.len() < off + 3 {
        return Err(ParseError::OffsetOutOfRange {
            offset: off,
            needed: 3,
            available: bytes.len(),
            context: None,
        });
    }
    let b0 = bytes[off] as u32;
    let b1 = bytes[off + 1] as u32;
    let b2 = bytes[off + 2] as u32;
    Ok((b0 << 16) | (b1 << 8) | b2)
}

/// Read a 32-bit little-endian signed integer from `bytes` at `off`.
///
/// This calls `read_u32_le_at` internally and then interprets the bit pattern
/// as an `i32` using little-endian encoding.
pub fn read_i32_le_at(bytes: &[u8], off: usize) -> Result<i32, ParseError> {
    let v = read_u32_le_at(bytes, off)?;
    Ok(i32::from_le_bytes(v.to_le_bytes()))
}

/// Write a 32-bit little-endian unsigned integer `v` into `buf` at `off`.
///
/// This function will copy four bytes into `buf[off..off+4]`. It does not
/// perform bounds checking; callers must ensure the destination range is valid.
pub fn write_u32(buf: &mut [u8], off: usize, v: u32) {
    let bytes = v.to_le_bytes();
    buf[off..off + 4].copy_from_slice(&bytes);
}

/// Write a 16-bit little-endian unsigned integer `v` into `buf` at `off`.
///
/// This function copies two bytes into `buf[off..off+2]`. It does not perform
/// bounds checking; callers must ensure the destination range is valid.
pub fn write_u16(buf: &mut [u8], off: usize, v: u16) {
    let bytes = v.to_le_bytes();
    buf[off..off + 2].copy_from_slice(&bytes);
}

/// Write a single byte `v` into `buf` at `off`.
///
/// This function writes `v` to `buf[off]`. It does not perform bounds
/// checking; callers must ensure `off` is a valid index.
pub fn write_u8(buf: &mut [u8], off: usize, v: u8) {
    buf[off] = v;
}

/// Copy the contents of `s` into `buf` starting at `off`.
///
/// This function copies `s.len()` bytes into `buf[off..off+s.len()]`. It does
/// not perform bounds checking; callers must ensure the destination range is
/// valid.
pub fn write_slice(buf: &mut [u8], off: usize, s: &[u8]) {
    buf[off..off + s.len()].copy_from_slice(s);
}
