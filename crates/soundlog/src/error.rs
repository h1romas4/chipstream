//! Errors returned by binary parsers and command streams.
use std::error::Error;
use std::fmt;

/// Error type returned by binary parsers and command streams.
#[derive(Debug)]
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

    /// A caller-defined error with a human-readable message.
    Other(String),

    /// A text field contains invalid UTF-16.
    InvalidUtf16 {
        field: &'static str,
        source: std::string::FromUtf16Error,
    },

    /// A variable-length field is missing its required terminator.
    MissingTerminator { field: &'static str },

    /// A command generator failed, retaining its original error and source chain.
    ///
    /// Use this for errors from [`crate::VgmCommandGenerator`] implementations.
    /// The bounds preserve compatibility with error-reporting APIs such as `anyhow`.
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::{ParseError, VgmCommand};
    ///
    /// let generated: Result<Option<VgmCommand>, std::io::Error> =
    ///     Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid frame"));
    /// let result = generated.map_err(|error| ParseError::GeneratorError(Box::new(error)));
    /// let error = result.unwrap_err();
    /// let source = std::error::Error::source(&error).unwrap();
    /// assert!(source.downcast_ref::<std::io::Error>().is_some());
    /// ```
    GeneratorError(Box<dyn Error + Send + Sync + 'static>),

    /// A bit width is outside the inclusive range supported by an operation.
    /// `field` identifies the parameter; `bits`, `min`, and `max` are bit counts.
    InvalidBitWidth {
        field: &'static str,
        bits: usize,
        min: usize,
        max: usize,
    },

    /// A bit-packing compression sub-type is not supported.
    UnsupportedBitPackingSubType(u8),

    /// A data-block compression type is not supported.
    UnsupportedCompressionType(u8),

    /// An operation is not supported by the stream's input source.
    /// `operation` is the method name without parentheses. `source` is
    /// `"buffer"`, `"file"`, `"document"`, or `"generator"`.
    UnsupportedStreamOperation {
        operation: &'static str,
        source: &'static str,
    },

    /// Adding a chunk would exceed the parsing buffer size limit.
    /// All sizes are byte counts, and the rejected chunk does not change the buffer.
    BufferSizeExceeded {
        current_size: usize,
        limit: usize,
        attempted_size: usize,
    },

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
            ParseError::InvalidUtf16 { field, source } => {
                write!(f, "invalid UTF-16 in {field}: {source}")
            }
            ParseError::MissingTerminator { field } => {
                write!(f, "missing terminator for {field}")
            }
            ParseError::GeneratorError(error) => error.fmt(f),
            ParseError::InvalidBitWidth {
                field,
                bits,
                min,
                max,
            } => write!(
                f,
                "invalid {field} width: {bits} bits (expected {min}..={max})"
            ),
            ParseError::UnsupportedBitPackingSubType(sub_type) => {
                write!(f, "unsupported bit-packing sub-type: 0x{sub_type:02X}")
            }
            ParseError::UnsupportedCompressionType(compression_type) => {
                write!(f, "unsupported compression type: 0x{compression_type:02X}")
            }
            ParseError::UnsupportedStreamOperation { operation, source } => {
                write!(
                    f,
                    "{operation}() is not supported for {source}-backed streams"
                )
            }
            ParseError::BufferSizeExceeded {
                current_size,
                limit,
                attempted_size,
            } => write!(
                f,
                "buffer size limit exceeded: current {current_size} bytes, limit {limit} bytes, attempted to add {attempted_size} bytes"
            ),
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

impl Error for ParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ParseError::InvalidUtf16 { source, .. } => Some(source),
            ParseError::GeneratorError(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_error_display_variants() {
        for (error, message) in [
            (
                ParseError::InvalidBitWidth {
                    field: "bits_compressed",
                    bits: 33,
                    min: 1,
                    max: 32,
                },
                "invalid bits_compressed width: 33 bits (expected 1..=32)",
            ),
            (
                ParseError::UnsupportedBitPackingSubType(0xFE),
                "unsupported bit-packing sub-type: 0xFE",
            ),
            (
                ParseError::UnsupportedCompressionType(0xFE),
                "unsupported compression type: 0xFE",
            ),
            (
                ParseError::UnsupportedStreamOperation {
                    operation: "push_chunk",
                    source: "document",
                },
                "push_chunk() is not supported for document-backed streams",
            ),
            (
                ParseError::BufferSizeExceeded {
                    current_size: 100,
                    limit: 200,
                    attempted_size: 150,
                },
                "buffer size limit exceeded: current 100 bytes, limit 200 bytes, attempted to add 150 bytes",
            ),
            (
                ParseError::MissingTerminator { field: "MDX title" },
                "missing terminator for MDX title",
            ),
        ] {
            assert_eq!(error.to_string(), message);
            assert!(error.source().is_none());
        }
    }

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
}
