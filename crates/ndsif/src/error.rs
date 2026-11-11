//! Errors reported by local frame, message, and timing validation.

use core::fmt;

/// Invalid frame, message, argument, or output buffer encountered by the codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The frame payload exceeds the protocol maximum.
    PayloadTooLong {
        /// Attempted payload length in bytes.
        length: usize,
    },
    /// The encoded frame body exceeds its maximum length.
    FrameTooLong,
    /// The input is not a valid delimiter-free COBS body.
    InvalidCobs,
    /// The decoded frame has an invalid magic header.
    InvalidMagic,
    /// The frame uses an unsupported wire version.
    UnsupportedVersion(u8),
    /// The frame contains an unknown message-type value.
    InvalidMessageType(u8),
    /// A frame or message has an invalid length.
    InvalidLength,
    /// The frame CRC does not match the calculated checksum.
    CrcMismatch {
        /// Calculated checksum.
        expected: u16,
        /// Checksum received in the frame.
        received: u16,
    },
    /// The caller's output buffer is too small.
    BufferTooSmall {
        /// Required buffer length in bytes.
        required: usize,
    },
    /// An argument or locally validated message value is invalid.
    InvalidArgument,
    /// A message contains an unsupported opcode.
    UnexpectedOpcode(u8),
    /// A response contains an unknown status value.
    InvalidStatus(u8),
    /// Device information contains invalid UTF-8.
    InvalidUtf8,
    /// The response does not correspond to the supplied request.
    ResponseMismatch,
}

impl fmt::Display for Error {
    /// Format the protocol or transport error for diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLong { length } => {
                write!(formatter, "payload exceeds 256 bytes: {length}")
            }
            Self::FrameTooLong => formatter.write_str("COBS body exceeds 269 bytes"),
            Self::InvalidCobs => formatter.write_str("invalid COBS frame"),
            Self::InvalidMagic => formatter.write_str("invalid NDSIF magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported protocol version: {version}")
            }
            Self::InvalidMessageType(value) => {
                write!(formatter, "invalid message type: {value:02X}")
            }
            Self::InvalidLength => formatter.write_str("invalid frame or message length"),
            Self::CrcMismatch { expected, received } => write!(
                formatter,
                "CRC mismatch: expected {expected:04X}, received {received:04X}"
            ),
            Self::BufferTooSmall { required } => {
                write!(formatter, "output buffer needs {required} bytes")
            }
            Self::InvalidArgument => formatter.write_str("invalid command argument"),
            Self::UnexpectedOpcode(opcode) => {
                write!(formatter, "unexpected command ID: {opcode:02X}")
            }
            Self::InvalidStatus(status) => {
                write!(formatter, "unknown response status: {status:02X}")
            }
            Self::InvalidUtf8 => formatter.write_str("invalid UTF-8 device information"),
            Self::ResponseMismatch => formatter.write_str("response does not match request"),
        }
    }
}

impl core::error::Error for Error {}
