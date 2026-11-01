use std::error::Error;
use std::fmt;

/// Failure to configure or initialize an NDSIF serial transport.
#[derive(Debug)]
#[non_exhaustive]
pub enum NdsifTransportError {
    /// The serial port name is empty or consists only of whitespace.
    EmptyPort,
    /// The same port was specified more than once.
    DuplicatePort { port: String },
    /// The serial baud rate must be nonzero.
    InvalidBaudRate { port: String },
    /// The response timeout must be nonzero.
    InvalidTimeout { port: String },
    /// An error occurred while initializing the named device.
    Device {
        port: String,
        source: Box<NdsifTransportError>,
    },
    /// Opening or configuring the serial port failed.
    Serial {
        operation: &'static str,
        source: serialport::Error,
    },
    /// Reading, writing, or flushing the serial port failed.
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    /// Encoding or decoding an NDSIF message failed.
    Protocol {
        operation: &'static str,
        source: ndsif::Error,
    },
    /// The device did not respond before the configured timeout.
    Timeout { command: u8, transaction_id: u16 },
    /// The device rejected a command.
    Rejected { command: u8, status: ndsif::Status },
    /// The response contained a different reply than expected.
    UnexpectedResponse { operation: &'static str },
    /// Capability pagination failed to advance.
    InvalidCapabilitiesPagination,
    /// A DAC status reply referred to a different profile.
    UnexpectedDacProfile { profile: u8 },
    /// A device stream was not idle during initialization.
    DeviceBusy { stream: &'static str },
}

impl fmt::Display for NdsifTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPort => f.write_str("NDSIF serial port cannot be empty"),
            Self::DuplicatePort { port } => {
                write!(f, "NDSIF serial port is configured more than once: {port}")
            }
            Self::InvalidBaudRate { port } => {
                write!(f, "NDSIF baud rate cannot be zero for {port}")
            }
            Self::InvalidTimeout { port } => {
                write!(f, "NDSIF response timeout cannot be zero for {port}")
            }
            Self::Device { port, source } => write!(f, "NDSIF port {port}: {source}"),
            Self::Serial { operation, source } => write!(f, "{operation}: {source}"),
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Protocol { operation, source } => write!(f, "{operation}: {source}"),
            Self::Timeout {
                command,
                transaction_id,
            } => write!(
                f,
                "timed out waiting for NDSIF response to command 0x{command:02x} transaction {transaction_id}"
            ),
            Self::Rejected { command, status } => {
                write!(f, "NDSIF command 0x{command:02x} was rejected ({status:?})")
            }
            Self::UnexpectedResponse { operation } => {
                write!(f, "NDSIF {operation} returned an unexpected response")
            }
            Self::InvalidCapabilitiesPagination => {
                f.write_str("NDSIF capabilities pagination did not advance")
            }
            Self::UnexpectedDacProfile { profile } => write!(
                f,
                "NDSIF returned DAC profile {profile} status while profile 1 was requested"
            ),
            Self::DeviceBusy { stream } => write!(
                f,
                "NDSIF {stream} is not idle; enable ndsif-reset or reset the device"
            ),
        }
    }
}

impl Error for NdsifTransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Device { source, .. } => Some(source.as_ref()),
            Self::Serial { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::Protocol { source, .. } => Some(source),
            _ => None,
        }
    }
}
