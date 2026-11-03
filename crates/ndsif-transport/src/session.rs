//! Serial request and response exchange.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use ndsif::{Command, Decoder, Response, Status};
use serialport::SerialPort;

use crate::NdsifTransportError;

const DAC_STATUS_RETRY_INTERVAL: Duration = Duration::from_millis(30);
const DAC_STATUS_READ_POLL: Duration = Duration::from_millis(10);
const DAC_STATUS_MAX_RETRIES: usize = 2;

pub(super) struct SerialSession {
    port: Box<dyn SerialPort>,
    response_timeout: Duration,
    decoder: Decoder,
    transaction_id: u16,
    unconfirmed_bytes: usize,
}

impl SerialSession {
    /// Initialize request state for an already opened serial port.
    pub(super) fn new(port: Box<dyn SerialPort>, response_timeout: Duration) -> Self {
        Self {
            port,
            response_timeout,
            decoder: Decoder::new(),
            transaction_id: 0,
            unconfirmed_bytes: 0,
        }
    }

    /// Return the bytes sent since the last successfully matched response.
    pub(super) fn unconfirmed_bytes(&self) -> usize {
        self.unconfirmed_bytes
    }

    /// Exchange a command and validate its response, preserving typed errors.
    pub(super) fn request(
        &mut self,
        command: Command<'_>,
    ) -> Result<ndsif::Frame, NdsifTransportError> {
        let retry_status = matches!(command, Command::DacStatus { .. });
        let original_timeout = self.port.timeout();
        let poll_timeout = original_timeout.min(DAC_STATUS_READ_POLL);
        if retry_status && poll_timeout != original_timeout {
            self.port
                .set_timeout(poll_timeout)
                .map_err(|source| NdsifTransportError::Serial {
                    operation: "failed to set NDSIF status read timeout",
                    source,
                })?;
        }
        let result = self.request_inner(command, retry_status);
        if retry_status && poll_timeout != original_timeout {
            self.port.set_timeout(original_timeout).map_err(|source| {
                NdsifTransportError::Serial {
                    operation: "failed to restore NDSIF read timeout",
                    source,
                }
            })?;
        }
        result
    }

    fn send(&mut self, command: Command<'_>) -> Result<ndsif::Frame, NdsifTransportError> {
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
        self.unconfirmed_bytes = self
            .unconfirmed_bytes
            .saturating_add(encoded.as_bytes().len());
        Ok(request)
    }

    fn request_inner(
        &mut self,
        command: Command<'_>,
        retry_status: bool,
    ) -> Result<ndsif::Frame, NdsifTransportError> {
        let request = self.send(command)?;
        if !command.expects_response() {
            return Ok(request);
        }
        let mut requests = vec![request];
        self.port
            .flush()
            .map_err(|source| NdsifTransportError::Io {
                operation: "failed to flush NDSIF request",
                source,
            })?;

        let deadline = Instant::now() + self.response_timeout;
        let mut retry_at = Instant::now() + DAC_STATUS_RETRY_INTERVAL;
        let mut bytes = [0_u8; 256];
        loop {
            if Instant::now() >= deadline {
                let request = requests.last().expect("at least one request");
                return Err(NdsifTransportError::Timeout {
                    command: request.command_id(),
                    transaction_id: request.transaction_id(),
                });
            }
            if retry_status
                && requests.len() <= DAC_STATUS_MAX_RETRIES
                && Instant::now() >= retry_at
            {
                self.decoder.expire_partial_frame();
                requests.push(self.send(command)?);
                self.port
                    .flush()
                    .map_err(|source| NdsifTransportError::Io {
                        operation: "failed to flush NDSIF status retry",
                        source,
                    })?;
                retry_at = Instant::now() + DAC_STATUS_RETRY_INTERVAL;
            }
            match self.port.read(&mut bytes) {
                Ok(0) => continue,
                Ok(count) => {
                    let mut results = Vec::new();
                    self.decoder
                        .push(&bytes[..count], |result| results.push(result));
                    let mut latest = None;
                    for result in results {
                        let frame = result.map_err(|source| NdsifTransportError::Protocol {
                            operation: "invalid NDSIF response frame",
                            source,
                        })?;
                        let response = Response::decode(&frame).map_err(|source| {
                            NdsifTransportError::Protocol {
                                operation: "invalid NDSIF response",
                                source,
                            }
                        })?;
                        let expected = requests
                            .iter()
                            .find(|request| request.transaction_id() == response.transaction_id)
                            .unwrap_or_else(|| requests.last().expect("at least one request"));
                        response.matches_request(expected).map_err(|source| {
                            NdsifTransportError::Protocol {
                                operation: "NDSIF response does not match request",
                                source,
                            }
                        })?;
                        if response.status != Status::Complete {
                            return Err(NdsifTransportError::Rejected {
                                command: expected.command_id(),
                                status: response.status,
                            });
                        }
                        if expected.transaction_id()
                            == requests
                                .last()
                                .expect("at least one request")
                                .transaction_id()
                        {
                            latest = Some(frame);
                        }
                    }
                    if let Some(frame) = latest {
                        self.unconfirmed_bytes = 0;
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
    use ndsif::{Frame, MessageType, Reply, WriteId};

    fn read_frame(device: &mut serialport::TTYPort, decoder: &mut Decoder) -> Frame {
        loop {
            let mut byte = [0];
            device.read_exact(&mut byte).unwrap();
            if let Some(result) = decoder.push_byte(byte[0]) {
                return result.unwrap();
            }
        }
    }

    fn status_response(request: &Frame) -> Vec<u8> {
        let mut payload = [0; 73];
        payload[1] = 1;
        Frame::new(
            MessageType::Response,
            request.command_id(),
            request.transaction_id(),
            &payload,
        )
        .unwrap()
        .encode()
        .as_bytes()
        .to_vec()
    }

    #[test]
    fn retries_status_with_late_or_lost_responses() {
        for mode in 0..4 {
            let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
            device.set_timeout(Duration::from_secs(2)).unwrap();
            port.set_timeout(Duration::from_secs(2)).unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut decoder = Decoder::new();
                let first = read_frame(&mut device, &mut decoder);
                let response = status_response(&first);
                if mode < 2 {
                    device.write_all(&response[..23]).unwrap();
                }
                let retry = read_frame(&mut device, &mut decoder);
                assert_eq!(
                    retry.transaction_id(),
                    first.transaction_id().wrapping_add(1)
                );
                let mut combined = Vec::new();
                if mode == 0 {
                    combined.extend_from_slice(&response[23..]);
                } else if mode == 2 {
                    combined.extend_from_slice(&response);
                }
                combined.extend_from_slice(&status_response(&retry));
                device.write_all(&combined).unwrap();
                let ping = read_frame(&mut device, &mut decoder);
                let response = Response::for_request(&ping, Reply::Ping(ping.payload()))
                    .unwrap()
                    .to_frame()
                    .unwrap()
                    .encode();
                device.write_all(response.as_bytes()).unwrap();
                wait.recv().unwrap();
            });
            let mut session = SerialSession::new(Box::new(port), Duration::from_millis(250));
            session.transaction_id = u16::MAX;
            let reply = session.request(Command::DacStatus { profile: 1 }).unwrap();
            assert_eq!(reply.transaction_id(), 0);
            assert_eq!(session.unconfirmed_bytes(), 0);
            assert_eq!(session.port.timeout(), Duration::from_secs(2));
            assert_eq!(
                session
                    .request(Command::Ping(b"next"))
                    .unwrap()
                    .transaction_id(),
                1
            );
            release.send(()).unwrap();
            worker.join().unwrap();
        }
    }

    #[test]
    fn status_retry_is_bounded_and_timeout_is_reported() {
        let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
        device.set_timeout(Duration::from_secs(2)).unwrap();
        port.set_timeout(Duration::from_secs(2)).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut decoder = Decoder::new();
            for id in 0..=2 {
                assert_eq!(read_frame(&mut device, &mut decoder).transaction_id(), id);
            }
            wait.recv().unwrap();
        });
        let mut session = SerialSession::new(Box::new(port), Duration::from_millis(120));
        let error = session
            .request(Command::DacStatus { profile: 1 })
            .unwrap_err();
        assert!(matches!(
            error,
            NdsifTransportError::Timeout {
                command: 0x1c,
                transaction_id: 2
            }
        ));
        assert!(session.unconfirmed_bytes() > 0);
        assert_eq!(session.port.timeout(), Duration::from_secs(2));
        release.send(()).unwrap();
        worker.join().unwrap();
    }

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
