use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use chipstream::chip::{
    ChipError, ClockDivider, NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent,
    NdsifOkim6258, NdsifPsgEvent, NdsifRegisterEvent, NdsifSn76489, NdsifSoundChip, NdsifTransport,
    Okim6258, OutputBits, Sn76496, Sn76496Config, YmfmChip,
};
use chipstream::sequencer::{Chip, ChipFactoryRegistry, Instance};
use ndsif::{
    AudioEvent, AudioStreamState, AudioTiming, BytePosition, ClockMode, Command, Decoder, Divider,
    OkiClock, Reply, Response, Status, WriteId,
};
use serialport::SerialPort;

const AUDIO_STREAM_ID: u8 = 0xb7;
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(1);
const AUDIO_PREBUFFER_SECONDS: u64 = 2;

/// Serial connection and reset options for an NDSIF device.
#[derive(Debug, Clone)]
pub struct NdsifSerialConfig {
    /// Path to the serial device.
    pub port: String,
    /// Serial baud rate.
    pub baud_rate: u32,
    /// Timeout for each request/response transaction.
    pub response_timeout: Duration,
    /// Reset the device before querying capabilities.
    pub reset_device: bool,
}

/// Open configured NDSIF devices and register hardware-backed factories.
///
/// Each chip instance is routed to the first configured device advertising a
/// compatible chip/clock capability. When no device matches, the normal
/// emulated backend is retained. An empty list returns an empty registry.
pub fn create_chip_factories(configs: &[NdsifSerialConfig]) -> Result<ChipFactoryRegistry, String> {
    let mut ports = HashSet::with_capacity(configs.len());
    let mut transports = Vec::with_capacity(configs.len());
    for config in configs {
        if config.port.trim().is_empty() {
            return Err("NDSIF serial port cannot be empty".to_owned());
        }
        if !ports.insert(&config.port) {
            return Err(format!(
                "NDSIF serial port is configured more than once: {}",
                config.port
            ));
        }
        if config.baud_rate == 0 {
            return Err(format!(
                "NDSIF baud rate cannot be zero for {}",
                config.port
            ));
        }
        if config.response_timeout.is_zero() {
            return Err(format!(
                "NDSIF response timeout cannot be zero for {}",
                config.port
            ));
        }
        transports.push(Rc::new(RefCell::new(
            NdsifSerialTransport::open(
                &config.port,
                config.baud_rate,
                config.response_timeout,
                config.reset_device,
            )
            .map_err(|error| format!("NDSIF port {}: {error}", config.port))?,
        )));
    }

    let transports = Rc::new(transports);
    let mut factories = ChipFactoryRegistry::new();

    for chip in [
        Chip::Ym2413,
        Chip::Ym2151,
        Chip::Ym2203,
        Chip::Ym2608,
        Chip::Ym2610b,
        Chip::Ym2612,
        Chip::Ym3526,
        Chip::Y8950,
        Chip::Ym3812,
        Chip::Ymf262,
        Chip::Ymf278b,
    ] {
        let Some(write_id) = ymfm_write_id(&chip) else {
            continue;
        };
        let registered_chip = chip.clone();
        let chip_transports = Rc::clone(&transports);
        factories.register(chip, move |config| {
            let instance = instance_number(config.instance());
            let oki_clock = if registered_chip == Chip::Ym2151 {
                config.header().okim6258_clock & 0x3fff_ffff
            } else {
                0
            };
            let oki_timing_clock = (oki_clock != 0).then_some(oki_clock);
            let transport = find_transport(
                &chip_transports,
                &registered_chip,
                write_id,
                instance,
                config.clock(),
                oki_timing_clock,
            )?;
            if let Some(transport) = transport {
                Ok(Box::new(NdsifSoundChip::new(config, transport)?))
            } else {
                Ok(Box::new(YmfmChip::new(
                    registered_chip.clone(),
                    config.clock(),
                )?))
            }
        });
    }

    let psg_transports = Rc::clone(&transports);
    factories.register(Chip::Sn76489, move |config| {
        let flags = &config.header().sn76489_flags;
        if config.header().misc().t6w28_detected == Some(true) {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "T6W28 PSG is not supported",
            });
        }
        let write_id = match config.instance() {
            Instance::Primary => WriteId::SN76489,
            Instance::Secondary => WriteId::SN76489_SECOND,
        };
        let instance = instance_number(config.instance());
        let transport = if flags.gamegear_stereo {
            None
        } else {
            find_transport(
                &psg_transports,
                &Chip::Sn76489,
                write_id,
                instance,
                config.clock(),
                None,
            )?
        };
        if let Some(transport) = transport {
            return Ok(Box::new(NdsifSn76489::new(config, transport)?));
        }

        let feedback = u16::from(config.header().sn76489_feedback);
        let width = u8::from(config.header().sn76489_shift_register_width);
        Ok(Box::new(Sn76496::with_config(
            config.clock(),
            Sn76496Config {
                feedback: if feedback == 0 { 0x0009 } else { feedback },
                shift_width: if width == 0 { 16 } else { width },
                negate: flags.output_negate,
                stereo: flags.gamegear_stereo,
                divider_8: flags.clock_divider_8,
                frequency_zero_is_1024: flags.frequency_0_is_400,
                xnor: flags.xnor_noise_mode,
            },
        )?))
    });

    let oki_transports = Rc::clone(&transports);
    factories.register(Chip::Okim6258, move |config| {
        let instance = instance_number(config.instance());
        if let Some(transport) = find_transport(
            &oki_transports,
            &Chip::Okim6258,
            WriteId::OKIM6258,
            instance,
            config.clock(),
            Some(config.clock()),
        )? {
            return Ok(Box::new(NdsifOkim6258::new(config, transport)?));
        }

        let flags = &config.header().okim6258_flags;
        let mut chip = Okim6258::new_for_vgm(config.clock())?;
        chip.set_divider(match flags.clock_divider {
            0 => ClockDivider::Div1024,
            1 => ClockDivider::Div768,
            _ => ClockDivider::Div512,
        })?;
        chip.set_output_bits(if flags.output_12bit {
            OutputBits::Bits12
        } else {
            OutputBits::Bits10
        });
        Ok(Box::new(chip))
    });

    Ok(factories)
}

fn find_transport(
    transports: &[Rc<RefCell<NdsifSerialTransport>>],
    chip: &Chip,
    write_id: WriteId,
    instance: u8,
    clock_hz: u32,
    oki_timing_clock: Option<u32>,
) -> Result<Option<Rc<RefCell<NdsifSerialTransport>>>, ChipError> {
    for transport in transports {
        let device = transport.try_borrow().map_err(|_| ChipError::External {
            chip: chip.clone(),
            message: "NDSIF transport is already borrowed".to_owned(),
        })?;
        if !device.supports_clock(write_id, instance, clock_hz) {
            continue;
        }
        if oki_timing_clock.is_some_and(|clock| {
            !device.supports_clock(WriteId::OKIM6258, 0, clock)
                || !device.supports_audio_stream(0, clock)
        }) {
            continue;
        }
        return Ok(Some(Rc::clone(transport)));
    }
    Ok(None)
}

fn ymfm_write_id(chip: &Chip) -> Option<WriteId> {
    let write_id = match chip {
        Chip::Ym2413 => 0x51,
        Chip::Ym2151 => 0x54,
        Chip::Ym2203 => 0x55,
        Chip::Ym2608 => 0x56,
        Chip::Ym2610b => 0x58,
        Chip::Ym2612 => 0x52,
        Chip::Ym3526 => 0x5b,
        Chip::Y8950 => 0x5c,
        Chip::Ym3812 => 0x5a,
        Chip::Ymf262 => 0x5e,
        Chip::Ymf278b => 0xd0,
        _ => return None,
    };
    WriteId::try_from(write_id).ok()
}

fn instance_number(instance: Instance) -> u8 {
    match instance {
        Instance::Primary => 0,
        Instance::Secondary => 1,
    }
}

struct AudioStream {
    instance: u8,
    state: AudioStreamState,
    prebuffer_bytes: u32,
}

fn audio_prebuffer_bytes(clock_hz: u32, divider: Divider, capacity: u32) -> u32 {
    let safe_capacity = capacity
        .saturating_sub(ndsif::MAX_AUDIO_DATA_BYTES as u32)
        .max(1);
    (u64::from(clock_hz) * AUDIO_PREBUFFER_SECONDS / (u64::from(divider as u32) * 2))
        .min(u64::from(safe_capacity)) as u32
}

#[derive(Clone)]
enum PendingEvent {
    Register(NdsifRegisterEvent),
    Psg(NdsifPsgEvent),
    Data(NdsifAdpcmEvent),
    Control(NdsifAudioControlEvent),
}

impl PendingEvent {
    fn sample_frame(&self) -> usize {
        match self {
            Self::Register(event) => event.sample_frame,
            Self::Psg(event) => event.sample_frame,
            Self::Data(event) => event.sample_frame,
            Self::Control(event) => event.sample_frame,
        }
    }

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

pub struct NdsifSerialTransport {
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
    queued_events: VecDeque<(u32, u32)>,
    queued_event_count: u32,
    last_status_check: Option<Instant>,
    wall_clock_origin: Option<Instant>,
}

impl NdsifSerialTransport {
    pub fn open(
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
            queued_events: VecDeque::new(),
            queued_event_count: 0,
            last_status_check: None,
            wall_clock_origin: None,
        };
        if reset_device {
            transport.request(Command::Reset)?;
        }
        transport.request(Command::Ping(b"chipstream"))?;
        transport.load_capabilities()?;
        transport.verify_audio_stream_idle()?;
        Ok(transport)
    }

    pub fn supports_clock(&self, write_id: WriteId, instance: u8, clock_hz: u32) -> bool {
        self.capability(write_id, instance)
            .is_some_and(|capability| match capability.clock_mode {
                ClockMode::Variable => true,
                ClockMode::Fixed => capability.fixed_clock_hz == clock_hz,
                ClockMode::Unknown => false,
            })
    }

    pub fn supports_audio_stream(&self, instance: u8, clock_hz: u32) -> bool {
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
        stream
            .state
            .position_for_frame(sample_frame, output_rate)
            .map_err(|error| format!("invalid NDSIF audio position: {error}"))
    }

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
                self.record_queued_events(position.get(), 1)
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
        self.record_queued_events(position_value, writes.len())
    }

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
        if position != stream.state.accepted() {
            return Err(format!(
                "NDSIF expected ADPCM position {}, got {}",
                stream.state.accepted().get(),
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
        self.ensure_audio_data_capacity(data.len())?;
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
        stream
            .state
            .record_data_accepted(position, data.len())
            .map_err(|error| format!("invalid accepted NDSIF audio position: {error}"))?;
        Ok(())
    }

    fn ensure_audio_data_capacity(&mut self, incoming_bytes: usize) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let Some((instance, accepted, played, started)) =
                self.audio_stream.as_ref().map(|stream| {
                    (
                        stream.instance,
                        stream.state.accepted().get(),
                        stream.state.played().get(),
                        stream.state.started(),
                    )
                })
            else {
                return Err("NDSIF ADPCM data has no configured stream".to_owned());
            };
            let pending = accepted.saturating_sub(played);
            if u64::from(pending) + incoming_bytes as u64 <= u64::from(self.data_capacity) {
                return Ok(());
            }
            if !started {
                return Err(format!(
                    "NDSIF prebuffer exceeds device capacity (pending={pending}, incoming={incoming_bytes}, capacity={})",
                    self.data_capacity
                ));
            }
            let status = self.query_audio_status(instance)?;
            self.update_audio_progress(status)?;
            let pending = accepted.saturating_sub(status.played);
            if u64::from(pending) + incoming_bytes as u64 <= u64::from(self.data_capacity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF audio buffer capacity (pending={pending}, incoming={incoming_bytes}, capacity={})",
                    self.data_capacity
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn query_audio_status(&mut self, instance: u8) -> Result<ndsif::AudioStatus, String> {
        let frame = self.request(Command::AudioStatus {
            stream_id: AUDIO_STREAM_ID,
            instance,
        })?;
        let response = Response::decode(&frame).map_err(|error| error.to_string())?;
        match response.reply {
            Reply::AudioStatus(status) if !status.flags.fault() => Ok(status),
            Reply::AudioStatus(status) => Err(format!(
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
            )),
            _ => Err("NDSIF AUDIO_STATUS returned an unexpected response".to_owned()),
        }
    }

    fn ensure_event_capacity(&mut self, incoming_events: usize) -> Result<(), String> {
        let incoming_events =
            u32::try_from(incoming_events).map_err(|_| "NDSIF event count overflow".to_owned())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.prune_queued_events();
            if self.queued_event_count + incoming_events <= u32::from(self.event_capacity) {
                return Ok(());
            }
            let Some((instance, accepted, started)) = self.audio_stream.as_ref().map(|stream| {
                (
                    stream.instance,
                    stream.state.accepted().get(),
                    stream.state.started(),
                )
            }) else {
                return Err("NDSIF audio event has no configured stream".to_owned());
            };
            if !started {
                if accepted == 0 {
                    return Err(
                        "NDSIF event capacity exhausted before audio data was queued".to_owned(),
                    );
                }
                self.start_stream_if_ready(true)?;
            }
            let status = self.query_audio_status(instance)?;
            self.update_audio_progress(status)?;
            if self.queued_event_count + incoming_events <= u32::from(self.event_capacity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF event capacity (queued={}, incoming={}, capacity={})",
                    self.queued_event_count, incoming_events, self.event_capacity
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn record_queued_events(&mut self, position: u32, count: usize) -> Result<(), String> {
        let count = u32::try_from(count).map_err(|_| "NDSIF event count overflow".to_owned())?;
        self.queued_event_count = self
            .queued_event_count
            .checked_add(count)
            .ok_or_else(|| "NDSIF queued event count overflow".to_owned())?;
        self.queued_events.push_back((position, count));
        Ok(())
    }

    fn prune_queued_events(&mut self) {
        let played = self
            .audio_stream
            .as_ref()
            .map_or(0, |stream| stream.state.played().get());
        while self
            .queued_events
            .front()
            .is_some_and(|(position, _)| *position <= played)
        {
            let (_, count) = self.queued_events.pop_front().expect("front was checked");
            self.queued_event_count = self.queued_event_count.saturating_sub(count);
        }
    }

    fn update_audio_progress(&mut self, status: ndsif::AudioStatus) -> Result<(), String> {
        if let Some(stream) = self.audio_stream.as_mut() {
            stream
                .state
                .update_status(status)
                .map_err(|error| format!("invalid NDSIF audio status: {error}"))?;
        }
        self.prune_queued_events();
        Ok(())
    }

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
    fn start_stream_if_ready(&mut self, force: bool) -> Result<(), String> {
        let Some((instance, clock, divider)) = self.audio_stream.as_ref().and_then(|stream| {
            stream
                .state
                .should_start(stream.prebuffer_bytes, force)
                .then_some((
                    stream.instance,
                    stream.state.timing().clock(),
                    stream.state.timing().divider(),
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
        let status = self.query_audio_status(instance)?;
        if !status.flags.running() {
            return Err(format!(
                "NDSIF audio stream did not enter the running state (accepted={}, played={}, pending={}, underflows={}, overflows={}, rejected={}, max_pending={}, flags={:?})",
                status.accepted,
                status.played,
                status.pending,
                status.underflows,
                status.overflows,
                status.rejected,
                status.max_pending,
                status.flags
            ));
        }
        self.update_audio_progress(status)?;
        self.last_status_check = Some(Instant::now());
        Ok(())
    }

    fn check_status_if_due(&mut self) -> Result<(), String> {
        let Some(instance) = self.audio_stream.as_ref().and_then(|stream| {
            (stream.state.started()
                && self
                    .last_status_check
                    .is_some_and(|last| last.elapsed() >= STATUS_POLL_INTERVAL))
            .then_some(stream.instance)
        }) else {
            return Ok(());
        };
        let status = self.query_audio_status(instance)?;
        self.update_audio_progress(status)?;
        self.last_status_check = Some(Instant::now());
        Ok(())
    }

    fn max_audio_data_batch_len(&self) -> usize {
        let maximum = self.data_capacity.min(ndsif::MAX_AUDIO_DATA_BYTES as u32) as usize;
        maximum.max(1)
    }

    fn max_register_event_batch_len(&self) -> usize {
        if self.audio_stream.is_some() {
            ndsif::MAX_REGISTER_EVENT_WRITES
                .min(self.event_capacity as usize)
                .max(1)
        } else {
            ndsif::MAX_REGISTER_WRITES
        }
    }

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
        if self.audio_stream.is_none() && self.configured_chip {
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
    fn maps_supported_ymfm_chips_to_ndsif_write_ids() {
        for (chip, expected) in [
            (Chip::Ym2413, 0x51),
            (Chip::Ym2151, 0x54),
            (Chip::Ym2203, 0x55),
            (Chip::Ym2608, 0x56),
            (Chip::Ym2610b, 0x58),
            (Chip::Ym2612, 0x52),
            (Chip::Ym3526, 0x5b),
            (Chip::Y8950, 0x5c),
            (Chip::Ym3812, 0x5a),
            (Chip::Ymf262, 0x5e),
            (Chip::Ymf278b, 0xd0),
        ] {
            assert_eq!(ymfm_write_id(&chip).map(WriteId::get), Some(expected));
        }
        assert_eq!(ymfm_write_id(&Chip::Sn76489), None);
    }

    fn stream_with_data(accepted: usize) -> AudioStream {
        let mut state =
            AudioStreamState::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();
        let mut position = BytePosition::new(0);
        let mut remaining = accepted;
        while remaining > 0 {
            let count = remaining.min(ndsif::MAX_AUDIO_DATA_BYTES);
            state.record_data_accepted(position, count).unwrap();
            position = position.checked_advance(count).unwrap();
            remaining -= count;
        }
        AudioStream {
            instance: 0,
            state,
            prebuffer_bytes: 4096,
        }
    }

    #[test]
    fn audio_prebuffer_respects_device_capacity() {
        assert_eq!(
            audio_prebuffer_bytes(4_000_000, Divider::Div1024, 8192),
            3906
        );
        assert_eq!(audio_prebuffer_bytes(4_000_000, Divider::Div1024, 512), 262);
    }

    #[test]
    fn audio_data_is_sent_before_time_positioned_events() {
        let control = PendingEvent::Control(NdsifAudioControlEvent {
            instance: 0,
            position: BytePosition::new(0),
            sample_frame: 0,
            sample_rate: 48_000,
            control: NdsifAudioControl::Pan(ndsif::Pan::Both),
        });
        let data = PendingEvent::Data(NdsifAdpcmEvent {
            instance: 0,
            position: BytePosition::new(0),
            sample_frame: 1_024,
            sample_rate: 48_000,
            value: 0,
        });
        let events =
            NdsifSerialTransport::order_events(vec![control, data], Some(&stream_with_data(0)))
                .unwrap();

        assert!(matches!(events[0], PendingEvent::Data(_)));
        assert!(matches!(events[1], PendingEvent::Control(_)));
    }

    #[test]
    fn audio_events_are_ordered_by_byte_position() {
        let stream = stream_with_data(4);
        let later = PendingEvent::Control(NdsifAudioControlEvent {
            instance: 0,
            position: BytePosition::new(2),
            sample_frame: 0,
            sample_rate: 44_100,
            control: NdsifAudioControl::Pan(ndsif::Pan::Both),
        });
        let earlier = PendingEvent::Register(NdsifRegisterEvent {
            chip: soundlog::chip::Chip::Ym2151,
            write_id: WriteId::YM2151,
            instance: 0,
            page: 0,
            write: ndsif::RegisterWrite::new(0x20, 0xc7),
            sample_frame: 0,
            sample_rate: 44_100,
        });

        let events =
            NdsifSerialTransport::order_events(vec![later, earlier], Some(&stream)).unwrap();
        assert!(matches!(events[0], PendingEvent::Register(_)));
        assert!(matches!(events[1], PendingEvent::Control(_)));
    }

    #[test]
    fn timed_events_wait_until_the_audio_position_is_accepted() {
        let stream = stream_with_data(0);
        let control = PendingEvent::Control(NdsifAudioControlEvent {
            instance: 0,
            position: BytePosition::new(1),
            sample_frame: 0,
            sample_rate: 44_100,
            control: NdsifAudioControl::Pan(ndsif::Pan::Both),
        });

        assert!(!NdsifSerialTransport::event_is_ready(&control, Some(&stream)).unwrap());

        let stream = stream_with_data(1);
        assert!(NdsifSerialTransport::event_is_ready(&control, Some(&stream)).unwrap());
    }
}
