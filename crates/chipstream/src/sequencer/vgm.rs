//! VGM command playback, chip routing, and stereo audio rendering.

mod timing;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::rc::Rc;

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
};
#[cfg(feature = "okim6295")]
use crate::chip::Okim6295;
#[cfg(feature = "pwm")]
use crate::chip::Pwm;
#[cfg(feature = "segapcm")]
use crate::chip::SegaPcm;
#[cfg(feature = "c140")]
use crate::chip::{C140, C140Type};
use crate::chip::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, YmfmChip};
#[cfg(feature = "okim6258")]
use crate::chip::{ClockDivider, Okim6258, OutputBits};
#[cfg(feature = "sn76496")]
use crate::chip::{Sn76496, Sn76496Config};
use crate::fixed::{advance, divide_ratio};

use super::{compat::PwmCompatibility, event, volume};
use event::{VgmEventFilter, VgmWriteEvent};
use timing::{Timing, TimingProfile};

const WAIT_STEP: u32 = 256;
const SRC_CHUNK: usize = 256;
const MAX_ROM_BYTES: usize = 64 * 1024 * 1024;
const MAX_PENDING_WRITES: usize = 1_048_576;
const SCALE: f32 = 1.0 / 32768.0;

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
    InvalidEventFilter,
    EventConfigurationAfterPlayback,
    PwmCompatibilityConfigurationAfterPlayback,
    FadeoutConfigurationAfterPlayback,
    EventCallbackRequired,
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
            Self::InvalidEventFilter => write!(f, "invalid VGM event filter or queue limit"),
            Self::EventConfigurationAfterPlayback => {
                write!(f, "event configuration must be set before rendering")
            }
            Self::PwmCompatibilityConfigurationAfterPlayback => {
                write!(f, "PWM compatibility must be configured before rendering")
            }
            Self::FadeoutConfigurationAfterPlayback => {
                write!(f, "fadeout must be configured before rendering")
            }
            Self::EventCallbackRequired => {
                write!(f, "configured event collection requires render_with_events")
            }
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

#[derive(Default)]
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

struct EventContext<'a> {
    filter: Option<&'a VgmEventFilter>,
    events: &'a mut VecDeque<(usize, VgmWriteEvent)>,
}

impl Voice {
    fn apply_write(
        &mut self,
        write: ChipRegisterWrite,
        position: usize,
        context: &mut EventContext<'_>,
    ) -> Result<(), VgmSequencerError> {
        let (bank, register, value) = write.parts();
        self.route.chip_mut().apply_register_write(write)?;
        if let Some(filter) = context.filter {
            let state_events = if let Some(tracker) = self.tracker.as_mut() {
                let narrow = u8::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
                    chip: self.chip.clone(),
                    register,
                    value,
                })?;
                tracker(bank, register, narrow).unwrap_or_default()
            } else {
                Vec::new()
            };
            if filter.matches_write(&self.chip, self.instance, bank, register)
                && (filter.register_writes || !state_events.is_empty())
            {
                if context.events.len() >= filter.max_pending_events {
                    return Err(VgmSequencerError::EventQueueFull {
                        limit: filter.max_pending_events,
                    });
                }
                let index = context
                    .events
                    .partition_point(|(offset, _)| *offset <= position);
                context.events.insert(
                    index,
                    (
                        position,
                        VgmWriteEvent {
                            frame_offset: 0,
                            chip: self.chip.clone(),
                            instance: self.instance,
                            port: bank,
                            register,
                            value,
                            state_events,
                        },
                    ),
                );
            }
        }
        Ok(())
    }

    fn render(
        &mut self,
        ticks: u32,
        output_rate: u32,
        output_phase: u32,
        available: usize,
        context: &mut EventContext<'_>,
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
            let fraction = if initial_phase == 0 {
                0
            } else {
                (1_u128 << 32) - u128::from(initial_phase)
            };
            let native_offset = ((rendered as u128) << 32) + fraction;
            let output_offset =
                (native_offset * u128::from(rate.denominator().get()) * u128::from(output_rate)
                    / u128::from(rate.numerator())
                    + u128::from(output_phase))
                    >> 32;
            let position = available
                .checked_add(
                    usize::try_from(output_offset)
                        .map_err(|_| VgmSequencerError::DurationOverflow)?,
                )
                .ok_or(VgmSequencerError::DurationOverflow)?;
            self.apply_write(write, position, context)?;
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
/// T6W28 PSG and OKIM6258 3-bit ADPCM return errors.
///
/// Register write pacing is determined by the device timing profile. YMFM
/// writes are queued per chip instance and applied in order, at most one per
/// native sample. YM2203/YM2608 and YM2610(B) use intervals of 18 and 9
/// native samples respectively so that their oversampled FM engines advance
/// between writes. A burst can therefore delay subsequent writes; exceeding
/// 1,048,576 pending writes per instance is an error.
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
pub struct VgmSequencer {
    stream: VgmStream,
    voices: Vec<Voice>,
    output_rate: u32,
    chunk_frames: usize,
    wait: u32,
    available: usize,
    output_phase: u32,
    output_step: u32,
    ended: bool,
    failed: bool,
    started: bool,
    event_filter: Option<VgmEventFilter>,
    events: VecDeque<(usize, VgmWriteEvent)>,
    pwm_compatibility: Option<[PwmCompatibility; 2]>,
    fade_samples: Option<u32>,
    fade_remaining: Option<u32>,
    fade_frames: Option<(u64, u64)>,
    rendered_frames: u64,
    mdx_loop_marker: Option<MdxLoopMarker>,
}

impl VgmSequencer {
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
        Self::validate_output(output_rate, chunk_frames)?;
        let data = data.into();
        let header = VgmHeader::from_bytes(&data)?;
        let extra = header.extra_header_from_bytes(&data)?;
        let mut timing = Timing::new(&Self::supported_clocks(&header));
        if timing.needs_scan() {
            visit_non_data_block_commands(&data, &header, |command| {
                timing.observe(command);
            })?;
        }
        let voices = Self::make_voices(&header, extra.as_ref(), &timing, output_rate)?;
        let stream = VgmStream::from_vgm(data)?;
        Self::from_parts(stream, voices, output_rate, chunk_frames)
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
        let backend: Box<dyn SoundChip> = if chip == Chip::Sn76489 {
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
                if flags.adpcm_3bit_select {
                    return Err(VgmSequencerError::UnsupportedOkiAdpcm);
                }
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
        )?;
        Self::from_parts(
            VgmStream::from_document(document),
            voices,
            output_rate,
            chunk_frames,
        )
    }

    pub(super) fn from_mdx_stream(
        stream: VgmStream,
        ym2151_clock: u32,
        okim6258_clock: Option<u32>,
        output_rate: u32,
        chunk_frames: usize,
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
        let voices = Self::make_voices(&header, None, &timing, output_rate)?;
        Self::from_parts(stream, voices, output_rate, chunk_frames)
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
                )?);
            }
        }
        Ok(voices)
    }

    fn from_parts(
        stream: VgmStream,
        voices: Vec<Voice>,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, VgmSequencerError> {
        Ok(Self {
            stream,
            voices,
            output_rate,
            chunk_frames,
            wait: 0,
            available: 0,
            output_phase: 1 << 31,
            output_step: divide_ratio(output_rate, VGM_SAMPLE_RATE, 1)
                .ok_or(VgmSequencerError::DurationOverflow)?
                .1,
            ended: false,
            failed: false,
            started: false,
            event_filter: None,
            events: VecDeque::new(),
            pwm_compatibility: Some([PwmCompatibility::default(); 2]),
            fade_samples: None,
            fade_remaining: None,
            fade_frames: None,
            rendered_frames: 0,
            mdx_loop_marker: None,
        })
    }

    /// Fade linearly to silence after the final VGM loop, over this many
    /// 44,100 Hz samples. Files without a loop and infinite playback ignore it.
    /// A zero duration disables the fade. Configure before rendering.
    pub fn set_fadeout_samples(&mut self, samples: Option<u32>) -> Result<(), VgmSequencerError> {
        self.configure_fadeout(samples, None)?;
        self.stream
            .set_fadeout_samples(samples.filter(|samples| *samples != 0).map(|n| n as usize));
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

    /// Enable filtered event collection before the first render call.
    ///
    /// State is tracked for all writes to matching chips, even when the port
    /// or register filter suppresses their notifications.
    /// Once configured, use [`Self::render_with_events`]; [`Self::render`]
    /// returns an error to prevent events from being lost. Exceeding the
    /// pending-event limit also returns an error and stops the sequencer.
    pub fn configure_events(&mut self, filter: VgmEventFilter) -> Result<(), VgmSequencerError> {
        if self.started {
            return Err(VgmSequencerError::EventConfigurationAfterPlayback);
        }
        if filter.max_pending_events == 0
            || (!filter.register_writes && !filter.state_changes)
            || u32::try_from(self.chunk_frames).is_err()
        {
            return Err(VgmSequencerError::InvalidEventFilter);
        }
        for voice in &mut self.voices {
            voice.tracker =
                if filter.state_changes && filter.matches_chip(&voice.chip, voice.instance) {
                    event::tracker(&voice.chip, voice.clock)
                } else {
                    None
                };
        }
        self.event_filter = Some(filter);
        Ok(())
    }

    /// Set the number of playthroughs (`None` loops indefinitely).
    /// This should be called before rendering begins.
    pub fn set_loop_count(&mut self, count: Option<u32>) {
        self.stream.set_loop_count(count);
    }

    /// Override the VGM loop modifier before rendering.
    pub fn set_loop_modifier(&mut self, modifier: u8) {
        self.stream.set_loop_modifier(modifier);
    }

    /// Override the VGM loop base before rendering.
    pub fn set_loop_base(&mut self, base: i8) {
        self.stream.set_loop_base(base);
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

    /// Return the next chunk; the final block can be shorter. `None` means EOF.
    pub fn render(&mut self) -> Result<Option<StereoBlock>, VgmSequencerError> {
        if self.event_filter.is_some() && !self.failed {
            return Err(VgmSequencerError::EventCallbackRequired);
        }
        self.render_with_events(|_| {})
    }

    /// Render PCM and deliver configured events relative to that block.
    ///
    /// Events at a block boundary belong to the next block. At EOF, events
    /// at the final boundary are delivered with an offset equal to the block
    /// length, or zero when no PCM block remains. Callbacks run synchronously,
    /// not at device time.
    pub fn render_with_events(
        &mut self,
        mut callback: impl FnMut(&VgmWriteEvent),
    ) -> Result<Option<StereoBlock>, VgmSequencerError> {
        if self.failed {
            return Err(VgmSequencerError::StreamFailed);
        }
        self.started = true;
        let result = self.render_inner();
        if result.is_err() {
            self.failed = true;
        }
        if let Ok(ref block) = result {
            let count = block.as_ref().map_or(0, |block| block.frames().len());
            while self.events.front().is_some_and(|(offset, _)| {
                *offset < count || (self.ended && self.available == 0 && *offset <= count)
            }) {
                let (offset, mut event) = self.events.pop_front().expect("front exists");
                event.frame_offset = u32::try_from(offset).expect("configured chunk fits u32");
                callback(&event);
            }
            for (offset, _) in &mut self.events {
                *offset -= count;
            }
        }
        result
    }

    fn render_inner(&mut self) -> Result<Option<StereoBlock>, VgmSequencerError> {
        loop {
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
                let next_rendered_frames = self
                    .rendered_frames
                    .checked_add(count as u64)
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                let mut bus = StereoBlock::silence(SampleRate::from_hz(self.output_rate), count)?;
                for voice in &mut self.voices {
                    let mut frames = Vec::with_capacity(count);
                    for _ in 0..count {
                        frames.push(voice.queue.pop_front().unwrap_or([0.0; 2]));
                    }
                    let source = StereoBlock::new(SampleRate::from_hz(self.output_rate), frames)?;
                    StereoMixer::mix(&mut bus, &source, voice.gain)?;
                }
                if let Some((start, length)) = self.fade_frames {
                    for (index, frame) in bus.frames.iter_mut().enumerate() {
                        let position = self.rendered_frames + index as u64;
                        if position >= start {
                            let remaining = length.saturating_sub(position - start + 1);
                            let gain = if length <= 1 {
                                0.0
                            } else {
                                remaining as f32 / (length - 1) as f32
                            };
                            frame[0] *= gain;
                            frame[1] *= gain;
                        }
                    }
                }
                self.available -= count;
                self.rendered_frames = next_rendered_frames;
                return Ok(Some(bus));
            }
            if self.fade_remaining == Some(0) {
                self.flush_paced_writes()?;
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
                        &mut EventContext {
                            filter: self.event_filter.as_ref(),
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
                .expect("VgmStream iterator is continuous")?;
            if self.fade_frames.is_none()
                && (self.stream.is_fading_out()
                    || self
                        .mdx_loop_marker
                        .as_ref()
                        .is_some_and(MdxLoopMarker::reached))
                && let Some(samples) = self.fade_samples
            {
                let start = self
                    .rendered_frames
                    .checked_add(self.available as u64)
                    .ok_or(VgmSequencerError::DurationOverflow)?;
                let step = (u128::from(self.output_rate / VGM_SAMPLE_RATE) << 32)
                    + u128::from(self.output_step);
                let length = (u128::from(self.output_phase) + u128::from(samples) * step) >> 32;
                let length =
                    u64::try_from(length).map_err(|_| VgmSequencerError::DurationOverflow)?;
                self.fade_frames = Some((start, length));
                self.fade_remaining = Some(samples);
            }
            match result {
                StreamResult::EndOfStream => {
                    self.flush_paced_writes()?;
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
                StreamResult::Command(command) => self.command(command)?,
            }
        }
    }

    fn command(&mut self, command: VgmCommand) -> Result<(), VgmSequencerError> {
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
            self.write_chip(instance, write)?;
        }
        Ok(())
    }

    fn write_chip(
        &mut self,
        instance: Instance,
        write: ChipRegisterWrite,
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
                voice.apply_write(
                    write,
                    self.available,
                    &mut EventContext {
                        filter: self.event_filter.as_ref(),
                        events: &mut self.events,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn flush_paced_writes(&mut self) -> Result<(), VgmSequencerError> {
        for voice in &mut self.voices {
            while let Some(write) = voice.paced_writes.as_mut().and_then(VecDeque::pop_front) {
                voice.apply_write(
                    write,
                    self.available,
                    &mut EventContext {
                        filter: self.event_filter.as_ref(),
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
        player.write_chip(Instance::Primary, write.clone()).unwrap();
        assert!(matches!(
            player.write_chip(Instance::Primary, write.clone()),
            Err(VgmSequencerError::WriteQueueFull {
                chip: Chip::Ym2151,
                instance: Instance::Primary,
                limit: MAX_PENDING_WRITES,
            })
        ));
        player.write_chip(Instance::Secondary, write).unwrap();
    }
}
