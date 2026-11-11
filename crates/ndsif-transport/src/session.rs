//! Serial request and response exchange.

use std::collections::VecDeque;
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
    sent_bytes: u64,
    confirmed_bytes: u64,
    pending_status: Vec<(ndsif::Frame, u64)>,
    retired_status: VecDeque<(ndsif::Frame, Instant)>,
    status_replies: VecDeque<ndsif::Frame>,
}

impl SerialSession {
    /// Initialize request state for an already opened serial port.
    pub(super) fn new(port: Box<dyn SerialPort>, response_timeout: Duration) -> Self {
        Self {
            port,
            response_timeout,
            decoder: Decoder::new(),
            transaction_id: 0,
            sent_bytes: 0,
            confirmed_bytes: 0,
            pending_status: Vec::new(),
            retired_status: VecDeque::new(),
            status_replies: VecDeque::new(),
        }
    }

    /// Return the bytes sent since the last successfully matched response.
    pub(super) fn unconfirmed_bytes(&self) -> usize {
        usize::try_from(self.sent_bytes - self.confirmed_bytes).unwrap_or(usize::MAX)
    }

    /// Send a read-only status request without waiting for a response.
    pub(super) fn begin_dac_status(&mut self, profile: u8) -> Result<u16, NdsifTransportError> {
        if self.pending_status.len() >= 32 {
            return Err(NdsifTransportError::UnexpectedResponse {
                operation: "too many outstanding DAC_STATUS requests",
            });
        }
        let frame = self.send(Command::DacStatus { profile })?;
        let transaction_id = frame.transaction_id();
        self.pending_status.push((frame, self.sent_bytes));
        Ok(transaction_id)
    }

    /// Return already available STATUS replies without waiting for serial input.
    pub(super) fn poll_dac_status(&mut self) -> Result<Option<ndsif::Frame>, NdsifTransportError> {
        if let Some(frame) = self.status_replies.pop_front() {
            return Ok(Some(frame));
        }
        let available =
            self.port
                .bytes_to_read()
                .map_err(|source| NdsifTransportError::Serial {
                    operation: "failed to query NDSIF serial input",
                    source,
                })?;
        if available == 0 {
            return Ok(None);
        }
        let mut bytes = [0; 256];
        let length = usize::try_from(available)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let count =
            self.port
                .read(&mut bytes[..length])
                .map_err(|source| NdsifTransportError::Io {
                    operation: "failed to read NDSIF response",
                    source,
                })?;
        let mut frames = Vec::new();
        self.decoder
            .push(&bytes[..count], |result| frames.push(result));
        for result in frames {
            let frame = result.map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid NDSIF response frame",
                source,
            })?;
            if !self.capture_dac_status(frame)? {
                return Err(NdsifTransportError::UnexpectedResponse {
                    operation: "unsolicited NDSIF response",
                });
            }
        }
        Ok(self.status_replies.pop_front())
    }

    /// Retire an unanswered request while still recognizing a late response.
    pub(super) fn retire_dac_status(
        &mut self,
        transaction_id: u16,
    ) -> Result<(), NdsifTransportError> {
        let Some(index) = self
            .pending_status
            .iter()
            .position(|(request, _)| request.transaction_id() == transaction_id)
        else {
            return Ok(());
        };
        let request = self.pending_status[index].0.clone();
        self.store_retired_status(request)?;
        self.pending_status.remove(index);
        Ok(())
    }

    /// Retain a completed status request so late replies can be correlated.
    fn store_retired_status(&mut self, request: ndsif::Frame) -> Result<(), NdsifTransportError> {
        while self
            .retired_status
            .front()
            .is_some_and(|(_, when)| when.elapsed() >= Duration::from_secs(60))
        {
            self.retired_status.pop_front();
        }
        if self.retired_status.len() >= 1024 {
            return Err(NdsifTransportError::UnexpectedResponse {
                operation: "too many retired DAC_STATUS requests",
            });
        }
        self.retired_status.push_back((request, Instant::now()));
        Ok(())
    }

    pub(super) fn clear_dac_status(&mut self) -> Result<(), NdsifTransportError> {
        while let Some((request, _)) = self.pending_status.last() {
            self.retire_dac_status(request.transaction_id())?;
        }
        self.status_replies.clear();
        Ok(())
    }

    pub(super) fn expire_partial_dac_status(&mut self) {
        self.decoder.expire_partial_frame();
    }

    /// Match and store an asynchronous DAC status frame when applicable.
    fn capture_dac_status(&mut self, frame: ndsif::Frame) -> Result<bool, NdsifTransportError> {
        if let Some(index) = self
            .retired_status
            .iter()
            .position(|(request, _)| request.transaction_id() == frame.transaction_id())
        {
            let (request, _) = self.retired_status.remove(index).expect("index exists");
            let response =
                Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                    operation: "invalid retired NDSIF response",
                    source,
                })?;
            response
                .matches_request(&request)
                .map_err(|source| NdsifTransportError::Protocol {
                    operation: "retired NDSIF response does not match request",
                    source,
                })?;
            if response.status != Status::Complete {
                return Err(NdsifTransportError::Rejected {
                    command: request.command_id(),
                    status: response.status,
                });
            }
            return Ok(true);
        }
        let Some(index) = self
            .pending_status
            .iter()
            .position(|(request, _)| request.transaction_id() == frame.transaction_id())
        else {
            return Ok(false);
        };
        let (request, fence) = self.pending_status.remove(index);
        let response =
            Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid NDSIF response",
                source,
            })?;
        response
            .matches_request(&request)
            .map_err(|source| NdsifTransportError::Protocol {
                operation: "NDSIF response does not match request",
                source,
            })?;
        if response.status != Status::Complete {
            return Err(NdsifTransportError::Rejected {
                command: request.command_id(),
                status: response.status,
            });
        }
        self.confirmed_bytes = self.confirmed_bytes.max(fence);
        self.status_replies.push_back(frame);
        Ok(true)
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

    /// Encode and write a command using an unused transaction identifier.
    fn send(&mut self, command: Command<'_>) -> Result<ndsif::Frame, NdsifTransportError> {
        let mut transaction_id = self.transaction_id;
        while self
            .pending_status
            .iter()
            .any(|(request, _)| request.transaction_id() == transaction_id)
            || self
                .retired_status
                .iter()
                .any(|(request, _)| request.transaction_id() == transaction_id)
        {
            transaction_id = transaction_id.wrapping_add(1);
        }
        let request =
            command
                .to_frame(transaction_id)
                .map_err(|source| NdsifTransportError::Protocol {
                    operation: "invalid NDSIF command",
                    source,
                })?;
        self.transaction_id = transaction_id.wrapping_add(1);
        let encoded = request.encode();
        let sent_bytes = self
            .sent_bytes
            .checked_add(encoded.as_bytes().len() as u64)
            .ok_or(NdsifTransportError::UnexpectedResponse {
                operation: "NDSIF serial byte counter overflow",
            })?;
        self.port
            .write_all(encoded.as_bytes())
            .map_err(|source| NdsifTransportError::Io {
                operation: "failed to write NDSIF frame",
                source,
            })?;
        self.sent_bytes = sent_bytes;
        Ok(request)
    }

    /// Send a command and wait for its correlated response.
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
                        if self.capture_dac_status(frame.clone())? {
                            continue;
                        }
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
                        if retry_status {
                            for previous in requests.iter().take(requests.len() - 1) {
                                self.store_retired_status(previous.clone())?;
                            }
                        }
                        self.confirmed_bytes = self.sent_bytes;
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
    fn async_status_fence_does_not_confirm_later_writes() {
        let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
        device.set_timeout(Duration::from_secs(2)).unwrap();
        port.set_timeout(Duration::from_secs(2)).unwrap();
        let (ready, wait) = std::sync::mpsc::channel();
        let (release, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut decoder = Decoder::new();
            let status = read_frame(&mut device, &mut decoder);
            let response = status_response(&status);
            device.write_all(&response[..20]).unwrap();
            ready.send(()).unwrap();
            let _write = read_frame(&mut device, &mut decoder);
            device.write_all(&response[20..]).unwrap();
            done.recv().unwrap();
        });
        let mut session = SerialSession::new(Box::new(port), Duration::from_secs(2));
        let id = session.begin_dac_status(1).unwrap();
        wait.recv().unwrap();
        assert!(session.poll_dac_status().unwrap().is_none());
        let write = Command::WriteBytes {
            write_id: WriteId::SN76489,
            instance: 0,
            page: 0,
            bytes: &[0x90],
        };
        let write_size = write.to_frame(1).unwrap().encode().as_bytes().len();
        session.request(write).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let response = loop {
            if let Some(frame) = session.poll_dac_status().unwrap() {
                break frame;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(response.transaction_id(), id);
        assert_eq!(session.unconfirmed_bytes(), write_size);
        release.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn async_status_is_routed_while_waiting_for_a_synchronous_reply() {
        let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
        device.set_timeout(Duration::from_secs(2)).unwrap();
        port.set_timeout(Duration::from_secs(2)).unwrap();
        let (release, done) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut decoder = Decoder::new();
            let status = read_frame(&mut device, &mut decoder);
            let ping = read_frame(&mut device, &mut decoder);
            let mut output = status_response(&status);
            output.extend_from_slice(
                Response::for_request(&ping, Reply::Ping(ping.payload()))
                    .unwrap()
                    .to_frame()
                    .unwrap()
                    .encode()
                    .as_bytes(),
            );
            device.write_all(&output).unwrap();
            done.recv().unwrap();
        });
        let mut session = SerialSession::new(Box::new(port), Duration::from_secs(2));
        let id = session.begin_dac_status(1).unwrap();
        session.request(Command::Ping(b"fence")).unwrap();
        assert_eq!(session.unconfirmed_bytes(), 0);
        assert_eq!(
            session.poll_dac_status().unwrap().unwrap().transaction_id(),
            id
        );
        release.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn synchronous_status_retry_accepts_old_reply_after_completion() {
        let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
        device.set_timeout(Duration::from_secs(2)).unwrap();
        port.set_timeout(Duration::from_secs(2)).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut decoder = Decoder::new();
            let first = read_frame(&mut device, &mut decoder);
            let retry = read_frame(&mut device, &mut decoder);
            device.write_all(&status_response(&retry)).unwrap();
            let ping = read_frame(&mut device, &mut decoder);
            let mut responses = status_response(&first);
            responses.extend_from_slice(
                Response::for_request(&ping, Reply::Ping(ping.payload()))
                    .unwrap()
                    .to_frame()
                    .unwrap()
                    .encode()
                    .as_bytes(),
            );
            device.write_all(&responses).unwrap();
            wait.recv().unwrap();
        });
        let mut session = SerialSession::new(Box::new(port), Duration::from_secs(2));
        session.request(Command::DacStatus { profile: 1 }).unwrap();
        session.request(Command::Ping(b"next")).unwrap();
        release.send(()).unwrap();
        worker.join().unwrap();
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
