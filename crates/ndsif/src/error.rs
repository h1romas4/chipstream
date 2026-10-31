use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    PayloadTooLong { length: usize },
    FrameTooLong,
    InvalidCobs,
    InvalidMagic,
    UnsupportedVersion(u8),
    InvalidMessageType(u8),
    InvalidLength,
    CrcMismatch { expected: u16, received: u16 },
    BufferTooSmall { required: usize },
    InvalidArgument,
    UnexpectedOpcode(u8),
    InvalidStatus(u8),
    InvalidUtf8,
    ResponseMismatch,
}

impl fmt::Display for Error {
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
