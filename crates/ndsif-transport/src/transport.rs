//! Scheduled device transport and event routing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use chipstream::chip::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent, NdsifPsgEvent, NdsifRegisterEvent,
    NdsifTransport,
};
use ndsif::{
    AudioEvent, AudioStreamState, AudioTiming, BytePosition, CONTROL_FEATURE_DAC_PROFILE,
    ClockMode, Command, DAC_PROFILE_YM2612, DAC_TICKS_PER_SECOND, DacCapabilities, Divider,
    OkiClock, Reply, Response, WriteId,
};

use crate::NdsifTransportError;
use crate::factory::validate_config;
use crate::session::SerialSession;

mod audio;
mod dac;
#[cfg(test)]
#[path = "../tests/unit/transport.rs"]
mod tests;

use audio::{AudioStream, audio_prebuffer_bytes};
use dac::TimedDacStream;

const AUDIO_STREAM_ID: u8 = 0xb7;
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DAC_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
enum PendingEvent {
    Register(NdsifRegisterEvent),
    Psg(NdsifPsgEvent),
    Data(NdsifAdpcmEvent),
    Control(NdsifAudioControlEvent),
}

impl PendingEvent {
    /// Return the event's absolute host output-frame position.
    fn sample_frame(&self) -> usize {
        match self {
            Self::Register(event) => event.sample_frame,
            Self::Psg(event) => event.sample_frame,
            Self::Data(event) => event.sample_frame,
            Self::Control(event) => event.sample_frame,
        }
    }

    /// Order data, writes, and end controls at the same position.
    fn priority(&self) -> u8 {
        match self {
            Self::Data(_) => 0,
            Self::Register(_) => 1,
            Self::Psg(_) => 1,
            Self::Control(event) if matches!(event.control, NdsifAudioControl::End(_)) => 2,
            Self::Control(_) => 1,
        }
    }
}

/// Serial connection implementing chipstream's scheduled NDSIF transport.
pub struct NdsifSerialTransport {
    session: SerialSession,
    capabilities: Vec<ndsif::CapabilityEntry>,
    data_capacity: u32,
    event_capacity: u16,
    timed_dac_capabilities: Option<DacCapabilities>,
    configured_chip: bool,
    audio_stream: Option<AudioStream>,
    timed_dac_stream: Option<TimedDacStream>,
    pending: Vec<PendingEvent>,
    wall_clock_origin: Option<Instant>,
}

impl NdsifSerialTransport {
    /// Open a serial device, discover capabilities, and verify its streams are idle.
    ///
    /// During initialization no reset is issued unless `reset_device` is true.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration error, or a [`NdsifTransportError::Device`]
    /// identifying the port and preserving the initialization failure.
    pub fn open(
        path: &str,
        baud_rate: u32,
        timeout: Duration,
        reset_device: bool,
    ) -> Result<Self, NdsifTransportError> {
        validate_config(path, baud_rate, timeout)?;
        Self::open_device(path, baud_rate, timeout, reset_device).map_err(|source| {
            NdsifTransportError::Device {
                port: path.to_owned(),
                source: Box::new(source),
            }
        })
    }

    /// Open and initialize a validated device, optionally resetting it.
    fn open_device(
        path: &str,
        baud_rate: u32,
        timeout: Duration,
        reset_device: bool,
    ) -> Result<Self, NdsifTransportError> {
        let port = serialport::new(path, baud_rate)
            .timeout(timeout)
            .open()
            .map_err(|source| NdsifTransportError::Serial {
                operation: "failed to open NDSIF serial port",
                source,
            })?;
        port.clear(serialport::ClearBuffer::Input)
            .map_err(|source| NdsifTransportError::Serial {
                operation: "failed to clear pending NDSIF serial input",
                source,
            })?;
        let mut transport = Self {
            session: SerialSession::new(port, timeout),
            capabilities: Vec::new(),
            data_capacity: 0,
            event_capacity: 0,
            timed_dac_capabilities: None,
            configured_chip: false,
            audio_stream: None,
            timed_dac_stream: None,
            pending: Vec::new(),
            wall_clock_origin: None,
        };
        if reset_device {
            transport.request_typed(Command::Reset)?;
        }
        transport.request_typed(Command::Ping(b"chipstream"))?;
        transport.load_capabilities()?;
        transport.verify_audio_stream_idle()?;
        transport.verify_timed_dac_idle()?;
        Ok(transport)
    }

    /// Check cached capabilities for the chip instance and requested clock.
    ///
    /// Variable-clock targets are accepted; fixed clocks must match exactly.
    /// This does not set the device clock or perform I/O.
    pub fn supports_clock(&self, write_id: WriteId, instance: u8, clock_hz: u32) -> bool {
        self.capability(write_id, instance)
            .is_some_and(|capability| match capability.clock_mode {
                ClockMode::Variable => true,
                ClockMode::Fixed => capability.fixed_clock_hz == clock_hz,
                ClockMode::Unknown => false,
            })
    }

    /// Check cached capability and capacity for a primary OKIM6258 stream.
    ///
    /// The clock must be supported by `ndsif::OkiClock`. This does not check
    /// the chip's advertised clock mode; callers should also use
    /// [`Self::supports_clock`].
    pub fn supports_audio_stream(&self, instance: u8, clock_hz: u32) -> bool {
        instance == 0
            && self.data_capacity > 0
            && self.event_capacity > 0
            && OkiClock::try_from(clock_hz).is_ok()
            && self.capability(WriteId::OKIM6258, instance).is_some()
    }

    /// Read all capability pages and discover the supported timed DAC profile.
    fn load_capabilities(&mut self) -> Result<(), NdsifTransportError> {
        let mut offset = 0_u16;
        let mut timed_dac_supported = false;
        loop {
            let frame = self.request_typed(Command::GetCapabilities { offset })?;
            let response =
                Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                    operation: "invalid NDSIF GET_CAPABILITIES response",
                    source,
                })?;
            let Reply::Capabilities(capabilities) = response.reply else {
                return Err(NdsifTransportError::UnexpectedResponse {
                    operation: "GET_CAPABILITIES",
                });
            };
            self.data_capacity = capabilities.data_capacity;
            self.event_capacity = capabilities.event_capacity;
            timed_dac_supported |= capabilities.control_features & CONTROL_FEATURE_DAC_PROFILE != 0;
            self.capabilities.extend(capabilities.entries());
            if capabilities.next_offset == u16::MAX {
                break;
            }
            if capabilities.next_offset <= offset {
                return Err(NdsifTransportError::InvalidCapabilitiesPagination);
            }
            offset = capabilities.next_offset;
        }
        if timed_dac_supported {
            let frame = self.request_typed(Command::GetDacCapabilities)?;
            let response =
                Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                    operation: "invalid NDSIF GET_DAC_CAPABILITIES response",
                    source,
                })?;
            let Reply::DacCapabilities(capabilities) = response.reply else {
                return Err(NdsifTransportError::UnexpectedResponse {
                    operation: "GET_DAC_CAPABILITIES",
                });
            };
            if capabilities.profile == DAC_PROFILE_YM2612
                && capabilities.revision == 40
                && capabilities.ticks_per_second == DAC_TICKS_PER_SECOND
                && capabilities.record_bytes == 4
            {
                self.timed_dac_capabilities = Some(capabilities);
            }
        }
        Ok(())
    }

    /// Reject a timed DAC profile with existing activity or faults.
    fn verify_timed_dac_idle(&mut self) -> Result<(), NdsifTransportError> {
        if self.timed_dac_capabilities.is_none() {
            return Ok(());
        }
        let status = self.query_timed_dac_status_typed()?;
        if status.state == 0
            && status.fault == 0
            && !status.sealed
            && status.accepted == 0
            && status.consumed == 0
            && status.queued == 0
            && status.event_queued == 0
            && status.event_consumed == 0
            && status.underflows == 0
            && status.overflows == 0
            && status.rejected == 0
            && status.late_dac == 0
            && status.accepted_tick == 0
            && status.played_tick == 0
            && status.executed_tick == 0
        {
            Ok(())
        } else {
            Err(NdsifTransportError::DeviceBusy {
                stream: "timed DAC profile",
            })
        }
    }

    /// Reject an OKIM6258 audio stream with existing activity or faults.
    fn verify_audio_stream_idle(&mut self) -> Result<(), NdsifTransportError> {
        if self.capability(WriteId::OKIM6258, 0).is_none() {
            return Ok(());
        }
        let frame = self.request_typed(Command::AudioStatus {
            stream_id: AUDIO_STREAM_ID,
            instance: 0,
        })?;
        let response =
            Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid NDSIF AUDIO_STATUS response",
                source,
            })?;
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
            Reply::AudioStatus(_) => Err(NdsifTransportError::DeviceBusy {
                stream: "OKIM6258 stream",
            }),
            _ => Err(NdsifTransportError::UnexpectedResponse {
                operation: "AUDIO_STATUS",
            }),
        }
    }

    /// Send a command using the playback trait's string error boundary.
    fn request(&mut self, command: Command<'_>) -> Result<ndsif::Frame, String> {
        self.request_typed(command)
            .map_err(|error| error.to_string())
    }

    /// Exchange a command and validate its response, preserving typed errors.
    fn request_typed(&mut self, command: Command<'_>) -> Result<ndsif::Frame, NdsifTransportError> {
        self.session.request(command)
    }

    /// Find a cached capability for a chip target and instance.
    fn capability(&self, write_id: WriteId, instance: u8) -> Option<ndsif::CapabilityEntry> {
        self.capabilities
            .iter()
            .copied()
            .find(|entry| entry.write_id == write_id.get() && entry.instance == instance)
    }

    /// Convert a host frame to the audio stream's byte timeline.
    fn position_for_frame(
        stream: &AudioStream,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<BytePosition, String> {
        stream
            .state
            .position_for_frame(sample_frame, output_rate)
            .map_err(|error| format!("invalid NDSIF audio position: {error}"))
    }

    /// Check whether an event can be sent against the current audio position.
    fn event_is_ready(event: &PendingEvent, stream: Option<&AudioStream>) -> Result<bool, String> {
        let Some(stream) = stream else {
            return Ok(true);
        };
        let position = match event {
            PendingEvent::Register(event) => {
                Some(Self::position_for_frame(stream, event.sample_frame, event.sample_rate)?.get())
            }
            PendingEvent::Psg(event) => {
                Some(Self::position_for_frame(stream, event.sample_frame, event.sample_rate)?.get())
            }
            PendingEvent::Control(event) => Some(event.position.get()),
            PendingEvent::Data(_) => None,
        };
        Ok(
            position
                .is_none_or(|position| stream.state.event_is_ready(BytePosition::new(position))),
        )
    }

    /// Determine the position used to order an event on the active timeline.
    fn event_sort_position(
        event: &PendingEvent,
        stream: Option<&AudioStream>,
    ) -> Result<u128, String> {
        let Some(stream) = stream else {
            return Ok(event.sample_frame() as u128);
        };
        match event {
            PendingEvent::Register(event) => Ok(u128::from(
                Self::position_for_frame(stream, event.sample_frame, event.sample_rate)?.get(),
            )),
            PendingEvent::Psg(event) => Ok(u128::from(
                Self::position_for_frame(stream, event.sample_frame, event.sample_rate)?.get(),
            )),
            PendingEvent::Data(event) => Ok(u128::from(event.position.get())),
            PendingEvent::Control(event) => Ok(u128::from(event.position.get())),
        }
    }

    /// Sort pending events by timeline position and same-position priority.
    fn order_events(
        events: Vec<PendingEvent>,
        stream: Option<&AudioStream>,
    ) -> Result<Vec<PendingEvent>, String> {
        let mut ordered = Vec::with_capacity(events.len());
        for (sequence, event) in events.into_iter().enumerate() {
            let position = Self::event_sort_position(&event, stream)?;
            ordered.push((event.priority(), position, sequence, event));
        }
        ordered.sort_by_key(|(priority, position, sequence, _)| (*priority, *position, *sequence));
        Ok(ordered.into_iter().map(|(_, _, _, event)| event).collect())
    }

    /// Send one non-batched event and update audio-stream bookkeeping.
    fn send_event(&mut self, event: PendingEvent) -> Result<(), String> {
        match event {
            PendingEvent::Register(event) => self.send_registers(&[event]),
            PendingEvent::Psg(event) => self.send_psg(&[event]),
            PendingEvent::Control(event) => {
                let Some(stream) = self.audio_stream.as_ref() else {
                    return Err("NDSIF audio event has no configured OKIM6258 stream".to_owned());
                };
                let instance = stream.instance;
                let position = event.position;
                let is_end = matches!(event.control, NdsifAudioControl::End(_));
                if is_end {
                    stream
                        .state
                        .validate_end_position(position)
                        .map_err(|error| format!("invalid NDSIF END position: {error}"))?;
                } else {
                    stream
                        .state
                        .validate_event_position(position)
                        .map_err(|error| format!("invalid NDSIF event position: {error}"))?;
                }
                let audio_event = match event.control {
                    NdsifAudioControl::Clock { clock_hz, divider } => AudioEvent::Clock {
                        clock: OkiClock::try_from(clock_hz).map_err(|error| error.to_string())?,
                        divider,
                    },
                    NdsifAudioControl::Pan(pan) => AudioEvent::Pan(pan),
                    NdsifAudioControl::End(pair) => AudioEvent::End(pair),
                };
                self.ensure_event_capacity(1)?;
                self.request(Command::AudioEvent {
                    stream_id: AUDIO_STREAM_ID,
                    instance,
                    position,
                    event: audio_event,
                })?;
                let state = &mut self
                    .audio_stream
                    .as_mut()
                    .expect("audio stream was checked above")
                    .state;
                let result = if is_end {
                    state.record_end(position)
                } else {
                    state.record_event_position(position)
                };
                result.map_err(|error| format!("invalid NDSIF event position: {error}"))?;
                self.audio_stream
                    .as_mut()
                    .expect("audio stream was checked above")
                    .record_queued_events(position.get(), 1)
            }
            PendingEvent::Data(_) => {
                Err("ADPCM data must be sent in a contiguous batch".to_owned())
            }
        }
    }

    /// Send a compatible register batch directly or on the audio timeline.
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
        let stream_instance = stream.instance;
        let position_value = position.get();
        stream
            .state
            .validate_event_position(position)
            .map_err(|error| format!("invalid NDSIF register-event position: {error}"))?;
        self.ensure_event_capacity(writes.len())?;
        self.request(Command::AudioEvent {
            stream_id: AUDIO_STREAM_ID,
            instance: stream_instance,
            position,
            event: AudioEvent::Registers {
                write_id: first.write_id,
                instance: first.instance,
                page: first.page,
                writes: &writes,
            },
        })?;
        self.audio_stream
            .as_mut()
            .expect("audio stream was checked above")
            .state
            .record_event_position(position)
            .map_err(|error| format!("invalid NDSIF register-event position: {error}"))?;
        self.audio_stream
            .as_mut()
            .expect("audio stream was checked above")
            .record_queued_events(position_value, writes.len())
    }

    /// Send a compatible batch of raw PSG bytes.
    fn send_psg(&mut self, events: &[NdsifPsgEvent]) -> Result<(), String> {
        let Some(first) = events.first() else {
            return Ok(());
        };
        if self.audio_stream.is_some() {
            return Err("NDSIF raw PSG writes cannot share an audio stream".to_owned());
        }
        let bytes = events.iter().map(|event| event.value).collect::<Vec<_>>();
        self.request(Command::WriteBytes {
            write_id: first.write_id,
            instance: first.instance,
            page: first.page,
            bytes: &bytes,
        })
        .map(|_| ())
    }

    /// Pace untimed device writes against the host playback clock.
    fn pace_without_audio_stream(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        if output_rate == 0 {
            return Err("NDSIF output rate is zero".to_owned());
        }
        let target_nanos = (sample_frame as u128) * 1_000_000_000 / u128::from(output_rate);
        let target_nanos = u64::try_from(target_nanos)
            .map_err(|_| "NDSIF playback pacing duration overflow".to_owned())?;
        let origin = *self.wall_clock_origin.get_or_insert_with(Instant::now);
        let deadline = origin
            .checked_add(Duration::from_nanos(target_nanos))
            .ok_or_else(|| "NDSIF playback pacing deadline overflow".to_owned())?;
        if let Some(delay) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(delay);
        }
        Ok(())
    }
    /// Return the supported maximum ADPCM data batch length.
    fn max_audio_data_batch_len(&self) -> usize {
        let maximum = self.data_capacity.min(ndsif::MAX_AUDIO_DATA_BYTES as u32) as usize;
        maximum.max(1)
    }

    /// Return the supported maximum register-event batch length.
    fn max_register_event_batch_len(&self) -> usize {
        if self.audio_stream.is_some() {
            ndsif::MAX_REGISTER_EVENT_WRITES
                .min(self.event_capacity as usize)
                .max(1)
        } else {
            ndsif::MAX_REGISTER_WRITES
        }
    }

    /// Return the protocol limit for a raw PSG batch.
    fn max_psg_batch_len(&self) -> usize {
        ndsif::MAX_REGISTER_BYTES
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
        if write_id == WriteId::YM2612
            && instance == 0
            && let Some(capabilities) = self.timed_dac_capabilities
        {
            if self.audio_stream.is_some() {
                return Err(
                    "NDSIF timed DAC profile cannot share an OKIM6258 audio stream".to_owned(),
                );
            }
            if self.timed_dac_stream.is_none() {
                self.timed_dac_stream = Some(TimedDacStream::new(capabilities));
            }
        }
        Ok(())
    }

    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Register(event));
        Ok(())
    }

    fn schedule_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Psg(event));
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
        if self.timed_dac_stream.is_some() {
            return Err("NDSIF timed DAC profile cannot share an OKIM6258 audio stream".to_owned());
        }
        if self.data_capacity == 0 || self.event_capacity == 0 {
            return Err("NDSIF device does not advertise audio stream capacity".to_owned());
        }
        let clock = OkiClock::try_from(clock_hz).map_err(|error| error.to_string())?;
        let prebuffer_bytes = audio_prebuffer_bytes(clock as u32, divider, self.data_capacity);
        if self.audio_stream.is_some() {
            return Err("NDSIF transport supports only one OKIM6258 stream".to_owned());
        }
        let state = AudioStreamState::new(AudioTiming::new(clock, divider), output_rate)
            .map_err(|error| format!("invalid NDSIF audio stream configuration: {error}"))?;
        self.audio_stream = Some(AudioStream {
            instance,
            state,
            prebuffer_bytes,
            queued_events: VecDeque::new(),
            queued_event_count: 0,
            last_status_check: None,
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
            && stream.state.output_rate() != output_rate
        {
            return Err("NDSIF event output rate changed during playback".to_owned());
        }
        if self.audio_stream.is_none() && self.timed_dac_stream.is_none() && self.configured_chip {
            self.pace_without_audio_stream(sample_frame, output_rate)?;
        }
        let mut due = Vec::new();
        let mut future = Vec::new();
        let mut deferred = Vec::new();
        for event in self.pending.drain(..) {
            if event.sample_frame() <= sample_frame {
                due.push(event);
            } else {
                future.push(event);
            }
        }

        self.pending = future;
        if self.timed_dac_stream.is_some() {
            self.flush_timed_dac_until(due, sample_frame, output_rate)?;
            self.start_timed_dac_if_ready(false)?;
            self.check_timed_dac_status_if_due()?;
            return Ok(());
        }
        let mut due = Self::order_events(due, self.audio_stream.as_ref())?;
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
                    while index < due.len() && index - start < self.max_register_event_batch_len() {
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
                    if !Self::event_is_ready(&due[start], self.audio_stream.as_ref())? {
                        deferred.extend(due[start..index].iter().cloned());
                        continue;
                    }
                    self.send_registers(&registers)?;
                }
                PendingEvent::Psg(first) => {
                    let start = index;
                    index += 1;
                    while index < due.len()
                        && index - start < self.max_psg_batch_len()
                        && matches!(&due[index], PendingEvent::Psg(next)
                            if next.write_id == first.write_id
                                && next.instance == first.instance
                                && next.page == first.page
                                && next.sample_frame == first.sample_frame
                                && next.sample_rate == first.sample_rate)
                    {
                        index += 1;
                    }
                    let psg_writes = due[start..index]
                        .iter()
                        .map(|event| match event {
                            PendingEvent::Psg(event) => *event,
                            _ => unreachable!("PSG batch contains only PSG events"),
                        })
                        .collect::<Vec<_>>();
                    if !Self::event_is_ready(&due[start], self.audio_stream.as_ref())? {
                        deferred.extend(due[start..index].iter().cloned());
                        continue;
                    }
                    self.send_psg(&psg_writes)?;
                }
                PendingEvent::Data(_) => {
                    let start = index;
                    index += 1;
                    while index < due.len()
                        && matches!(&due[index], PendingEvent::Data(_))
                        && index - start < self.max_audio_data_batch_len()
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
                    if !Self::event_is_ready(&due[index], self.audio_stream.as_ref())? {
                        deferred.push(due[index].clone());
                        index += 1;
                        continue;
                    }
                    let event = due.remove(index);
                    self.send_event(event)?;
                }
            }
        }
        self.pending.extend(deferred);
        self.start_stream_if_ready(false)?;
        self.check_status_if_due()?;
        Ok(())
    }

    fn start_audio_stream(&mut self, _instance: u8) -> Result<(), String> {
        self.start_stream_if_ready(true)
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
        self.start_stream_if_ready(true)
    }

    fn finish_dac_stream(&mut self, sample_frame: usize, output_rate: u32) -> Result<(), String> {
        self.finish_timed_dac_stream(sample_frame, output_rate)
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
