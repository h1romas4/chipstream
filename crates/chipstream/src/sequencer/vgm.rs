//! VGM command playback, chip routing, and stereo audio rendering.
//!
//! [`VgmSequencer`] reads a parsed document, owned VGM bytes, or a lazy
//! command generator. It schedules writes, renders chip instances at their
//! native rates, and mixes resampled stereo blocks. Factories, write callbacks,
//! effects, and checkpoints provide control over playback and seeking.
//!
//! Rendering can read ahead and allocate; it is not guaranteed real-time safe.
//! Single-pass generators do not support checkpoints.

pub(super) mod config;
mod timing;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::sync::Arc;

use soundlog::chip::state::ChipState;
use soundlog::chip::{Chip, PwmSpec};
use soundlog::mdx::convert::MdxLoopMarker;
use soundlog::vgm::command::{Instance, VgmCommand};
use soundlog::vgm::detail::{DataBlockType, parse_data_block};
use soundlog::vgm::header::{ChipId, VgmExtraHeader};
use soundlog::vgm::parser::visit_non_data_block_commands;
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::{
    DacStreamSelector, DacStreamTarget, VGM_SAMPLE_RATE, VgmCommandGenerator, VgmDocument,
    VgmHeader, VgmStream,
};
use soundlog::xgm::XgmLoopMarker;
use ymfm_sys::{InterfaceHandler, ffi::AccessClass};

use crate::audio::{
    AudioError, ChipCapture, NativeEffect, RouteError, StereoBlock, StereoEffect, StereoMatrix,
    StereoMixer, StereoResampler, StereoResamplerState,
};
#[cfg(feature = "c352")]
use crate::chip::C352;
#[cfg(feature = "es550x")]
use crate::chip::Es550x;
#[cfg(feature = "es5503")]
use crate::chip::Es5503;
#[cfg(feature = "ga20")]
use crate::chip::Ga20;
#[cfg(feature = "gb_dmg")]
use crate::chip::GbDmg;
#[cfg(feature = "huc6280")]
use crate::chip::Huc6280;
#[cfg(feature = "k051649")]
use crate::chip::K051649;
#[cfg(feature = "k053260")]
use crate::chip::K053260;
#[cfg(feature = "mikey")]
use crate::chip::Mikey;
#[cfg(feature = "multipcm")]
use crate::chip::MultiPcm;
#[cfg(feature = "nes_apu")]
use crate::chip::NesApu;
#[cfg(feature = "okim6295")]
use crate::chip::Okim6295;
#[cfg(feature = "pokey")]
use crate::chip::Pokey;
#[cfg(feature = "pwm")]
use crate::chip::Pwm;
#[cfg(feature = "qsound")]
use crate::chip::Qsound;
#[cfg(feature = "rf5c")]
use crate::chip::Rf5c;
#[cfg(feature = "saa1099")]
use crate::chip::Saa1099;
#[cfg(feature = "scsp")]
use crate::chip::Scsp;
#[cfg(feature = "segapcm")]
use crate::chip::SegaPcm;
#[cfg(feature = "upd7759")]
use crate::chip::Upd7759;
#[cfg(feature = "vsu")]
use crate::chip::Vsu;
#[cfg(feature = "wonderswan")]
use crate::chip::WonderSwan;
#[cfg(feature = "x1010")]
use crate::chip::X1010;
#[cfg(feature = "ymf271")]
use crate::chip::Ymf271;
#[cfg(feature = "ymz280b")]
use crate::chip::Ymz280b;
#[cfg(feature = "c140")]
use crate::chip::{C140, C140Type};
use crate::chip::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, SoundChipState, YmfmChip};
#[cfg(feature = "okim6258")]
use crate::chip::{ClockDivider, Okim6258, Okim6258Stream, OutputBits};
#[cfg(feature = "k054539")]
use crate::chip::{K054539, K054539Config};
#[cfg(feature = "sn76496")]
use crate::chip::{Sn76496, T6w28Half};
use crate::fixed::{advance, divide_ratio};

use super::ChipBackendSettings;
#[cfg(feature = "okim6258")]
use super::Okim6258Divider;
use super::effects::{EffectError, EffectId, EffectRack, OutputGroupId};
use super::factory::{ChipFactoryConfig, ChipFactoryRegistry};
use super::{compat::PwmCompatibility, event, volume};
use config::{ChipConfiguration, VoiceConfig, chip_id};
use event::{CallbackRegistry, PendingWriteEvent, VgmStateTracker};
use timing::{Timing, TimingProfile};

const WAIT_STEP: u32 = 256;
const SRC_CHUNK: usize = 256;
const MAX_ROM_BYTES: usize = 64 * 1024 * 1024;
const MAX_PENDING_WRITES: usize = 1_048_576;
const SCALE: f32 = 1.0 / 32768.0;
const PHASE_SCALE: f32 = 1.0 / 4_294_967_296.0;

pub(super) type StreamFactory = Box<dyn Fn() -> Result<(VgmStream, Option<FadeMarker>), String>>;

#[derive(Clone)]
pub(super) enum FadeMarker {
    Mdx(MdxLoopMarker),
    Xgm(XgmLoopMarker),
}

impl FadeMarker {
    pub(super) fn reached(&self) -> bool {
        match self {
            Self::Mdx(marker) => marker.reached(),
            Self::Xgm(marker) => marker.reached(),
        }
    }
}

#[derive(Default)]
struct T6w28Pair {
    #[cfg(feature = "sn76496")]
    volumes: Rc<RefCell<[[i32; 4]; 2]>>,
    built_in: u8,
}

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
#[non_exhaustive]
pub enum VgmSequencerError {
    /// The output chunk size is zero.
    InvalidChunkSize,
    /// The output sample rate is zero.
    InvalidSampleRate,
    /// A gain is negative or non-finite.
    InvalidGain,
    /// The requested chip instance is not playable.
    ChipNotFound {
        /// Requested chip type.
        chip: Chip,
        /// Requested instance.
        instance: Instance,
    },
    /// The chip requires a disabled Cargo feature.
    FeatureDisabled {
        /// Chip requiring the feature.
        chip: Chip,
        /// Required Cargo feature name.
        feature: &'static str,
    },
    /// Native OKIM6258 3-bit ADPCM decoding is unsupported.
    UnsupportedOkiAdpcm,
    /// A ROM or RAM data block is invalid.
    InvalidRomBlock,
    /// Chip memory exceeds the supported size.
    RomTooLarge,
    /// The command stream ended without an end command.
    UnexpectedEnd,
    /// The sequencer cannot resume after a rendering or checkpoint restore error.
    StreamFailed,
    /// The playback duration exceeds supported limits.
    DurationOverflow,
    /// PWM compatibility was configured after playback began.
    PwmCompatibilityConfigurationAfterPlayback,
    /// Fadeout was configured after playback began.
    FadeoutConfigurationAfterPlayback,
    /// A checkpoint is incompatible with this sequencer.
    CheckpointMismatch,
    /// A single-pass generator cannot be reconstructed for a checkpoint.
    CheckpointUnsupported,
    /// A generator clock range is invalid or refers to an absent dynamic-clock chip.
    InvalidGeneratorClockRange {
        /// Chip whose declared clock range is invalid.
        chip: Chip,
        /// Instance whose declared clock range is invalid.
        instance: Instance,
    },
    /// The original stream could not be reconstructed for a checkpoint.
    StreamReconstruction(String),
    /// A seek target precedes its starting checkpoint.
    SeekTargetBeforeCheckpoint {
        /// Requested output-frame position.
        target: usize,
        /// Checkpoint output-frame position.
        checkpoint: usize,
    },
    /// Playback ended before the requested seek position.
    SeekTargetBeyondEnd {
        /// Requested output-frame position.
        target: usize,
        /// Output-frame position reached at the end.
        reached: usize,
    },
    /// A seek was cancelled by its caller.
    SeekCancelled,
    /// The pending callback event queue reached its limit.
    EventQueueFull {
        /// Maximum pending event count.
        limit: usize,
    },
    /// A chip's pending write queue reached its limit.
    WriteQueueFull {
        /// Target chip type.
        chip: Chip,
        /// Target chip instance.
        instance: Instance,
        /// Maximum pending write count.
        limit: usize,
    },
    /// Parsing or streaming failed.
    Parse(soundlog::ParseError),
    /// A chip backend failed.
    Chip(ChipError),
    /// Capturing native chip output failed.
    Route(RouteError),
    /// Audio processing failed.
    Audio(AudioError),
    /// An effect configuration or handle is invalid.
    Effect(EffectError),
}

impl fmt::Display for VgmSequencerError {
    /// Format the VGM playback failure.
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
            Self::StreamFailed => write!(
                f,
                "sequencer cannot continue after a rendering or checkpoint restore error"
            ),
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
            Self::CheckpointUnsupported => {
                write!(
                    f,
                    "checkpoints are not supported for single-pass generators"
                )
            }
            Self::InvalidGeneratorClockRange { chip, instance } => {
                write!(f, "invalid generator clock range for {chip:?} {instance:?}")
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
            Self::Effect(error) => write!(f, "effect configuration failed: {error}"),
        }
    }
}

impl Error for VgmSequencerError {
    /// Return the underlying parse, chip, routing, or audio error.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(e) => Some(e),
            Self::Chip(e) => Some(e),
            Self::Route(e) => Some(e),
            Self::Audio(e) => Some(e),
            Self::Effect(e) => Some(e),
            _ => None,
        }
    }
}

impl From<EffectError> for VgmSequencerError {
    fn from(error: EffectError) -> Self {
        Self::Effect(error)
    }
}

impl From<soundlog::ParseError> for VgmSequencerError {
    /// Wrap a soundlog parse failure.
    fn from(e: soundlog::ParseError) -> Self {
        Self::Parse(e)
    }
}
impl From<ChipError> for VgmSequencerError {
    /// Wrap a chip backend failure.
    fn from(e: ChipError) -> Self {
        Self::Chip(e)
    }
}
impl From<RouteError> for VgmSequencerError {
    /// Wrap a chip-routing failure.
    fn from(e: RouteError) -> Self {
        Self::Route(e)
    }
}
impl From<AudioError> for VgmSequencerError {
    /// Wrap an audio-processing failure.
    fn from(e: AudioError) -> Self {
        Self::Audio(e)
    }
}

/// ROM contents and write overlays shared by voices using the same memory.
#[derive(Clone, Default)]
struct Memory {
    /// ADPCM-A sample ROM.
    adpcm_a: Vec<u8>,
    /// ADPCM-B sample ROM.
    adpcm_b: Vec<u8>,
    /// PCM sample ROM.
    pcm: Vec<u8>,
    /// Writes indexed by memory class and address.
    writes: HashMap<(u8, u32), u8>,
}

impl Memory {
    /// Map supported external-memory classes to write-overlay keys.
    fn key(access: AccessClass) -> Option<u8> {
        match access {
            AccessClass::AdpcmA => Some(0),
            AccessClass::AdpcmB => Some(1),
            AccessClass::Pcm => Some(2),
            _ => None,
        }
    }

    /// Borrow the ROM backing a supported external-memory class.
    fn get(&self, access: AccessClass) -> Option<&Vec<u8>> {
        match access {
            AccessClass::AdpcmA => Some(&self.adpcm_a),
            AccessClass::AdpcmB => Some(&self.adpcm_b),
            AccessClass::Pcm => Some(&self.pcm),
            _ => None,
        }
    }
    /// Mutably borrow the ROM backing a supported external-memory class.
    fn get_mut(&mut self, access: AccessClass) -> Option<&mut Vec<u8>> {
        match access {
            AccessClass::AdpcmA => Some(&mut self.adpcm_a),
            AccessClass::AdpcmB => Some(&mut self.adpcm_b),
            AccessClass::Pcm => Some(&mut self.pcm),
            _ => None,
        }
    }
}

/// Record Game Gear PSG stereo-mask writes for each chip instance.
fn observe_psg_stereo(stereo: &mut [bool; 2], command: &VgmCommand) {
    if let VgmCommand::GameGearPsgWrite(instance, spec) = command
        && spec.value != 0xff
    {
        stereo[*instance as usize] = true;
    }
}

/// Chip instance and the state required to render it on the output timeline.
struct Voice {
    /// Chip type and instance.
    chip: Chip,
    instance: Instance,
    /// Effective input clock in Hz.
    clock: u32,
    /// PSG flags used when constructing a state tracker.
    sn_flags: soundlog::vgm::header::Sn76489Flags,
    /// Optional state observer of unfiltered writes.
    tracker: Option<event::Tracker>,
    /// Backend and its native-output capture.
    route: ChipCapture<Box<dyn SoundChip>>,
    /// Current native-to-stereo routing.
    matrix: StereoMatrix,
    /// Native-to-output-rate conversion and its history.
    resampler: StereoResampler,
    /// Memory shared with backends that access external samples.
    memory: Rc<RefCell<Memory>>,
    /// Fractional native-sample phase and cached conversion ratio.
    phase: u32,
    phase_rate: Option<SampleRate>,
    phase_step: u32,
    /// Resampled frames awaiting the sequencer mix.
    queue: VecDeque<[f32; 2]>,
    /// Post-resampling chip gain.
    gain: f32,
    /// Per-native-output gains applied before resampling.
    output_gains: Vec<f32>,
    /// Native and grouped effects and pan overrides.
    effects: EffectRack,
    /// Unmasked routing coefficients used when applying pan.
    base_coefficients: Vec<[f32; 2]>,
    /// Chip-controlled stereo enable masks for each output.
    output_masks: Vec<[bool; 2]>,
    /// Source-derived main and paired-output volume coefficients.
    main_coefficient: f32,
    paired_coefficient: f32,
    /// Writes waiting for their native-sample pacing interval.
    paced_writes: Option<VecDeque<ChipRegisterWrite>>,
    /// RF5C waveform bank used by VGM RAM transfers.
    rf5c_wave_bank: usize,
    /// Frames between paced writes and remaining frames until the next write.
    write_interval: usize,
    write_cooldown: usize,
}

/// Callback registrations and pending events shared while rendering a voice.
struct EventContext<'a, 'callbacks> {
    callbacks: &'a CallbackRegistry<'callbacks>,
    events: &'a mut VecDeque<(usize, PendingWriteEvent)>,
}

struct WritePosition {
    event: usize,
    frame: usize,
}

/// Linear output-frame fade, including frames already buffered at its start.
#[derive(Clone)]
struct Fade {
    /// Absolute output frame where the fade begins.
    start_frame: usize,
    /// Buffered frames to emit before applying the fade.
    pending: usize,
    /// Fade length in output frames.
    length: f32,
    /// Output rate used to accumulate elapsed fade time.
    rate: u32,
    /// Completed whole seconds and frames within the current second.
    seconds: u32,
    frames: u32,
}

impl Fade {
    /// Apply the pending delay and linear fade to a block in place.
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
    /// Apply a timed write, update state tracking, and enqueue its callback.
    fn apply_write(
        &mut self,
        write: ChipRegisterWrite,
        position: WritePosition,
        output_rate: u32,
        vgm_initial: bool,
        dac_stream_id: Option<u8>,
        context: &mut EventContext<'_, '_>,
    ) -> Result<bool, VgmSequencerError> {
        let WritePosition {
            event: event_position,
            frame: sample_frame,
        } = position;
        let (bank, register, value) = write.parts();
        let is_wonderswan_memory = matches!(write, ChipRegisterWrite::WonderSwan(_));
        let is_bank_write = matches!(write, ChipRegisterWrite::MultiPcmBank(_))
            || matches!(&write, ChipRegisterWrite::Upd7759(spec) if spec.register == 3);
        let pending_write = context
            .callbacks
            .has_callback(&write)
            .then(|| write.clone());
        let write = context
            .callbacks
            .filter_write(self.instance, write, sample_frame);
        let rf5c_bank = match &write {
            ChipRegisterWrite::Rf5c68U8(spec) if spec.offset == 7 && spec.value & 0x40 == 0 => {
                Some(usize::from(spec.value & 15) << 12)
            }
            ChipRegisterWrite::Rf5c164U8(spec) if spec.offset == 7 && spec.value & 0x40 == 0 => {
                Some(usize::from(spec.value & 15) << 12)
            }
            _ => None,
        };
        let chip = self.route.chip_mut();
        let stop_stream = if let Some(scheduled) = chip.scheduled() {
            let stop_stream = scheduled.stops_dac_stream_on_write(&write);
            if vgm_initial {
                scheduled.apply_vgm_initial_write(write, sample_frame, output_rate)?;
            } else {
                scheduled.apply_register_write_at(write, sample_frame, output_rate)?;
            }
            stop_stream
        } else if let Some(stream_id) = dac_stream_id {
            chip.apply_dac_stream_write(write, stream_id)?;
            false
        } else {
            chip.apply_register_write(write)?;
            false
        };
        if let Some(bank) = rf5c_bank {
            self.rf5c_wave_bank = bank;
        }
        let state_events = if let Some(tracker) = self.tracker.as_mut().filter(|_| !is_bank_write) {
            let changes = if let event::Tracker::WonderSwan(state) = tracker {
                let narrow = u8::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
                    chip: self.chip.clone(),
                    register,
                    value,
                })?;
                if is_wonderswan_memory {
                    state.on_waveform_write(u16::from_be_bytes([bank, register]), narrow)
                } else {
                    let (register, value) =
                        soundlog::chip::state::WonderSwanState::map_vgm_to_wonderswan_register(
                            register, narrow,
                        );
                    state.on_register_write(register, value)
                }
            } else if let event::Tracker::C352(state) = tracker {
                let wide = u16::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
                    chip: self.chip.clone(),
                    register,
                    value,
                })?;
                state.on_register_write(u16::from_be_bytes([bank, register]), wide)
            } else {
                let narrow = u8::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
                    chip: self.chip.clone(),
                    register,
                    value,
                })?;
                tracker.on_register_write(bank, register, narrow)
            };
            Some(changes.unwrap_or_default())
        } else {
            None
        };
        if let Some(write) = pending_write
            && context.callbacks.accepts(&write, state_events.as_deref())
        {
            if context.events.len() >= event::MAX_PENDING_EVENTS {
                if context.callbacks.drop_overflowing_state(&write) {
                    return Ok(stop_stream);
                }
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
        Ok(stop_stream)
    }

    /// Advance the chip timeline, pacing queued writes between native captures.
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
            self.apply_write(
                write,
                WritePosition {
                    event: event_position,
                    frame: sample_frame,
                },
                output_rate,
                false,
                None,
                context,
            )?;
            self.write_cooldown = self.write_interval;
        }
        let remaining = frames - rendered;
        self.capture(remaining)?;
        self.write_cooldown = self.write_cooldown.saturating_sub(remaining);
        Ok(())
    }

    /// Capture native frames, convert them to stereo, and queue resampled output.
    fn capture(&mut self, frames: usize) -> Result<(), VgmSequencerError> {
        if frames == 0 || self.route.chip().has_no_audio_output() {
            return Ok(());
        }
        let samples = frames
            .checked_mul(self.route.chip().channels())
            .ok_or(VgmSequencerError::DurationOverflow)?;
        let mut raw = vec![0; samples];
        let native = self.route.capture(&mut raw)?;
        let stereo = self.effects.process(
            &native,
            &self.matrix,
            &self.base_coefficients,
            &self.output_masks,
            &self.output_gains,
        )?;
        self.queue
            .extend(self.resampler.process(&stereo)?.frames().iter().copied());
        Ok(())
    }
}

/// A VGM player that returns stereo f32 PCM blocks without an audio device.
///
/// Playback requests one traversal by default; VGM header loop settings can
/// change the effective count. Rendering uses lookahead and may consume
/// commands beyond the returned block.
///
/// Writes and waits use the 44,100 Hz VGM timeline. The header selects chip
/// instances and their configuration; supported chips may require Cargo
/// features. Missing features can return [`VgmSequencerError::FeatureDisabled`];
/// absent or unimplemented chips are silent. [`VgmStream`] handles DAC
/// streams, compressed sample blocks and loops.
///
/// T6W28 requires either two built-in PSG halves or two factory backends.
/// OKIM6258's 3-bit flag uses the 4-bit compatibility decoder; its built-in
/// backend queues data and applies a 20 Hz DC blocker. YMF271 routes its first
/// two outputs to stereo, while ES5505/ES5506 output pairs are mixed to stereo.
/// QSound supports VGM ROM blocks and legacy 4 MHz key-on behavior. MultiPCM
/// legacy `/180` clocks are converted to `/224` for all backends.
///
/// YMFM writes are paced per chip, so bursts may delay writes. Queue overflow
/// is an error; pending writes are applied at the final boundary without
/// generating audio. Paced event positions may differ from exact arithmetic
/// due to `f32` rounding.
///
/// Header volumes and backend balancing multiply per-chip gains, including for
/// custom backends; resulting levels may differ from other players.
///
/// PCM is not clipped or normalized. Rendering can allocate and is not
/// guaranteed real-time safe. After a rendering or checkpoint restore error,
/// create a new sequencer.
pub struct VgmSequencer<'a> {
    /// Source command stream.
    stream: VgmStream,
    /// Optional factory for reconstructing the source stream.
    stream_factory: Option<StreamFactory>,
    /// Whether this source supports checkpoint reconstruction.
    rewindable: bool,
    /// Header loop base used when resetting a replayable stream.
    initial_loop_base: i8,
    /// Header loop modifier used when resetting a replayable stream.
    initial_loop_modifier: u8,
    /// Number of consumed stream results for checkpoint replay.
    stream_steps: usize,
    /// Stream settings changes and their positions for checkpoint replay.
    stream_settings: Vec<(usize, StreamSetting)>,
    /// Active chip instances.
    voices: Vec<Voice>,
    /// Output sample rate in frames per second.
    output_rate: u32,
    /// Maximum returned block length in output frames.
    chunk_frames: usize,
    /// Number of output frames delivered or skipped by seeking.
    position_frames: usize,
    /// Remaining source wait in 44.1 kHz samples.
    wait: u32,
    /// Output frames available for mixing.
    available: usize,
    /// Fractional output-frame phase.
    output_phase: u32,
    /// Fractional output-frame step per source sample.
    output_step: u32,
    /// Whether the stream has ended.
    ended: bool,
    /// Whether a terminal rendering or checkpoint restore error occurred.
    failed: bool,
    /// Whether rendering has started.
    started: bool,
    /// Registered write callbacks and filters.
    callbacks: CallbackRegistry<'a>,
    /// Callback events pending output delivery, with frame offsets.
    events: VecDeque<(usize, PendingWriteEvent)>,
    /// Frames preserved when seeking into the middle of a block.
    pending_output: VecDeque<[f32; 2]>,
    /// Per-instance PWM initialization compatibility state, when enabled.
    pwm_compatibility: Option<[PwmCompatibility; 2]>,
    /// Configured fade duration in source samples.
    fade_samples: Option<u32>,
    /// Remaining source samples after fading starts, or `None` before it starts.
    fade_remaining: Option<u32>,
    /// Output-frame fade state, once fading begins.
    fade_frames: Option<Fade>,
    /// Generator marker signalling the start of fading.
    fade_marker: Option<FadeMarker>,
    /// Identity tying checkpoints to this sequencer.
    source_identity: Arc<()>,
}

/// Meaning of one native output in a sequencer's stereo matrix.
///
/// These are backend outputs, not necessarily individual synthesis channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeOutput {
    /// A mono output.
    Mono,
    /// The left stereo output.
    Left,
    /// The right stereo output.
    Right,
    /// The second left output of a four-output stereo chip.
    LeftAux,
    /// The second right output of a four-output stereo chip.
    RightAux,
    /// A mono FM output.
    Fm,
    /// The left combined FM/ADPCM output.
    FmAdpcmLeft,
    /// The right combined FM/ADPCM output.
    FmAdpcmRight,
    /// An SSG channel, numbered 0 (A), 1 (B), or 2 (C).
    SsgChannel(u8),
    /// A mixed mono SSG output.
    SsgMixed,
    /// YM2413 melodic output.
    Melodic,
    /// YM2413 rhythm output.
    Rhythm,
    /// A native output not routed into the sequencer's stereo mix.
    Unmixed(u8),
}

/// Playback options for a VGM file or parsed document.
///
/// Defaults to requesting one traversal with no fade and preserves the header
/// loop settings, which can change the effective count. The PWM compatibility
/// workaround is enabled by default. Generator sources handle their own looping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VgmSequencerOptions {
    /// Loop count and fade duration.
    pub playback: super::PlaybackOptions,
    /// Override the loop base from the VGM header, or use the header when `None`.
    pub loop_base: Option<i8>,
    /// Override the loop modifier from the VGM header, or use the header when `None`.
    pub loop_modifier: Option<u8>,
    /// Enable the PWM initialization workaround for malformed VGM rips.
    ///
    /// Enabled by default. See [`VgmSequencer::set_pwm_compatibility`] for
    /// the heuristic and its effect on register writes.
    pub pwm_compatibility: bool,
}

impl Default for VgmSequencerOptions {
    fn default() -> Self {
        Self {
            playback: super::PlaybackOptions::default(),
            loop_base: None,
            loop_modifier: None,
            pwm_compatibility: true,
        }
    }
}

/// Initialization information for a single-pass VGM command generator.
///
/// The header and optional extra header declare the same chips, settings,
/// clocks, and volumes as for VGM playback; no command pre-scan is performed.
#[derive(Default)]
pub struct GeneratorConfig {
    /// Chip clocks, settings, and global volume.
    pub header: VgmHeader,
    /// Optional per-instance clocks and volumes.
    pub extra_header: Option<VgmExtraHeader>,
    /// Per-instance inclusive clock bounds for OKIM6258 and OKIM6295.
    ///
    /// Include every clock reached during playback, including intermediate
    /// values from individual clock-byte writes. The initial clock is included
    /// automatically. Out-of-range sample rates cause a rendering error.
    pub clock_ranges: Vec<(Chip, Instance, RangeInclusive<u32>)>,
    /// Whether each SN76489 instance uses a non-default Game Gear stereo mask.
    ///
    /// Set this for external factories when the header enables stereo.
    pub sn76489_stereo_used: [bool; 2],
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

/// Snapshot of one voice's backend, routing, and pending audio.
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
    output_gains: Vec<f32>,
    effects: EffectRack,
    output_masks: Vec<[bool; 2]>,
    main_coefficient: f32,
    paired_coefficient: f32,
    paced_writes: Option<VecDeque<ChipRegisterWrite>>,
    rf5c_wave_bank: usize,
    write_interval: usize,
    write_cooldown: usize,
}

/// Collect distinct memory allocations shared among chip voices.
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

/// Reapply a saved loop or fade setting to a reconstructed stream.
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

/// Reset a reconstructed stream to the sequencer's playback defaults.
fn reset_replay_stream(stream: &mut VgmStream, loop_base: i8, loop_modifier: u8) {
    stream.reset();
    stream.set_loop_count(Some(1));
    stream.set_loop_base(loop_base);
    stream.set_loop_modifier(loop_modifier);
    stream.set_fadeout_samples(None);
    stream.set_emit_stream_controls(true);
}

/// Replay a fixed number of commands while restoring saved stream settings.
fn replay_stream(
    stream: &mut VgmStream,
    steps: usize,
    settings: &[(usize, StreamSetting)],
) -> Result<(), VgmSequencerError> {
    let mut setting_index = 0;
    for step in 0..=steps {
        while settings
            .get(setting_index)
            .is_some_and(|(at, _)| *at == step)
        {
            apply_stream_setting(stream, &settings[setting_index].1);
            setting_index += 1;
        }
        if step == steps {
            break;
        }
        let result = stream
            .next()
            .ok_or(VgmSequencerError::UnexpectedEnd)?
            .map_err(VgmSequencerError::Parse)?;
        if matches!(result, StreamResult::EndOfStream) && step + 1 != steps {
            return Err(VgmSequencerError::UnexpectedEnd);
        }
    }
    if setting_index != settings.len() {
        return Err(VgmSequencerError::CheckpointMismatch);
    }
    Ok(())
}

impl<'a> VgmSequencer<'a> {
    /// Create a sequencer from an uncompressed VGM file.
    ///
    /// Commands are parsed on demand. The file bytes remain in memory for
    /// looping; passing a `Vec<u8>` transfers its allocation to the stream.
    /// Passing a borrowed byte slice copies it into an owned buffer.
    /// Decompress VGZ before calling this method. Sample data and audio buffers
    /// may allocate separately. GD3 metadata is not parsed or validated.
    ///
    /// For OKIM6258/6295, construction scans commands to determine the
    /// resampler's clock range. This scan may report malformed commands earlier
    /// and skips data-block payloads without copying them.
    pub fn from_vgm(
        data: impl Into<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
        options: VgmSequencerOptions,
    ) -> Result<Self, VgmSequencerError> {
        Self::from_vgm_with_factories(
            data,
            output_rate,
            chunk_frames,
            options,
            ChipFactoryRegistry::new(),
        )
    }

    /// Create a sequencer with replacement factories for selected chip types.
    ///
    /// Factories replace the built-in backend only for registered chip types.
    /// They receive the chip instance, effective clock, and chip-specific settings.
    /// If an SN76489 factory is registered and the header enables Game Gear
    /// stereo, commands are also scanned for non-default stereo masks. This
    /// may report malformed commands during construction; data-block payloads
    /// are skipped without copying them.
    pub fn from_vgm_with_factories(
        data: impl Into<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
        options: VgmSequencerOptions,
        mut factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let data = data.into();
        let header = VgmHeader::from_bytes(&data)?;
        let extra = header.extra_header_from_bytes(&data)?;
        let mut config = ChipConfiguration::from_header(&header, extra.as_ref(), true);
        let scan_psg = header.sn76489_clock & 0x3fff_ffff != 0
            && header.sn76489_flags.gamegear_stereo
            && factories.contains(&Chip::Sn76489);
        let mut psg_stereo = [false; 2];
        if config.timing.needs_scan() || scan_psg {
            visit_non_data_block_commands(&data, &header, |command| {
                config.timing.observe(command);
                if scan_psg {
                    observe_psg_stereo(&mut psg_stereo, command);
                }
            })?;
        }
        config.set_psg_stereo(psg_stereo);
        let voices = Self::make_voices(config, output_rate, &mut factories)?;
        let stream = VgmStream::from_vgm(data)?;
        let mut player = Self::from_parts(stream, None, true, voices, output_rate, chunk_frames)?;
        player.apply_options(options)?;
        Ok(player)
    }

    /// Construct a backend, routing matrix, and resampler for one chip instance.
    fn make_voice(
        header: &VgmHeader,
        extra: Option<&VgmExtraHeader>,
        config: VoiceConfig,
        timing: &Timing,
        output_rate: u32,
        factories: &mut ChipFactoryRegistry,
        _t6w28: &mut T6w28Pair,
    ) -> Result<Voice, VgmSequencerError> {
        let VoiceConfig {
            chip,
            instance,
            clock,
            dual,
            settings,
            okim6258_dc_blocker,
        } = config;
        #[cfg(not(feature = "okim6258"))]
        let _ = okim6258_dc_blocker;
        let profile = TimingProfile::for_chip(&chip);
        let clock = if chip == Chip::MultiPcm {
            (clock / 45) * 56 + (clock % 45) * 56 / 45
        } else if chip == Chip::C140 && clock < 1_000_000 {
            match (u8::from(header.c140_chip_type), clock) {
                (2, 44_100) => 25_056_500,
                (0 | 1, 21_390) => 12_288_000,
                _ => clock * 576,
            }
        } else if chip == Chip::K054539 && clock < 1_000_000 {
            clock * 384
        } else {
            clock
        };
        let memory = Rc::new(RefCell::new(Memory::default()));
        let factory_config = ChipFactoryConfig::new(
            chip.clone(),
            instance,
            clock,
            output_rate,
            settings,
            (header.okim6258_clock & 0x3fff_ffff != 0)
                .then_some(header.okim6258_clock & 0x3fff_ffff),
        );
        let backend: Box<dyn SoundChip> = if let Some(backend) =
            factories.create(&factory_config)?
        {
            backend
        } else if chip == Chip::Ay8910 {
            return Err(ChipError::UnsupportedChip(chip).into());
        } else if chip == Chip::Sn76489 {
            feature_backend!("sn76496", chip, {
                let ChipBackendSettings::Sn76489(settings) = factory_config.settings() else {
                    return Err(ChipError::InvalidConfiguration {
                        chip,
                        reason: "missing SN76489 settings",
                    }
                    .into());
                };
                if settings.t6w28 {
                    let side = instance as usize;
                    _t6w28.built_in |= 1 << side;
                    Box::new(T6w28Half::new(
                        clock,
                        (*settings).into(),
                        side,
                        Rc::clone(&_t6w28.volumes),
                    )?)
                } else {
                    Box::new(Sn76496::with_config(clock, (*settings).into())?)
                }
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
        } else if chip == Chip::GbDmg {
            feature_backend!("gb_dmg", chip, Box::new(GbDmg::new(clock)?))
        } else if chip == Chip::WonderSwan {
            feature_backend!("wonderswan", chip, Box::new(WonderSwan::new(clock)?))
        } else if chip == Chip::Vsu {
            feature_backend!("vsu", chip, Box::new(Vsu::new(clock)?))
        } else if chip == Chip::Es5503 {
            feature_backend!("es5503", chip, {
                let ChipBackendSettings::Es5503 { output_channels } = factory_config.settings()
                else {
                    return Err(ChipError::InvalidConfiguration {
                        chip: Chip::Es5503,
                        reason: "missing ES5503 output channel configuration",
                    }
                    .into());
                };
                Box::new(Es5503::with_output_channels(
                    clock,
                    (*output_channels).max(1),
                )?)
            })
        } else if chip == Chip::Scsp {
            feature_backend!("scsp", chip, Box::new(Scsp::new(clock)?))
        } else if matches!(chip, Chip::Es5505 | Chip::Es5506U8 | Chip::Es5506U16) {
            feature_backend!("es550x", chip, Box::new(Es550x::new(chip.clone(), clock)?))
        } else if matches!(chip, Chip::Rf5c68 | Chip::Rf5c164) {
            feature_backend!("rf5c", chip, Box::new(Rf5c::new(chip.clone(), clock)?))
        } else if chip == Chip::Saa1099 {
            feature_backend!("saa1099", chip, Box::new(Saa1099::new(clock)?))
        } else if chip == Chip::K051649 {
            feature_backend!("k051649", chip, Box::new(K051649::new(clock)?))
        } else if chip == Chip::Huc6280 {
            feature_backend!("huc6280", chip, Box::new(Huc6280::new(clock)?))
        } else if chip == Chip::Pokey {
            feature_backend!("pokey", chip, Box::new(Pokey::for_vgm(clock)?))
        } else if chip == Chip::Ga20 {
            feature_backend!("ga20", chip, Box::new(Ga20::new(clock)?))
        } else if chip == Chip::K053260 {
            feature_backend!("k053260", chip, Box::new(K053260::new(clock)?))
        } else if chip == Chip::K054539 {
            feature_backend!("k054539", chip, {
                let ChipBackendSettings::K054539 {
                    reverse_stereo,
                    disable_reverb,
                    update_at_keyon,
                    ..
                } = factory_config.settings()
                else {
                    return Err(ChipError::InvalidConfiguration {
                        chip,
                        reason: "missing K054539 settings",
                    }
                    .into());
                };
                Box::new(K054539::with_config(
                    clock,
                    K054539Config {
                        reverse_stereo: *reverse_stereo,
                        disable_reverb: *disable_reverb,
                        update_at_keyon: *update_at_keyon,
                    },
                )?)
            })
        } else if chip == Chip::X1010 {
            feature_backend!("x1010", chip, Box::new(X1010::new(clock)?))
        } else if chip == Chip::Ymz280b {
            feature_backend!("ymz280b", chip, Box::new(Ymz280b::new(clock)?))
        } else if chip == Chip::Ymf271 {
            feature_backend!("ymf271", chip, Box::new(Ymf271::new(clock)?))
        } else if chip == Chip::Qsound {
            feature_backend!("qsound", chip, Box::new(Qsound::for_vgm(clock)?))
        } else if chip == Chip::MultiPcm {
            feature_backend!("multipcm", chip, Box::new(MultiPcm::new(clock)?))
        } else if chip == Chip::Mikey {
            feature_backend!("mikey", chip, Box::new(Mikey::new(clock)?))
        } else if chip == Chip::NesApu {
            if matches!(factory_config.settings(), ChipBackendSettings::NesApuFds) {
                feature_backend!("nes_fds", chip, Box::new(NesApu::with_fds(clock)?))
            } else {
                feature_backend!("nes_apu", chip, Box::new(NesApu::new(clock)?))
            }
        } else if chip == Chip::Upd7759 {
            feature_backend!("upd7759", chip, {
                let ChipBackendSettings::Upd7759 { slave } = factory_config.settings() else {
                    return Err(ChipError::InvalidConfiguration {
                        chip: Chip::Upd7759,
                        reason: "missing uPD7759 settings",
                    }
                    .into());
                };
                Box::new(Upd7759::with_slave_mode(clock, *slave)?)
            })
        } else if chip == Chip::C352 {
            feature_backend!("c352", chip, {
                let ChipBackendSettings::C352 { divider, .. } = factory_config.settings() else {
                    return Err(ChipError::InvalidConfiguration {
                        chip,
                        reason: "missing C352 settings",
                    }
                    .into());
                };
                Box::new(C352::with_divider(clock, *divider)?)
            })
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
                let ChipBackendSettings::Okim6258(settings) = factory_config.settings() else {
                    return Err(ChipError::InvalidConfiguration {
                        chip,
                        reason: "missing OKIM6258 settings",
                    }
                    .into());
                };
                // The MAME-backed vgm2wav compatibility path also ignores this selector.
                let mut oki = Okim6258::new(clock)?;
                oki.set_divider(match settings.divider {
                    Okim6258Divider::Div1024 => ClockDivider::Div1024,
                    Okim6258Divider::Div768 => ClockDivider::Div768,
                    Okim6258Divider::Div512 => ClockDivider::Div512,
                })?;
                oki.set_output_bits(if settings.output_12bit {
                    OutputBits::Bits12
                } else {
                    OutputBits::Bits10
                });
                let stream = Okim6258Stream::new(oki).with_direct_write_queue();
                Box::new(if okim6258_dc_blocker {
                    stream.with_dc_blocker()
                } else {
                    stream
                })
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
        let id = chip_id(&chip);
        let has_ym2612 = header
            .misc()
            .use_ym2413_clock_for_ym2612
            .unwrap_or(header.ym2612_clock)
            & 0x3fff_ffff
            != 0;
        let built_in_t6w28 = _t6w28.built_in & (1 << instance as usize) != 0;
        let t6w28_gain = if built_in_t6w28 { 2.0 } else { 1.0 };
        let main = SCALE
            * global
            * t6w28_gain
            * volume::mix_gain(&chip, false, has_ym2612)
            * volume::chip_gain(extra, &chip, id, instance, false, dual && !built_in_t6w28);
        let paired = SCALE
            * global
            * volume::mix_gain(&chip, true, has_ym2612)
            * volume::chip_gain(extra, &chip, id, instance, true, dual);
        let coefficients = match chip {
            Chip::Ay8910 if backend.channels() == 3 => vec![[main / 3.0, main / 3.0]; 3],
            Chip::Ym2203 => vec![
                [main, main],
                [paired, paired],
                [paired, paired],
                [paired, paired],
            ],
            Chip::Ym2608 | Chip::Ym2610b => vec![[main, 0.0], [0.0, main], [paired, paired]],
            Chip::Ym2413 => vec![[main, main]; 2],
            Chip::Ymf262 => vec![[main, 0.0], [0.0, main], [main, 0.0], [0.0, main]],
            Chip::Ymf271 => vec![
                [main, 0.0],
                [0.0, main],
                [0.0, 0.0],
                [0.0, 0.0],
                [0.0, 0.0],
                [0.0, 0.0],
                [0.0, 0.0],
                [0.0, 0.0],
            ],
            Chip::C352 => {
                let rear = if header.c352_clock & 0x8000_0000 != 0 {
                    0.0
                } else {
                    main
                };
                vec![[main, 0.0], [0.0, main], [rear, 0.0], [0.0, rear]]
            }
            Chip::Ymf278b => vec![[0.0, 0.0]; 4]
                .into_iter()
                .chain([[main, 0.0], [0.0, main]])
                .collect(),
            _ if backend.channels() == 1 => vec![[main, main]],
            _ => vec![[main, 0.0], [0.0, main]],
        };
        let matrix = StereoMatrix::new(coefficients)?;
        let expected_channels = if chip == Chip::Ay8910 && backend.channels() == 3 {
            3
        } else {
            Self::matrix_channels(&chip)
        };
        if backend.channels() != expected_channels {
            return Err(VgmSequencerError::Audio(AudioError::ChannelMismatch {
                expected: expected_channels,
                actual: backend.channels(),
            }));
        }
        let (min_rate, max_rate) =
            timing.bounds(&chip, instance, clock, backend.sample_rate_ratio());
        let output_gains = vec![1.0; backend.channels()];
        let effects = EffectRack::new(backend.channels());
        let output_masks = vec![[true; 2]; backend.channels()];
        let base_coefficients = matrix.coefficients().to_vec();
        let resampler = if matches!(
            chip,
            Chip::Es5503 | Chip::Es5505 | Chip::Es5506U8 | Chip::Es5506U16
        ) {
            StereoResampler::new_adaptive(output_rate, min_rate, max_rate, SRC_CHUNK)?
        } else {
            StereoResampler::new(output_rate, min_rate, max_rate, SRC_CHUNK)?
        };
        Ok(Voice {
            chip,
            instance,
            clock,
            rf5c_wave_bank: 0,
            sn_flags: header.sn76489_flags,
            tracker: None,
            route: ChipCapture::new(backend),
            matrix,
            resampler,
            memory,
            phase: 0,
            phase_rate: None,
            phase_step: 0,
            queue: VecDeque::new(),
            gain: 1.0,
            output_gains,
            effects,
            base_coefficients,
            output_masks,
            main_coefficient: main,
            paired_coefficient: paired,
            paced_writes: profile.write_interval.map(|_| VecDeque::new()),
            write_interval: profile.write_interval.unwrap_or(1),
            write_cooldown: 0,
        })
    }

    /// Return the native output count expected by the chip's stereo matrix.
    fn matrix_channels(chip: &Chip) -> usize {
        match chip {
            Chip::Ym2203 | Chip::Ymf262 | Chip::C352 => 4,
            Chip::Ym2608 | Chip::Ym2610b => 3,
            Chip::Ymf278b => 6,
            Chip::Ymf271 => 8,
            Chip::Ym2413
            | Chip::Ym2151
            | Chip::Ym2612
            | Chip::Sn76489
            | Chip::SegaPcm
            | Chip::Pwm
            | Chip::Rf5c68
            | Chip::Rf5c164
            | Chip::Es5503
            | Chip::Scsp
            | Chip::Es5505
            | Chip::Es5506U8
            | Chip::Es5506U16
            | Chip::GbDmg
            | Chip::WonderSwan
            | Chip::Vsu
            | Chip::Saa1099
            | Chip::Huc6280
            | Chip::Ga20
            | Chip::K053260
            | Chip::K054539
            | Chip::X1010
            | Chip::Ymz280b
            | Chip::Qsound
            | Chip::MultiPcm
            | Chip::Mikey
            | Chip::C140 => 2,
            _ => 1,
        }
    }

    /// Create a sequencer from a parsed document.
    pub fn new(
        document: VgmDocument,
        output_rate: u32,
        chunk_frames: usize,
        options: VgmSequencerOptions,
    ) -> Result<Self, VgmSequencerError> {
        Self::new_with_factories(
            document,
            output_rate,
            chunk_frames,
            options,
            ChipFactoryRegistry::new(),
        )
    }

    /// Create a sequencer from a parsed document with selected chip factories.
    pub fn new_with_factories(
        document: VgmDocument,
        output_rate: u32,
        chunk_frames: usize,
        options: VgmSequencerOptions,
        mut factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let mut config =
            ChipConfiguration::from_header(&document.header, document.extra_header.as_ref(), true);
        let scan_psg = document.header.sn76489_clock & 0x3fff_ffff != 0
            && document.header.sn76489_flags.gamegear_stereo
            && factories.contains(&Chip::Sn76489);
        let mut psg_stereo = [false; 2];
        if config.timing.needs_scan() || scan_psg {
            let mut psg_ended = false;
            for command in &document.commands {
                config.timing.observe(command);
                if matches!(command, VgmCommand::EndOfData(_)) {
                    psg_ended = true;
                }
                if scan_psg && !psg_ended {
                    observe_psg_stereo(&mut psg_stereo, command);
                }
            }
        }
        config.set_psg_stereo(psg_stereo);
        let voices = Self::make_voices(config, output_rate, &mut factories)?;
        let stream = VgmStream::from_document(document);
        let mut player = Self::from_parts(stream, None, true, voices, output_rate, chunk_frames)?;
        player.apply_options(options)?;
        Ok(player)
    }

    /// Play commands generated lazily using the supplied chip configuration.
    ///
    /// The generator must emit `EndOfData` before returning `None`; otherwise
    /// rendering fails with [`VgmSequencerError::UnexpectedEnd`]. It handles
    /// its own loops. Stream-level loop and fade settings do not provide a
    /// generator loop point. Checkpoints are unsupported for this single-pass
    /// source. The generator is called synchronously during rendering, which
    /// may read ahead and is not guaranteed real-time safe.
    pub fn from_generator(
        generator: Box<dyn VgmCommandGenerator>,
        config: GeneratorConfig,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        Self::from_generator_with_factories(
            generator,
            config,
            output_rate,
            chunk_frames,
            ChipFactoryRegistry::new(),
        )
    }

    /// Play a generator with selected external chip factories.
    ///
    /// See [`Self::from_generator`] for the source contract.
    pub fn from_generator_with_factories(
        generator: Box<dyn VgmCommandGenerator>,
        config: GeneratorConfig,
        output_rate: u32,
        chunk_frames: usize,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let mut prepared =
            ChipConfiguration::from_header(&config.header, config.extra_header.as_ref(), true);
        prepared.set_psg_stereo(config.sn76489_stereo_used);
        for (chip, instance, range) in &config.clock_ranges {
            if !matches!(chip, Chip::Okim6258 | Chip::Okim6295)
                || range.is_empty()
                || *range.start() == 0
                || (*chip == Chip::Okim6295 && *range.end() > 0x7fff_ffff)
                || !prepared
                    .voices
                    .iter()
                    .any(|voice| &voice.chip == chip && voice.instance == *instance)
            {
                return Err(VgmSequencerError::InvalidGeneratorClockRange {
                    chip: chip.clone(),
                    instance: *instance,
                });
            }
            prepared
                .timing
                .include_range(chip, *instance, *range.start(), *range.end());
        }
        Self::from_generator_parts(
            generator,
            None,
            prepared,
            output_rate,
            chunk_frames,
            factories,
        )
    }

    /// Create a generator-backed sequencer, optionally with a replay source.
    pub(super) fn from_generator_parts(
        generator: Box<dyn VgmCommandGenerator>,
        stream_factory: Option<StreamFactory>,
        config: ChipConfiguration<'_>,
        output_rate: u32,
        chunk_frames: usize,
        mut factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        Self::validate_output(output_rate, chunk_frames)?;
        let voices = Self::make_voices(config, output_rate, &mut factories)?;
        let rewindable = stream_factory.is_some();
        Self::from_parts(
            VgmStream::from_generator(generator),
            stream_factory,
            rewindable,
            voices,
            output_rate,
            chunk_frames,
        )
    }

    /// Reject zero output rates and chunk sizes before allocating voices.
    fn validate_output(output_rate: u32, chunk_frames: usize) -> Result<(), VgmSequencerError> {
        if output_rate == 0 {
            return Err(VgmSequencerError::InvalidSampleRate);
        }
        if chunk_frames == 0 {
            return Err(VgmSequencerError::InvalidChunkSize);
        }
        Ok(())
    }

    /// Create voices from the prepared per-instance settings.
    fn make_voices(
        config: ChipConfiguration<'_>,
        output_rate: u32,
        factories: &mut ChipFactoryRegistry,
    ) -> Result<Vec<Voice>, VgmSequencerError> {
        let mut voices = Vec::new();
        let mut t6w28 = T6w28Pair::default();
        for voice in config.voices {
            if voice.chip == Chip::Ay8910 && !factories.contains(&voice.chip) {
                continue;
            }
            voices.push(Self::make_voice(
                config.header,
                config.extra,
                voice,
                &config.timing,
                output_rate,
                factories,
                &mut t6w28,
            )?);
        }
        if t6w28.built_in != 0 && t6w28.built_in != 3 {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "T6W28 requires two built-in halves or two factory backends",
            }
            .into());
        }
        Ok(voices)
    }

    /// Initialize playback state from an existing stream and constructed voices.
    fn from_parts(
        mut stream: VgmStream,
        stream_factory: Option<StreamFactory>,
        rewindable: bool,
        voices: Vec<Voice>,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        let initial_loop_base = stream.loop_base();
        let initial_loop_modifier = stream.loop_modifier();
        stream.set_emit_stream_controls(true);
        Ok(Self {
            stream,
            stream_factory,
            rewindable,
            initial_loop_base,
            initial_loop_modifier,
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
            fade_marker: None,
            source_identity: Arc::new(()),
        })
    }

    fn apply_options(&mut self, options: VgmSequencerOptions) -> Result<(), VgmSequencerError> {
        let fadeout = super::options::fadeout_samples(options.playback.fadeout)?;
        self.configure_fadeout(fadeout, None)?;
        self.set_pwm_compatibility(options.pwm_compatibility)?;
        self.stream.set_loop_count(options.playback.loop_count);
        self.stream_settings.push((
            self.stream_steps,
            StreamSetting::LoopCount(options.playback.loop_count),
        ));
        self.stream
            .set_fadeout_samples(fadeout.map(|samples| samples as usize));
        self.stream_settings
            .push((self.stream_steps, StreamSetting::FadeoutSamples(fadeout)));
        if let Some(base) = options.loop_base {
            self.stream.set_loop_base(base);
            self.stream_settings
                .push((self.stream_steps, StreamSetting::LoopBase(base)));
        }
        if let Some(modifier) = options.loop_modifier {
            self.stream.set_loop_modifier(modifier);
            self.stream_settings
                .push((self.stream_steps, StreamSetting::LoopModifier(modifier)));
        }
        Ok(())
    }

    /// Set the fade duration and optional generator loop marker before playback.
    pub(super) fn configure_fadeout(
        &mut self,
        samples: Option<u32>,
        marker: Option<FadeMarker>,
    ) -> Result<(), VgmSequencerError> {
        if self.started {
            return Err(VgmSequencerError::FadeoutConfigurationAfterPlayback);
        }
        self.fade_samples = samples.filter(|samples| *samples != 0);
        self.fade_marker = marker;
        Ok(())
    }

    /// Enable or disable the bad-rip PWM channel initialization workaround.
    ///
    /// Enabled by default. For VGM files and documents, this overrides
    /// [`VgmSequencerOptions::pwm_compatibility`]. Configuration is only
    /// allowed before rendering, including for generator sources.
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

    /// Transform a typed chip write immediately before it reaches its chip.
    ///
    /// The filter receives the source instance and absolute output-frame position.
    /// It returns one write of the same type; it cannot add or suppress writes.
    /// Registering another filter for the same type replaces the previous one.
    /// Observers and state trackers receive the original write. Filter calls may
    /// occur ahead of returned audio and are not globally ordered across voices;
    /// they do not imply that a hardware device has played the write. Changes
    /// do not affect already generated audio or scheduled hardware writes.
    /// Filter closure state is not captured or restored by checkpoints. The
    /// caller must restore its state consistently before restoring or seeking;
    /// seeking invokes the filter while advancing from the checkpoint.
    pub fn filter_write<C, F>(&mut self, filter: F)
    where
        C: event::VgmWriteCallbackTarget,
        F: FnMut(Instance, C, usize) -> C + 'a,
    {
        self.callbacks.register_filter::<C, F>(filter);
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
        self.callbacks.register::<C, F>(callback, false);
    }

    /// Register a callback only for writes that produced tracked state changes.
    ///
    /// Enable the corresponding tracker before rendering. Like [`Self::on_write`],
    /// this replaces any existing callback for the same write type. If the
    /// event buffer fills, state-only events are dropped without stopping
    /// playback; check [`Self::take_state_event_overflow`] after rendering.
    pub fn on_state_change<C, F>(&mut self, mut callback: F)
    where
        C: event::VgmWriteCallbackTarget,
        F: FnMut(Instance, usize, Vec<soundlog::chip::event::StateEvent>) + 'a,
    {
        self.callbacks.register::<C, _>(
            move |instance, _, frame, events| {
                if let Some(events) = events {
                    callback(instance, frame, events);
                }
            },
            true,
        );
    }

    /// Report and clear whether any state-only events were dropped.
    ///
    /// After an overflow, a consumer's reconstructed state may be incomplete.
    pub fn take_state_event_overflow(&mut self) -> bool {
        self.callbacks.take_state_overflow()
    }

    /// Enable state tracking for a chip type and instance.
    ///
    /// State tracking is available for SN76489, YM2151, YM2203, YM2413, YM2608, YM2610B,
    /// YM2612, YM3526, Y8950, YM3812, YMF262, YMF278B, ES5503, Game Boy DMG,
    /// WonderSwan, and VSU.
    pub fn track_state<S>(&mut self, instance: Instance, clock: f32)
    where
        S: VgmStateTracker,
    {
        let chip = event::tracker_chip(S::KIND);
        for voice in &mut self.voices {
            if voice.chip == chip && voice.instance == instance {
                voice.tracker = Some(event::tracker(S::KIND, clock, voice.sn_flags));
            }
        }
    }

    /// Enable state tracking using the effective clock of each matching voice.
    ///
    /// This includes per-instance clock overrides from the VGM extra header.
    pub fn track_state_with_voice_clocks<S>(&mut self)
    where
        S: VgmStateTracker,
    {
        let chip = event::tracker_chip(S::KIND);
        for voice in &mut self.voices {
            if voice.chip == chip {
                voice.tracker = Some(event::tracker(S::KIND, voice.clock as f32, voice.sn_flags));
            }
        }
    }

    /// Return the output-frame position where the current fade begins, once
    /// rendering has reached its loop boundary.
    ///
    /// Rendering may discover this position ahead of the returned PCM. It is
    /// absent for tracks without a fade or before the boundary is encountered.
    pub fn fade_start_frame(&self) -> Option<usize> {
        self.fade_frames.as_ref().map(|fade| fade.start_frame)
    }

    /// Change the post-resampling mix gain for one chip instance.
    ///
    /// This includes unmixed buffered PCM and multiplies the header volume.
    /// Audio already mixed during a seek is unaffected. Hardware output is
    /// not controlled by this gain.
    pub fn set_chip_gain(
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

    /// Return the playable chip instances in this sequencer.
    pub fn chip_instances(&self) -> impl Iterator<Item = (Chip, Instance)> + '_ {
        self.voices
            .iter()
            .map(|voice| (voice.chip.clone(), voice.instance))
    }

    /// Return the native output layout in the order accepted by
    /// [`Self::set_output_gains`].
    ///
    /// Layouts describe the backend's host PCM outputs, including outputs
    /// not routed into the stereo mix. Hardware audio is not controlled.
    pub fn output_layout(
        &self,
        chip: Chip,
        instance: Instance,
    ) -> Result<&'static [NativeOutput], VgmSequencerError> {
        let voice = self
            .voices
            .iter()
            .find(|voice| voice.chip == chip && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound { chip, instance })?;
        use NativeOutput::*;
        Ok(match voice.chip {
            Chip::Ay8910 if voice.output_gains.len() == 3 => {
                &[SsgChannel(0), SsgChannel(1), SsgChannel(2)]
            }
            Chip::Ym2203 => &[Fm, SsgChannel(0), SsgChannel(1), SsgChannel(2)],
            Chip::Ym2608 | Chip::Ym2610b => &[FmAdpcmLeft, FmAdpcmRight, SsgMixed],
            Chip::Ym2413 => &[Melodic, Rhythm],
            Chip::Ymf262 | Chip::C352 => &[Left, Right, LeftAux, RightAux],
            Chip::Ymf278b => &[Unmixed(0), Unmixed(1), Unmixed(2), Unmixed(3), Left, Right],
            Chip::Ymf271 => &[
                Left,
                Right,
                LeftAux,
                RightAux,
                Unmixed(4),
                Unmixed(5),
                Unmixed(6),
                Unmixed(7),
            ],
            _ if voice.output_gains.len() == 1 => &[Mono],
            _ => &[Left, Right],
        })
    }

    /// Set one gain per native output, in [`Self::output_layout`] order.
    ///
    /// Gains must be finite and nonnegative. They multiply the existing stereo
    /// matrix coefficients, chip gain, and header levels. Changes affect newly
    /// captured PCM; buffered audio and resampler history retain prior levels.
    /// This does not control hardware audio.
    pub fn set_output_gains(
        &mut self,
        chip: Chip,
        instance: Instance,
        gains: &[f32],
    ) -> Result<(), VgmSequencerError> {
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.chip == chip && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound { chip, instance })?;
        if gains.len() != voice.output_gains.len() {
            return Err(AudioError::ChannelMismatch {
                expected: voice.output_gains.len(),
                actual: gains.len(),
            }
            .into());
        }
        if gains.iter().any(|gain| !gain.is_finite() || *gain < 0.0) {
            return Err(VgmSequencerError::InvalidGain);
        }
        voice.output_gains.copy_from_slice(gains);
        Ok(())
    }

    /// Set linear pan for a native output, or restore its original routing with `None`.
    ///
    /// `-1` routes left, `0` routes equally to both sides, and `1` routes
    /// right. VGM channel masks still apply. Changes affect newly captured PCM.
    pub fn set_output_pan(
        &mut self,
        chip: Chip,
        instance: Instance,
        output: usize,
        pan: Option<f32>,
    ) -> Result<(), VgmSequencerError> {
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.chip == chip && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound { chip, instance })?;
        voice.effects.set_pan(output, pan)?;
        Ok(())
    }

    /// Append an effect to one native output, before gain and stereo routing.
    ///
    /// `Clone` must produce an independent snapshot of all effect parameters
    /// and processing state for checkpointing. Effects run at the current
    /// native rate; their tails are truncated at the source's end.
    pub fn add_output_effect<T: NativeEffect + Clone + 'static>(
        &mut self,
        chip: Chip,
        instance: Instance,
        output: usize,
        effect: T,
    ) -> Result<EffectId<T>, VgmSequencerError> {
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.chip == chip && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound { chip, instance })?;
        Ok(voice.effects.add_native(output, effect)?)
    }

    /// Group native outputs for stereo effects. Each output may be in one group.
    ///
    /// Outputs are routed through their normal matrix and masks before group
    /// effects; ungrouped outputs are mixed normally.
    pub fn create_output_group(
        &mut self,
        chip: Chip,
        instance: Instance,
        outputs: &[usize],
    ) -> Result<OutputGroupId, VgmSequencerError> {
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.chip == chip && voice.instance == instance)
            .ok_or(VgmSequencerError::ChipNotFound { chip, instance })?;
        Ok(voice.effects.create_group(outputs)?)
    }

    /// Remove a group and its effects; its outputs return to the ordinary mix.
    pub fn remove_output_group(&mut self, group: &OutputGroupId) -> Result<(), VgmSequencerError> {
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.effects.contains_group(group))
            .ok_or(EffectError::GroupNotFound)?;
        voice.effects.remove_group(group)?;
        Ok(())
    }

    /// Append a stereo effect to an output group.
    ///
    /// `Clone` must produce an independent snapshot of all parameters and
    /// processing state. Effects run at the native rate and their tails are
    /// truncated at the source's end.
    pub fn add_group_effect<T: StereoEffect + Clone + 'static>(
        &mut self,
        group: &OutputGroupId,
        effect: T,
    ) -> Result<EffectId<T>, VgmSequencerError> {
        let voice = self
            .voices
            .iter_mut()
            .find(|voice| voice.effects.contains_group(group))
            .ok_or(EffectError::GroupNotFound)?;
        Ok(voice.effects.add_stereo(group, effect)?)
    }

    /// Access an installed effect's parameters and state.
    ///
    /// Changes affect newly captured PCM, not existing queues or resampler
    /// history. Call between `render` calls, using exclusive sequencer access.
    pub fn effect_mut<T: 'static>(
        &mut self,
        id: &EffectId<T>,
    ) -> Result<&mut T, VgmSequencerError> {
        self.voices
            .iter_mut()
            .find_map(|voice| voice.effects.effect_mut(id))
            .ok_or_else(|| EffectError::EffectNotFound.into())
    }

    /// Remove an installed effect.
    pub fn remove_effect<T>(&mut self, id: &EffectId<T>) -> Result<(), VgmSequencerError> {
        if self
            .voices
            .iter_mut()
            .any(|voice| voice.effects.remove_effect(id))
        {
            Ok(())
        } else {
            Err(EffectError::EffectNotFound.into())
        }
    }

    /// Move an effect to a zero-based position within its existing chain.
    pub fn move_effect<T>(
        &mut self,
        id: &EffectId<T>,
        position: usize,
    ) -> Result<(), VgmSequencerError> {
        for voice in &mut self.voices {
            if voice.effects.move_effect(id, position)? {
                return Ok(());
            }
        }
        Err(EffectError::EffectNotFound.into())
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
    /// Single-pass generators return [`VgmSequencerError::CheckpointUnsupported`].
    pub fn checkpoint(&mut self) -> Result<VgmSequencerCheckpoint, VgmSequencerError> {
        if self.failed {
            return Err(VgmSequencerError::StreamFailed);
        }
        if !self.rewindable {
            return Err(VgmSequencerError::CheckpointUnsupported);
        }

        let mut voices = Vec::with_capacity(self.voices.len());
        for voice in &mut self.voices {
            if let Some(scheduled) = voice.route.chip_mut().scheduled()
                && (!scheduled.supports_checkpoint()
                    || (voice.chip == Chip::Okim6258
                        && scheduled.stops_dac_stream_on_write(&ChipRegisterWrite::Okim6258(
                            soundlog::chip::Okim6258Spec {
                                register: 0,
                                value: 1,
                            },
                        ))))
            {
                return Err(ChipError::UnsupportedStateSnapshot(voice.chip.clone()).into());
            }
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
                output_gains: voice.output_gains.clone(),
                effects: voice.effects.clone(),
                output_masks: voice.output_masks.clone(),
                main_coefficient: voice.main_coefficient,
                paired_coefficient: voice.paired_coefficient,
                paced_writes: voice.paced_writes.clone(),
                rf5c_wave_bank: voice.rf5c_wave_bank,
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
    /// Any restore error permanently prevents further playback or restoration;
    /// create a new sequencer instead.
    pub fn restore_checkpoint(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
    ) -> Result<(), VgmSequencerError> {
        if self.failed {
            return Err(VgmSequencerError::StreamFailed);
        }
        let result = self.restore_checkpoint_inner(checkpoint);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn restore_checkpoint_inner(
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
                        || !voice.effects.same_topology(&saved.effects)
                })
            || unique_memories(&self.voices).len() != checkpoint.memory.len()
        {
            return Err(VgmSequencerError::CheckpointMismatch);
        }

        let (mut replacement, marker) = if let Some(factory) = &self.stream_factory {
            let (mut stream, marker) =
                factory().map_err(VgmSequencerError::StreamReconstruction)?;
            stream.set_emit_stream_controls(true);
            (Some(stream), marker)
        } else {
            reset_replay_stream(
                &mut self.stream,
                self.initial_loop_base,
                self.initial_loop_modifier,
            );
            (None, None)
        };
        let stream = replacement.as_mut().unwrap_or(&mut self.stream);
        replay_stream(stream, checkpoint.stream_steps, &checkpoint.stream_settings)?;

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
            voice.output_gains.clone_from(&saved.output_gains);
            voice.effects.clone_from(&saved.effects);
            voice.output_masks.clone_from(&saved.output_masks);
            voice.main_coefficient = saved.main_coefficient;
            voice.paired_coefficient = saved.paired_coefficient;
            voice.paced_writes.clone_from(&saved.paced_writes);
            voice.rf5c_wave_bank = saved.rf5c_wave_bank;
            voice.write_interval = saved.write_interval;
            voice.write_cooldown = saved.write_cooldown;
        }

        if let Some(stream) = replacement {
            self.stream = stream;
        }
        self.stream_steps = checkpoint.stream_steps;
        self.stream_settings.clone_from(&checkpoint.stream_settings);
        self.fade_marker = marker;
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
        if self.failed {
            return Err(VgmSequencerError::StreamFailed);
        }
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

    /// Discard events skipped during seeking and rebase remaining offsets.
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
            for voice in &mut self.voices {
                if let Some(scheduled) = voice.route.chip_mut().scheduled() {
                    scheduled.prepare_scheduled()?;
                }
            }
            if let Some(block) = &block {
                let sample_end = sample_start
                    .checked_add(block.frames().len())
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                for voice in &mut self.voices {
                    if let Some(scheduled) = voice.route.chip_mut().scheduled() {
                        scheduled.flush_scheduled_until(sample_end, self.output_rate)?;
                    }
                }
            } else {
                for voice in &mut self.voices {
                    if let Some(scheduled) = voice.route.chip_mut().scheduled() {
                        scheduled.flush_scheduled_until(sample_start, self.output_rate)?;
                    }
                }
                for voice in &mut self.voices {
                    if let Some(scheduled) = voice.route.chip_mut().scheduled() {
                        scheduled.finish_scheduled(sample_start, self.output_rate)?;
                    }
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

    /// Generate and mix a block, advancing the stream until output is available.
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
                    && self.voices.iter().all(|voice| {
                        voice.route.chip().has_no_audio_output()
                            || voice.queue.len() >= self.chunk_frames
                    }))
            {
                let count = self.available.min(self.chunk_frames);
                if count == 0 {
                    return Ok(None);
                }
                let mut bus = StereoBlock::silence(SampleRate::from_hz(self.output_rate), count)?;
                for voice in &mut self.voices {
                    if voice.route.chip().has_no_audio_output() {
                        continue;
                    }
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
                    if !voice.route.chip().has_no_audio_output() {
                        voice
                            .queue
                            .extend(voice.resampler.finish()?.frames().iter().copied());
                    }
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
            let was_initial = self.stream.is_initial_timestamp();
            let result = self
                .stream
                .next()
                .expect("VgmStream iterator is continuous");
            self.stream_steps = self
                .stream_steps
                .checked_add(1)
                .ok_or(VgmSequencerError::DurationOverflow)?;
            let result = result?;
            if was_initial && !self.stream.is_initial_timestamp() {
                for voice in &mut self.voices {
                    if let Some(scheduled) = voice.route.chip_mut().scheduled()
                        && scheduled.wants_vgm_initialization()?
                    {
                        scheduled.finish_vgm_initialization()?;
                    }
                }
            }
            if self.fade_frames.is_none()
                && (self.stream.is_fading_out()
                    || self.fade_marker.as_ref().is_some_and(FadeMarker::reached))
                && let Some(samples) = self.fade_samples
            {
                let step = (self.output_rate / VGM_SAMPLE_RATE) as f32
                    + self.output_step as f32 * PHASE_SCALE;
                let length =
                    (self.output_phase as f32 * PHASE_SCALE + samples as f32 * step).floor();
                self.fade_frames = Some(Fade {
                    start_frame: sample_frame_origin
                        .checked_add(self.available)
                        .ok_or(VgmSequencerError::DurationOverflow)?,
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
                        if !voice.route.chip().has_no_audio_output() {
                            voice
                                .queue
                                .extend(voice.resampler.finish()?.frames().iter().copied());
                        }
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

    /// Handle a VGM command's routing, compatibility effects, and data blocks.
    fn command(
        &mut self,
        command: VgmCommand,
        sample_frame_origin: usize,
    ) -> Result<(), VgmSequencerError> {
        use VgmCommand::*;
        let write = ChipRegisterWrite::from_vgm_command(&command);
        match command {
            StartStream(ref start) => {
                self.notify_dac_stream(
                    DacStreamSelector::Stream(start.stream_id),
                    true,
                    sample_frame_origin,
                )?;
            }
            StartStreamFastCall(ref start) => {
                self.notify_dac_stream(
                    DacStreamSelector::Stream(start.stream_id),
                    true,
                    sample_frame_origin,
                )?;
            }
            StopStream(ref stop) => {
                self.notify_dac_stream(stop.into(), false, sample_frame_origin)?;
            }
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
                    voice.output_masks = vec![
                        [true, true],
                        [mask.left_ch1, mask.right_ch1],
                        [mask.left_ch2, mask.right_ch2],
                        [mask.left_ch3, mask.right_ch3],
                    ];
                }
            }
            AY8910StereoMask(mask) => {
                if let Some(voice) = self.voices.iter_mut().find(|voice| {
                    voice.chip == Chip::Ay8910 && voice.instance == mask.chip_instance
                }) {
                    if voice.route.chip().channels() == 3 {
                        let gain = voice.main_coefficient / 3.0;
                        let coefficient = |left, right| {
                            [
                                if left { gain } else { 0.0 },
                                if right { gain } else { 0.0 },
                            ]
                        };
                        voice.matrix = StereoMatrix::new(vec![
                            coefficient(mask.left_ch1, mask.right_ch1),
                            coefficient(mask.left_ch2, mask.right_ch2),
                            coefficient(mask.left_ch3, mask.right_ch3),
                        ])?;
                        voice.output_masks = vec![
                            [mask.left_ch1, mask.right_ch1],
                            [mask.left_ch2, mask.right_ch2],
                            [mask.left_ch3, mask.right_ch3],
                        ];
                    } else if !(mask.left_ch1
                        && mask.right_ch1
                        && mask.left_ch2
                        && mask.right_ch2
                        && mask.left_ch3
                        && mask.right_ch3)
                    {
                        return Err(ChipError::InvalidConfiguration {
                            chip: Chip::Ay8910,
                            reason: "AY8910 per-channel stereo masks are not supported by this backend",
                        }.into());
                    }
                }
            }
            DataBlock(block) => {
                self.load_block(*block)?;
            }
            PcmRamWrite(write) => {
                let chip = match write.chip_type {
                    soundlog::vgm::detail::StreamChipType::Rf5c68Pcm => Chip::Rf5c68,
                    soundlog::vgm::detail::StreamChipType::Rf5c164Pcm => Chip::Rf5c164,
                    _ => return Ok(()),
                };
                if let Some(voice) = self
                    .voices
                    .iter_mut()
                    .find(|voice| voice.chip == chip && voice.instance == write.instance)
                {
                    let start = write.read_offset as usize;
                    let size = if write.size == 0 {
                        0x1000000
                    } else {
                        write.size as usize
                    };
                    let data = self
                        .stream
                        .pcm_bank_range(u8::from(write.chip_type), start, size)
                        .ok_or(VgmSequencerError::InvalidRomBlock)?;
                    voice
                        .route
                        .chip_mut()
                        .write_ram(write.write_offset as usize | voice.rf5c_wave_bank, &data)?;
                }
            }
            _ => {}
        }
        if let Some((instance, write)) = write {
            self.write_chip(instance, write, sample_frame_origin)?;
        }
        Ok(())
    }

    /// Notify OKIM6258 adapters when a matching DAC stream starts or stops.
    fn notify_dac_stream(
        &mut self,
        selector: DacStreamSelector,
        playing: bool,
        sample_frame_origin: usize,
    ) -> Result<(), VgmSequencerError> {
        let instances: Vec<_> = self
            .stream
            .dac_stream_targets(selector)
            .filter_map(|target| {
                (target.chip == ChipId::Okim6258 && target.port == 0 && target.register == 1)
                    .then_some(target.instance)
            })
            .collect();
        let sample_frame = sample_frame_origin
            .checked_add(self.available)
            .ok_or(VgmSequencerError::DurationOverflow)?;
        for voice in &mut self.voices {
            if voice.chip == Chip::Okim6258
                && ((!playing && selector == DacStreamSelector::All)
                    || instances.contains(&voice.instance))
            {
                let chip = voice.route.chip_mut();
                if let Some(scheduled) = chip.scheduled() {
                    scheduled.set_dac_stream_playing_at(playing, sample_frame, self.output_rate)?;
                } else if !playing {
                    let stream_id = match selector {
                        DacStreamSelector::All => None,
                        DacStreamSelector::Stream(id) => Some(id),
                    };
                    chip.stop_dac_stream(stream_id)?;
                }
            }
        }
        Ok(())
    }

    /// Route a write to its voice, queuing it when pacing is required.
    fn write_chip(
        &mut self,
        instance: Instance,
        write: ChipRegisterWrite,
        sample_frame_origin: usize,
    ) -> Result<(), VgmSequencerError> {
        let chip = write.chip();
        let source_initial = self.stream.is_initial_timestamp();
        let dac_stream_id = self.stream.last_stream_write_id();
        if let Some(voice) = self
            .voices
            .iter_mut()
            .find(|v| write.matches_chip(&v.chip) && v.instance == instance)
        {
            let vgm_initial = if source_initial {
                match voice.route.chip_mut().scheduled() {
                    Some(scheduled) => scheduled.wants_vgm_initialization()?,
                    None => false,
                }
            } else {
                false
            };
            if !vgm_initial && let Some(queue) = &mut voice.paced_writes {
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
                let stop_stream = voice.apply_write(
                    write,
                    WritePosition {
                        event: self.available,
                        frame: sample_frame,
                    },
                    self.output_rate,
                    vgm_initial,
                    dac_stream_id,
                    &mut EventContext {
                        callbacks: &self.callbacks,
                        events: &mut self.events,
                    },
                )?;
                if stop_stream {
                    self.stream.stop_dac_streams_for_target(DacStreamTarget {
                        chip: ChipId::Okim6258,
                        instance,
                        port: 0,
                        register: 1,
                    });
                }
            }
        }
        Ok(())
    }

    /// Apply any remaining paced writes at the current output position.
    fn flush_paced_writes(&mut self, sample_frame_origin: usize) -> Result<(), VgmSequencerError> {
        for voice in &mut self.voices {
            while let Some(write) = voice.paced_writes.as_mut().and_then(VecDeque::pop_front) {
                let sample_frame = sample_frame_origin
                    .checked_add(self.available)
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                voice.apply_write(
                    write,
                    WritePosition {
                        event: self.available,
                        frame: sample_frame,
                    },
                    self.output_rate,
                    false,
                    None,
                    &mut EventContext {
                        callbacks: &self.callbacks,
                        events: &mut self.events,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Load a supported ROM/RAM block into the target chip's memory.
    fn load_block(
        &mut self,
        block: soundlog::vgm::command::DataBlock,
    ) -> Result<(), VgmSequencerError> {
        let instance = if block.chip_instance == 0 {
            Instance::Primary
        } else {
            Instance::Secondary
        };
        if matches!(block.data_type, 0xe0 | 0xe1) {
            let chip = if block.data_type == 0xe0 {
                Chip::Scsp
            } else {
                Chip::Es5503
            };
            let Some(voice) = self
                .voices
                .iter_mut()
                .find(|voice| voice.chip == chip && voice.instance == instance)
            else {
                return Ok(());
            };
            let DataBlockType::RamWrite32(write) =
                parse_data_block(block).map_err(|_| VgmSequencerError::InvalidRomBlock)?
            else {
                return Err(VgmSequencerError::InvalidRomBlock);
            };
            voice
                .route
                .chip_mut()
                .write_ram(write.start_address as usize, &write.data)?;
            return Ok(());
        }
        if matches!(block.data_type, 0xc0..=0xc2) {
            let chip = match block.data_type {
                0xc0 => Chip::Rf5c68,
                0xc1 => Chip::Rf5c164,
                _ => Chip::NesApu,
            };
            let Some(voice) = self
                .voices
                .iter_mut()
                .find(|voice| voice.chip == chip && voice.instance == instance)
            else {
                return Ok(());
            };
            let DataBlockType::RamWrite16(write) =
                parse_data_block(block).map_err(|_| VgmSequencerError::InvalidRomBlock)?
            else {
                return Err(VgmSequencerError::InvalidRomBlock);
            };
            voice.route.chip_mut().write_ram(
                usize::from(write.start_address) | voice.rf5c_wave_bank,
                &write.data,
            )?;
            return Ok(());
        }
        if block.data_type == 0x90 {
            let Some(voice) = self.voices.iter_mut().find(|voice| {
                matches!(voice.chip, Chip::Es5505 | Chip::Es5506U8 | Chip::Es5506U16)
                    && voice.instance == instance
            }) else {
                return Ok(());
            };
            let DataBlockType::RomRamDump(dump) =
                parse_data_block(block).map_err(|_| VgmSequencerError::InvalidRomBlock)?
            else {
                return Err(VgmSequencerError::InvalidRomBlock);
            };
            if dump.start_address & 0x4000_0000 != 0 {
                return Err(VgmSequencerError::InvalidRomBlock);
            }
            let region = ((dump.start_address >> 28) & 3) as usize;
            let offset = (dump.start_address & 0x0fff_ffff) as usize;
            let end = offset
                .checked_add(dump.data.len())
                .ok_or(VgmSequencerError::RomTooLarge)?;
            if end > dump.rom_size as usize {
                return Err(VgmSequencerError::InvalidRomBlock);
            }
            let eight_bit = dump.start_address & 0x8000_0000 != 0;
            let width = if eight_bit { 2 } else { 1 };
            let end = end
                .checked_mul(width)
                .ok_or(VgmSequencerError::RomTooLarge)?;
            if end > MAX_ROM_BYTES {
                return Err(VgmSequencerError::RomTooLarge);
            }
            let start = (region << 28) | (offset * width);
            if eight_bit {
                let mut data = Vec::with_capacity(dump.data.len() * 2);
                for byte in dump.data {
                    data.extend_from_slice(&[0, byte]);
                }
                voice.route.chip_mut().write_rom(start, &data)?;
            } else {
                voice.route.chip_mut().write_rom(start, &dump.data)?;
            }
            return Ok(());
        }
        let (chip, access) = match block.data_type {
            0x80 => (Chip::SegaPcm, None),
            0x8b => (Chip::Okim6295, None),
            0x8d => (Chip::C140, None),
            0x93 => (Chip::Ga20, None),
            0x92 => (Chip::C352, None),
            0x8e => (Chip::K053260, None),
            0x8c => (Chip::K054539, None),
            0x91 => (Chip::X1010, None),
            0x86 => (Chip::Ymz280b, None),
            0x85 => (Chip::Ymf271, None),
            0x8f => (Chip::Qsound, None),
            0x89 => (Chip::MultiPcm, None),
            0x8a => (Chip::Upd7759, None),
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
        if end > MAX_ROM_BYTES || (chip == Chip::Qsound && dump.rom_size as usize > MAX_ROM_BYTES) {
            return Err(VgmSequencerError::RomTooLarge);
        }
        let Some(access) = access else {
            if chip == Chip::Qsound {
                voice
                    .route
                    .chip_mut()
                    .write_rom(dump.rom_size as usize, &[])?;
            }
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
            start_frame: 0,
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
            start_frame: 0,
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
        let mut player =
            VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
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
