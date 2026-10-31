//! VGM command playback, chip routing, and stereo audio rendering.

mod timing;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use soundlog::chip::{Chip, PwmSpec};
use soundlog::mdx::convert::MdxLoopMarker;
use soundlog::vgm::command::{Instance, VgmCommand};
use soundlog::vgm::detail::{DataBlockType, parse_data_block};
use soundlog::vgm::header::{ChipId, VgmExtraHeader};
use soundlog::vgm::parser::visit_non_data_block_commands;
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::{VGM_SAMPLE_RATE, VgmDocument, VgmHeader, VgmStream};
use ymfm_sys::{InterfaceHandler, ffi::AccessClass};

use crate::audio::{
    AudioError, ChipRoute, RouteError, StereoBlock, StereoMatrix, StereoMixer, StereoResampler,
    StereoResamplerState,
};
#[cfg(feature = "okim6295")]
use crate::chip::Okim6295;
#[cfg(feature = "pwm")]
use crate::chip::Pwm;
#[cfg(feature = "segapcm")]
use crate::chip::SegaPcm;
#[cfg(feature = "c140")]
use crate::chip::{C140, C140Type};
use crate::chip::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, SoundChipState, YmfmChip};
#[cfg(feature = "okim6258")]
use crate::chip::{ClockDivider, Okim6258, OutputBits};
#[cfg(feature = "sn76496")]
use crate::chip::{Sn76496, Sn76496Config};
use crate::fixed::{advance, divide_ratio};

use super::factory::{ChipFactoryConfig, ChipFactoryRegistry};
use super::{compat::PwmCompatibility, event, volume};
use event::{CallbackRegistry, PendingWriteEvent, VgmStateTracker};
use timing::{Timing, TimingProfile};

const WAIT_STEP: u32 = 256;
const SRC_CHUNK: usize = 256;
const MAX_ROM_BYTES: usize = 64 * 1024 * 1024;
const MAX_PENDING_WRITES: usize = 1_048_576;
const SCALE: f32 = 1.0 / 32768.0;
const PHASE_SCALE: f32 = 1.0 / 4_294_967_296.0;

pub(super) type StreamFactory = Box<dyn Fn() -> Result<(VgmStream, Option<MdxLoopMarker>), String>>;

#[derive(Clone)]
enum StreamSetting {
    LoopCount(Option<u32>),
    LoopModifier(u8),
    LoopBase(i8),
    FadeoutSamples(Option<u32>),
}

macro_rules! feature_backend {
    ($feature:literal, $chip:expr, $body:expr) => {{
        #[cfg(feature = $feature)]
        {
            $body
        }
        #[cfg(not(feature = $feature))]
        {
            return Err(VgmSequencerError::FeatureDisabled {
                chip: $chip.clone(),
                feature: $feature,
            });
        }
    }};
}

/// Failure to initialize or render a VGM stream.
#[derive(Debug)]
pub enum VgmSequencerError {
    InvalidChunkSize,
    InvalidSampleRate,
    InvalidGain,
    ChipNotFound {
        chip: Chip,
        instance: Instance,
    },
    FeatureDisabled {
        chip: Chip,
        feature: &'static str,
    },
    UnsupportedOkiAdpcm,
    InvalidRomBlock,
    RomTooLarge,
    UnexpectedEnd,
    StreamFailed,
    DurationOverflow,
    PwmCompatibilityConfigurationAfterPlayback,
    FadeoutConfigurationAfterPlayback,
    CheckpointMismatch,
    StreamReconstruction(String),
    SeekTargetBeforeCheckpoint {
        target: usize,
        checkpoint: usize,
    },
    SeekTargetBeyondEnd {
        target: usize,
        reached: usize,
    },
    SeekCancelled,
    EventQueueFull {
        limit: usize,
    },
    WriteQueueFull {
        chip: Chip,
        instance: Instance,
        limit: usize,
    },
    Parse(soundlog::ParseError),
    Chip(ChipError),
    Route(RouteError),
    Audio(AudioError),
}

impl fmt::Display for VgmSequencerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChunkSize => write!(f, "output chunk size must be nonzero"),
            Self::InvalidSampleRate => write!(f, "output sample rate must be nonzero"),
            Self::InvalidGain => write!(f, "gain must be finite and nonnegative"),
            Self::ChipNotFound { chip, instance } => {
                write!(f, "no playable {chip:?} {instance:?} chip")
            }
            Self::FeatureDisabled { chip, feature } => {
                write!(
                    f,
                    "{chip:?} playback requires the chipstream feature '{feature}'"
                )
            }
            Self::UnsupportedOkiAdpcm => write!(f, "OKIM6258 3-bit ADPCM is not implemented"),
            Self::InvalidRomBlock => write!(f, "invalid VGM ROM/RAM block"),
            Self::RomTooLarge => write!(f, "VGM chip memory exceeds the supported size"),
            Self::UnexpectedEnd => write!(f, "VGM command stream ended without an end command"),
            Self::StreamFailed => write!(f, "sequencer cannot continue after a rendering error"),
            Self::DurationOverflow => write!(f, "VGM playback duration exceeds supported limits"),
            Self::PwmCompatibilityConfigurationAfterPlayback => {
                write!(f, "PWM compatibility must be configured before rendering")
            }
            Self::FadeoutConfigurationAfterPlayback => {
                write!(f, "fadeout must be configured before rendering")
            }
            Self::CheckpointMismatch => {
                write!(
                    f,
                    "checkpoint does not belong to this sequencer configuration"
                )
            }
            Self::StreamReconstruction(error) => {
                write!(
                    f,
                    "failed to reconstruct command stream for checkpoint: {error}"
                )
            }
            Self::SeekTargetBeforeCheckpoint { target, checkpoint } => write!(
                f,
                "seek target {target} precedes checkpoint position {checkpoint}"
            ),
            Self::SeekTargetBeyondEnd { target, reached } => {
                write!(
                    f,
                    "seek target {target} is beyond end of stream at {reached}"
                )
            }
            Self::SeekCancelled => write!(f, "seek was cancelled"),
            Self::EventQueueFull { limit } => {
                write!(f, "VGM pending event limit exceeded ({limit})")
            }
            Self::WriteQueueFull {
                chip,
                instance,
                limit,
            } => {
                write!(
                    f,
                    "VGM pending writes for {chip:?} {instance:?} exceed {limit}"
                )
            }
            Self::Parse(error) => write!(f, "VGM parsing failed: {error}"),
            Self::Chip(error) => write!(f, "chip failed: {error}"),
            Self::Route(error) => write!(f, "chip capture failed: {error}"),
            Self::Audio(error) => write!(f, "audio processing failed: {error}"),
        }
    }
}

impl Error for VgmSequencerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
            Self::Chip(e) => Some(e),
            Self::Route(e) => Some(e),
            Self::Audio(e) => Some(e),
            _ => None,
        }
    }
}

impl From<soundlog::ParseError> for VgmSequencerError {
    fn from(e: soundlog::ParseError) -> Self {
        Self::Parse(e)
    }
}
impl From<ChipError> for VgmSequencerError {
    fn from(e: ChipError) -> Self {
        Self::Chip(e)
    }
}
impl From<RouteError> for VgmSequencerError {
    fn from(e: RouteError) -> Self {
        Self::Route(e)
    }
}
impl From<AudioError> for VgmSequencerError {
    fn from(e: AudioError) -> Self {
        Self::Audio(e)
    }
}

#[derive(Clone, Default)]
struct Memory {
    adpcm_a: Vec<u8>,
    adpcm_b: Vec<u8>,
    pcm: Vec<u8>,
    writes: HashMap<(u8, u32), u8>,
}

impl Memory {
    fn key(access: AccessClass) -> Option<u8> {
        match access {
            AccessClass::AdpcmA => Some(0),
            AccessClass::AdpcmB => Some(1),
            AccessClass::Pcm => Some(2),
            _ => None,
        }
    }

    fn get(&self, access: AccessClass) -> Option<&Vec<u8>> {
        match access {
            AccessClass::AdpcmA => Some(&self.adpcm_a),
            AccessClass::AdpcmB => Some(&self.adpcm_b),
            AccessClass::Pcm => Some(&self.pcm),
            _ => None,
        }
    }
    fn get_mut(&mut self, access: AccessClass) -> Option<&mut Vec<u8>> {
        match access {
            AccessClass::AdpcmA => Some(&mut self.adpcm_a),
            AccessClass::AdpcmB => Some(&mut self.adpcm_b),
            AccessClass::Pcm => Some(&mut self.pcm),
            _ => None,
        }
    }
}

struct VoiceConfig {
    chip: Chip,
    instance: Instance,
    clock: u32,
    dual: bool,
}

struct Voice {
    chip: Chip,
    instance: Instance,
    clock: u32,
    tracker: Option<event::Tracker>,
    route: ChipRoute<Box<dyn SoundChip>>,
    matrix: StereoMatrix,
    resampler: StereoResampler,
    memory: Rc<RefCell<Memory>>,
    phase: u32,
    phase_rate: Option<SampleRate>,
    phase_step: u32,
    queue: VecDeque<[f32; 2]>,
    gain: f32,
    main_coefficient: f32,
    paired_coefficient: f32,
    paced_writes: Option<VecDeque<ChipRegisterWrite>>,
    write_interval: usize,
    write_cooldown: usize,
}

struct EventContext<'a, 'callbacks> {
    callbacks: &'a CallbackRegistry<'callbacks>,
    events: &'a mut VecDeque<(usize, PendingWriteEvent)>,
}

#[derive(Clone)]
struct Fade {
    pending: usize,
    length: f32,
    rate: u32,
    seconds: u32,
    frames: u32,
}

impl Fade {
    fn process(&mut self, frames: &mut [[f32; 2]]) {
        let divisor = self.length - 1.0;
        for frame in frames {
            if self.pending > 0 {
                self.pending -= 1;
            } else {
                let position = self.seconds as f32 * self.rate as f32 + self.frames as f32;
                let gain = if self.length <= 1.0 {
                    0.0
                } else {
                    ((divisor - position) / divisor).clamp(0.0, 1.0)
                };
                frame[0] *= gain;
                frame[1] *= gain;
                if self.frames == self.rate - 1 {
                    self.seconds += 1;
                    self.frames = 0;
                } else {
                    self.frames += 1;
                }
            }
        }
    }
}

impl Voice {
    fn apply_write(
        &mut self,
        write: ChipRegisterWrite,
        event_position: usize,
        sample_frame: usize,
        output_rate: u32,
        context: &mut EventContext<'_, '_>,
    ) -> Result<(), VgmSequencerError> {
        let (bank, register, value) = write.parts();
        let pending_write = context
            .callbacks
            .has_callback(&write)
            .then(|| write.clone());
        self.route
            .chip_mut()
            .apply_register_write_at(write, sample_frame, output_rate)?;
        let state_events = if let Some(tracker) = self.tracker.as_mut() {
            let narrow = u8::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
                chip: self.chip.clone(),
                register,
                value,
            })?;
            tracker
                .on_register_write(bank, register, narrow)
                .unwrap_or_default()
                .into()
        } else {
            None
        };
        if let Some(write) = pending_write {
            if context.events.len() >= event::MAX_PENDING_EVENTS {
                return Err(VgmSequencerError::EventQueueFull {
                    limit: event::MAX_PENDING_EVENTS,
                });
            }
            let index = context
                .events
                .partition_point(|(offset, _)| *offset <= event_position);
            context.events.insert(
                index,
                (
                    event_position,
                    PendingWriteEvent {
                        instance: self.instance,
                        write,
                        state_events,
                    },
                ),
            );
        }
        Ok(())
    }

    fn render(
        &mut self,
        ticks: u32,
        output_rate: u32,
        output_phase: u32,
        available: usize,
        sample_frame_origin: usize,
        context: &mut EventContext<'_, '_>,
    ) -> Result<(), VgmSequencerError> {
        let rate = self.route.chip().sample_rate_ratio();
        let initial_phase = self.phase;
        let had_fraction = self.phase != 0;
        if self.phase_rate != Some(rate) {
            self.phase_step =
                divide_ratio(rate.numerator(), rate.denominator().get(), VGM_SAMPLE_RATE)
                    .ok_or(VgmSequencerError::DurationOverflow)?
                    .1;
            self.phase_rate = Some(rate);
        }
        let whole_step = rate.numerator() / rate.denominator().get() / VGM_SAMPLE_RATE;
        let mut frames = 0_usize;
        for _ in 0..ticks {
            frames = frames
                .checked_add(
                    advance(&mut self.phase, whole_step, self.phase_step)
                        .ok_or(VgmSequencerError::DurationOverflow)? as usize,
                )
                .ok_or(VgmSequencerError::DurationOverflow)?;
        }
        // Generate the first sample at the beginning of each chip timeline.
        frames = frames
            .checked_add(usize::from(self.phase != 0))
            .and_then(|value| value.checked_sub(usize::from(had_fraction)))
            .ok_or(VgmSequencerError::DurationOverflow)?;
        if frames == 0 {
            return Ok(());
        }
        let mut rendered = 0;
        while rendered < frames {
            if self.write_cooldown != 0 {
                let count = self.write_cooldown.min(frames - rendered);
                self.capture(count)?;
                self.write_cooldown -= count;
                rendered += count;
                continue;
            }
            let Some(write) = self.paced_writes.as_mut().and_then(VecDeque::pop_front) else {
                break;
            };
            let fraction = initial_phase.wrapping_neg() as f32 * PHASE_SCALE;
            let offset =
                (rendered as f32 + fraction) * rate.denominator().get() as f32 * output_rate as f32
                    / rate.numerator() as f32
                    + output_phase as f32 * PHASE_SCALE;
            if !offset.is_finite() || offset >= 4_294_967_296.0 {
                return Err(VgmSequencerError::DurationOverflow);
            }
            let relative_position = available
                .checked_add(offset as u32 as usize)
                .ok_or(VgmSequencerError::DurationOverflow)?;
            let sample_frame = sample_frame_origin
                .checked_add(relative_position)
                .ok_or(VgmSequencerError::DurationOverflow)?;
            let event_position = if context.callbacks.has_callback(&write) {
                relative_position
            } else {
                0
            };
            self.apply_write(write, event_position, sample_frame, output_rate, context)?;
            self.write_cooldown = self.write_interval;
        }
        let remaining = frames - rendered;
        self.capture(remaining)?;
        self.write_cooldown = self.write_cooldown.saturating_sub(remaining);
        Ok(())
    }

    fn capture(&mut self, frames: usize) -> Result<(), VgmSequencerError> {
        if frames == 0 {
            return Ok(());
        }
        let samples = frames
            .checked_mul(self.route.chip().channels())
            .ok_or(VgmSequencerError::DurationOverflow)?;
        let mut raw = vec![0; samples];
        let native = self.route.capture(&mut raw)?;
        let stereo = self.matrix.convert(&native)?;
        self.queue
            .extend(self.resampler.process(&stereo)?.frames().iter().copied());
        Ok(())
    }
}

/// A VGM player that returns stereo f32 PCM blocks without an audio device.
///
/// The stream is played once by default. Rendering uses lookahead and may
/// consume commands beyond the returned block.
///
/// Writes and waits are scheduled at 44,100 Hz. `soundlog::VgmStream`
/// handles DAC streams, compressed sample blocks, and loops. The header
/// selects YMFM chips, OKIM6258/6295, SN76489/SN76496, SegaPCM, PWM, and
/// C140/219. The non-YMFM chips require their corresponding Cargo features;
/// a chip declared in the header without its feature returns
/// [`VgmSequencerError::FeatureDisabled`]. Absent or unimplemented chips are
/// silent. Both YM2610 variants,
/// dual-chip flags, per-instance extra-header clocks, YM2203 SSG stereo masks,
/// OKIM6258 pan and rate changes, SN76489 header settings and Game Gear
/// stereo, SegaPCM banking and ROM, OKIM6295 ROM banking and rate changes,
/// C140 board types and ROM, and supported Yamaha ROM blocks are handled.
/// T6W28 PSG returns an error. The OKIM6258 3-bit selection flag is accepted
/// using the 4-bit compatibility decoder, matching the bundled vgm2wav MAME
/// behavior; native 3-bit ADPCM decoding is not implemented.
///
/// Register write pacing is determined by the device timing profile. YMFM
/// writes are queued per chip instance and applied in order, at most one per
/// native sample. YM2203/YM2608 and YM2610(B) use intervals of 18 and 9
/// native samples respectively so that their oversampled FM engines advance
/// between writes. A burst can therefore delay subsequent writes; exceeding
/// 1,048,576 pending writes per instance is an error.
/// Paced write event positions use `f32` arithmetic and may differ from
/// exact rational arithmetic due to rounding.
/// At the end of playback, pending writes are applied at the final boundary
/// without generating additional audio.
/// Devices with variable clocks use a conservative range derived from their
/// timing profile and the stream's clock writes to configure resampling.
///
/// The global volume modifier and extra-header chip volumes multiply the
/// per-chip gain. Relative volumes multiply the existing level; absolute
/// volumes are normalized against libvgm defaults, halved for dual-chip files.
/// Thus absolute levels need not match other players. Separate paired SSG
/// volumes are supported for YM2203, YM2608, and YM2610(B); paired entries
/// for other chips are ignored. The first matching entry is used. Single-chip
/// reference levels are `0x200` for YM2413, `0x1c0` for OKIM6258, `0x180`
/// for SegaPCM, `0xe0` for PWM, `0x80` for SN76489, YM2608 and YM2610(B)
/// and their SSG, `0x80` for YM2203 SSG, and `0x100` for other outputs.
///
/// PCM is not clipped or normalized. Rendering can allocate and is not
/// guaranteed real-time safe. After a rendering error, create a new sequencer.
pub struct VgmSequencer<'a> {
    stream: VgmStream,
    stream_factory: StreamFactory,
    stream_steps: usize,
    stream_settings: Vec<(usize, StreamSetting)>,
    voices: Vec<Voice>,
    output_rate: u32,
    chunk_frames: usize,
    position_frames: usize,
    wait: u32,
    available: usize,
    output_phase: u32,
    output_step: u32,
    ended: bool,
    failed: bool,
    started: bool,
    callbacks: CallbackRegistry<'a>,
    events: VecDeque<(usize, PendingWriteEvent)>,
    pending_output: VecDeque<[f32; 2]>,
    pwm_compatibility: Option<[PwmCompatibility; 2]>,
    fade_samples: Option<u32>,
    fade_remaining: Option<u32>,
    fade_frames: Option<Fade>,
    mdx_loop_marker: Option<MdxLoopMarker>,
    source_identity: Arc<()>,
}

/// Exact sequencer state captured at an output-block boundary.
///
/// A checkpoint can be restored repeatedly on the sequencer that created it.
/// Restoring rebuilds the command stream by replaying commands only; chip
/// emulation and audio rendering are not repeated for the checkpoint prefix.
pub struct VgmSequencerCheckpoint {
    source_identity: Arc<()>,
    output_rate: u32,
    chunk_frames: usize,
    stream_steps: usize,
    stream_settings: Vec<(usize, StreamSetting)>,
    voices: Vec<VoiceCheckpoint>,
    memory: Vec<Memory>,
    position_frames: usize,
    wait: u32,
    available: usize,
    output_phase: u32,
    output_step: u32,
    ended: bool,
    failed: bool,
    started: bool,
    events: VecDeque<(usize, PendingWriteEvent)>,
    pending_output: VecDeque<[f32; 2]>,
    pwm_compatibility: Option<[PwmCompatibility; 2]>,
    fade_samples: Option<u32>,
    fade_remaining: Option<u32>,
    fade_frames: Option<Fade>,
}

struct VoiceCheckpoint {
    chip: Chip,
    instance: Instance,
    clock: u32,
    state: Box<dyn SoundChipState>,
    tracker: Option<event::Tracker>,
    matrix: StereoMatrix,
    resampler: StereoResamplerState,
    phase: u32,
    phase_rate: Option<SampleRate>,
    phase_step: u32,
    queue: VecDeque<[f32; 2]>,
    gain: f32,
    main_coefficient: f32,
    paired_coefficient: f32,
    paced_writes: Option<VecDeque<ChipRegisterWrite>>,
    write_interval: usize,
    write_cooldown: usize,
}

fn unique_memories(voices: &[Voice]) -> Vec<Rc<RefCell<Memory>>> {
    let mut memories: Vec<Rc<RefCell<Memory>>> = Vec::new();
    for voice in voices {
        if !memories
            .iter()
            .any(|memory| Rc::ptr_eq(memory, &voice.memory))
        {
            memories.push(Rc::clone(&voice.memory));
        }
    }
    memories
}

fn apply_stream_setting(stream: &mut VgmStream, setting: &StreamSetting) {
    match setting {
        StreamSetting::LoopCount(count) => stream.set_loop_count(*count),
        StreamSetting::LoopModifier(modifier) => stream.set_loop_modifier(*modifier),
        StreamSetting::LoopBase(base) => stream.set_loop_base(*base),
        StreamSetting::FadeoutSamples(samples) => {
            stream.set_fadeout_samples(samples.map(|samples| samples as usize))
        }
    }
}

impl<'a> VgmSequencer<'a> {
    /// Create a sequencer from a borrowed uncompressed VGM file.
    ///
    /// The bytes are copied into the stream. Use [`Self::from_vgm`] to pass
    /// an owned `Vec<u8>` without that copy.
    pub fn from_bytes(
        data: &[u8],
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        Self::from_vgm(data.to_vec(), output_rate, chunk_frames)
    }

    /// Create a sequencer from an owned uncompressed VGM file.
    ///
    /// Commands are parsed on demand. The file bytes remain in memory for
    /// looping; passing a `Vec<u8>` transfers its allocation to the stream.
    /// Decompress VGZ before calling this method. Sample data and audio buffers
    /// may allocate separately. GD3 metadata is not parsed or validated.
    ///
    /// For OKIM6258/6295, construction scans commands to determine the
    /// resampler's clock range and may report malformed commands earlier. The
    /// scan skips data-block payloads without copying them.
    pub fn from_vgm(
        data: impl Into<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        Self::from_vgm_with_factories(data, output_rate, chunk_frames, ChipFactoryRegistry::new())
    }

    /// Create a sequencer with replacement factories for selected chip types.
    ///
    /// Factories replace the built-in backend only for registered chip types.
    /// They receive the parsed header, chip instance, and effective clock.
    pub fn from_vgm_with_factories(
        data: impl Into<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
        mut factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let data = Arc::new(data.into());
        let header = VgmHeader::from_bytes(&data)?;
        let extra = header.extra_header_from_bytes(&data)?;
        let mut timing = Timing::new(&Self::supported_clocks(&header));
        if timing.needs_scan() {
            visit_non_data_block_commands(&data, &header, |command| {
                timing.observe(command);
            })?;
        }
        let voices = Self::make_voices(
            &header,
            extra.as_ref(),
            &timing,
            output_rate,
            &mut factories,
        )?;
        let replay_data = Arc::clone(&data);
        let stream = VgmStream::from_vgm_shared(Arc::clone(&data))?;
        let stream_factory: StreamFactory = Box::new(move || {
            VgmStream::from_vgm_shared(Arc::clone(&replay_data))
                .map(|stream| (stream, None))
                .map_err(|error| error.to_string())
        });
        Self::from_parts(stream, stream_factory, voices, output_rate, chunk_frames)
    }

    fn supported_clocks(header: &VgmHeader) -> [(Chip, u32); 17] {
        let misc = header.misc();
        [
            (Chip::Sn76489, header.sn76489_clock),
            (Chip::SegaPcm, header.sega_pcm_clock),
            (Chip::Pwm, header.pwm_clock),
            (Chip::C140, header.c140_clock),
            (Chip::Ym2413, header.ym2413_clock),
            (
                Chip::Ym2612,
                misc.use_ym2413_clock_for_ym2612
                    .unwrap_or(header.ym2612_clock),
            ),
            (
                Chip::Ym2151,
                misc.use_ym2413_clock_for_ym2151
                    .unwrap_or(header.ym2151_clock),
            ),
            (Chip::Ym2203, header.ym2203_clock),
            (Chip::Ym2608, header.ym2608_clock),
            (Chip::Ym2610b, header.ym2610b_clock),
            (Chip::Ym3812, header.ym3812_clock),
            (Chip::Ym3526, header.ym3526_clock),
            (Chip::Y8950, header.y8950_clock),
            (Chip::Ymf262, header.ymf262_clock),
            (Chip::Ymf278b, header.ymf278b_clock),
            (Chip::Okim6258, header.okim6258_clock),
            (Chip::Okim6295, header.okim6295_clock),
        ]
    }

    fn chip_id(chip: &Chip) -> ChipId {
        match chip {
            Chip::Sn76489 => ChipId::Sn76489,
            Chip::SegaPcm => ChipId::SegaPcm,
            Chip::Pwm => ChipId::Pwm,
            Chip::C140 => ChipId::C140,
            Chip::Ym2413 => ChipId::Ym2413,
            Chip::Ym2612 => ChipId::Ym2612,
            Chip::Ym2151 => ChipId::Ym2151,
            Chip::Ym2203 => ChipId::Ym2203,
            Chip::Ym2608 => ChipId::Ym2608,
            Chip::Ym2610b => ChipId::Ym2610,
            Chip::Ym3812 => ChipId::Ym3812,
            Chip::Ym3526 => ChipId::Ym3526,
            Chip::Y8950 => ChipId::Y8950,
            Chip::Ymf262 => ChipId::Ymf262,
            Chip::Ymf278b => ChipId::Ymf278b,
            Chip::Okim6258 => ChipId::Okim6258,
            Chip::Okim6295 => ChipId::Okim6295,
            _ => unreachable!("only supported chips have sequencer voices"),
        }
    }

    fn make_voice(
        header: &VgmHeader,
        extra: Option<&VgmExtraHeader>,
        config: VoiceConfig,
        timing: &Timing,
        output_rate: u32,
        factories: &mut ChipFactoryRegistry,
    ) -> Result<Voice, VgmSequencerError> {
        let VoiceConfig {
            chip,
            instance,
            clock,
            dual,
        } = config;
        let profile = TimingProfile::for_chip(&chip);
        let clock = if chip == Chip::C140 && clock < 1_000_000 {
            match (u8::from(header.c140_chip_type), clock) {
                (2, 44_100) => 25_056_500,
                (0 | 1, 21_390) => 12_288_000,
                _ => clock * 576,
            }
        } else {
            clock
        };
        let memory = Rc::new(RefCell::new(Memory::default()));
        let factory_config =
            ChipFactoryConfig::new(chip.clone(), instance, clock, output_rate, header, extra);
        let backend: Box<dyn SoundChip> =
            if let Some(backend) = factories.create(&factory_config)? {
                backend
            } else if chip == Chip::Sn76489 {
                feature_backend!("sn76496", chip, {
                    if header.misc().t6w28_detected == Some(true) {
                        return Err(ChipError::InvalidConfiguration {
                            chip,
                            reason: "T6W28 PSG is not supported",
                        }
                        .into());
                    }
                    let flags = header.sn76489_flags;
                    let feedback = u16::from(header.sn76489_feedback);
                    let width = u8::from(header.sn76489_shift_register_width);
                    Box::new(Sn76496::with_config(
                        clock,
                        Sn76496Config {
                            feedback: if feedback == 0 { 0x0009 } else { feedback },
                            shift_width: if width == 0 { 16 } else { width },
                            negate: flags.output_negate,
                            stereo: flags.gamegear_stereo,
                            divider_8: flags.clock_divider_8,
                            frequency_zero_is_1024: flags.frequency_0_is_400,
                            xnor: flags.xnor_noise_mode,
                        },
                    )?)
                })
            } else if chip == Chip::C140 {
                feature_backend!("c140", chip, {
                    let kind = match u8::from(header.c140_chip_type) {
                        0 => C140Type::System2,
                        1 => C140Type::System21,
                        2 => C140Type::Asic219,
                        _ => {
                            return Err(ChipError::InvalidConfiguration {
                                chip,
                                reason: "unknown C140 board type",
                            }
                            .into());
                        }
                    };
                    Box::new(C140::new(clock, kind)?)
                })
            } else if chip == Chip::Pwm {
                feature_backend!("pwm", chip, Box::new(Pwm::new(clock)?))
            } else if chip == Chip::SegaPcm {
                feature_backend!("segapcm", chip, {
                    Box::new(SegaPcm::with_banking(
                        clock,
                        header.spcm_interface as u8,
                        (header.spcm_interface >> 16) as u8,
                    )?)
                })
            } else if chip == Chip::Okim6295 {
                feature_backend!("okim6295", chip, {
                    Box::new(Okim6295::with_pin7(
                        clock,
                        header.okim6295_clock & 0x8000_0000 != 0,
                    )?)
                })
            } else if chip == Chip::Okim6258 {
                feature_backend!("okim6258", chip, {
                    let flags = &header.okim6258_flags;
                    // The MAME-backed vgm2wav compatibility path also ignores this selector.
                    let mut oki = Okim6258::new_for_vgm(clock)?;
                    oki.set_divider(match flags.clock_divider {
                        0 => ClockDivider::Div1024,
                        1 => ClockDivider::Div768,
                        _ => ClockDivider::Div512,
                    })?;
                    oki.set_output_bits(if flags.output_12bit {
                        OutputBits::Bits12
                    } else {
                        OutputBits::Bits10
                    });
                    Box::new(oki)
                })
            } else {
                let shared = Rc::clone(&memory);
                let handler = InterfaceHandler {
                    ymfm_external_read: Some(Box::new(move |access, address| {
                        let memory = shared.borrow();
                        Memory::key(access)
                            .and_then(|key| memory.writes.get(&(key, address)))
                            .or_else(|| {
                                memory
                                    .get(access)
                                    .and_then(|data| data.get(address as usize))
                            })
                            .copied()
                            .unwrap_or(0)
                    })),
                    ymfm_external_write: Some(Box::new({
                        let shared = Rc::clone(&memory);
                        move |access, address, value| {
                            if let Some(key) = Memory::key(access) {
                                shared.borrow_mut().writes.insert((key, address), value);
                            }
                        }
                    })),
                    ..Default::default()
                };
                if chip == Chip::Ym2610b {
                    Box::new(YmfmChip::with_ym2610_variant(
                        clock,
                        header.misc().ym2610b_detected == Some(true),
                        handler,
                    )?)
                } else {
                    Box::new(YmfmChip::with_handler(chip.clone(), clock, handler)?)
                }
            };
        let global = volume::global_gain(header.volume_modifier);
        let id = Self::chip_id(&chip);
        let main = SCALE * global * volume::chip_gain(extra, &chip, id, instance, false, dual);
        let paired = SCALE * global * volume::chip_gain(extra, &chip, id, instance, true, dual);
        let coefficients = match chip {
            Chip::Ym2203 => vec![
                [main, main],
                [paired, paired],
                [paired, paired],
                [paired, paired],
            ],
            Chip::Ym2608 | Chip::Ym2610b => vec![[main, 0.0], [0.0, main], [paired, paired]],
            Chip::Ym2413 => vec![[main, main]; 2],
            Chip::Ymf262 => vec![[main, 0.0], [0.0, main], [main, 0.0], [0.0, main]],
            Chip::Ymf278b => vec![[0.0, 0.0]; 4]
                .into_iter()
                .chain([[main, 0.0], [0.0, main]])
                .collect(),
            _ if backend.channels() == 1 => vec![[main, main]],
            _ => vec![[main, 0.0], [0.0, main]],
        };
        let matrix = StereoMatrix::new(coefficients)?;
        if backend.channels() != Self::matrix_channels(&chip) {
            return Err(VgmSequencerError::Audio(AudioError::ChannelMismatch {
                expected: Self::matrix_channels(&chip),
                actual: backend.channels(),
            }));
        }
        let (min_rate, max_rate) =
            timing.bounds(&chip, instance, clock, backend.sample_rate_ratio());
        Ok(Voice {
            chip,
            instance,
            clock,
            tracker: None,
            route: ChipRoute::new(backend),
            matrix,
            resampler: StereoResampler::new(output_rate, min_rate, max_rate, SRC_CHUNK)?,
            memory,
            phase: 0,
            phase_rate: None,
            phase_step: 0,
            queue: VecDeque::new(),
            gain: 1.0,
            main_coefficient: main,
            paired_coefficient: paired,
            paced_writes: profile.write_interval.map(|_| VecDeque::new()),
            write_interval: profile.write_interval.unwrap_or(1),
            write_cooldown: 0,
        })
    }

    fn matrix_channels(chip: &Chip) -> usize {
        match chip {
            Chip::Ym2203 | Chip::Ymf262 => 4,
            Chip::Ym2608 | Chip::Ym2610b => 3,
            Chip::Ymf278b => 6,
            Chip::Ym2413
            | Chip::Ym2151
            | Chip::Ym2612
            | Chip::Sn76489
            | Chip::SegaPcm
            | Chip::Pwm
            | Chip::C140 => 2,
            _ => 1,
        }
    }

    /// Create a sequencer from a parsed document.
    pub fn new(
        document: VgmDocument,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        Self::new_with_factories(
            document,
            output_rate,
            chunk_frames,
            ChipFactoryRegistry::new(),
        )
    }

    /// Create a sequencer from a parsed document with selected chip factories.
    pub fn new_with_factories(
        document: VgmDocument,
        output_rate: u32,
        chunk_frames: usize,
        mut factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let mut timing = Timing::new(&Self::supported_clocks(&document.header));
        if timing.needs_scan() {
            for command in &document.commands {
                timing.observe(command);
            }
        }
        let voices = Self::make_voices(
            &document.header,
            document.extra_header.as_ref(),
            &timing,
            output_rate,
            &mut factories,
        )?;
        let document = Arc::new(document);
        let replay_document = Arc::clone(&document);
        let stream = VgmStream::from_document_shared(Arc::clone(&document));
        let stream_factory: StreamFactory = Box::new(move || {
            Ok((
                VgmStream::from_document_shared(Arc::clone(&replay_document)),
                None,
            ))
        });
        Self::from_parts(stream, stream_factory, voices, output_rate, chunk_frames)
    }

    pub(super) fn from_mdx_stream(
        stream: VgmStream,
        stream_factory: StreamFactory,
        ym2151_clock: u32,
        okim6258_clock: Option<u32>,
        output_rate: u32,
        chunk_frames: usize,
        mut factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let mut header = VgmHeader {
            ym2151_clock,
            okim6258_clock: okim6258_clock.unwrap_or(0),
            ..VgmHeader::default()
        };
        header.okim6258_flags.clock_divider = soundlog::mdx::pcm_mixer::PCM8_OKIM6258_CLOCK_DIVIDER;
        let mut timing = Timing::new(&Self::supported_clocks(&header));
        if okim6258_clock.is_some() {
            // Legacy MDX PCM rate commands can switch between these clocks.
            for clock in [4_000_000_u32, 8_000_000_u32] {
                timing.observe_clock(&Chip::Okim6258, Instance::Primary, clock);
            }
        }
        let voices = Self::make_voices(&header, None, &timing, output_rate, &mut factories)?;
        Self::from_parts(stream, stream_factory, voices, output_rate, chunk_frames)
    }

    fn validate_output(output_rate: u32, chunk_frames: usize) -> Result<(), VgmSequencerError> {
        if output_rate == 0 {
            return Err(VgmSequencerError::InvalidSampleRate);
        }
        if chunk_frames == 0 {
            return Err(VgmSequencerError::InvalidChunkSize);
        }
        Ok(())
    }

    fn make_voices(
        header: &VgmHeader,
        extra: Option<&VgmExtraHeader>,
        timing: &Timing,
        output_rate: u32,
        factories: &mut ChipFactoryRegistry,
    ) -> Result<Vec<Voice>, VgmSequencerError> {
        let mut voices = Vec::new();
        for (chip, raw) in Self::supported_clocks(header) {
            if raw & 0x3fff_ffff == 0 {
                continue;
            }
            for instance in [Instance::Primary, Instance::Secondary] {
                if instance == Instance::Secondary && raw & 0x4000_0000 == 0 {
                    continue;
                }
                let clock = extra
                    .and_then(|extra| {
                        extra.chip_clocks.iter().find(|entry| {
                            entry.chip_id == Self::chip_id(&chip) && entry.instance == instance
                        })
                    })
                    .map_or(raw & 0x3fff_ffff, |entry| entry.clock & 0x3fff_ffff);
                voices.push(Self::make_voice(
                    header,
                    extra,
                    VoiceConfig {
                        chip: chip.clone(),
                        instance,
                        clock,
                        dual: raw & 0x4000_0000 != 0,
                    },
                    timing,
                    output_rate,
                    factories,
                )?);
            }
        }
        Ok(voices)
    }

    fn from_parts(
        stream: VgmStream,
        stream_factory: StreamFactory,
        voices: Vec<Voice>,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        Ok(Self {
            stream,
            stream_factory,
            stream_steps: 0,
            stream_settings: Vec::new(),
            voices,
            output_rate,
            chunk_frames,
            position_frames: 0,
            wait: 0,
            available: 0,
            output_phase: 1 << 31,
            output_step: divide_ratio(output_rate, VGM_SAMPLE_RATE, 1)
                .ok_or(VgmSequencerError::DurationOverflow)?
                .1,
            ended: false,
            failed: false,
            started: false,
            callbacks: CallbackRegistry::default(),
            events: VecDeque::new(),
            pending_output: VecDeque::new(),
            pwm_compatibility: Some([PwmCompatibility::default(); 2]),
            fade_samples: None,
            fade_remaining: None,
            fade_frames: None,
            mdx_loop_marker: None,
            source_identity: Arc::new(()),
        })
    }

    /// Fade linearly to silence after the final VGM loop, over this many
    /// 44,100 Hz samples. Files without a loop and infinite playback ignore it.
    /// A zero duration disables the fade. Configure before rendering. Fade
    /// length and gain use `f32` arithmetic, so long fades have reduced
    /// frame-level precision.
    pub fn set_fadeout_samples(&mut self, samples: Option<u32>) -> Result<(), VgmSequencerError> {
        self.configure_fadeout(samples, None)?;
        let samples = samples.filter(|samples| *samples != 0);
        self.stream
            .set_fadeout_samples(samples.map(|samples| samples as usize));
        self.stream_settings
            .push((self.stream_steps, StreamSetting::FadeoutSamples(samples)));
        Ok(())
    }

    pub(super) fn configure_fadeout(
        &mut self,
        samples: Option<u32>,
        marker: Option<MdxLoopMarker>,
    ) -> Result<(), VgmSequencerError> {
        if self.started {
            return Err(VgmSequencerError::FadeoutConfigurationAfterPlayback);
        }
        self.fade_samples = samples.filter(|samples| *samples != 0);
        self.mdx_loop_marker = marker;
        Ok(())
    }

    /// Enable or disable the bad-rip PWM channel initialization workaround.
    ///
    /// Enabled by default. Configuration is only allowed before rendering.
    /// Per chip instance, 32 writes to non-control registers before a control
    /// write with a nonzero low byte cause a `0x005` control write immediately
    /// before the 32nd write. The cycle register counts too; zero-low-byte
    /// control writes do not reset the count. The decision is made once per
    /// instance and persists across loops. This heuristic may enable
    /// intentionally disabled PWM. Synthetic writes are reported through the
    /// event API before the triggering write at the same frame offset.
    pub fn set_pwm_compatibility(&mut self, enabled: bool) -> Result<(), VgmSequencerError> {
        if self.started {
            return Err(VgmSequencerError::PwmCompatibilityConfigurationAfterPlayback);
        }
        self.pwm_compatibility = enabled.then_some([PwmCompatibility::default(); 2]);
        Ok(())
    }

    /// Register a typed chip-write callback.
    ///
    /// The callback receives the instance, typed write, absolute output-frame
    /// position at this sequencer's sample rate, and state changes when state
    /// tracking is enabled. Register callbacks before rendering. Registering
    /// another callback for the same write type replaces the previous one.
    pub fn on_write<C, F>(&mut self, callback: F)
    where
        C: event::VgmWriteCallbackTarget,
        F: FnMut(Instance, C, usize, Option<Vec<soundlog::chip::event::StateEvent>>) + 'a,
    {
        self.callbacks.register::<C, F>(callback);
    }

    /// Enable state tracking for a chip type and instance.
    ///
    /// State tracking is available for YM2151, YM2203, YM2413, YM2608, YM2610B,
    /// YM2612, YM3526, Y8950, YM3812, YMF262, and YMF278B.
    pub fn track_state<S>(&mut self, instance: Instance, clock: f32)
    where
        S: VgmStateTracker,
    {
        let chip = event::tracker_chip(S::KIND);
        for voice in &mut self.voices {
            if voice.chip == chip && voice.instance == instance {
                voice.tracker = Some(event::tracker(S::KIND, clock));
            }
        }
    }

    /// Set the number of playthroughs (`None` loops indefinitely).
    /// This should be called before rendering begins.
    pub fn set_loop_count(&mut self, count: Option<u32>) {
        self.stream.set_loop_count(count);
        self.stream_settings
            .push((self.stream_steps, StreamSetting::LoopCount(count)));
    }

    /// Override the VGM loop modifier before rendering.
    pub fn set_loop_modifier(&mut self, modifier: u8) {
        self.stream.set_loop_modifier(modifier);
        self.stream_settings
            .push((self.stream_steps, StreamSetting::LoopModifier(modifier)));
    }

    /// Override the VGM loop base before rendering.
    pub fn set_loop_base(&mut self, base: i8) {
        self.stream.set_loop_base(base);
        self.stream_settings
            .push((self.stream_steps, StreamSetting::LoopBase(base)));
    }

    /// Change the gain for one chip instance, including already buffered PCM.
    /// This multiplies, rather than replaces, the VGM header's volume.
    pub fn set_gain(
        &mut self,
        chip: Chip,
        instance: Instance,
        gain: f32,
    ) -> Result<(), VgmSequencerError> {
        if !gain.is_finite() || gain < 0.0 {
            return Err(VgmSequencerError::InvalidGain);
        }
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.chip == chip && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound { chip, instance })?;
        voice.gain = gain;
        Ok(())
    }

    /// Supply the unbundled YM2608 rhythm ROM before rendering.
    ///
    /// Without it, the internal rhythm instruments cannot play as intended.
    pub fn set_ym2608_rhythm_rom(
        &mut self,
        instance: Instance,
        data: &[u8],
    ) -> Result<(), VgmSequencerError> {
        if data.len() > MAX_ROM_BYTES {
            return Err(VgmSequencerError::RomTooLarge);
        }
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.chip == Chip::Ym2608 && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound {
                chip: Chip::Ym2608,
                instance,
            })?;
        let mut memory = voice.memory.borrow_mut();
        memory.adpcm_a = data.to_vec();
        memory.writes.retain(|(kind, _), _| *kind != 0);
        Ok(())
    }

    /// Return the current command-stream position in 44.1 kHz samples.
    ///
    /// This follows [`soundlog::vgm::VgmStream::current_sample`]: the position
    /// is relative to the current loop iteration and resets when a loop begins.
    /// It tracks command processing, which may run ahead of emitted PCM.
    pub fn current_sample(&self) -> usize {
        self.stream.current_sample()
    }

    /// Return the number of output frames returned by this sequencer.
    ///
    /// Unlike [`Self::current_sample`], this position is monotonic across loops
    /// and uses the configured output rate. It does not account for downstream
    /// buffering or device playback latency.
    pub fn position_frames(&self) -> usize {
        self.position_frames
    }

    /// Capture the exact state at the current output-block boundary.
    ///
    /// The checkpoint can be restored repeatedly on this sequencer. It stores
    /// chip and audio-filter state; restoring reconstructs the parser by
    /// replaying the consumed command stream without generating PCM.
    pub fn checkpoint(&mut self) -> Result<VgmSequencerCheckpoint, VgmSequencerError> {
        if self.failed {
            return Err(VgmSequencerError::StreamFailed);
        }

        let mut voices = Vec::with_capacity(self.voices.len());
        for voice in &mut self.voices {
            voices.push(VoiceCheckpoint {
                chip: voice.chip.clone(),
                instance: voice.instance,
                clock: voice.clock,
                state: voice.route.chip_mut().save_state()?,
                tracker: voice.tracker.clone(),
                matrix: voice.matrix.clone(),
                resampler: voice.resampler.save_state(),
                phase: voice.phase,
                phase_rate: voice.phase_rate,
                phase_step: voice.phase_step,
                queue: voice.queue.clone(),
                gain: voice.gain,
                main_coefficient: voice.main_coefficient,
                paired_coefficient: voice.paired_coefficient,
                paced_writes: voice.paced_writes.clone(),
                write_interval: voice.write_interval,
                write_cooldown: voice.write_cooldown,
            });
        }

        Ok(VgmSequencerCheckpoint {
            source_identity: Arc::clone(&self.source_identity),
            output_rate: self.output_rate,
            chunk_frames: self.chunk_frames,
            stream_steps: self.stream_steps,
            stream_settings: self.stream_settings.clone(),
            voices,
            memory: unique_memories(&self.voices)
                .iter()
                .map(|memory| memory.borrow().clone())
                .collect(),
            position_frames: self.position_frames,
            wait: self.wait,
            available: self.available,
            output_phase: self.output_phase,
            output_step: self.output_step,
            ended: self.ended,
            failed: self.failed,
            started: self.started,
            events: self.events.clone(),
            pending_output: self.pending_output.clone(),
            pwm_compatibility: self.pwm_compatibility,
            fade_samples: self.fade_samples,
            fade_remaining: self.fade_remaining,
            fade_frames: self.fade_frames.clone(),
        })
    }

    /// Restore a checkpoint created by this sequencer.
    ///
    /// The command parser is recreated from the original source and advanced
    /// through the checkpoint's consumed commands. Chip synthesis and PCM
    /// rendering are not repeated for that prefix.
    pub fn restore_checkpoint(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
    ) -> Result<(), VgmSequencerError> {
        if !Arc::ptr_eq(&self.source_identity, &checkpoint.source_identity)
            || self.output_rate != checkpoint.output_rate
            || self.chunk_frames != checkpoint.chunk_frames
            || self.voices.len() != checkpoint.voices.len()
            || self
                .voices
                .iter()
                .zip(&checkpoint.voices)
                .any(|(voice, saved)| {
                    voice.chip != saved.chip
                        || voice.instance != saved.instance
                        || voice.clock != saved.clock
                })
            || unique_memories(&self.voices).len() != checkpoint.memory.len()
        {
            return Err(VgmSequencerError::CheckpointMismatch);
        }

        let (mut stream, marker) =
            (self.stream_factory)().map_err(VgmSequencerError::StreamReconstruction)?;
        let mut setting_index = 0;
        for step in 0..=checkpoint.stream_steps {
            while checkpoint
                .stream_settings
                .get(setting_index)
                .is_some_and(|(at, _)| *at == step)
            {
                apply_stream_setting(&mut stream, &checkpoint.stream_settings[setting_index].1);
                setting_index += 1;
            }
            if step == checkpoint.stream_steps {
                break;
            }
            let result = stream
                .next()
                .ok_or(VgmSequencerError::UnexpectedEnd)?
                .map_err(VgmSequencerError::Parse)?;
            if matches!(result, StreamResult::EndOfStream) && step + 1 != checkpoint.stream_steps {
                return Err(VgmSequencerError::UnexpectedEnd);
            }
        }
        if setting_index != checkpoint.stream_settings.len() {
            return Err(VgmSequencerError::CheckpointMismatch);
        }

        for (memory, saved) in unique_memories(&self.voices).iter().zip(&checkpoint.memory) {
            *memory.borrow_mut() = saved.clone();
        }
        for (voice, saved) in self.voices.iter_mut().zip(&checkpoint.voices) {
            voice.route.chip_mut().restore_state(saved.state.as_ref())?;
            voice.tracker = saved.tracker.clone();
            voice.matrix = saved.matrix.clone();
            voice.resampler.restore_state(&saved.resampler);
            voice.phase = saved.phase;
            voice.phase_rate = saved.phase_rate;
            voice.phase_step = saved.phase_step;
            voice.queue.clone_from(&saved.queue);
            voice.gain = saved.gain;
            voice.main_coefficient = saved.main_coefficient;
            voice.paired_coefficient = saved.paired_coefficient;
            voice.paced_writes.clone_from(&saved.paced_writes);
            voice.write_interval = saved.write_interval;
            voice.write_cooldown = saved.write_cooldown;
        }

        self.stream = stream;
        self.stream_steps = checkpoint.stream_steps;
        self.stream_settings.clone_from(&checkpoint.stream_settings);
        self.mdx_loop_marker = marker;
        self.position_frames = checkpoint.position_frames;
        self.wait = checkpoint.wait;
        self.available = checkpoint.available;
        self.output_phase = checkpoint.output_phase;
        self.output_step = checkpoint.output_step;
        self.ended = checkpoint.ended;
        self.failed = checkpoint.failed;
        self.started = checkpoint.started;
        self.events.clone_from(&checkpoint.events);
        self.pending_output.clone_from(&checkpoint.pending_output);
        self.pwm_compatibility = checkpoint.pwm_compatibility;
        self.fade_samples = checkpoint.fade_samples;
        self.fade_remaining = checkpoint.fade_remaining;
        self.fade_frames = checkpoint.fade_frames.clone();
        Ok(())
    }

    /// Restore a checkpoint and advance to an output-frame position.
    ///
    /// The target must not precede the checkpoint. Audio before the target is
    /// rendered only from the checkpoint; if the target falls inside a block,
    /// the remaining frames are preserved for the next render call.
    pub fn seek_to_frame(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
        target: usize,
    ) -> Result<(), VgmSequencerError> {
        self.seek_to_frame_with_cancel(checkpoint, target, || false)
    }

    /// Restore a checkpoint and seek while allowing the caller to cancel.
    pub fn seek_to_frame_with_cancel(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
        target: usize,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), VgmSequencerError> {
        if target < checkpoint.position_frames {
            return Err(VgmSequencerError::SeekTargetBeforeCheckpoint {
                target,
                checkpoint: checkpoint.position_frames,
            });
        }
        self.restore_checkpoint(checkpoint)?;
        while self.position_frames < target {
            if is_cancelled() {
                return Err(VgmSequencerError::SeekCancelled);
            }
            let Some(block) = (match self.render_inner(self.position_frames) {
                Ok(block) => block,
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }) else {
                return Err(VgmSequencerError::SeekTargetBeyondEnd {
                    target,
                    reached: self.position_frames,
                });
            };
            let start = self.position_frames;
            let discard = (target - start).min(block.frames().len());
            self.discard_events(discard);
            self.position_frames = self
                .position_frames
                .checked_add(discard)
                .ok_or(VgmSequencerError::DurationOverflow)?;
            self.pending_output
                .extend(block.frames()[discard..].iter().copied());
        }
        Ok(())
    }

    fn discard_events(&mut self, frames: usize) {
        while self
            .events
            .front()
            .is_some_and(|(offset, _)| *offset < frames)
        {
            self.events.pop_front();
        }
        for (offset, _) in &mut self.events {
            *offset = offset.saturating_sub(frames);
        }
    }

    /// Return the next chunk; the final block can be shorter. `None` means EOF.
    /// Registered write callbacks are invoked synchronously before this returns.
    pub fn render(&mut self) -> Result<Option<StereoBlock>, VgmSequencerError> {
        if self.failed {
            return Err(VgmSequencerError::StreamFailed);
        }
        self.started = true;
        let sample_start = self.position_frames;
        let result = self.render_inner(sample_start).and_then(|block| {
            if let Some(block) = &block {
                let sample_end = sample_start
                    .checked_add(block.frames().len())
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                for voice in &mut self.voices {
                    voice
                        .route
                        .chip_mut()
                        .flush_scheduled_until(sample_end, self.output_rate)?;
                }
            } else {
                for voice in &mut self.voices {
                    voice
                        .route
                        .chip_mut()
                        .flush_scheduled_until(sample_start, self.output_rate)?;
                }
                for voice in &mut self.voices {
                    voice
                        .route
                        .chip_mut()
                        .finish_scheduled(sample_start, self.output_rate)?;
                }
            }
            Ok(block)
        });
        if result.is_err() {
            self.failed = true;
        }
        if let Ok(ref block) = result {
            let count = block.as_ref().map_or(0, |block| block.frames().len());
            if let Some(block) = block {
                let Some(position) = self.position_frames.checked_add(block.frames().len()) else {
                    self.failed = true;
                    return Err(VgmSequencerError::DurationOverflow);
                };
                self.position_frames = position;
            }
            while self.events.front().is_some_and(|(offset, _)| {
                *offset < count || (self.ended && self.available == 0 && *offset <= count)
            }) {
                let (offset, event) = self.events.pop_front().expect("front exists");
                let sample = sample_start
                    .checked_add(offset)
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                self.callbacks.dispatch(event, sample);
            }
            for (offset, _) in &mut self.events {
                *offset -= count;
            }
        }
        result
    }

    fn render_inner(
        &mut self,
        sample_frame_origin: usize,
    ) -> Result<Option<StereoBlock>, VgmSequencerError> {
        loop {
            if !self.pending_output.is_empty() {
                let count = self.pending_output.len().min(self.chunk_frames);
                let frames = self.pending_output.drain(..count).collect();
                return Ok(Some(StereoBlock::new(
                    SampleRate::from_hz(self.output_rate),
                    frames,
                )?));
            }
            if self.ended
                || (self.available >= self.chunk_frames
                    && self
                        .voices
                        .iter()
                        .all(|voice| voice.queue.len() >= self.chunk_frames))
            {
                let count = self.available.min(self.chunk_frames);
                if count == 0 {
                    return Ok(None);
                }
                let mut bus = StereoBlock::silence(SampleRate::from_hz(self.output_rate), count)?;
                for voice in &mut self.voices {
                    let mut frames = Vec::with_capacity(count);
                    for _ in 0..count {
                        frames.push(voice.queue.pop_front().unwrap_or([0.0; 2]));
                    }
                    let source = StereoBlock::new(SampleRate::from_hz(self.output_rate), frames)?;
                    StereoMixer::mix(&mut bus, &source, voice.gain)?;
                }
                if let Some(fade) = &mut self.fade_frames {
                    fade.process(&mut bus.frames);
                }
                self.available -= count;
                return Ok(Some(bus));
            }
            if self.fade_remaining == Some(0) {
                self.flush_paced_writes(sample_frame_origin)?;
                for voice in &mut self.voices {
                    voice
                        .queue
                        .extend(voice.resampler.finish()?.frames().iter().copied());
                }
                self.ended = true;
                continue;
            }
            if self.wait > 0 {
                let ticks = self
                    .wait
                    .min(WAIT_STEP)
                    .min(self.fade_remaining.unwrap_or(u32::MAX));
                for voice in &mut self.voices {
                    voice.render(
                        ticks,
                        self.output_rate,
                        self.output_phase,
                        self.available,
                        sample_frame_origin,
                        &mut EventContext {
                            callbacks: &self.callbacks,
                            events: &mut self.events,
                        },
                    )?;
                }
                self.wait -= ticks;
                if let Some(remaining) = &mut self.fade_remaining {
                    *remaining -= ticks;
                }
                for _ in 0..ticks {
                    let frames = advance(
                        &mut self.output_phase,
                        self.output_rate / VGM_SAMPLE_RATE,
                        self.output_step,
                    )
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                    self.available = self
                        .available
                        .checked_add(frames as usize)
                        .ok_or(VgmSequencerError::DurationOverflow)?;
                }
                continue;
            }
            let result = self
                .stream
                .next()
                .expect("VgmStream iterator is continuous");
            self.stream_steps = self
                .stream_steps
                .checked_add(1)
                .ok_or(VgmSequencerError::DurationOverflow)?;
            let result = result?;
            if self.fade_frames.is_none()
                && (self.stream.is_fading_out()
                    || self
                        .mdx_loop_marker
                        .as_ref()
                        .is_some_and(MdxLoopMarker::reached))
                && let Some(samples) = self.fade_samples
            {
                let step = (self.output_rate / VGM_SAMPLE_RATE) as f32
                    + self.output_step as f32 * PHASE_SCALE;
                let length =
                    (self.output_phase as f32 * PHASE_SCALE + samples as f32 * step).floor();
                self.fade_frames = Some(Fade {
                    pending: self.available,
                    length,
                    rate: self.output_rate,
                    seconds: 0,
                    frames: 0,
                });
                self.fade_remaining = Some(samples);
            }
            match result {
                StreamResult::EndOfStream => {
                    self.flush_paced_writes(sample_frame_origin)?;
                    for voice in &mut self.voices {
                        voice
                            .queue
                            .extend(voice.resampler.finish()?.frames().iter().copied());
                    }
                    self.ended = true;
                }
                StreamResult::NeedsMoreData => return Err(VgmSequencerError::UnexpectedEnd),
                StreamResult::Command(VgmCommand::WaitSamples(wait)) => {
                    self.wait = u32::from(wait.0)
                }
                StreamResult::Command(command) => self.command(command, sample_frame_origin)?,
            }
        }
    }

    fn command(
        &mut self,
        command: VgmCommand,
        sample_frame_origin: usize,
    ) -> Result<(), VgmSequencerError> {
        use VgmCommand::*;
        let write = ChipRegisterWrite::from_vgm_command(&command);
        match command {
            PwmWrite(i, s) => {
                let index = match i {
                    Instance::Primary => 0,
                    Instance::Secondary => 1,
                };
                let active = self
                    .voices
                    .iter()
                    .any(|v| v.chip == Chip::Pwm && v.instance == i);
                if active
                    && self
                        .pwm_compatibility
                        .as_mut()
                        .is_some_and(|states| states[index].observe(s.register, s.value))
                {
                    self.write_chip(
                        i,
                        ChipRegisterWrite::Pwm(PwmSpec {
                            register: 0,
                            value: 5,
                        }),
                        sample_frame_origin,
                    )?;
                }
            }
            AY8910StereoMask(mask) if mask.is_ym2203 => {
                if let Some(voice) = self
                    .voices
                    .iter_mut()
                    .find(|v| v.chip == Chip::Ym2203 && v.instance == mask.chip_instance)
                {
                    let coefficient = |left, right| {
                        [
                            if left { voice.paired_coefficient } else { 0.0 },
                            if right { voice.paired_coefficient } else { 0.0 },
                        ]
                    };
                    voice.matrix = StereoMatrix::new(vec![
                        [voice.main_coefficient, voice.main_coefficient],
                        coefficient(mask.left_ch1, mask.right_ch1),
                        coefficient(mask.left_ch2, mask.right_ch2),
                        coefficient(mask.left_ch3, mask.right_ch3),
                    ])?;
                }
            }
            DataBlock(block) => {
                self.load_block(*block)?;
            }
            _ => {}
        }
        if let Some((instance, write)) = write {
            self.write_chip(instance, write, sample_frame_origin)?;
        }
        Ok(())
    }

    fn write_chip(
        &mut self,
        instance: Instance,
        write: ChipRegisterWrite,
        sample_frame_origin: usize,
    ) -> Result<(), VgmSequencerError> {
        let chip = write.chip();
        if let Some(voice) = self
            .voices
            .iter_mut()
            .find(|v| v.chip == chip && v.instance == instance)
        {
            if let Some(queue) = &mut voice.paced_writes {
                if queue.len() == MAX_PENDING_WRITES {
                    return Err(VgmSequencerError::WriteQueueFull {
                        chip,
                        instance,
                        limit: MAX_PENDING_WRITES,
                    });
                }
                queue.push_back(write);
            } else {
                let sample_frame = sample_frame_origin
                    .checked_add(self.available)
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                voice.apply_write(
                    write,
                    self.available,
                    sample_frame,
                    self.output_rate,
                    &mut EventContext {
                        callbacks: &self.callbacks,
                        events: &mut self.events,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn flush_paced_writes(&mut self, sample_frame_origin: usize) -> Result<(), VgmSequencerError> {
        for voice in &mut self.voices {
            while let Some(write) = voice.paced_writes.as_mut().and_then(VecDeque::pop_front) {
                let sample_frame = sample_frame_origin
                    .checked_add(self.available)
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                voice.apply_write(
                    write,
                    self.available,
                    sample_frame,
                    self.output_rate,
                    &mut EventContext {
                        callbacks: &self.callbacks,
                        events: &mut self.events,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn load_block(
        &mut self,
        block: soundlog::vgm::command::DataBlock,
    ) -> Result<(), VgmSequencerError> {
        let instance = if block.chip_instance == 0 {
            Instance::Primary
        } else {
            Instance::Secondary
        };
        let (chip, access) = match block.data_type {
            0x80 => (Chip::SegaPcm, None),
            0x8b => (Chip::Okim6295, None),
            0x8d => (Chip::C140, None),
            0x81 => (Chip::Ym2608, Some(AccessClass::AdpcmB)),
            0x82 => (Chip::Ym2610b, Some(AccessClass::AdpcmA)),
            0x83 => (Chip::Ym2610b, Some(AccessClass::AdpcmB)),
            0x84 | 0x87 => (Chip::Ymf278b, Some(AccessClass::Pcm)),
            0x88 => (Chip::Y8950, Some(AccessClass::AdpcmB)),
            _ => return Ok(()),
        };
        let Some(voice) = self
            .voices
            .iter_mut()
            .find(|v| v.chip == chip && v.instance == instance)
        else {
            return Ok(());
        };
        let DataBlockType::RomRamDump(dump) =
            parse_data_block(block).map_err(|_| VgmSequencerError::InvalidRomBlock)?
        else {
            return Err(VgmSequencerError::InvalidRomBlock);
        };
        let start = dump.start_address as usize;
        let end = start
            .checked_add(dump.data.len())
            .ok_or(VgmSequencerError::RomTooLarge)?;
        if end > dump.rom_size as usize {
            return Err(VgmSequencerError::InvalidRomBlock);
        }
        if end > MAX_ROM_BYTES {
            return Err(VgmSequencerError::RomTooLarge);
        }
        let Some(access) = access else {
            voice.route.chip_mut().write_rom(start, &dump.data)?;
            return Ok(());
        };
        let mut memory = voice.memory.borrow_mut();
        let key = Memory::key(access).expect("ROM uses a memory access class");
        memory.writes.retain(|(kind, address), _| {
            *kind != key || (*address as usize) < start || (*address as usize) >= end
        });
        let target = memory
            .get_mut(access)
            .expect("ROM uses a memory access class");
        if target.len() < end {
            target.resize(end, 0);
        }
        target[start..end].copy_from_slice(&dump.data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::chip::Ym2151Spec;
    use soundlog::vgm::VgmBuilder;

    #[test]
    fn fade_waits_for_buffered_frames_and_continues_across_blocks() {
        let mut fade = Fade {
            pending: 2,
            length: 5.0,
            rate: 3,
            seconds: 0,
            frames: 0,
        };
        let mut first = [[1.0; 2]; 4];
        let mut second = [[1.0; 2]; 4];
        fade.process(&mut first);
        fade.process(&mut second);
        assert_eq!(first, [[1.0; 2], [1.0; 2], [1.0; 2], [0.75; 2]]);
        assert_eq!(second, [[0.5; 2], [0.25; 2], [0.0; 2], [0.0; 2]]);
    }

    #[test]
    fn fade_supports_more_than_u32_frames() {
        let mut fade = Fade {
            pending: 0,
            length: 8_589_934_592.0,
            rate: 48_000,
            seconds: 89_478,
            frames: 47_999,
        };
        let mut frames = [[1.0; 2]; 2];
        fade.process(&mut frames);
        assert!(frames.iter().all(|frame| (frame[0] - 0.5).abs() < 0.001));
        assert_eq!(fade.seconds, 89_479);
        assert_eq!(fade.frames, 1);
    }

    #[test]
    fn pending_write_limit_is_per_instance_and_explicit() {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Ym2151, Instance::Secondary, 4_000_000);
        let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64).unwrap();
        let write = ChipRegisterWrite::Ym2151(Ym2151Spec {
            register: 0x20,
            value: 0xc7,
        });
        player.voices[0]
            .paced_writes
            .as_mut()
            .unwrap()
            .resize(MAX_PENDING_WRITES - 1, write.clone());
        player
            .write_chip(Instance::Primary, write.clone(), 0)
            .unwrap();
        assert!(matches!(
            player.write_chip(Instance::Primary, write.clone(), 0),
            Err(VgmSequencerError::WriteQueueFull {
                chip: Chip::Ym2151,
                instance: Instance::Primary,
                limit: MAX_PENDING_WRITES,
            })
        ));
        player.write_chip(Instance::Secondary, write, 0).unwrap();
    }
}
