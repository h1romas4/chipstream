//! Serial request and response exchange.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use ndsif::{Command, Decoder, Response, Status};
use serialport::SerialPort;

use crate::NdsifTransportError;

pub(super) struct SerialSession {
    port: Box<dyn SerialPort>,
    response_timeout: Duration,
    decoder: Decoder,
    transaction_id: u16,
}

impl SerialSession {
    /// Initialize request state for an already opened serial port.
    pub(super) fn new(port: Box<dyn SerialPort>, response_timeout: Duration) -> Self {
        Self {
            port,
            response_timeout,
            decoder: Decoder::new(),
            transaction_id: 0,
        }
    }

    /// Exchange a command and validate its response, preserving typed errors.
    pub(super) fn request(
        &mut self,
        command: Command<'_>,
    ) -> Result<ndsif::Frame, NdsifTransportError> {
        let transaction_id = self.transaction_id;
        let request =
            command
                .to_frame(transaction_id)
                .map_err(|source| NdsifTransportError::Protocol {
                    operation: "invalid NDSIF command",
                    source,
                })?;
        self.transaction_id = self.transaction_id.wrapping_add(1);
        let encoded = request.encode();
        self.port
            .write_all(encoded.as_bytes())
            .map_err(|source| NdsifTransportError::Io {
                operation: "failed to write NDSIF frame",
                source,
            })?;

        if !command.expects_response() {
            return Ok(request);
        }
        self.port
            .flush()
            .map_err(|source| NdsifTransportError::Io {
                operation: "failed to flush NDSIF request",
                source,
            })?;

        let deadline = Instant::now() + self.response_timeout;
        let mut bytes = [0_u8; 256];
        loop {
            if Instant::now() >= deadline {
                return Err(NdsifTransportError::Timeout {
                    command: request.command_id(),
                    transaction_id,
                });
            }
            match self.port.read(&mut bytes) {
                Ok(0) => continue,
                Ok(count) => {
                    let mut received = None;
                    let mut decode_error = None;
                    self.decoder.push(&bytes[..count], |result| match result {
                        Ok(frame) => received = Some(frame),
                        Err(error) => decode_error = Some(error),
                    });
                    if let Some(error) = decode_error {
                        return Err(NdsifTransportError::Protocol {
                            operation: "invalid NDSIF response frame",
                            source: error,
                        });
                    }
                    if let Some(frame) = received {
                        let response = Response::decode(&frame).map_err(|source| {
                            NdsifTransportError::Protocol {
                                operation: "invalid NDSIF response",
                                source,
                            }
                        })?;
                        response.matches_request(&request).map_err(|source| {
                            NdsifTransportError::Protocol {
                                operation: "NDSIF response does not match request",
                                source,
                            }
                        })?;
                        if response.status != Status::Complete {
                            return Err(NdsifTransportError::Rejected {
                                command: request.command_id(),
                                status: response.status,
                            });
                        }
                        return Ok(frame);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(source) => {
                    return Err(NdsifTransportError::Io {
                        operation: "failed to read NDSIF response",
                        source,
                    });
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ndsif::{Reply, WriteId};

    #[test]
    fn exchanges_responses_and_advances_transaction_ids_for_unanswered_commands() {
        let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
        device.set_timeout(Duration::from_secs(2)).unwrap();
        port.set_timeout(Duration::from_secs(2)).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut decoder = Decoder::new();
            for id in 0..3 {
                let mut frame = None;
                while frame.is_none() {
                    let mut byte = [0];
                    device.read_exact(&mut byte).unwrap();
                    decoder.push(&byte, |result| frame = Some(result.unwrap()));
                }
                let frame = frame.unwrap();
                assert_eq!(frame.transaction_id(), id);
                if id != 1 {
                    let response = Response::for_request(&frame, Reply::Ping(frame.payload()))
                        .unwrap()
                        .to_frame()
                        .unwrap()
                        .encode();
                    let bytes = response.as_bytes();
                    let middle = bytes.len() / 2;
                    device.write_all(&bytes[..middle]).unwrap();
                    device.write_all(&bytes[middle..]).unwrap();
                }
            }
            wait.recv().unwrap();
        });

        let mut session = SerialSession::new(Box::new(port), Duration::from_secs(2));
        assert_eq!(
            session
                .request(Command::Ping(b"first"))
                .unwrap()
                .transaction_id(),
            0
        );
        assert!(
            session
                .request(Command::WriteBytes {
                    write_id: WriteId::SN76489,
                    instance: 0,
                    page: 0,
                    bytes: &[],
                })
                .is_err()
        );
        assert_eq!(
            session
                .request(Command::WriteBytes {
                    write_id: WriteId::SN76489,
                    instance: 0,
                    page: 0,
                    bytes: &[0x90],
                })
                .unwrap()
                .transaction_id(),
            1
        );
        assert_eq!(
            session
                .request(Command::Ping(b"last"))
                .unwrap()
                .transaction_id(),
            2
        );
        release.send(()).unwrap();
        worker.join().unwrap();
    }
}
