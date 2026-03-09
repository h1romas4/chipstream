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

    /// Data inconsistency or validation error.
    ///
    /// This error indicates that the data structure is inconsistent or
    /// invalid, such as missing required components or conflicting settings.
    DataInconsistency(String),

    /// Data block size limit exceeded.
    ///
    /// - `current_size` is the total size of data blocks accumulated so far.
    /// - `limit` is the maximum allowed size.
    /// - `attempted_size` is the size of the block that would exceed the limit.
    DataBlockSizeExceeded {
        current_size: usize,
        limit: usize,
        attempted_size: usize,
    },
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
            ParseError::DataInconsistency(s) => write!(f, "data inconsistency: {}", s),
            ParseError::DataBlockSizeExceeded {
                current_size,
                limit,
                attempted_size,
            } => write!(
                f,
                "data block size limit exceeded: current {} bytes, limit {} bytes, attempted to add {} bytes",
                current_size, limit, attempted_size
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_display_variants() {
        // Simple variants
        assert_eq!(
            format!("{}", ParseError::UnexpectedEof),
            "unexpected end of input"
        );
        assert_eq!(
            format!("{}", ParseError::InvalidIdent([0x41, 0x42, 0x43, 0x44])),
            "invalid ident: [65, 66, 67, 68]"
        );
        assert_eq!(
            format!("{}", ParseError::UnsupportedVersion(7)),
            "unsupported version: 7"
        );
        assert_eq!(
            format!("{}", ParseError::HeaderTooShort("VGM header".into())),
            "header too short: VGM header"
        );
        assert_eq!(format!("{}", ParseError::Other("boom".into())), "boom");
        assert_eq!(
            format!(
                "{}",
                ParseError::UnknownOpcode {
                    opcode: 0xAB,
                    offset: 0x10
                }
            ),
            "unknown opcode 0xAB at offset 0x10"
        );
        assert_eq!(
            format!("{}", ParseError::DataInconsistency("missing".into())),
            "data inconsistency: missing"
        );
        assert_eq!(
            format!(
                "{}",
                ParseError::DataBlockSizeExceeded {
                    current_size: 100,
                    limit: 200,
                    attempted_size: 150
                }
            ),
            "data block size limit exceeded: current 100 bytes, limit 200 bytes, attempted to add 150 bytes"
        );

        // OffsetOutOfRange without context
        let e = ParseError::OffsetOutOfRange {
            offset: 2,
            needed: 4,
            available: 3,
            context: None,
        };
        assert_eq!(
            format!("{}", e),
            "offset out of range: 0x2 (needed 4 bytes, available 3)"
        );

        // OffsetOutOfRange with context
        let e2 = ParseError::OffsetOutOfRange {
            offset: 0x10,
            needed: 2,
            available: 5,
            context: Some("header_size".into()),
        };
        assert_eq!(
            format!("{}", e2),
            "offset out of range at header_size: 0x10 (needed 2 bytes, available 5)"
        );
    }

    #[test]
    fn read_errors_and_values() {
        // Small buffer to trigger various OffsetOutOfRange errors
        let buf: [u8; 3] = [0x01, 0x02, 0x03];

        // read_u32_le_at -> needs 4 bytes
        match read_u32_le_at(&buf, 0) {
            Err(e) => assert_eq!(
                format!("{}", e),
                "offset out of range: 0x0 (needed 4 bytes, available 3)"
            ),
            Ok(_) => panic!("expected error for read_u32_le_at with insufficient bytes"),
        }

        // read_u16_le_at at off=2 -> needs 2 bytes but only 1 remains
        match read_u16_le_at(&buf, 2) {
            Err(e) => assert_eq!(
                format!("{}", e),
                "offset out of range: 0x2 (needed 2 bytes, available 3)"
            ),
            Ok(_) => panic!("expected error for read_u16_le_at with insufficient bytes"),
        }

        // read_u8_at at off=3 -> out of bounds
        match read_u8_at(&buf, 3) {
            Err(e) => assert_eq!(
                format!("{}", e),
                "offset out of range: 0x3 (needed 1 bytes, available 3)"
            ),
            Ok(_) => panic!("expected error for read_u8_at with out-of-bounds index"),
        }

        // read_slice should report remaining bytes from `off` as available and include context
        match read_slice(&buf, 1, 3) {
            Err(e) => assert_eq!(
                format!("{}", e),
                "offset out of range at read_slice: 0x1 (needed 3 bytes, available 2)"
            ),
            Ok(_) => panic!("expected error for read_slice with insufficient bytes"),
        }

        // read_u24_be_at success
        let buf2: [u8; 4] = [0x01, 0x02, 0x03, 0x04];
        assert_eq!(read_u24_be_at(&buf2, 0).unwrap(), 0x01_02_03);

        // read_u24_be_at error when not enough bytes
        match read_u24_be_at(&buf2, 2) {
            Err(e) => assert_eq!(
                format!("{}", e),
                "offset out of range: 0x2 (needed 3 bytes, available 4)"
            ),
            Ok(_) => panic!("expected error for read_u24_be_at with insufficient bytes"),
        }

        // read_i32_le_at success
        let buf3: [u8; 4] = [0xFF, 0xFF, 0xFF, 0x7F]; // 0x7FFFFFFF -> i32::MAX
        assert_eq!(read_i32_le_at(&buf3, 0).unwrap(), 2_147_483_647);
    }

    #[test]
    fn write_and_slice() {
        let mut buf = [0u8; 8];

        write_u32(&mut buf, 0, 0x1122_3344);
        assert_eq!(&buf[0..4], &0x1122_3344u32.to_le_bytes());

        write_u16(&mut buf, 4, 0xAABB);
        assert_eq!(&buf[4..6], &0xAABBu16.to_le_bytes());

        write_u8(&mut buf, 6, 0x7F);
        assert_eq!(buf[6], 0x7F);

        write_slice(&mut buf, 7, &[0x99]);
        assert_eq!(buf[7], 0x99);
    }
}
