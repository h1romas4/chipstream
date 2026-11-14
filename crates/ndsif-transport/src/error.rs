//! Typed errors for serial device configuration and initialization.

use std::error::Error;
use std::fmt;

/// Failure to configure or initialize an NDSIF serial transport.
#[derive(Debug)]
#[non_exhaustive]
pub enum NdsifTransportError {
    /// The serial port name is empty or consists only of whitespace.
    EmptyPort,
    /// The same port was specified more than once.
    DuplicatePort {
        /// Port specified more than once.
        port: String,
    },
    /// The serial baud rate must be nonzero.
    InvalidBaudRate {
        /// Port with a zero baud rate.
        port: String,
    },
    /// The response timeout must be nonzero.
    InvalidTimeout {
        /// Port with a zero response timeout.
        port: String,
    },
    /// An error occurred while initializing the named device.
    Device {
        /// Port being initialized.
        port: String,
        /// Underlying device initialization failure.
        source: Box<NdsifTransportError>,
    },
    /// Opening or configuring the serial port failed.
    Serial {
        /// Description of the failed serial operation.
        operation: &'static str,
        /// Underlying serial-port error.
        source: serialport::Error,
    },
    /// Reading, writing, or flushing the serial port failed.
    Io {
        /// Description of the failed I/O operation.
        operation: &'static str,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Encoding or decoding an NDSIF message failed.
    Protocol {
        /// Description of the failed protocol operation.
        operation: &'static str,
        /// Underlying codec error.
        source: ndsif::Error,
    },
    /// The device did not respond before the configured timeout.
    Timeout {
        /// Command ID for which a response was expected.
        command: u8,
        /// Transaction ID of the request.
        transaction_id: u16,
    },
    /// The device rejected a command.
    Rejected {
        /// Rejected command ID.
        command: u8,
        /// Device response status.
        status: ndsif::Status,
    },
    /// The response contained a different reply than expected.
    UnexpectedResponse {
        /// Operation whose response had an unexpected reply.
        operation: &'static str,
    },
    /// Capability pagination failed to advance.
    InvalidCapabilitiesPagination,
    /// Device-wide output attenuation is not supported.
    UnsupportedOutputVolume,
    /// The device did not advertise the requested optional live profile.
    UnsupportedLiveProfile {
        /// Profile identifier.
        profile: u8,
    },
    /// A live capability or diagnostic response is inconsistent or unsupported.
    InvalidLiveCapabilities {
        /// Explanation of the invalid response.
        reason: &'static str,
    },
    /// Live and scheduled playback cannot be mixed without a successful reset.
    LiveResetRequired,
    /// An invalid live operation or record was requested.
    InvalidLiveOperation {
        /// Explanation of the invalid operation.
        reason: &'static str,
    },
    /// A failed live write may have partially reached the device.
    LiveWriteIndeterminate,
    /// A DAC status reply referred to a different profile.
    UnexpectedDacProfile {
        /// Profile returned by the device.
        profile: u8,
    },
    /// A device stream was not idle during initialization.
    DeviceBusy {
        /// Name of the stream or profile that is not idle.
        stream: &'static str,
    },
    /// Host pacing was already started or a host timeline was already set.
    HostTimelineAlreadySet,
    /// Host pacing cannot control an autonomous device audio stream.
    HostTimelineUnsupportedStream,
}

impl fmt::Display for NdsifTransportError {
    /// Format the transport failure and its operation context.
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
            Self::UnsupportedOutputVolume => {
                f.write_str("NDSIF device does not advertise output-volume control")
            }
            Self::UnsupportedLiveProfile { profile } => {
                write!(f, "NDSIF device does not advertise live profile {profile}")
            }
            Self::InvalidLiveCapabilities { reason } => {
                write!(
                    f,
                    "invalid or unsupported NDSIF live capabilities: {reason}"
                )
            }
            Self::LiveResetRequired => f.write_str(
                "NDSIF live and scheduled playback require a successful reset before switching",
            ),
            Self::InvalidLiveOperation { reason } => {
                write!(f, "invalid NDSIF live operation: {reason}")
            }
            Self::LiveWriteIndeterminate => f.write_str(
                "NDSIF live write may have partially succeeded; reset or close the device",
            ),
            Self::UnexpectedDacProfile { profile } => write!(
                f,
                "NDSIF returned DAC profile {profile} status while profile 1 was requested"
            ),
            Self::DeviceBusy { stream } => write!(
                f,
                "NDSIF {stream} is not idle; enable ndsif-reset or reset the device"
            ),
            Self::HostTimelineAlreadySet => {
                f.write_str("NDSIF host pacing timeline is already set or started")
            }
            Self::HostTimelineUnsupportedStream => {
                f.write_str("NDSIF host pacing cannot synchronize a device-timed stream")
            }
        }
    }
}

impl Error for NdsifTransportError {
    /// Expose the underlying device, serial, I/O, or protocol error.
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
