//! Byte readers and writers used by binary parsers and serializers.
use crate::ParseError;

/// Read a 32-bit little-endian unsigned integer from `bytes` at `off`.
///
/// Returns `Ok(u32)` when the four bytes starting at `off` are available and
/// were successfully interpreted as a little-endian `u32`. Returns
/// `Err(ParseError::OffsetOutOfRange)` when the buffer is too short.
pub fn read_u32_le_at(bytes: &[u8], off: usize) -> Result<u32, ParseError> {
    if off > bytes.len() || bytes.len() - off < 4 {
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
    if off > bytes.len() || bytes.len() - off < 2 {
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
    if off > bytes.len() || bytes.len() - off < len {
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
    if off > bytes.len() || bytes.len() - off < 3 {
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

/// Read a 16-bit big-endian unsigned integer from `bytes` at `off`.
///
/// Returns `Ok(u16)` when the two bytes starting at `off` are available and
/// were successfully interpreted as a big-endian `u16`. Returns
/// `Err(ParseError::OffsetOutOfRange)` when the buffer is too short.
#[cfg(feature = "mdx")]
pub fn read_u16_be_at(bytes: &[u8], off: usize) -> Result<u16, ParseError> {
    if off > bytes.len() || bytes.len() - off < 2 {
        return Err(ParseError::OffsetOutOfRange {
            offset: off,
            needed: 2,
            available: bytes.len(),
            context: None,
        });
    }
    let mut tmp: [u8; 2] = [0; 2];
    tmp.copy_from_slice(&bytes[off..off + 2]);
    Ok(u16::from_be_bytes(tmp))
}

/// Read a 16-bit big-endian signed integer from `bytes` at `off`.
///
/// This preserves the two-byte representation read by `read_u16_be_at` and
/// interprets it as an `i16`.
#[cfg(feature = "mdx")]
pub fn read_i16_be_at(bytes: &[u8], off: usize) -> Result<i16, ParseError> {
    let value = read_u16_be_at(bytes, off)?;
    Ok(i16::from_be_bytes(value.to_be_bytes()))
}

/// Read a 32-bit little-endian signed integer from `bytes` at `off`.
///
/// This calls `read_u32_le_at` internally and then interprets the bit pattern
/// as an `i32` using little-endian encoding.
pub fn read_i32_le_at(bytes: &[u8], off: usize) -> Result<i32, ParseError> {
    let v = read_u32_le_at(bytes, off)?;
    Ok(i32::from_le_bytes(v.to_le_bytes()))
}

/// Read a 32-bit big-endian unsigned integer, rejecting out-of-range offsets.
#[cfg(feature = "mdx")]
pub fn read_u32_be_at(bytes: &[u8], off: usize) -> Result<u32, ParseError> {
    let value = read_u32_le_at(bytes, off)?;
    Ok(u32::from_be_bytes(value.to_le_bytes()))
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
    fn reads_reject_overflowing_ranges() {
        let bytes = [1, 2, 3, 4];
        for offset in [usize::MAX, usize::MAX - 1, usize::MAX - 2] {
            assert!(matches!(
                read_u32_le_at(&bytes, offset),
                Err(ParseError::OffsetOutOfRange { .. })
            ));
            assert!(matches!(
                read_u16_le_at(&bytes, offset),
                Err(ParseError::OffsetOutOfRange { .. })
            ));
            assert!(matches!(
                read_u24_be_at(&bytes, offset),
                Err(ParseError::OffsetOutOfRange { .. })
            ));
            assert!(matches!(
                read_i32_le_at(&bytes, offset),
                Err(ParseError::OffsetOutOfRange { .. })
            ));
            assert!(matches!(
                read_u8_at(&bytes, offset),
                Err(ParseError::OffsetOutOfRange { .. })
            ));
            #[cfg(feature = "mdx")]
            {
                assert!(matches!(
                    read_u32_be_at(&bytes, offset),
                    Err(ParseError::OffsetOutOfRange { .. })
                ));
                assert!(matches!(
                    read_u16_be_at(&bytes, offset),
                    Err(ParseError::OffsetOutOfRange { .. })
                ));
                assert!(matches!(
                    read_i16_be_at(&bytes, offset),
                    Err(ParseError::OffsetOutOfRange { .. })
                ));
            }
        }
        for (offset, length) in [(1, usize::MAX), (usize::MAX, 1), (usize::MAX, 0)] {
            assert!(matches!(
                read_slice(&bytes, offset, length),
                Err(ParseError::OffsetOutOfRange { .. })
            ));
        }
        assert_eq!(read_slice(&bytes, bytes.len(), 0).unwrap(), &[]);
        #[cfg(feature = "mdx")]
        assert_eq!(read_u32_be_at(&bytes, 0).unwrap(), 0x0102_0304);
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
