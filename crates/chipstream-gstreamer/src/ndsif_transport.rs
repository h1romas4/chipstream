use std::io::{Read, Write};
use std::time::{Duration, Instant};

use chipstream::chip::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent, NdsifRegisterEvent, NdsifTransport,
};
use ndsif::{
    AudioEvent, BytePosition, ClockMode, Command, Decoder, Divider, OkiClock, Reply, Response,
    Status, WriteId,
};
use serialport::SerialPort;

const AUDIO_STREAM_ID: u8 = 0xb7;
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);

struct AudioStream {
    instance: u8,
    clock: OkiClock,
    divider: Divider,
    output_rate: u32,
    accepted: u32,
    started: bool,
}

enum PendingEvent {
    Register(NdsifRegisterEvent),
    Data(NdsifAdpcmEvent),
    Control(NdsifAudioControlEvent),
}

impl PendingEvent {
    fn sample_frame(&self) -> usize {
        match self {
            Self::Register(event) => event.sample_frame,
            Self::Data(event) => event.sample_frame,
            Self::Control(event) => event.sample_frame,
        }
    }

    fn priority(&self) -> u8 {
        match self {
            Self::Register(_) => 0,
            Self::Data(_) => 1,
            Self::Control(event) if matches!(event.control, NdsifAudioControl::End(_)) => 2,
            Self::Control(_) => 0,
        }
    }
}

pub(super) struct NdsifSerialTransport {
    port: Box<dyn SerialPort>,
    response_timeout: Duration,
    decoder: Decoder,
    transaction_id: u16,
    capabilities: Vec<ndsif::CapabilityEntry>,
    data_capacity: u32,
    event_capacity: u16,
    configured_chip: bool,
    audio_stream: Option<AudioStream>,
    pending: Vec<PendingEvent>,
    last_status_check: Option<Instant>,
}

impl NdsifSerialTransport {
    pub(super) fn open(
        path: &str,
        baud_rate: u32,
        timeout: Duration,
        reset_device: bool,
    ) -> Result<Self, String> {
        let port = serialport::new(path, baud_rate)
            .timeout(timeout)
            .open()
            .map_err(|error| format!("failed to open NDSIF serial port {path}: {error}"))?;
        port.clear(serialport::ClearBuffer::Input)
            .map_err(|error| {
                format!("failed to clear pending NDSIF serial input on {path}: {error}")
            })?;
        let mut transport = Self {
            port,
            response_timeout: timeout,
            decoder: Decoder::new(),
            transaction_id: 0,
            capabilities: Vec::new(),
            data_capacity: 0,
            event_capacity: 0,
            configured_chip: false,
            audio_stream: None,
            pending: Vec::new(),
            last_status_check: None,
        };
        if reset_device {
            transport.request(Command::Reset)?;
        }
        transport.request(Command::Ping(b"chipstream"))?;
        transport.load_capabilities()?;
        transport.verify_audio_stream_idle()?;
        Ok(transport)
    }

    pub(super) fn supports_clock(&self, write_id: WriteId, instance: u8, clock_hz: u32) -> bool {
        self.capability(write_id, instance)
            .is_some_and(|capability| match capability.clock_mode {
                ClockMode::Variable => true,
                ClockMode::Fixed => capability.fixed_clock_hz == clock_hz,
                ClockMode::Unknown => false,
            })
    }

    pub(super) fn supports_audio_stream(&self, instance: u8, clock_hz: u32) -> bool {
        instance == 0
            && self.data_capacity > 0
            && self.event_capacity > 0
            && OkiClock::try_from(clock_hz).is_ok()
            && self.capability(WriteId::OKIM6258, instance).is_some()
    }

    fn load_capabilities(&mut self) -> Result<(), String> {
        let mut offset = 0_u16;
        loop {
            let frame = self.request(Command::GetCapabilities { offset })?;
            let response = Response::decode(&frame).map_err(|error| error.to_string())?;
            let Reply::Capabilities(capabilities) = response.reply else {
                return Err("NDSIF GET_CAPABILITIES returned an unexpected response".to_owned());
            };
            self.data_capacity = capabilities.data_capacity;
            self.event_capacity = capabilities.event_capacity;
            self.capabilities.extend(capabilities.entries());
            if capabilities.next_offset == u16::MAX {
                break;
            }
            if capabilities.next_offset <= offset {
                return Err("NDSIF capabilities pagination did not advance".to_owned());
            }
            offset = capabilities.next_offset;
        }
        Ok(())
    }

    fn verify_audio_stream_idle(&mut self) -> Result<(), String> {
        if self.capability(WriteId::OKIM6258, 0).is_none() {
            return Ok(());
        }
        let frame = self.request(Command::AudioStatus {
            stream_id: AUDIO_STREAM_ID,
            instance: 0,
        })?;
        let response = Response::decode(&frame).map_err(|error| error.to_string())?;
        match response.reply {
            Reply::AudioStatus(status)
                if status.accepted == 0
                    && status.played == 0
                    && status.pending == 0
                    && !status.flags.running()
                    && !status.flags.fault() =>
            {
                Ok(())
            }
            Reply::AudioStatus(_) => Err(
                "NDSIF OKIM6258 stream is not idle; enable ndsif-reset or reset the device"
                    .to_owned(),
            ),
            _ => Err("NDSIF AUDIO_STATUS returned an unexpected response".to_owned()),
        }
    }

    fn request(&mut self, command: Command<'_>) -> Result<ndsif::Frame, String> {
        let transaction_id = self.transaction_id;
        let request = command
            .to_frame(transaction_id)
            .map_err(|error| format!("invalid NDSIF command: {error}"))?;
        self.transaction_id = self.transaction_id.wrapping_add(1);
        let encoded = request.encode();
        self.port
            .write_all(encoded.as_bytes())
            .map_err(|error| format!("failed to write NDSIF frame: {error}"))?;

        if !command.expects_response() {
            return Ok(request);
        }
        self.port
            .flush()
            .map_err(|error| format!("failed to flush NDSIF request: {error}"))?;

        let deadline = Instant::now() + self.response_timeout;
        let mut bytes = [0_u8; 256];
        loop {
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF response to command 0x{:02x} transaction {transaction_id}",
                    request.command_id()
                ));
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
                        return Err(format!("invalid NDSIF response frame: {error}"));
                    }
                    if let Some(frame) = received {
                        let response =
                            Response::decode(&frame).map_err(|error| error.to_string())?;
                        response
                            .matches_request(&request)
                            .map_err(|error| error.to_string())?;
                        if response.status != Status::Complete {
                            return Err(format!(
                                "NDSIF command 0x{:02x} was rejected",
                                request.command_id()
                            ));
                        }
                        return Ok(frame);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(format!("failed to read NDSIF response: {error}")),
            }
        }
    }

    fn capability(&self, write_id: WriteId, instance: u8) -> Option<ndsif::CapabilityEntry> {
        self.capabilities
            .iter()
            .copied()
            .find(|entry| entry.write_id == write_id.get() && entry.instance == instance)
    }

    fn position_for_frame(
        stream: &AudioStream,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<BytePosition, String> {
        if output_rate != stream.output_rate || output_rate == 0 {
            return Err("NDSIF event output rate changed during playback".to_owned());
        }
        let position = (sample_frame as u128) * u128::from(stream.clock as u32)
            / (u128::from(output_rate) * u128::from(stream.divider as u32) * 2);
        let position =
            u32::try_from(position).map_err(|_| "NDSIF audio position overflow".to_owned())?;
        Ok(BytePosition::new(position))
    }

    fn send_event(&mut self, event: PendingEvent) -> Result<(), String> {
        match event {
            PendingEvent::Register(event) => self.send_registers(&[event]),
            PendingEvent::Control(event) => {
                let Some(stream) = self.audio_stream.as_ref() else {
                    return Err("NDSIF audio event has no configured OKIM6258 stream".to_owned());
                };
                let audio_event = match event.control {
                    NdsifAudioControl::Clock { clock_hz, divider } => AudioEvent::Clock {
                        clock: OkiClock::try_from(clock_hz).map_err(|error| error.to_string())?,
                        divider,
                    },
                    NdsifAudioControl::Pan(pan) => AudioEvent::Pan(pan),
                    NdsifAudioControl::End(pair) => AudioEvent::End(pair),
                };
                self.request(Command::AudioEvent {
                    stream_id: AUDIO_STREAM_ID,
                    instance: stream.instance,
                    position: event.position,
                    event: audio_event,
                })
                .map(|_| ())
            }
            PendingEvent::Data(_) => {
                Err("ADPCM data must be sent in a contiguous batch".to_owned())
            }
        }
    }

    fn send_registers(&mut self, events: &[NdsifRegisterEvent]) -> Result<(), String> {
        let Some(first) = events.first() else {
            return Ok(());
        };
        let writes = events.iter().map(|event| event.write).collect::<Vec<_>>();
        let Some(stream) = self.audio_stream.as_ref() else {
            return self
                .request(Command::WriteRegisters {
                    write_id: first.write_id,
                    instance: first.instance,
                    page: first.page,
                    writes: &writes,
                })
                .map(|_| ());
        };
        let position = Self::position_for_frame(stream, first.sample_frame, first.sample_rate)?;
        self.request(Command::AudioEvent {
            stream_id: AUDIO_STREAM_ID,
            instance: stream.instance,
            position,
            event: AudioEvent::Registers {
                write_id: first.write_id,
                instance: first.instance,
                page: first.page,
                writes: &writes,
            },
        })
        .map(|_| ())
    }

    fn send_data(&mut self, events: &[NdsifAdpcmEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let stream = self
            .audio_stream
            .as_ref()
            .ok_or_else(|| "NDSIF ADPCM data has no configured stream".to_owned())?;
        let instance = stream.instance;
        let position = events[0].position;
        if position.get() != stream.accepted {
            return Err(format!(
                "NDSIF expected ADPCM position {}, got {}",
                stream.accepted,
                position.get()
            ));
        }
        for (index, event) in events.iter().enumerate() {
            let expected = position
                .checked_advance(index)
                .map_err(|error| error.to_string())?;
            if event.instance != instance || event.position != expected {
                return Err("NDSIF ADPCM byte positions are not contiguous".to_owned());
            }
        }
        let data = events.iter().map(|event| event.value).collect::<Vec<_>>();
        self.request(Command::AudioData {
            stream_id: AUDIO_STREAM_ID,
            instance,
            position,
            adpcm: &data,
        })?;
        let stream = self
            .audio_stream
            .as_mut()
            .expect("stream was checked above");
        stream.accepted = stream
            .accepted
            .checked_add(data.len() as u32)
            .ok_or_else(|| "NDSIF accepted audio position overflow".to_owned())?;
        Ok(())
    }

    fn start_stream_if_ready(&mut self) -> Result<(), String> {
        let Some((instance, clock, divider)) = self.audio_stream.as_ref().and_then(|stream| {
            (!stream.started && stream.accepted > 0).then_some((
                stream.instance,
                stream.clock,
                stream.divider,
            ))
        }) else {
            return Ok(());
        };
        self.request(Command::Ping(b"chipstream-start"))?;
        self.request(Command::AudioStart {
            stream_id: AUDIO_STREAM_ID,
            instance,
            clock,
            divider,
        })?;
        let frame = self.request(Command::AudioStatus {
            stream_id: AUDIO_STREAM_ID,
            instance,
        })?;
        let response = Response::decode(&frame).map_err(|error| error.to_string())?;
        match response.reply {
            Reply::AudioStatus(status) if status.flags.running() => {}
            Reply::AudioStatus(_) => {
                return Err("NDSIF audio stream did not enter the running state".to_owned());
            }
            _ => return Err("NDSIF AUDIO_STATUS returned an unexpected response".to_owned()),
        }
        self.audio_stream
            .as_mut()
            .expect("stream was checked above")
            .started = true;
        self.last_status_check = Some(Instant::now());
        Ok(())
    }

    fn check_status_if_due(&mut self) -> Result<(), String> {
        let Some(instance) = self.audio_stream.as_ref().and_then(|stream| {
            (stream.started
                && self
                    .last_status_check
                    .is_some_and(|last| last.elapsed() >= STATUS_POLL_INTERVAL))
            .then_some(stream.instance)
        }) else {
            return Ok(());
        };
        let frame = self.request(Command::AudioStatus {
            stream_id: AUDIO_STREAM_ID,
            instance,
        })?;
        let response = Response::decode(&frame).map_err(|error| error.to_string())?;
        match response.reply {
            Reply::AudioStatus(status) if !status.flags.fault() => {}
            Reply::AudioStatus(status) => {
                return Err(format!(
                    "NDSIF audio stream faulted (accepted={}, played={}, pending={}, underflows={}, overflows={}, rejected={}, max_pending={}, flags={:?}, late_events={}, max_event_lag={})",
                    status.accepted,
                    status.played,
                    status.pending,
                    status.underflows,
                    status.overflows,
                    status.rejected,
                    status.max_pending,
                    status.flags,
                    status.late_events,
                    status.max_event_lag
                ));
            }
            _ => return Err("NDSIF AUDIO_STATUS returned an unexpected response".to_owned()),
        }
        self.last_status_check = Some(Instant::now());
        Ok(())
    }
}

impl NdsifTransport for NdsifSerialTransport {
    fn configure_chip(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), String> {
        let capability = self.capability(write_id, instance).ok_or_else(|| {
            format!(
                "NDSIF does not support chip 0x{:02x} instance {instance}",
                write_id.get()
            )
        })?;
        self.configured_chip = true;
        match capability.clock_mode {
            ClockMode::Variable => {
                self.request(Command::SetChipClock {
                    write_id,
                    instance,
                    hz: clock_hz,
                    response: true,
                })?;
            }
            ClockMode::Fixed if capability.fixed_clock_hz == clock_hz => {}
            ClockMode::Fixed => {
                return Err(format!(
                    "NDSIF chip 0x{:02x} instance {instance} has fixed clock {} Hz, requested {clock_hz} Hz",
                    write_id.get(),
                    capability.fixed_clock_hz
                ));
            }
            ClockMode::Unknown => {
                return Err(format!(
                    "NDSIF chip 0x{:02x} instance {instance} has an unknown clock mode",
                    write_id.get()
                ));
            }
        }
        Ok(())
    }

    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Register(event));
        Ok(())
    }

    fn configure_audio_stream(
        &mut self,
        instance: u8,
        clock_hz: u32,
        divider: Divider,
        output_rate: u32,
    ) -> Result<(), String> {
        if instance != 0 {
            return Err("NDSIF transport supports only the primary OKIM6258 stream".to_owned());
        }
        if self.data_capacity == 0 || self.event_capacity == 0 {
            return Err("NDSIF device does not advertise audio stream capacity".to_owned());
        }
        let clock = OkiClock::try_from(clock_hz).map_err(|error| error.to_string())?;
        if self.audio_stream.is_some() {
            return Err("NDSIF transport supports only one OKIM6258 stream".to_owned());
        }
        self.audio_stream = Some(AudioStream {
            instance,
            clock,
            divider,
            output_rate,
            accepted: 0,
            started: false,
        });
        Ok(())
    }

    fn schedule_adpcm_data(&mut self, event: NdsifAdpcmEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Data(event));
        Ok(())
    }

    fn schedule_audio_control(&mut self, event: NdsifAudioControlEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Control(event));
        Ok(())
    }

    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        if let Some(stream) = self.audio_stream.as_ref()
            && stream.output_rate != output_rate
        {
            return Err("NDSIF event output rate changed during playback".to_owned());
        }
        let mut due = Vec::new();
        let mut future = Vec::new();
        for event in self.pending.drain(..) {
            if event.sample_frame() <= sample_frame {
                due.push(event);
            } else {
                future.push(event);
            }
        }

        self.pending = future;
        due.sort_by_key(|event| (event.sample_frame(), event.priority()));
        let mut index = 0;
        while index < due.len() {
            match &due[index] {
                PendingEvent::Register(first) => {
                    let start = index;
                    let position = self
                        .audio_stream
                        .as_ref()
                        .map(|stream| {
                            Self::position_for_frame(stream, first.sample_frame, first.sample_rate)
                        })
                        .transpose()?;
                    index += 1;
                    while index < due.len() && index - start < ndsif::MAX_REGISTER_EVENT_WRITES {
                        let PendingEvent::Register(next) = &due[index] else {
                            break;
                        };
                        let same_position = match (self.audio_stream.as_ref(), position) {
                            (Some(stream), Some(position)) => {
                                Self::position_for_frame(
                                    stream,
                                    next.sample_frame,
                                    next.sample_rate,
                                )? == position
                            }
                            _ => true,
                        };
                        if next.write_id != first.write_id
                            || next.instance != first.instance
                            || next.page != first.page
                            || !same_position
                        {
                            break;
                        }
                        index += 1;
                    }
                    let registers = due[start..index]
                        .iter()
                        .map(|event| match event {
                            PendingEvent::Register(event) => event.clone(),
                            _ => unreachable!("register batch contains only register events"),
                        })
                        .collect::<Vec<_>>();
                    self.send_registers(&registers)?;
                }
                PendingEvent::Data(_) => {
                    let start = index;
                    index += 1;
                    while index < due.len()
                        && matches!(&due[index], PendingEvent::Data(_))
                        && index - start < ndsif::MAX_AUDIO_DATA_BYTES
                    {
                        index += 1;
                    }
                    let data = due[start..index]
                        .iter()
                        .map(|event| match event {
                            PendingEvent::Data(event) => *event,
                            _ => unreachable!("data batch contains only data events"),
                        })
                        .collect::<Vec<_>>();
                    self.send_data(&data)?;
                }
                _ => {
                    let event = due.remove(index);
                    self.send_event(event)?;
                }
            }
        }
        self.start_stream_if_ready()?;
        self.check_status_if_due()?;
        Ok(())
    }

    fn start_audio_stream(&mut self, _instance: u8) -> Result<(), String> {
        self.start_stream_if_ready()
    }

    fn finish_audio_stream(
        &mut self,
        instance: u8,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        self.flush_scheduled_until(sample_frame, output_rate)?;
        if self
            .audio_stream
            .as_ref()
            .is_some_and(|stream| stream.instance != instance)
        {
            return Err("NDSIF finish request does not match the configured stream".to_owned());
        }
        self.start_stream_if_ready()
    }
}

impl Drop for NdsifSerialTransport {
    fn drop(&mut self) {
        if !self.configured_chip {
            return;
        }
        if let Err(error) = self.request(Command::Reset) {
            eprintln!("failed to reset NDSIF device while closing: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_output_frames_to_adpcm_byte_positions() {
        let stream = AudioStream {
            instance: 0,
            clock: OkiClock::Mhz4,
            divider: Divider::Div1024,
            output_rate: 44_100,
            accepted: 0,
            started: false,
        };

        assert_eq!(
            NdsifSerialTransport::position_for_frame(&stream, 441, 44_100).unwrap(),
            BytePosition::new(19)
        );
        assert_eq!(
            NdsifSerialTransport::position_for_frame(&stream, 882, 44_100).unwrap(),
            BytePosition::new(39)
        );
    }
}
