//! Conversion of MDX playback into VGM commands.
//!
//! This module drives parsed MDX tracks through the MXDRV timing and control
//! semantics needed to produce VGM output. It supports both eager document
//! conversion and lazy command generation for bounded-memory streaming.
//! Playback semantics and PCM8/PCM8A handling are informed by the behavior of
//! NanoDrive8 (by Fujix-san).
//!
//! Responsibilities:
//! - Translate OPM register writes, waits, loops, LFOs, and track control into
//!   VGM commands while preserving MDX playback timing.
//! - Decode and mix optional PCM8/PCM8A data through the OKIM6258 path.
//! - Expose conversion options and errors without leaking playback state into
//!   the public format model.

use crate::ParseError;
use crate::chip::{Chip, Okim6258Spec, Ym2151Spec};
use crate::mdx::command::{
    MdxCommand, MdxExtended2Command, MdxExtendedCommand, MdxLfoWaveform, MdxOpmLfo, MdxPan,
    MdxPitchLfo, MdxVolumeLfo,
};
use crate::mdx::package::{MdxPackage, MdxPcmReference};
use crate::mdx::pcm::{AdpcmEncoder, Pcm8aFormat, decode_pcm8a_with_pcm16_15khz};
use crate::mdx::pcm_mixer::{self, PcmChannelState, PcmOutputFilter};
use crate::mdx::tone::MdxTone;
use crate::vgm::command::{EndOfData, Instance, VgmCommand, WaitSamples};
use crate::vgm::stream::VgmCommandGenerator;
use crate::vgm::{VGM_SAMPLE_RATE, VgmBuilder, VgmDocument, VgmStream};
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;

/// Number of microseconds in one second, used as the fixed-point time scale
/// for sample and PCM-byte remainder calculations.
const MICROSECONDS_PER_SECOND: u32 = 1_000_000;
/// MXDRV tempo used when an MDX stream has not issued a tempo command yet.
const DEFAULT_TEMPO: u8 = 200;
/// Fadeout attenuation level at which MXDRV ends playback.
const FADEOUT_FINAL_LEVEL: u8 = 0x3e;
/// YM2151 key-code values indexed by the 7-bit pitch key-code field.
const YM2151_KEYCODE_TABLE: [u8; 96] = [
    0x00, 0x01, 0x02, 0x04, 0x05, 0x06, 0x08, 0x09, 0x0a, 0x0c, 0x0d, 0x0e, 0x10, 0x11, 0x12, 0x14,
    0x15, 0x16, 0x18, 0x19, 0x1a, 0x1c, 0x1d, 0x1e, 0x20, 0x21, 0x22, 0x24, 0x25, 0x26, 0x28, 0x29,
    0x2a, 0x2c, 0x2d, 0x2e, 0x30, 0x31, 0x32, 0x34, 0x35, 0x36, 0x38, 0x39, 0x3a, 0x3c, 0x3d, 0x3e,
    0x40, 0x41, 0x42, 0x44, 0x45, 0x46, 0x48, 0x49, 0x4a, 0x4c, 0x4d, 0x4e, 0x50, 0x51, 0x52, 0x54,
    0x55, 0x56, 0x58, 0x59, 0x5a, 0x5c, 0x5d, 0x5e, 0x60, 0x61, 0x62, 0x64, 0x65, 0x66, 0x68, 0x69,
    0x6a, 0x6c, 0x6d, 0x6e, 0x70, 0x71, 0x72, 0x74, 0x75, 0x76, 0x78, 0x79, 0x7a, 0x7c, 0x7d, 0x7e,
];
/// Per-track PCM8A ADPCM/PCM rate-select table (`0xed` on tracks >= 8),
/// mapping the command's value to a Q16.16 resampler rate step and the
/// sample data format it selects.
const PCM8A_MODE_TABLE: [(u32, Pcm8aFormat); 13] = [
    (0x04000, Pcm8aFormat::Adpcm), // F0:  ADPCM,  3.906kHz
    (0x05555, Pcm8aFormat::Adpcm), // F1:  ADPCM,  5.208kHz
    (0x08000, Pcm8aFormat::Adpcm), // F2:  ADPCM,  7.812kHz
    (0x0aaaa, Pcm8aFormat::Adpcm), // F3:  ADPCM, 10.416kHz
    (0x10000, Pcm8aFormat::Adpcm), // F4:  ADPCM, 15.625kHz
    (0x10000, Pcm8aFormat::Pcm16), // F5:  16bit PCM, 15.625kHz
    (0x10000, Pcm8aFormat::Pcm8),  // F6:   8bit PCM, 15.625kHz
    (0x15555, Pcm8aFormat::Adpcm), // F7:  ADPCM, 20.833kHz
    (0x15555, Pcm8aFormat::Pcm16), // F8:  16bit PCM, 20.833kHz
    (0x15555, Pcm8aFormat::Pcm8),  // F9:   8bit PCM, 20.833kHz
    (0x20000, Pcm8aFormat::Adpcm), // F10: ADPCM, 31.250kHz
    (0x20000, Pcm8aFormat::Pcm16), // F11: 16bit PCM, 31.250kHz
    (0x20000, Pcm8aFormat::Pcm8),  // F12:  8bit PCM, 31.250kHz
];

/// TL(0x60+) register offsets by algorithm (CON) that carry the note volume.
const CARRIER_TL_SLOTS: [u8; 8] = [0x08, 0x08, 0x08, 0x08, 0x0c, 0x0e, 0x0e, 0x0f];

/// Key-on slot mask fallback by algorithm (CON), used when a tone does not
/// specify its own key-on slot mask.
const CARRIER_KEYON_SLOTS: [u8; 8] = [0x40, 0x40, 0x40, 0x40, 0x50, 0x70, 0x70, 0x78];
/// `@v` (0..15) attenuation table for the FM volume command.
const FM_VOLUME_TABLE: [u8; 16] = [
    0x2a, 0x28, 0x25, 0x22, 0x20, 0x1d, 0x1a, 0x18, 0x15, 0x12, 0x10, 0x0d, 0x0a, 0x08, 0x05, 0x02,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MdxPcmMode {
    LegacyAdpcm,
    Pcm8a,
}

/// Schedules the OKIM6258 data-register writes driven by the hardware MCK.
///
/// The hardware MCK produces one output event per PCM byte. VGM has no MCK
/// command, so this scheduler carries the fractional event phase across MDX
/// ticks and exposes the number of events due in each tick.
const MCK_RATE_DENOMINATOR: u32 = 24;

struct MckScheduler {
    byte_rate_units: u32,
    time_remainder: u32,
    started: bool,
}

impl MckScheduler {
    /// Creates a scheduler with a rate scaled by [`MCK_RATE_DENOMINATOR`].
    fn new(byte_rate_units: u32) -> Self {
        Self {
            byte_rate_units,
            time_remainder: 0,
            started: false,
        }
    }

    /// Changes the event rate while preserving the accumulated fractional phase.
    fn set_byte_rate(&mut self, byte_rate_units: u32) {
        self.byte_rate_units = byte_rate_units;
    }

    /// Advances elapsed time and returns the number of PCM-byte events due.
    fn advance(&mut self, tick_microseconds: u32) -> u32 {
        let denominator = MICROSECONDS_PER_SECOND * MCK_RATE_DENOMINATOR;
        let whole_rate_hz = self.byte_rate_units / MCK_RATE_DENOMINATOR;
        let fractional_rate_units = self.byte_rate_units % MCK_RATE_DENOMINATOR;
        let whole_rate_accumulator = tick_microseconds * whole_rate_hz;
        let bytes_due = whole_rate_accumulator / MICROSECONDS_PER_SECOND;
        let fractional_accumulator = self.time_remainder
            + (whole_rate_accumulator % MICROSECONDS_PER_SECOND) * MCK_RATE_DENOMINATOR
            + tick_microseconds * fractional_rate_units;
        let fractional_bytes_due = fractional_accumulator / denominator;
        self.time_remainder = fractional_accumulator % denominator;
        bytes_due + fractional_bytes_due
    }

    /// Returns `true` once to emit the stream's initial PCM-byte event.
    fn take_initial_event(&mut self) -> bool {
        if self.started {
            false
        } else {
            self.started = true;
            true
        }
    }
}

impl MdxPcmMode {
    /// Selects PCM8A for 16-track MDX files and legacy ADPCM otherwise.
    fn from_track_count(track_count: usize) -> Self {
        if track_count == 16 {
            Self::Pcm8a
        } else {
            Self::LegacyAdpcm
        }
    }
}

/// ADPCM processing mode for MDX PCM output.
///
/// PCM8A playback is mixed and re-encoded into the single OKIM6258 stream
/// required by VGM. `Through` and `Resample` share the unfiltered path for
/// PCM8A; `Lpf` additionally applies the output filter before re-encoding.
/// For legacy ADPCM, `Through` passes the encoded source bytes directly, while
/// `Resample` and `Lpf` use the decoded mixer path.
///
/// Embedded MDX fadeout attenuation is applied to FM output and to PCM
/// channels that pass through the software mixer. Legacy ADPCM `Through`
/// output is passed as encoded source bytes and is not attenuated; PCM8A
/// `Through` still uses the mixer and is attenuated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdpcmMode {
    /// Pass legacy ADPCM source bytes through without decoding or mixing. This
    /// is the default and does not apply embedded MDX fadeout to legacy ADPCM
    /// output. PCM8A `Through` output still passes through the mixer.
    #[default]
    Through,
    /// Resample the PCM channels without the output filter.
    Resample,
    /// Resample and apply the output filter.
    Lpf,
}

/// Options controlling MDX to VGM conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxToVgmOptions {
    /// YM2151 master clock in Hz, written to the VGM header by eager
    /// conversion.
    pub ym2151_clock: u32,
    /// OKIM6258 master clock in Hz, written to the VGM header when a PDX
    /// package is present. It is paired with the `/512` clock divider;
    /// changing it changes the playback speed of the PCM8/PCM8A stream.
    pub okim6258_clock: u32,
    /// ADPCM processing mode for PCM8/PCM8A output.
    pub adpcm_mode: AdpcmMode,
    /// Number of traversals for song-level repeats: backward `Jump`s,
    /// count-zero `LoopEnd`s, and per-track `F1` markers. Finite nested
    /// `LoopStart`/`LoopEnd` counts are unaffected.
    ///
    /// `None` (default) uses a native VGM loop point for eager conversion when
    /// possible; lazy streaming repeats internally, subject to `max_ticks`. Eager F1 loops synchronize
    /// at a shared boundary, and embedded fadeouts run to completion.
    ///
    /// `Some(n)` emits finite playback without a native loop point. For F1
    /// loops, playback includes the intro and first loop traversal, then makes
    /// `n - 1` further traversals. `Some(0)` is treated as `Some(1)`.
    pub loop_count: Option<u32>,
    /// Maximum elapsed playback ticks across all song traversals, including
    /// synchronization waits. Applies to eager conversion and lazy streaming.
    /// `None` disables the limit. The default is 100,000 ticks, approximately
    /// 17 minutes 4 seconds at MML tempo `t120` (MDX tempo byte 216).
    pub max_ticks: Option<u32>,
    /// Maximum MDX commands executed across all tracks without advancing
    /// playback time. The count resets only when a tick of time elapses,
    /// not at generator calls or loop boundaries. Applies to eager conversion
    /// and lazy streaming. The default is 100,000; `None` disables the limit.
    pub max_commands_per_tick: Option<u32>,
    /// Maximum VGM commands retained by eager conversion, including
    /// initialization, closing commands, and all song traversals.
    /// Checked after each step and finalization, so temporary storage can
    /// exceed the limit by one step and include unused vector capacity.
    /// The default is 14,000,000; `None` disables the limit.
    /// Lazy streaming and playback checking do not use this limit.
    pub max_output_commands: Option<u32>,
    /// Maximum decoded `i16` PCM samples cached during playback. Applies to
    /// eager conversion, lazy streaming, and playback checking. Shared ranges
    /// count once; new ranges are checked before decoding or allocation.
    /// The default is 32,000,000 samples (64 MB of sample data); `None` disables
    /// the limit. Vector capacity and temporary decoder storage are additional.
    pub max_decoded_pcm_samples: Option<u32>,
}

impl Default for MdxToVgmOptions {
    fn default() -> Self {
        Self {
            ym2151_clock: 4_000_000,
            okim6258_clock: pcm_mixer::PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ,
            adpcm_mode: AdpcmMode::default(),
            loop_count: None,
            max_ticks: Some(100_000),
            max_commands_per_tick: Some(100_000),
            max_output_commands: Some(14_000_000),
            max_decoded_pcm_samples: Some(32_000_000),
        }
    }
}

/// Errors returned while converting an MDX playback stream into VGM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdxConvertError {
    /// Playback would exceed the configured elapsed-tick limit.
    TickLimitExceeded {
        /// Configured maximum number of elapsed playback ticks.
        max_ticks: u32,
    },
    /// Playback would execute too many MDX commands without advancing time.
    CommandLimitExceeded {
        /// Configured maximum number of commands between elapsed ticks.
        max_commands_per_tick: u32,
    },
    /// Eager conversion would retain too many generated VGM commands.
    OutputCommandLimitExceeded {
        /// Configured maximum number of retained VGM commands.
        max_output_commands: u32,
    },
    /// Playback would cache too many decoded PCM samples.
    DecodedPcmSampleLimitExceeded {
        /// Configured maximum number of cached decoded `i16` samples.
        max_decoded_pcm_samples: u32,
    },
    /// Conversion options contain an unsupported or inconsistent value.
    InvalidOptions(&'static str),
    /// The current MDX document has an unrepresentable layout.
    InvalidDocument(String),
    /// A track contains a command that the converter cannot process.
    UnsupportedCommand {
        /// Zero-based index of the track containing the command.
        track: usize,
        /// Name of the unsupported command.
        command: &'static str,
    },
    /// An FM track selects a voice that has no tone definition.
    MissingTone {
        /// Voice number whose tone definition is missing.
        voice: u8,
    },
    /// An FM key-on pitch is outside the supported YM2151 key-code range.
    PitchOutOfRange {
        /// Zero-based index of the track containing the note.
        track: usize,
        /// Computed pitch in units of 1/64 semitone; valid values are 0..=6143.
        pitch: u16,
    },
}

impl fmt::Display for MdxConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MdxConvertError::TickLimitExceeded { max_ticks } => {
                write!(
                    f,
                    "MDX conversion tick limit exceeded (maximum {max_ticks} ticks)"
                )
            }
            MdxConvertError::InvalidOptions(reason) => write!(f, "invalid options: {reason}"),
            MdxConvertError::DecodedPcmSampleLimitExceeded {
                max_decoded_pcm_samples,
            } => write!(
                f,
                "MDX conversion decoded PCM sample limit exceeded (maximum {max_decoded_pcm_samples} samples)"
            ),
            MdxConvertError::CommandLimitExceeded {
                max_commands_per_tick,
            } => write!(
                f,
                "MDX conversion command limit exceeded (maximum {max_commands_per_tick} commands without advancing playback time)"
            ),
            MdxConvertError::OutputCommandLimitExceeded {
                max_output_commands,
            } => write!(
                f,
                "MDX conversion output command limit exceeded (maximum {max_output_commands} VGM commands)"
            ),
            MdxConvertError::InvalidDocument(reason) => write!(f, "invalid MDX document: {reason}"),
            MdxConvertError::UnsupportedCommand { track, command } => {
                write!(f, "unsupported command `{command}` on track {track}")
            }
            MdxConvertError::MissingTone { voice } => write!(f, "missing tone for voice {voice}"),
            MdxConvertError::PitchOutOfRange { track, pitch } => write!(
                f,
                "FM pitch {pitch} is outside the supported range 0..=6143 (track {track})"
            ),
        }
    }
}

impl Error for MdxConvertError {}

impl From<MdxConvertError> for ParseError {
    fn from(error: MdxConvertError) -> Self {
        Self::GeneratorError(Box::new(error))
    }
}

impl MdxToVgmOptions {
    /// Normalizes values shared by both the eager [`to_vgm_document`] path
    /// and the lazy [`MdxVgmGenerator`].
    fn normalize(&mut self) {
        if self.loop_count == Some(0) {
            self.loop_count = Some(1);
        }
    }
}

/// Build a lazy VGM command generator for `package` using custom `options`,
/// for `VgmCallbackStream::from_generator`. Mirrors
/// [`to_vgm_document`] on the eager side.
///
/// Unlike [`to_vgm_document`], this never accumulates the whole song's worth
/// of commands in memory: each call to the returned generator's
/// `next_command` produces (and hands over) at most a tick's worth of
/// commands, so this is the low-memory path intended for streaming
/// straight to a chip driver (e.g. on a microcontroller) rather than
/// building a `VgmDocument`.
///
/// # Examples
/// ```
/// use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator};
/// use soundlog::mdx::document::MdxBuilder;
/// use soundlog::mdx::package::MdxPackage;
/// use soundlog::vgm::VgmCallbackStream;
///
/// let package = MdxPackage {
///     mdx: MdxBuilder::new().finalize().unwrap(),
///     pdx: None,
/// };
/// let generator = to_vgm_stream_generator(package, MdxToVgmOptions::default())
///     .expect("create a lazy VGM command generator");
/// let _stream = VgmCallbackStream::from_generator(generator);
/// ```
pub fn to_vgm_stream_generator(
    package: MdxPackage,
    options: MdxToVgmOptions,
) -> Result<Box<dyn VgmCommandGenerator>, MdxConvertError> {
    Ok((package, options).into())
}

/// Convert the basic FM portion of an MDX package into a VGM document.
///
/// Thin eager wrapper around `MdxVgmGenerator` (the same tick-by-tick
/// driver used by the lazy `VgmStream`/`VgmCallbackStream` path), run to
/// completion up front and finalized into a full [`VgmDocument`].
///
/// # Note
///
/// The returned document retains every generated command. If the package has
/// ADPCM/PCM playback, this includes the individual OKIM6258 data writes, so
/// the resulting [`VgmDocument`] can become substantially larger than the
/// source MDX/PDX files. Use [`to_vgm_stream_generator`] with
/// [`VgmCallbackStream`](crate::vgm::VgmCallbackStream) when commands should
/// be generated and consumed incrementally instead.
pub fn to_vgm_document(
    package: &MdxPackage,
    options: &MdxToVgmOptions,
) -> Result<VgmDocument, MdxConvertError> {
    convert_document(package, options, false).map_err(|failure| failure.error)
}

/// Converts playback to VGM with zero-based MDX coordinates on errors.
///
/// Runs the same conversion as [`to_vgm_document`], without an additional
/// validation pass or playback-check budgets. Coordinates are not MML lines.
/// Only [`MdxPlaybackCheckError::Conversion`] is returned on failure.
pub fn to_vgm_document_with_diagnostics(
    package: &MdxPackage,
    options: &MdxToVgmOptions,
) -> Result<VgmDocument, MdxPlaybackCheckError> {
    convert_document(package, options, true).map_err(|failure| MdxPlaybackCheckError::Conversion {
        error: failure.error,
        track: failure.position.map(|(track, _)| track),
        command_index: failure.position.map(|(_, command)| command),
    })
}

struct ConversionFailure {
    error: MdxConvertError,
    position: Option<(usize, usize)>,
}

impl From<MdxConvertError> for ConversionFailure {
    fn from(error: MdxConvertError) -> Self {
        Self {
            error,
            position: None,
        }
    }
}

fn convert_document(
    package: &MdxPackage,
    options: &MdxToVgmOptions,
    diagnostics: bool,
) -> Result<VgmDocument, ConversionFailure> {
    let mut generator = MdxVgmGenerator::new(package, *options, true);
    if diagnostics {
        generator.playback.check_state = Some(PlaybackCheckState::default());
    }
    // Header fields with no bearing on the command stream itself (and thus
    // no counterpart in the lazy generator, which never produces a
    // `VgmDocument`); only needed here, for the serialized document.
    generator
        .builder
        .set_sample_rate(VGM_SAMPLE_RATE)
        .register_chip(Chip::Ym2151, Instance::Primary, options.ym2151_clock);

    generator
        .run_eager_steps(0)
        .map_err(|error| generator.playback.conversion_failure(error))?;

    // For F1 cases that could not establish a synchronized native loop point,
    // retain the finite restart-pass fallback. Ordinary eager F1 loops record
    // their shared point directly at the first all-track barrier.
    if options.loop_count.is_none()
        && generator.playback.song_loop.loop_index.is_none()
        && generator.playback.song_loop.track_end_loop_seen
        && !generator.playback.fadeout.seen
    {
        let loop_index = generator.builder.command_count();
        let repeat_options = MdxToVgmOptions {
            loop_count: Some(1),
            ..*options
        };
        let mut repeat = MdxVgmGenerator::new(package, repeat_options, true);
        repeat.playback.pcm_output.samples =
            std::mem::take(&mut generator.playback.pcm_output.samples);
        repeat.playback.pcm_output.sample_ranges =
            std::mem::take(&mut generator.playback.pcm_output.sample_ranges);
        repeat.playback.timing.elapsed_ticks = generator.playback.timing.elapsed_ticks;
        repeat.playback.timing.commands_since_tick = generator.playback.timing.commands_since_tick;
        if diagnostics {
            repeat.playback.check_state = Some(PlaybackCheckState::default());
        }
        repeat
            .run_eager_steps(loop_index)
            .map_err(|error| repeat.playback.conversion_failure(error))?;
        for command in repeat.builder.drain_commands() {
            generator.builder.add_vgm_command(command);
        }
        generator.playback.song_loop.loop_index = Some(loop_index);
    }

    let mut document = generator.playback.finalize_with_pcm(generator.builder);
    check_output_command_limit(options.max_output_commands, document.commands.len(), 0)
        .map_err(|error| generator.playback.conversion_failure(error))?;
    if package.pdx.is_some() {
        document.header.okim6258_flags.clock_divider = pcm_mixer::PCM8_OKIM6258_CLOCK_DIVIDER;
    }
    Ok(document)
}

/// Checks eager output against the limit, including commands retained from earlier passes.
/// `None` disables the limit; exceeding it returns `OutputCommandLimitExceeded`.
fn check_output_command_limit(
    max_output_commands: Option<u32>,
    commands: usize,
    retained_commands: usize,
) -> Result<(), MdxConvertError> {
    if let Some(max_output_commands) = max_output_commands
        && (retained_commands > max_output_commands as usize
            || commands > (max_output_commands as usize).saturating_sub(retained_commands))
    {
        return Err(MdxConvertError::OutputCommandLimitExceeded {
            max_output_commands,
        });
    }
    Ok(())
}

/// Outcome of one [`PlaybackState::step`] call.
enum StepOutcome {
    /// The song has finished; no further steps should be run.
    Finished,
    /// The song is still playing.
    Continue,
}

/// Resolved outcome of a `LoopEnd` command.
enum LoopEndAction {
    /// Repeat the (finite) loop body again from `start_index`.
    Repeat(usize),
    /// The loop body repeats forever (file-encoded count `0`); resolve via
    /// a native VGM loop point back to `start_index`.
    SongLoop(usize),
    /// No active loop; continue past this command without jumping.
    Fallthrough,
}

struct PcmTrackConfig {
    /// PDX bank selector for tracks >= 8, set by `0xfd`.
    bank: u8,
    /// Q16.16 resampler rate step; `0x10000` is 1.0x playback speed.
    rate_step: u32,
    /// Sample data format selected by the last `0xed` on a PCM8A track.
    data_kind: Pcm8aFormat,
}

#[derive(Default)]
struct LfoState {
    /// Per-channel PMS/AMS sensitivity, applied to register `0x38+ch`.
    pms_ams: u8,
    /// Whether the OPM's hardware LFO waveform position is reset on the
    /// next key-on (register `0x01` LFO_RESET pulse).
    opm_reset_pending: bool,
    /// Configured and remaining delay before pitch and volume LFO activation.
    delay: u8,
    /// Ticks remaining before the delayed LFOs are reset and activated.
    delay_counter: u8,
    /// Whether the pitch LFO is enabled.
    pitch_enabled: bool,
    /// Selected pitch LFO waveform.
    pitch_type: Option<MdxLfoWaveform>,
    /// Configured pitch LFO period in ticks.
    pitch_length: u16,
    /// Effective pitch LFO period after waveform-specific adjustment.
    pitch_length_cooked: u16,
    /// Ticks remaining in the current pitch LFO period.
    pitch_length_counter: u16,
    /// Initial pitch LFO step in fixed-point units.
    pitch_delta_start: i32,
    /// Current pitch LFO step in fixed-point units.
    pitch_delta: i32,
    /// Initial pitch LFO offset in fixed-point units.
    pitch_offset_start: i32,
    /// Current pitch LFO offset in fixed-point units.
    pitch_offset: i32,
    /// Whether the volume LFO is enabled.
    volume_enabled: bool,
    /// Selected volume LFO waveform.
    volume_type: Option<MdxLfoWaveform>,
    /// Configured volume LFO period in ticks.
    volume_length: u16,
    /// Ticks remaining in the current volume LFO period.
    volume_length_counter: u16,
    /// Initial volume LFO step.
    volume_delta_start: u16,
    /// Current volume LFO step.
    volume_delta: u16,
    /// Waveform-adjusted volume LFO step used by the update logic.
    volume_delta_cooked: u16,
    /// Current volume LFO attenuation offset.
    volume_offset: u16,
}

impl LfoState {
    /// Applies pitch-LFO enablement or waveform, frequency, and amplitude settings.
    fn configure_pitch(&mut self, command: MdxPitchLfo) {
        match command {
            MdxPitchLfo::SetEnabled { enabled } => {
                if enabled {
                    self.reset_pitch();
                    self.pitch_enabled = true;
                } else {
                    self.pitch_enabled = false;
                    self.pitch_offset = 0;
                }
            }
            MdxPitchLfo::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                self.pitch_enabled = true;
                let wave_type = waveform.base();
                let mode = wave_type << 1;
                self.pitch_type = Some(waveform.base_waveform());
                self.pitch_length = frequency;

                let mut cooked = frequency;
                if mode != 0x02 {
                    cooked >>= 1;
                    if mode == 0x06 {
                        cooked = 1;
                    }
                }
                self.pitch_length_cooked = cooked;

                let mut delta = i32::from(amplitude) << 8;
                if waveform.has_extended_amplitude() {
                    delta <<= 8;
                }
                self.pitch_delta_start = delta;
                self.pitch_offset_start = if wave_type == 0x02 { delta } else { 0 };
                self.reset_pitch();
            }
        }
    }

    /// Applies volume-LFO enablement or waveform, frequency, and amplitude settings.
    fn configure_volume(&mut self, command: MdxVolumeLfo) {
        match command {
            MdxVolumeLfo::SetEnabled { enabled } => {
                if enabled {
                    self.reset_volume();
                    self.volume_enabled = true;
                } else {
                    self.volume_enabled = false;
                    self.volume_offset = 0;
                }
            }
            MdxVolumeLfo::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                self.volume_enabled = true;
                let mode = waveform.raw() << 1;
                self.volume_type = Some(waveform.base_waveform());
                self.volume_length = frequency;
                self.volume_delta_start = amplitude;

                let mut cooked = i32::from(amplitude as i16);
                if mode & 0x02 == 0 {
                    cooked = cooked.wrapping_mul(i32::from(frequency as i16));
                }
                cooked = cooked.wrapping_neg();
                if cooked < 0 {
                    cooked = 0;
                }
                self.volume_delta_cooked = cooked as u16;
                self.reset_volume();
            }
        }
    }

    /// Restores the pitch LFO's initial phase, step, and offset.
    fn reset_pitch(&mut self) {
        self.pitch_length_counter = self.pitch_length_cooked;
        self.pitch_delta = self.pitch_delta_start;
        self.pitch_offset = self.pitch_offset_start;
    }

    /// Restores the volume LFO's initial phase, step, and offset.
    fn reset_volume(&mut self) {
        self.volume_length_counter = self.volume_length;
        self.volume_delta = self.volume_delta_start;
        self.volume_offset = self.volume_delta_cooked;
    }

    /// Starts the configured activation delay and clears both LFO offsets.
    fn start_delay(&mut self) {
        self.delay_counter = self.delay;
        self.pitch_offset = 0;
        self.volume_offset = 0;
        self.delay_counter = self.delay_counter.wrapping_sub(1);
        if self.delay_counter == 0 {
            if self.pitch_enabled {
                self.reset_pitch();
            }
            if self.volume_enabled {
                self.reset_volume();
            }
        }
    }

    /// Advances the activation delay and reports whether this tick skips LFO updates.
    fn should_skip_tick(&mut self, key_on_delay_counter: u8) -> bool {
        if self.delay == 0 {
            return false;
        }
        if key_on_delay_counter != 0 {
            return true;
        }
        if self.delay_counter == 0 {
            return false;
        }

        self.delay_counter -= 1;
        if self.delay_counter == 0 {
            if self.pitch_enabled {
                self.reset_pitch();
            }
            if self.volume_enabled {
                self.reset_volume();
            }
        }
        true
    }

    /// Advances the enabled pitch waveform by one tick.
    fn update_pitch(&mut self, random_seed: &mut u16) {
        if !self.pitch_enabled {
            return;
        }
        let Some(waveform) = self.pitch_type else {
            return;
        };
        match waveform {
            MdxLfoWaveform::Sawtooth => {
                self.pitch_offset = self.pitch_offset.wrapping_add(self.pitch_delta);
                self.pitch_length_counter = self.pitch_length_counter.wrapping_sub(1);
                if self.pitch_length_counter == 0 {
                    self.pitch_length_counter = self.pitch_length;
                    self.pitch_offset = self.pitch_offset.wrapping_neg();
                }
            }
            MdxLfoWaveform::Square => {
                self.pitch_offset = self.pitch_delta;
                self.pitch_length_counter = self.pitch_length_counter.wrapping_sub(1);
                if self.pitch_length_counter == 0 {
                    self.pitch_length_counter = self.pitch_length;
                    self.pitch_delta = self.pitch_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::Triangle => {
                self.pitch_offset = self.pitch_offset.wrapping_add(self.pitch_delta);
                self.pitch_length_counter = self.pitch_length_counter.wrapping_sub(1);
                if self.pitch_length_counter == 0 {
                    self.pitch_length_counter = self.pitch_length;
                    self.pitch_delta = self.pitch_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::RandomNoise => {
                self.pitch_length_counter = self.pitch_length_counter.wrapping_sub(1);
                if self.pitch_length_counter == 0 {
                    let random = i32::from(Self::next_random(random_seed) as i16);
                    self.pitch_offset = random.wrapping_mul(self.pitch_delta);
                    self.pitch_length_counter = self.pitch_length;
                }
            }
            MdxLfoWaveform::Unknown(_) => {}
        }
    }

    /// Advances the enabled volume waveform by one tick.
    fn update_volume(&mut self, random_seed: &mut u16) {
        if !self.volume_enabled {
            return;
        }
        let Some(waveform) = self.volume_type else {
            return;
        };
        match waveform {
            MdxLfoWaveform::Sawtooth => {
                self.volume_offset = self.volume_offset.wrapping_add(self.volume_delta);
                self.volume_length_counter = self.volume_length_counter.wrapping_sub(1);
                if self.volume_length_counter == 0 {
                    self.volume_length_counter = self.volume_length;
                    self.volume_offset = self.volume_delta_cooked;
                }
            }
            MdxLfoWaveform::Square => {
                self.volume_length_counter = self.volume_length_counter.wrapping_sub(1);
                if self.volume_length_counter == 0 {
                    self.volume_length_counter = self.volume_length;
                    self.volume_offset = self.volume_offset.wrapping_add(self.volume_delta);
                    self.volume_delta = self.volume_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::Triangle => {
                self.volume_offset = self.volume_offset.wrapping_add(self.volume_delta);
                self.volume_length_counter = self.volume_length_counter.wrapping_sub(1);
                if self.volume_length_counter == 0 {
                    self.volume_length_counter = self.volume_length;
                    self.volume_delta = self.volume_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::RandomNoise => {
                self.volume_length_counter = self.volume_length_counter.wrapping_sub(1);
                if self.volume_length_counter == 0 {
                    let random = i32::from(Self::next_random(random_seed) as i16);
                    let delta = i32::from(self.volume_delta as i16);
                    self.volume_offset = random.wrapping_mul(delta) as u16;
                    self.volume_length_counter = self.volume_length;
                }
            }
            MdxLfoWaveform::Unknown(_) => {}
        }
    }

    /// Advances the shared LFO random seed and returns the next value.
    fn next_random(seed: &mut u16) -> u16 {
        let value = u32::from(*seed).wrapping_mul(0xc549).wrapping_add(0x0c);
        *seed = value as u16;
        (value >> 8) as u16
    }
}

impl Default for PcmTrackConfig {
    fn default() -> Self {
        Self {
            bank: 0,
            rate_step: 0x10000,
            data_kind: Pcm8aFormat::Adpcm,
        }
    }
}

/// Per-track voice and note state; volume and key-off settings also feed PCM playback.
struct FmTrackState {
    /// Whether an FM note is currently keyed on.
    key_on: bool,
    /// Currently selected FM voice number.
    voice: u8,
    /// Set once a voice-select command has run at least once.
    voice_selected: bool,
    /// Whether a voice update is pending until the next key-on.
    voice_pending: bool,
    /// Whether a pan update is pending until the next key-on.
    pan_pending: bool,
    /// CON/FL of the currently selected tone (`con | fl << 3`).
    con_fl: u8,
    /// Tone key-on slot mask combined with the FM channel.
    key_on_slot: u8,
    /// Physical YM2151 channel currently used by this track.
    fm_channel: u8,
    /// FM pan bits written to YM2151 register `0x20`.
    pan: u8,
    /// Current MDX volume value, using the signed attenuation encoding.
    volume: u8,
    /// Gate ratio or signed gate adjustment used to calculate key-off timing.
    gate: i8,
    /// Whether the next note preserves the current sound instead of keying off.
    key_off_disabled: bool,
    /// Number of ticks to delay the next FM key-on.
    key_on_delay: u8,
    /// Remaining ticks in the pending FM key-on delay.
    key_on_delay_counter: u8,
    /// Whether a delayed FM key-on is waiting to be triggered.
    key_on_pending: bool,
    /// Fixed pitch offset applied to notes by the detune command.
    detune: i16,
    /// Signed semitone offset applied to subsequently played notes.
    transpose: i32,
    /// Current MDX note pitch before bend and LFO offsets are applied.
    note_pitch: Option<u16>,
    /// Last pitch written to the OPM registers, used to avoid redundant writes.
    last_written_pitch: Option<u16>,
    /// Accumulated pitch-bend offset in the playback fixed-point representation.
    bend_offset: i32,
    /// Per-tick pitch-bend increment from the portamento command.
    bend_delta: i32,
    /// Whether portamento applies to the current note.
    portamento_active: bool,
}

impl Default for FmTrackState {
    fn default() -> Self {
        Self {
            key_on: false,
            voice: 0,
            voice_selected: false,
            voice_pending: false,
            pan_pending: false,
            con_fl: 0,
            key_on_slot: 0,
            fm_channel: 0,
            pan: 0xc0,
            volume: 8,
            gate: 8,
            key_off_disabled: false,
            key_on_delay: 0,
            key_on_delay_counter: 0,
            key_on_pending: false,
            detune: 0,
            transpose: 0,
            note_pitch: None,
            last_written_pitch: None,
            bend_offset: 0,
            bend_delta: 0,
            portamento_active: false,
        }
    }
}

impl FmTrackState {
    /// Adjusts the encoded MDX volume down by one step.
    fn volume_down(&mut self) {
        self.volume = if self.volume & 0x80 == 0 {
            self.volume.saturating_sub(1)
        } else if self.volume != 0xff {
            self.volume + 1
        } else {
            self.volume
        };
    }

    /// Adjusts the encoded MDX volume up by one step.
    fn volume_up(&mut self) {
        self.volume = if self.volume & 0x80 == 0 {
            if self.volume < 15 {
                self.volume + 1
            } else {
                self.volume
            }
        } else if self.volume != 0x80 {
            self.volume - 1
        } else {
            self.volume
        };
    }

    /// Resolves the configured key-on slot mask or the algorithm default.
    fn resolved_key_on_slot(&self) -> u8 {
        if self.key_on_slot & 0xf8 == 0 {
            CARRIER_KEYON_SLOTS[(self.con_fl & 0x07) as usize] | self.fm_channel
        } else {
            self.key_on_slot
        }
    }
}

struct TrackState {
    /// Index of the next MDX command to process for this track.
    command_index: usize,
    /// Remaining ticks before this track can process another command.
    wait_ticks: u16,
    /// Remaining ticks before the currently sounding note is keyed off.
    key_off_ticks: u16,
    /// Whether this track still has commands or a pending playback state.
    active: bool,
    /// FM voice, pitch, key-on, and volume state for this track.
    fm: FmTrackState,
    /// Whether this track is blocked at a synchronization wait.
    sync_wait: bool,
    /// Nested loop stack containing remaining counts and command indices.
    loop_stack: Vec<(u32, usize)>,
    /// OPM hardware and software LFO configuration and phase.
    lfo: LfoState,
    /// Per-track PCM playback selection for PCM tracks >= 8.
    pcm: PcmTrackConfig,
}

impl TrackState {
    /// Decrements the command wait timer by one tick, if it is active.
    fn advance_wait(&mut self) {
        self.wait_ticks = self.wait_ticks.saturating_sub(1);
    }

    /// Decrements the key-off timer and reports when it reaches zero.
    fn advance_key_off(&mut self) -> bool {
        if self.key_off_ticks == 0 {
            return false;
        }
        self.key_off_ticks -= 1;
        self.key_off_ticks == 0
    }

    /// Starts note timers using the track's gate and tie settings.
    fn start_note(&mut self, length: u16) {
        self.wait_ticks = length;
        self.key_off_ticks = if self.fm.key_off_disabled {
            0
        } else {
            let gate = i16::from(self.fm.gate);
            let raw_length = (length - 1).min(i16::MAX as u16) as i16;
            let key_off = if gate >= 0 {
                ((raw_length * gate) >> 3) + 1
            } else {
                (raw_length + gate).max(0) + 1
            };
            key_off as u16
        };
        self.fm.key_off_disabled = false;
    }

    /// Starts a rest and cancels any pending tie.
    fn start_rest(&mut self, ticks: u16) {
        self.wait_ticks = ticks;
        self.key_off_ticks = ticks;
        self.fm.key_off_disabled = false;
    }
}

struct PcmOutputState {
    /// Whether the package contains PCM playback for the OKIM6258 path.
    has_pcm: bool,
    /// Per-channel ADPCM/PCM playback state for tracks 8-15.
    channels: [PcmChannelState; 8],
    /// Single decoded PCM arena shared by all PCM channels.
    samples: Vec<i16>,
    /// Maximum cached decoded samples; `None` disables the limit.
    max_decoded_pcm_samples: Option<u32>,
    /// Ranges in `samples` indexed by PDX source start, size, format, and scaling.
    sample_ranges: HashMap<(u32, u32, u8, bool), (usize, usize)>,
    /// Persistent re-encoder state for the whole song's mixed PCM8 output.
    encoder: AdpcmEncoder,
    /// Persistent output-filter state for the mixed PCM8 stream.
    filter: PcmOutputFilter,
    /// PDX reference for the raw PCM1 ADPCM payload used by `Through` mode.
    raw_sample: Option<MdxPcmReference>,
    /// Length of the referenced raw payload.
    raw_length: usize,
    /// Next raw byte to emit in the `Through` mode.
    raw_position: usize,
    /// MCK-driven OKIM6258 data-register write scheduler.
    mck_scheduler: MckScheduler,
}

impl PcmOutputState {
    /// Initializes PCM output state for the package and selected processing mode.
    fn new(has_pcm: bool, adpcm_mode: AdpcmMode) -> Self {
        Self {
            has_pcm,
            channels: Default::default(),
            samples: Vec::new(),
            max_decoded_pcm_samples: MdxToVgmOptions::default().max_decoded_pcm_samples,
            sample_ranges: HashMap::new(),
            encoder: AdpcmEncoder::default(),
            filter: PcmOutputFilter::new(matches!(adpcm_mode, AdpcmMode::Lpf)),
            raw_sample: None,
            raw_length: 0,
            raw_position: 0,
            mck_scheduler: MckScheduler::new(
                pcm_mixer::PCM8_MASTER_SAMPLE_RATE * (MCK_RATE_DENOMINATOR / 2),
            ),
        }
    }

    /// Reports whether raw bytes or mixed channel blocks still need output.
    fn has_pending_output(&self, pcm_mode: MdxPcmMode, adpcm_mode: AdpcmMode) -> bool {
        let raw_pending =
            matches!(pcm_mode, MdxPcmMode::LegacyAdpcm) && self.raw_position < self.raw_length;
        let mixed_pending = !(matches!(pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(adpcm_mode, AdpcmMode::Through))
            && self
                .channels
                .iter()
                .any(|channel| channel.block_length != 0);
        raw_pending || mixed_pending
    }

    /// Stops a channel or holds its current block for a tied note.
    fn stop_channel(&mut self, channel: usize, hold: bool) {
        let state = &mut self.channels[channel];
        if hold {
            state.hold = true;
        } else {
            state.block_length = 0;
            state.block_key = None;
            state.pos_in_block = 0;
            state.rate_counter = 0;
            state.hold = false;
        }
    }

    /// Allows a held channel to continue past the end of its current block.
    fn release_channel_hold(&mut self, channel: usize) {
        self.channels[channel].hold = false;
    }

    /// Sets the output gain for a PCM channel.
    fn set_channel_gain(&mut self, channel: usize, gain: u8) {
        self.channels[channel].gain = gain;
    }

    /// Checks whether a channel is holding the requested sample block.
    fn is_holding_block(&self, channel: usize, block_key: (usize, usize, u8)) -> bool {
        let state = &self.channels[channel];
        state.hold && state.block_key == Some(block_key)
    }

    /// Initializes a channel to play the selected sample range and rate.
    fn start_channel(
        &mut self,
        channel: usize,
        block_key: (usize, usize, u8),
        range: Option<(usize, usize)>,
        rate_step: u32,
        gain: u8,
        hold: bool,
    ) {
        let state = &mut self.channels[channel];
        state.block_start = range.map_or(0, |(start, _)| start);
        state.block_length = range.map_or(0, |(_, length)| length as u32);
        state.block_key = range.map(|_| block_key);
        state.pos_in_block = 0;
        state.rate_counter = 0;
        state.rate_step = rate_step;
        state.gain = gain;
        state.hold = hold;
    }

    /// Selects a raw pass-through payload without copying it and rewinds playback.
    fn set_raw_sample(&mut self, reference: MdxPcmReference, length: usize) {
        self.raw_sample = Some(reference);
        self.raw_length = length;
        self.raw_position = 0;
    }

    /// Clears the raw pass-through reference and resets its read position.
    fn clear_raw_sample(&mut self) {
        self.raw_sample = None;
        self.raw_length = 0;
        self.raw_position = 0;
    }

    /// Returns a decoded range shared by identical PDX source ranges and decode conditions.
    /// Returns an error before decoding if the cache sample limit would be exceeded.
    fn decode_sample(
        &mut self,
        package: &MdxPackage,
        bank: usize,
        note: usize,
        format: Pcm8aFormat,
        pcm16_is_15khz: bool,
    ) -> Result<Option<(usize, usize)>, MdxConvertError> {
        let Some(pdx) = package.pdx.as_ref() else {
            return Ok(None);
        };
        let Some(sample) = pdx.entry(bank, note) else {
            return Ok(None);
        };
        let format_key = match format {
            Pcm8aFormat::Adpcm => 0u8,
            Pcm8aFormat::Pcm16 => 1u8,
            Pcm8aFormat::Pcm8 => 2u8,
        };
        let pcm16_is_15khz = format == Pcm8aFormat::Pcm16 && pcm16_is_15khz;
        let key = (sample.start, sample.size, format_key, pcm16_is_15khz);
        if let Some(&range) = self.sample_ranges.get(&key) {
            return Ok(Some(range));
        }
        let Some(bytes) = pdx.sample_bytes(bank, note) else {
            return Ok(None);
        };
        let length = match format {
            Pcm8aFormat::Adpcm => bytes.len().checked_mul(2),
            Pcm8aFormat::Pcm8 => Some(bytes.len()),
            Pcm8aFormat::Pcm16 if bytes.len().is_multiple_of(2) => Some(bytes.len() / 2),
            Pcm8aFormat::Pcm16 => return Ok(None),
        };
        if let Some(max_decoded_pcm_samples) = self.max_decoded_pcm_samples
            && (self.samples.len() > max_decoded_pcm_samples as usize
                || length.is_none_or(|length| {
                    length > (max_decoded_pcm_samples as usize).saturating_sub(self.samples.len())
                }))
        {
            return Err(MdxConvertError::DecodedPcmSampleLimitExceeded {
                max_decoded_pcm_samples,
            });
        }
        let Some(length) = length else {
            return Err(MdxConvertError::InvalidDocument(
                "decoded PCM sample length exceeds addressable storage".to_owned(),
            ));
        };
        let Ok(decoded) = decode_pcm8a_with_pcm16_15khz(format, bytes, pcm16_is_15khz) else {
            return Ok(None);
        };
        let start = self.samples.len();
        self.samples.extend_from_slice(&decoded);
        let range = (start, length);
        self.sample_ranges.insert(key, range);
        Ok(Some(range))
    }

    /// Produces the next mixed or raw ADPCM byte for the output stream.
    fn next_adpcm_byte(&mut self, mix_pcm: bool, package: &MdxPackage) -> u8 {
        if mix_pcm {
            pcm_mixer::mix_and_encode_byte(
                &mut self.channels,
                &self.samples,
                &mut self.encoder,
                &mut self.filter,
            )
        } else {
            let byte = self
                .raw_sample
                .as_ref()
                .and_then(|reference| package.pcm_sample_bytes(reference))
                .and_then(|bytes| bytes.get(self.raw_position))
                .copied()
                .unwrap_or(0x80);
            self.raw_position = self.raw_position.saturating_add(1);
            byte
        }
    }
}

#[derive(Default)]
struct PlaybackTimingState {
    /// Current MDX tempo, used to derive the duration of one playback tick.
    tempo: u8,
    /// Fractional VGM samples owed after converting elapsed tick time at the
    /// configured output sample rate.
    sample_remainder: u32,
    elapsed_ticks: u32,
    commands_since_tick: u32,
}

impl PlaybackTimingState {
    /// Starts at the default MDX tempo with no fractional samples carried over.
    fn new() -> Self {
        Self {
            tempo: DEFAULT_TEMPO,
            sample_remainder: 0,
            elapsed_ticks: 0,
            commands_since_tick: 0,
        }
    }

    /// Advances one MDX tick and returns its duration and VGM sample count.
    fn advance_tick(&mut self) -> (u32, u32) {
        self.elapsed_ticks = self.elapsed_ticks.saturating_add(1);
        self.commands_since_tick = 0;
        let tick_microseconds = 256 * u32::from(256u16 - u16::from(self.tempo));
        let sample_accumulator = self.sample_remainder + tick_microseconds * VGM_SAMPLE_RATE;
        let samples = sample_accumulator / MICROSECONDS_PER_SECOND;
        self.sample_remainder = sample_accumulator % MICROSECONDS_PER_SECOND;
        (tick_microseconds, samples)
    }
}

#[derive(Default)]
struct SongLoopState {
    /// Optional finite repeat limit for an unconditional whole-song repeat.
    loop_count: Option<u32>,
    /// VGM command index recorded on the first visit to each repeat target.
    loop_starts: HashMap<(usize, usize), usize>,
    /// Number of finite backward jumps taken at each jump command.
    jump_repeat_counts: HashMap<(usize, usize), u32>,
    /// Number of F1 boundaries reached by each track in unsynchronized cases.
    track_loop_counts: HashMap<usize, u32>,
    /// Tracks participating in a synchronized F1 loop.
    synchronized_f1_tracks: HashSet<usize>,
    /// Per-track command indices targeted by each track's F1 loop.
    synchronized_f1_targets: HashMap<usize, usize>,
    /// Tracks that have reached F1 during the current synchronized pass.
    synchronized_f1_arrivals: HashSet<usize>,
    /// Number of complete synchronized F1 loop bodies emitted for finite playback.
    f1_loop_passes_completed: u32,
    /// VGM command index to use as the native loop point, once detected.
    loop_index: Option<usize>,
    /// Set once a native loop point is established and internal playback stops.
    loop_complete: bool,
    /// Whether an `F1` per-track terminator was encountered.
    track_end_loop_seen: bool,
    /// Whether an unconditional repeat should be recorded as a native VGM loop.
    ///
    /// Eager conversion records a fixed loop point and stops at its second
    /// visit; lazy streaming keeps repeating because it cannot rewind output.
    mark_native_loop: bool,
}

impl SongLoopState {
    /// Initializes repeat tracking with the requested count and loop-point policy.
    fn new(
        loop_count: Option<u32>,
        mark_native_loop: bool,
        synchronized_f1_tracks: HashSet<usize>,
    ) -> Self {
        Self {
            loop_count,
            mark_native_loop,
            synchronized_f1_tracks,
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct FadeoutState {
    /// Whether any track contains an embedded fadeout command.
    has_command: bool,
    /// Whether a fadeout marker has been reached during playback.
    seen: bool,
    /// Fadeout counter reload value supplied by the MDX command.
    speed: u8,
    /// Remaining fadeout counter value, decremented by two on each tick.
    counter: i16,
    /// Current global fadeout attenuation level.
    level: u8,
}

impl FadeoutState {
    /// Starts fadeout with the requested counter reload speed.
    fn start(&mut self, speed: u8) {
        self.seen = true;
        self.speed = speed;
        self.counter = i16::from(speed);
    }

    /// Advances the fadeout and reports whether its attenuation level changed.
    fn advance(&mut self) -> bool {
        if !self.seen || self.level >= FADEOUT_FINAL_LEVEL {
            return false;
        }
        if self.counter >= 0 {
            self.counter -= 2;
            return false;
        }

        self.level += 1;
        self.counter = i16::from(self.speed);
        true
    }
}

struct PlaybackState<P: Borrow<MdxPackage>> {
    /// MDX/PDX package being consumed by the playback simulation.
    package: P,
    /// MDX PCM command semantics selected from the header's track layout.
    pcm_mode: MdxPcmMode,
    /// ADPCM processing mode selected by the caller.
    adpcm_mode: AdpcmMode,
    /// Maximum MDX commands executed across all tracks between elapsed ticks.
    /// `None` disables the limit.
    max_commands_per_tick: Option<u32>,
    /// Per-track command cursors and playback state for the MDX tracks.
    tracks: Vec<TrackState>,
    /// Current document ranges, cached on the first jump for this playback.
    jump_source_map: Option<Vec<Vec<(usize, usize)>>>,
    /// Optional diagnostic tracking, with budgets only for playback checking.
    check_state: Option<PlaybackCheckState>,
    /// PCM channel, sample, encoder, and byte-scheduler state.
    pcm_output: PcmOutputState,
    /// Tempo and fractional VGM sample timing state.
    timing: PlaybackTimingState,
    /// Shadow of OPM register `0x0f` (noise enable + frequency), needed to
    /// preserve the noise-enable bit when only the frequency is updated.
    opm_reg_0f: u8,
    /// Shadow of OPM register `0x1b` (CT1/CT2 + LFO waveform), needed to
    /// preserve the CT bits when only the LFO waveform is updated.
    opm_reg_1b: u8,
    /// Shared LFO random generator seed used across tracks.
    lfo_rand_seed: u16,
    /// Whole-song repeat detection and native VGM loop-point state.
    song_loop: SongLoopState,
    /// Embedded fadeout detection and attenuation progression.
    fadeout: FadeoutState,
}

impl<P: Borrow<MdxPackage>> PlaybackState<P> {
    /// Initializes track, timing, PCM, loop, and fadeout state for playback.
    fn new(
        package: P,
        pcm_mode: MdxPcmMode,
        adpcm_mode: AdpcmMode,
        loop_count: Option<u32>,
        mark_native_loop: bool,
    ) -> Self {
        let has_pcm = package.borrow().drives_okim6258();
        let has_fadeout_command = package.borrow().mdx.tracks.iter().flatten().any(|command| {
            matches!(
                command,
                MdxCommand::Extended(MdxExtendedCommand::Fadeout { .. })
            )
        });
        let has_other_unbounded_loop =
            package.borrow().mdx.tracks.iter().flatten().any(|command| {
                matches!(command, MdxCommand::Jump(jump) if jump.offset < 0)
                    || matches!(command, MdxCommand::LoopStart(start) if start.count == 0)
            });
        let synchronized_f1_tracks = if !has_other_unbounded_loop
            && (loop_count.is_some() || (mark_native_loop && !has_fadeout_command))
        {
            package
                .borrow()
                .mdx
                .tracks
                .iter()
                .enumerate()
                .filter_map(|(track, commands)| {
                    commands
                        .iter()
                        .take_while(|command| !matches!(command, MdxCommand::EndOfTrack(_)))
                        .any(|command| {
                            matches!(command, MdxCommand::EndOfTrackLoop(offset) if offset.offset < 0)
                        })
                        .then_some(track)
                })
                .collect()
        } else {
            HashSet::new()
        };
        let tracks = package
            .borrow()
            .mdx
            .tracks
            .iter()
            .enumerate()
            .map(|(track, commands)| TrackState {
                command_index: 0,
                wait_ticks: 0,
                key_off_ticks: 0,
                active: !commands.is_empty(),
                fm: FmTrackState {
                    fm_channel: track as u8,
                    ..FmTrackState::default()
                },
                sync_wait: false,
                loop_stack: Vec::new(),
                lfo: LfoState::default(),
                pcm: PcmTrackConfig::default(),
            })
            .collect();
        Self {
            package,
            pcm_mode,
            adpcm_mode,
            max_commands_per_tick: None,
            tracks,
            jump_source_map: None,
            check_state: None,
            pcm_output: PcmOutputState::new(has_pcm, adpcm_mode),
            timing: PlaybackTimingState::new(),
            opm_reg_0f: 0,
            opm_reg_1b: 0,
            lfo_rand_seed: 0x1234,
            song_loop: SongLoopState::new(loop_count, mark_native_loop, synchronized_f1_tracks),
            fadeout: FadeoutState {
                has_command: has_fadeout_command,
                ..FadeoutState::default()
            },
        }
    }

    /// Checks if playback has finished due to a completed song loop, a completed
    /// fadeout, or all tracks and pending PCM output being drained.
    fn finished(&self) -> bool {
        self.song_loop.loop_complete
            || (self.fadeout.seen && self.fadeout.level >= FADEOUT_FINAL_LEVEL)
            || (!self.fadeout.seen
                && self.tracks.iter().all(|track| !track.active)
                && !self.has_pending_pcm_output())
    }

    /// Returns whether raw or mixed PCM output remains to be emitted.
    fn has_pending_pcm_output(&self) -> bool {
        self.pcm_output
            .has_pending_output(self.pcm_mode, self.adpcm_mode)
    }

    /// Runs one tick of playback, appending any resulting commands to
    /// `builder`. Shared by the eager [`to_vgm_document`] loop and
    /// [`MdxVgmGenerator`], which drives the same steps lazily.
    fn step(
        &mut self,
        builder: &mut VgmBuilder,
        max_ticks: Option<u32>,
    ) -> Result<StepOutcome, MdxConvertError> {
        if self.finished() {
            return Ok(StepOutcome::Finished);
        }
        if let Some(state) = &mut self.check_state
            && let Some(limits) = state.limits
        {
            if state.ticks >= limits.max_ticks {
                state.position = None;
                state.limit_hit = Some("tick");
                return Err(MdxConvertError::InvalidOptions(
                    "playback check tick limit exceeded",
                ));
            }
            state.ticks += 1;
        }
        let loop_restarted = self.process_tick(builder)?;
        if self.finished() {
            return Ok(StepOutcome::Finished);
        }
        if loop_restarted {
            return Ok(StepOutcome::Continue);
        }
        if let Some(max_ticks) = max_ticks
            && self.timing.elapsed_ticks >= max_ticks
        {
            return Err(MdxConvertError::TickLimitExceeded { max_ticks });
        }
        self.emit_wait(builder);
        Ok(StepOutcome::Continue)
    }

    /// Finalizes the document, applying the native VGM loop point detected
    /// from an unconditional repeat, if any (see `take_repeating_jump`).
    fn finalize(&self, mut builder: VgmBuilder) -> VgmDocument {
        if let Some(loop_index) = self.song_loop.loop_index {
            builder.set_loop_index(loop_index);
        }
        builder.finalize()
    }

    /// Emits the initial YM2151 register setup commands to the VGM builder.
    fn emit_initialization(&mut self, builder: &mut VgmBuilder) {
        for channel in 0..8u8 {
            write_ym2151(builder, 0x38 + channel, 0);
        }
        write_ym2151(builder, 0x01, 0);
        write_ym2151(builder, 0x0f, 0);
        write_ym2151(builder, 0x19, 0);
        write_ym2151(builder, 0x19, 0x80);
        self.opm_reg_0f = 0;
        self.opm_reg_1b = 0;
    }

    /// Processes one tick for all active tracks, handling key-off, key-on,
    /// portamento, and command execution. Returns an error if any track
    /// encounters an issue during processing.
    fn process_tick(&mut self, builder: &mut VgmBuilder) -> Result<bool, MdxConvertError> {
        self.advance_fadeout(builder);
        if self.fadeout.level >= FADEOUT_FINAL_LEVEL {
            return Ok(false);
        }
        for track_index in 0..self.tracks.len() {
            if track_index >= 8 && self.synchronized_f1_barrier_ready() {
                break;
            }
            if !self.tracks[track_index].active {
                continue;
            }
            if let Some(state) = &mut self.check_state {
                state.position = Some((
                    track_index,
                    self.tracks[track_index].command_index.saturating_sub(1),
                ));
            }
            self.tracks[track_index].advance_wait();
            if track_index < 8 {
                self.update_fm_tick(track_index, builder)?;
            }
            self.process_key_off(track_index, builder);
            self.process_key_on_delay(track_index, builder)?;
            // Sync-wait only blocks new command processing; envelope, pitch
            // and LFO updates above still run while a track waits.
            if self.tracks[track_index].sync_wait {
                continue;
            }
            if self.tracks[track_index].wait_ticks > 0 {
                continue;
            }
            // Portamento only affects the note it immediately precedes.
            self.tracks[track_index].fm.portamento_active = false;
            self.process_commands(track_index, builder)?;
        }
        Ok(self.complete_synchronized_f1_barrier(builder))
    }

    /// Processes a key-off event for the specified track, handling both FM
    /// and ADPCM/PCM channels as appropriate. For FM channels (< 8), it
    /// sends the key-off command to the YM2151. For ADPCM/PCM channels (>= 8),
    /// it stops the PCM channel according to the track's key-off state.
    fn process_key_off(&mut self, track: usize, builder: &mut VgmBuilder) {
        if !self.tracks[track].advance_key_off() {
            return;
        }
        if track < 8 {
            if self.tracks[track].fm.key_on {
                write_ym2151(builder, 0x08, self.tracks[track].fm.fm_channel);
                self.tracks[track].fm.key_on = false;
            }
        } else {
            self.stop_pcm_channel(track);
        }
    }

    /// Stops the ADPCM/PCM channel mapped to `track` (>= 8). A tie
    /// (`key_off_disabled`) holds the channel at the end of its block instead
    /// of clearing it, so a following tied note is not retriggered.
    fn stop_pcm_channel(&mut self, track: usize) {
        let hold = self.tracks[track].fm.key_off_disabled;
        self.pcm_output.stop_channel(track - 8, hold);
    }

    /// Applies an ADPCM/PCM key-on for `track` (>= 8): resolves the note
    /// (`0x80`-based) against the track's current PDX bank and data format,
    /// referencing raw ADPCM or decoding and caching mixed PCM as needed.
    /// A held channel playing the same sample is not triggered again.
    fn begin_pcm_key_on(&mut self, track: usize, note: u8) -> Result<(), MdxConvertError> {
        let Some(note_index) = note.checked_sub(0x80) else {
            return Ok(());
        };
        let bank = usize::from(self.tracks[track].pcm.bank);
        let note_index = usize::from(note_index);
        let format = self.tracks[track].pcm.data_kind;
        let tie = self.tracks[track].fm.key_off_disabled;
        let channel = track - 8;

        let format_key = match format {
            Pcm8aFormat::Adpcm => 0u8,
            Pcm8aFormat::Pcm16 => 1u8,
            Pcm8aFormat::Pcm8 => 2u8,
        };
        let block_key = (bank, note_index, format_key);
        let rate_step = self.tracks[track].pcm.rate_step;
        let gain = self.pcm_channel_gain(track);
        if self.pcm_output.is_holding_block(channel, block_key) {
            // F7 followed by the same PCM note is a held note, not a second
            // trigger.
            return Ok(());
        }
        if matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(self.adpcm_mode, AdpcmMode::Through)
            && track == 8
        {
            let reference = MdxPcmReference {
                track,
                bank,
                note: note_index,
                sample: self
                    .package
                    .borrow()
                    .pdx
                    .as_ref()
                    .and_then(|pdx| pdx.entry(bank, note_index)),
            };
            let raw_length = self
                .package
                .borrow()
                .pcm_sample_bytes(&reference)
                .map_or(0, <[u8]>::len);
            self.pcm_output.set_raw_sample(reference, raw_length);
        }
        let pcm16_is_15khz = format == Pcm8aFormat::Pcm16 && rate_step == 0x10000;
        let range = if self.pcm_uses_mixer() {
            let package = self.package.borrow();
            self.pcm_output
                .decode_sample(package, bank, note_index, format, pcm16_is_15khz)?
        } else {
            self.package
                .borrow()
                .pdx
                .as_ref()
                .and_then(|pdx| pdx.sample_bytes(bank, note_index))
                .map(|bytes| (0, bytes.len() * 2))
        };
        self.pcm_output
            .start_channel(channel, block_key, range, rate_step, gain, tie);
        Ok(())
    }

    /// Applies a volume change immediately to a currently playing ADPCM/PCM
    /// channel (`track` >= 8), rather than waiting for the next key-on.
    fn apply_live_pcm_gain(&mut self, track: usize) {
        let gain = self.pcm_channel_gain(track);
        self.pcm_output.set_channel_gain(track - 8, gain);
    }

    /// Calculates a PCM track's output gain, including fadeout when it is mixed.
    fn pcm_channel_gain(&self, track: usize) -> u8 {
        let fadeout_level = if self.pcm_uses_mixer() {
            self.fadeout.level
        } else {
            0
        };
        if fadeout_level == 0 {
            pcm_mixer::pcm8_gain(self.tracks[track].fm.volume)
        } else {
            pcm_mixer::pcm8_gain_with_fadeout(self.tracks[track].fm.volume, fadeout_level)
        }
    }

    /// Returns whether PCM is decoded and mixed instead of passed through raw.
    fn pcm_uses_mixer(&self) -> bool {
        !(matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(self.adpcm_mode, AdpcmMode::Through))
    }

    /// Processes all pending commands for the specified track, updating the
    /// VGM builder as necessary. Handles note, rest, and other MDX commands,
    /// managing key-on, key-off, and wait ticks for the track. Returns an error
    /// if any command processing fails.
    fn process_commands(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        while self.tracks[track].active && self.tracks[track].wait_ticks == 0 {
            let Some(command) = self.package.borrow().mdx.tracks[track]
                .get(self.tracks[track].command_index)
                .cloned()
            else {
                self.tracks[track].active = false;
                break;
            };
            if let Some(state) = &mut self.check_state {
                state.position = Some((track, self.tracks[track].command_index));
                if let Some(limits) = state.limits {
                    if state.commands >= limits.max_commands {
                        state.limit_hit = Some("MDX command");
                        return Err(MdxConvertError::InvalidOptions(
                            "playback check command limit exceeded",
                        ));
                    }
                    state.commands += 1;
                }
            }
            if let Some(max_commands_per_tick) = self.max_commands_per_tick
                && self.timing.commands_since_tick >= max_commands_per_tick
            {
                return Err(MdxConvertError::CommandLimitExceeded {
                    max_commands_per_tick,
                });
            }
            self.timing.commands_since_tick = self.timing.commands_since_tick.saturating_add(1);
            self.tracks[track].command_index += 1;
            if matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
                && track == 8
                && !matches!(&command, MdxCommand::Note(_))
            {
                self.pcm_output.clear_raw_sample();
            }
            match command {
                MdxCommand::Rest(command) => {
                    // Rest always (re)starts the key-off countdown and
                    // cancels any pending tie, regardless of prior state.
                    self.tracks[track].start_rest(command.ticks);
                    if track >= 8 {
                        // A rest clears the ADPCM hold while allowing the
                        // sample to continue to its own end.
                        self.pcm_output.release_channel_hold(track - 8);
                    }
                }
                MdxCommand::Note(command) if track < 8 => {
                    let note = i32::from(command.note - 0x80) + self.tracks[track].fm.transpose;
                    let note = note.clamp(0, 127) as u16;
                    let pitch = (note << 6)
                        .saturating_add(5)
                        .saturating_add_signed(self.tracks[track].fm.detune);
                    self.tracks[track].fm.note_pitch = Some(pitch);
                    self.write_pitch(track, builder, pitch)?;
                    if self.tracks[track].fm.key_on_delay == 0 {
                        self.begin_key_on(track, builder)?;
                    } else {
                        self.tracks[track].fm.key_on_delay_counter =
                            self.tracks[track].fm.key_on_delay;
                        self.tracks[track].fm.key_on_pending = true;
                    }
                    self.tracks[track].start_note(command.length);
                }
                MdxCommand::Note(command) => {
                    // PCM key-on: `note` is an 0x80-based index into the
                    // track's current PDX bank rather than a pitch.
                    self.begin_pcm_key_on(track, command.note)?;
                    self.tracks[track].start_note(command.length);
                }
                MdxCommand::Tempo(command) => {
                    self.timing.tempo = command.value.max(1);
                }
                MdxCommand::OpmRegisterWrite(command) => {
                    if command.register == 0x0f {
                        self.opm_reg_0f = command.value;
                    } else if command.register == 0x1b {
                        self.opm_reg_1b = command.value;
                    }
                    write_ym2151(builder, command.register, command.value);
                }
                MdxCommand::VoiceOrPcmBank(command) if track < 8 => {
                    self.tracks[track].fm.voice = command.value;
                    self.tracks[track].fm.voice_selected = true;
                    self.tracks[track].fm.voice_pending = true;
                }
                MdxCommand::VoiceOrPcmBank(command) => {
                    self.tracks[track].pcm.bank = command.value;
                }
                MdxCommand::EndOfTrack(_) => {
                    // The reference clears the key-on flag without sending
                    // an explicit key-off; the note's own gate/key-off
                    // countdown is expected to have already released it.
                    self.tracks[track].active = false;
                    self.tracks[track].fm.key_on = false;
                }
                MdxCommand::Pan(command) if track < 8 => {
                    self.tracks[track].fm.pan = match command {
                        MdxPan::Right => 0x40,
                        MdxPan::Left => 0x80,
                        MdxPan::Center => 0xc0,
                        MdxPan::Mute | MdxPan::Unknown(_) => 0,
                    };
                    self.tracks[track].fm.pan_pending = true;
                }
                MdxCommand::Pan(command) => {
                    // MDX ADPCM pan values are 0=mute, 1=left, 2=right,
                    // 3=center. The VGM OKIM6258 pan extension uses
                    // 0=center, 1=left, 2=right, 3=mute.
                    let pan = match command {
                        MdxPan::Mute => 3,
                        MdxPan::Right => 1,
                        MdxPan::Left => 2,
                        MdxPan::Center => 0,
                        MdxPan::Unknown(value) => value & 0x03,
                    };
                    builder.add_vgm_command((
                        Instance::Primary,
                        Okim6258Spec {
                            register: 0x02,
                            value: pan,
                        },
                    ));
                }
                MdxCommand::Volume(command) if track < 8 => {
                    self.tracks[track].fm.volume = command.value;
                    self.emit_volume(track, builder);
                }
                MdxCommand::Volume(command) => {
                    self.tracks[track].fm.volume = command.value;
                    self.apply_live_pcm_gain(track);
                }
                MdxCommand::VolumeDown(_) if track < 8 => {
                    self.tracks[track].fm.volume_down();
                    self.emit_volume(track, builder);
                }
                MdxCommand::VolumeDown(_) => {
                    self.tracks[track].fm.volume_down();
                    self.apply_live_pcm_gain(track);
                }
                MdxCommand::VolumeUp(_) if track < 8 => {
                    self.tracks[track].fm.volume_up();
                    self.emit_volume(track, builder);
                }
                MdxCommand::VolumeUp(_) => {
                    self.tracks[track].fm.volume_up();
                    self.apply_live_pcm_gain(track);
                }
                MdxCommand::Gate(command) => self.tracks[track].fm.gate = command.value as i8,
                MdxCommand::KeyOffDisable(_) => self.tracks[track].fm.key_off_disabled = true,
                MdxCommand::KeyOnDelay(command) => {
                    self.tracks[track].fm.key_on_delay = command.value;
                    self.tracks[track].fm.key_on_delay_counter = 0;
                    self.tracks[track].fm.key_on_pending = false;
                }
                MdxCommand::Detune(command) => self.tracks[track].fm.detune = command.offset,
                MdxCommand::Portamento(command) => {
                    self.tracks[track].fm.bend_delta = i32::from(command.offset) << 8;
                    self.tracks[track].fm.portamento_active = true;
                }
                MdxCommand::SyncWait(_) => {
                    self.tracks[track].sync_wait = true;
                    self.tracks[track].wait_ticks = 1;
                }
                MdxCommand::SyncSend(command) => {
                    if let Some(target) = self.tracks.get_mut(usize::from(command.value))
                        && target.sync_wait
                    {
                        target.sync_wait = false;
                        target.wait_ticks = 0;
                    }
                }
                MdxCommand::AdpcmOrNoiseFrequency(command) if track < 8 => {
                    // Preserve the noise-enable bit (0x80); only the
                    // frequency bits are updated by this command.
                    let combined = (self.opm_reg_0f & 0x80) | (command.value & 0x1f);
                    self.opm_reg_0f = combined;
                    write_ym2151(builder, 0x0f, combined);
                }
                MdxCommand::AdpcmOrNoiseFrequency(command) => {
                    // Standard nine-track MDX uses the legacy ADPCM rate
                    // selector (F0-F4); extended MDX uses PCM8A F0-F12.
                    let mode = PCM8A_MODE_TABLE
                        .get(usize::from(command.value))
                        .copied()
                        .filter(|&(_, data_kind)| {
                            matches!(self.pcm_mode, MdxPcmMode::Pcm8a)
                                || data_kind == Pcm8aFormat::Adpcm
                        });
                    if let Some((rate_step, data_kind)) = mode {
                        self.tracks[track].pcm.rate_step = rate_step;
                        self.tracks[track].pcm.data_kind = data_kind;
                        if matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm) && track == 8 {
                            self.set_legacy_pcm_rate(command.value, builder);
                        }
                    }
                }
                MdxCommand::OpmLfo(command) => self.apply_opm_lfo(track, command, builder),
                MdxCommand::PitchLfo(command) => self.tracks[track].lfo.configure_pitch(command),
                MdxCommand::VolumeLfo(command) => self.tracks[track].lfo.configure_volume(command),
                MdxCommand::LfoDelay(command) => self.tracks[track].lfo.delay = command.value,
                MdxCommand::PcmMode(_) => {}
                MdxCommand::Extended(command) => match command {
                    MdxExtendedCommand::Fadeout { value } => {
                        self.fadeout.start(value);
                    }
                    // None of these E7 sub-commands perform an FM action here.
                    MdxExtendedCommand::Pcm8DirectDrive { .. }
                    | MdxExtendedCommand::KeyOff { .. }
                    | MdxExtendedCommand::ChannelControl { .. }
                    | MdxExtendedCommand::AddNoteLength { .. }
                    | MdxExtendedCommand::SetFlag { .. }
                    | MdxExtendedCommand::Error
                    | MdxExtendedCommand::Unknown(_) => {}
                },
                MdxCommand::Extended2(command) => match command {
                    MdxExtended2Command::Transpose { value } => {
                        self.tracks[track].fm.transpose = i32::from(value)
                    }
                    MdxExtended2Command::RelativeTranspose { value } => {
                        let next = self.tracks[track].fm.transpose + i32::from(value);
                        self.tracks[track].fm.transpose = next.clamp(-127, 127);
                    }
                    // Relative detune is parsed but has no playback effect.
                    MdxExtended2Command::RelativeDetune { .. }
                    | MdxExtended2Command::Error
                    | MdxExtended2Command::Unknown(_) => {}
                },
                MdxCommand::LoopStart(command) => {
                    // Nested repeat blocks always use their encoded count:
                    // overriding it per track would desync tracks whose
                    // blocks repeat a different number of times. `loop_count`
                    // only applies to the whole-song repeat (see
                    // `take_repeating_jump`).
                    let command_index = self.tracks[track].command_index;
                    self.tracks[track]
                        .loop_stack
                        .push((u32::from(command.count), command_index));
                }
                MdxCommand::LoopEnd(_) => {
                    // Use the position saved at `LoopStart` instead of
                    // recomputing it from this command's offset. This also
                    // keeps malformed offsets from redirecting the repeat.
                    let this_command_index = self.tracks[track].command_index - 1;
                    // `remaining == 0` is the file's "loop forever" marker.
                    // It can only be reached when `loop_count` is `None`,
                    // since `LoopStart` otherwise overrides it with a finite
                    // count. Resolved via a native VGM loop point rather
                    // than looping forever internally.
                    let action = match self.tracks[track].loop_stack.last_mut() {
                        Some((remaining, start_index)) if *remaining == 0 => {
                            LoopEndAction::SongLoop(*start_index)
                        }
                        Some((remaining, start_index)) if *remaining > 1 => {
                            *remaining -= 1;
                            LoopEndAction::Repeat(*start_index)
                        }
                        Some(_) => {
                            self.tracks[track].loop_stack.pop();
                            LoopEndAction::Fallthrough
                        }
                        // No active loop on the stack: fall through without
                        // jumping.
                        None => LoopEndAction::Fallthrough,
                    };
                    match action {
                        LoopEndAction::Repeat(start_index) => {
                            self.tracks[track].command_index = start_index;
                        }
                        LoopEndAction::SongLoop(start_index) => {
                            self.take_repeating_jump_to(
                                track,
                                this_command_index,
                                start_index,
                                builder,
                            );
                        }
                        LoopEndAction::Fallthrough => {}
                    }
                }
                MdxCommand::LoopEscape(command) => {
                    let should_jump = self.tracks[track]
                        .loop_stack
                        .last()
                        .is_some_and(|(count, _)| *count == 1);
                    if should_jump {
                        self.tracks[track].loop_stack.pop();
                        // The offset targets the corresponding 0xF5 opcode;
                        // add 2 to land past its operand and break out of the
                        // loop.
                        self.jump_relative(track, command.offset.saturating_add(2))?;
                    }
                }
                MdxCommand::Jump(command) => {
                    self.take_repeating_jump(track, command.offset, builder)?
                }
                MdxCommand::EndOfTrackLoop(command) => {
                    self.song_loop.track_end_loop_seen = true;
                    if self.song_loop.synchronized_f1_tracks.contains(&track) {
                        self.park_at_synchronized_f1(track, command.offset)?;
                        if self.synchronized_f1_barrier_ready() {
                            break;
                        }
                    } else if self.song_loop.loop_count.is_some() {
                        self.take_finite_track_end_loop(track, command.offset)?;
                    } else if self.song_loop.mark_native_loop && !self.fadeout.has_command {
                        // F1 is a per-track terminator in MDX. A short PCM
                        // track can loop long before the rest of the song, so
                        // it must not become the global VGM loop point.
                        self.tracks[track].active = false;
                    } else if self.song_loop.mark_native_loop {
                        self.jump_relative(track, command.offset)?;
                    } else {
                        self.take_repeating_jump(track, command.offset, builder)?;
                    }
                }
                MdxCommand::Raw(_) => {}
            }
        }
        Ok(())
    }

    /// Applies the tone's operator registers. Register `0x20` is handled
    /// separately when pending FM state is applied.
    fn emit_voice(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        let voice = self.tracks[track].fm.voice;
        let tone = self
            .package
            .borrow()
            .mdx
            .tone_bank
            .tones
            .iter()
            .find(|tone| tone.voice_number == voice)
            .ok_or(MdxConvertError::MissingTone { voice })?;
        let fm_channel = self.tracks[track].fm.fm_channel;
        self.tracks[track].fm.con_fl = tone.con | (tone.fl << 3);
        // Loading a voice also marks pan as pending, so register 0x20's
        // CON/FL bits are refreshed for the new algorithm at the next key-on.
        self.tracks[track].fm.pan_pending = true;
        // Store the tone's own key-on slot mask combined with the channel;
        // a zero mask falls back to the algorithm default at key-on time.
        self.tracks[track].fm.key_on_slot = ((tone.op & 0x0f) << 3) | fm_channel;
        emit_tone(builder, fm_channel, tone);
        self.emit_volume(track, builder);
        Ok(())
    }

    /// Applies a pending voice select and/or pan change. Both are deferred
    /// from the command that set them until the next key-on.
    fn apply_pending_fm_state(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        if self.tracks[track].fm.voice_pending {
            self.emit_voice(track, builder)?;
            self.tracks[track].fm.voice_pending = false;
        }
        if self.tracks[track].fm.pan_pending {
            let fm_channel = self.tracks[track].fm.fm_channel;
            let value = self.tracks[track].fm.pan | (self.tracks[track].fm.con_fl & 0x3f);
            write_ym2151(builder, 0x20 + fm_channel, value);
            self.tracks[track].fm.pan_pending = false;
        }
        Ok(())
    }

    /// Applies an FM key-on. An already-sounding note (tie/legato) is not
    /// retriggered, so its envelope and LFO delay continue uninterrupted.
    fn begin_key_on(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        self.apply_pending_fm_state(track, builder)?;
        let already_on = self.tracks[track].fm.key_on;
        if !already_on && self.tracks[track].lfo.delay > 0 {
            self.tracks[track].lfo.start_delay();
        }
        self.tracks[track].fm.bend_offset = 0;
        if !already_on {
            self.reset_opm_lfo_if_needed(track, builder);
            let slot = self.tracks[track].fm.resolved_key_on_slot();
            write_ym2151(builder, 0x08, slot);
            self.tracks[track].fm.key_on = true;
        }
        self.tracks[track].fm.key_on_pending = false;
        Ok(())
    }

    /// Processes the key-on delay for the specified track. If the key-on delay
    /// counter reaches zero and a key-on is pending, it triggers the key-on
    /// event. Otherwise, it decrements the key-on delay counter. Returns an
    /// error if beginning the key-on fails.
    fn process_key_on_delay(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        if self.tracks[track].fm.key_on_delay_counter == 0 || !self.tracks[track].fm.key_on_pending
        {
            return Ok(());
        }
        self.tracks[track].fm.key_on_delay_counter -= 1;
        if self.tracks[track].fm.key_on_delay_counter == 0 {
            self.begin_key_on(track, builder)?;
        }
        Ok(())
    }

    /// Per-tick FM modulation: pitch bend accumulation, pitch/volume LFO
    /// updates (gated by LFO delay), and the resulting register writes.
    fn update_fm_tick(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        let prev_volume_lfo_offset = self.tracks[track].lfo.volume_offset;
        // Updates the FM state for the current tick, including pitch bend
        // accumulation, LFO updates (if the LFO delay has elapsed), and
        // register writes for pitch and volume changes.
        if self.tracks[track].fm.portamento_active
            && self.tracks[track].fm.key_on_delay_counter == 0
        {
            self.tracks[track].fm.bend_offset = self.tracks[track]
                .fm
                .bend_offset
                .wrapping_add(self.tracks[track].fm.bend_delta);
        }
        // Determines whether the LFO updates should be skipped for this tick based on
        // the LFO delay and key-on delay counters.
        let key_on_delay_counter = self.tracks[track].fm.key_on_delay_counter;
        let skip_lfo = self.tracks[track]
            .lfo
            .should_skip_tick(key_on_delay_counter);
        if !skip_lfo {
            self.tracks[track].lfo.update_pitch(&mut self.lfo_rand_seed);
            self.tracks[track]
                .lfo
                .update_volume(&mut self.lfo_rand_seed);
        }
        // Updates the pitch and volume for the current tick based on the LFO and pitch bend.
        if self.tracks[track].fm.note_pitch.is_some() {
            self.update_pitch(track, builder)?;
        }
        // Emits the volume register write if the volume LFO offset has changed.
        if self.tracks[track].lfo.volume_offset != prev_volume_lfo_offset {
            self.emit_volume(track, builder);
        }
        Ok(())
    }

    /// Writes the pitch registers only if the computed pitch changed.
    fn update_pitch(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        let Some(note_pitch) = self.tracks[track].fm.note_pitch else {
            return Ok(());
        };
        let bend = self.tracks[track].fm.bend_offset >> 16;
        let lfo = self.tracks[track].lfo.pitch_offset >> 16;
        let pitch = i32::from(note_pitch)
            .saturating_add(bend)
            .saturating_add(lfo)
            .clamp(0, 0x17ff) as u16;
        if self.tracks[track].fm.last_written_pitch == Some(pitch) {
            return Ok(());
        }
        self.write_pitch(track, builder, pitch)
    }

    /// Writes the computed pitch to the YM2151 registers for the specified track.
    /// Updates the last written pitch to avoid redundant writes.
    fn write_pitch(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
        pitch: u16,
    ) -> Result<(), MdxConvertError> {
        let key_code = YM2151_KEYCODE_TABLE
            .get(usize::from(pitch >> 6))
            .copied()
            .ok_or(MdxConvertError::PitchOutOfRange { track, pitch })?;
        let fm_channel = self.tracks[track].fm.fm_channel;
        let pitch_register = pitch << 2;
        let key_fraction = pitch_register as u8;
        write_ym2151(builder, 0x30 + fm_channel, key_fraction);
        write_ym2151(builder, 0x28 + fm_channel, key_code);
        self.tracks[track].fm.last_written_pitch = Some(pitch);
        Ok(())
    }

    /// Emits the current volume settings for the specified track to the YM2151 registers.
    /// Takes into account the base volume, LFO-induced attenuation, and the carrier mask
    /// for the tone's operators.
    fn emit_volume(&self, track: usize, builder: &mut VgmBuilder) {
        let Some(tone) = self
            .package
            .borrow()
            .mdx
            .tone_bank
            .tones
            .iter()
            .find(|tone| tone.voice_number == self.tracks[track].fm.voice)
        else {
            return;
        };
        let base_attenuation = if self.tracks[track].fm.volume & 0x80 != 0 {
            u16::from(self.tracks[track].fm.volume & 0x7f)
        } else {
            u16::from(FM_VOLUME_TABLE[self.tracks[track].fm.volume.min(15) as usize])
        };
        let lfo_attenuation = self.tracks[track].lfo.volume_offset >> 8;
        let attenuation = base_attenuation + lfo_attenuation + u16::from(self.fadeout.level);
        let carrier_mask = CARRIER_TL_SLOTS[(tone.con & 0x07) as usize];
        let fm_channel = self.tracks[track].fm.fm_channel;
        for (operator, value) in tone.operators.iter().enumerate() {
            let level = if carrier_mask & (1 << operator) != 0 {
                (u16::from(value.ol) + attenuation).min(0x7f) as u8
            } else {
                value.ol
            };
            write_ym2151(builder, 0x60 + operator as u8 * 8 + fm_channel, level);
        }
    }

    /// Applies the OPM LFO settings for the specified track, updating the YM2151
    /// registers as necessary. Handles enabling/disabling the LFO and configuring
    /// its control, frequency, and modulation parameters.
    fn apply_opm_lfo(&mut self, track: usize, command: MdxOpmLfo, builder: &mut VgmBuilder) {
        match command {
            MdxOpmLfo::SetEnabled { enabled } => {
                let fm_channel = self.tracks[track].fm.fm_channel;
                let value = if enabled {
                    self.tracks[track].lfo.pms_ams
                } else {
                    0
                };
                write_ym2151(builder, 0x38 + fm_channel, value);
            }
            MdxOpmLfo::Configure {
                control,
                lfrq,
                pmd,
                amd,
                pms_ams,
            } => {
                self.tracks[track].lfo.opm_reset_pending = control & 0x40 != 0;
                // Preserve the CT1/CT2 bits (0xc0) already held in register
                // 0x1b; only the waveform/enable bits are updated here.
                let masked = (control & !0x40) | (self.opm_reg_1b & 0xc0);
                self.opm_reg_1b = masked;
                self.tracks[track].lfo.pms_ams = pms_ams;
                let fm_channel = self.tracks[track].fm.fm_channel;
                write_ym2151(builder, 0x1b, masked);
                write_ym2151(builder, 0x18, lfrq);
                write_ym2151(builder, 0x19, pmd);
                write_ym2151(builder, 0x19, amd);
                write_ym2151(builder, 0x38 + fm_channel, pms_ams);
            }
        }
    }

    /// Applies the pitch LFO settings for the specified track, updating the internal
    /// state as necessary. Handles enabling/disabling the LFO and configuring its
    /// waveform, frequency, and amplitude.
    /// Starts the LFO delay for the specified track. Initializes the delay counter
    /// and resets the pitch and volume LFO offsets. If the delay counter reaches zero,
    /// the pitch and volume LFOs are reset immediately.
    /// Resets the OPM LFO for the specified track if a reset is pending.
    /// Writes the necessary commands to the YM2151 registers to perform the reset.
    fn reset_opm_lfo_if_needed(&self, track: usize, builder: &mut VgmBuilder) {
        if !self.tracks[track].lfo.opm_reset_pending {
            return;
        }
        write_ym2151(builder, 0x01, 0x02);
        write_ym2151(builder, 0x01, 0x00);
    }

    /// Updates the pitch LFO for the specified track based on its type, delta, and length counter.
    /// Handles sawtooth, square, and triangle waveforms, updating the internal offset and
    /// length counter accordingly.
    /// Updates the volume LFO for the specified track based on its type, delta, and length counter.
    /// Handles sawtooth, square, and triangle waveforms, updating the internal offset and
    /// length counter accordingly.
    /// Resolves a relative jump offset (as encoded by `LoopEnd`,
    /// `LoopEscape` or `Jump`) to an absolute command index, if the target
    /// lands exactly on a command boundary.
    fn resolve_jump_target(
        &mut self,
        track: usize,
        offset: i16,
    ) -> Result<Option<usize>, MdxConvertError> {
        let command_index = self.tracks[track].command_index;
        if self.jump_source_map.is_none() {
            self.jump_source_map = Some(
                self.package
                    .borrow()
                    .mdx
                    .sourcemap()
                    .map_err(|error| MdxConvertError::InvalidDocument(error.to_string()))?,
            );
        }
        let Some(track_ranges) = self.jump_source_map.as_ref().and_then(|map| map.get(track))
        else {
            return Ok(None);
        };
        let Some((current_offset, current_length)) =
            track_ranges.get(command_index.saturating_sub(1)).copied()
        else {
            return Ok(None);
        };
        let Some(command_end) = current_offset.checked_add(current_length) else {
            return Ok(None);
        };
        let target_offset = if offset >= 0 {
            command_end.checked_add(offset as usize)
        } else {
            command_end.checked_sub(offset.unsigned_abs() as usize)
        };
        Ok(target_offset.and_then(|target_offset| {
            track_ranges
                .iter()
                .position(|(offset, _)| *offset == target_offset)
        }))
    }

    /// Performs a relative jump for the specified track by the given offset.
    /// If the target offset corresponds to a valid command boundary, updates the
    /// track's command index accordingly.
    fn jump_relative(&mut self, track: usize, offset: i16) -> Result<(), MdxConvertError> {
        if let Some(target_index) = self.resolve_jump_target(track, offset)? {
            self.tracks[track].command_index = target_index;
        }
        Ok(())
    }

    /// Takes an unconditional repeat back to `target_index` (infinite
    /// `LoopEnd`, or a backward whole-song `Jump`). `loop_count`, when set,
    /// overrides this with a finite number of playthroughs instead, keyed
    /// by the repeating command's own index (`jump_command_index`).
    ///
    /// Otherwise, behavior depends on `mark_native_loop`:
    /// - `true` (used by [`to_vgm_document`]): rather than looping forever
    ///   internally, the first time a given `(track, target_index)` repeat
    ///   point is reached its VGM position is remembered; the second time,
    ///   conversion stops there and the position is used as the file's
    ///   native loop point, so the whole thing fits in a finite
    ///   `VgmDocument`.
    /// - `false` (used by the streaming [`MdxVgmGenerator`] path): simply
    ///   keeps repeating indefinitely, exactly like real hardware would.
    ///   Nothing needs to be remembered since the stream never rewinds to
    ///   a previously produced command.
    /// - Once an embedded fadeout has started, repeats remain internal in
    ///   either path until the global fadeout reaches its final level.
    fn take_repeating_jump_to(
        &mut self,
        track: usize,
        jump_command_index: usize,
        target_index: usize,
        builder: &mut VgmBuilder,
    ) {
        if let Some(limit) = self.song_loop.loop_count {
            let count = self
                .song_loop
                .jump_repeat_counts
                .entry((track, jump_command_index))
                .or_insert(0);
            *count += 1;
            if *count >= limit {
                return;
            }
            self.tracks[track].command_index = target_index;
            return;
        }
        if self.fadeout.seen || !self.song_loop.mark_native_loop {
            self.tracks[track].command_index = target_index;
            return;
        }
        let key = (track, target_index);
        if let Some(&loop_index) = self.song_loop.loop_starts.get(&key) {
            self.song_loop.loop_index.get_or_insert(loop_index);
            self.song_loop.loop_complete = true;
            return;
        }
        self.song_loop
            .loop_starts
            .insert(key, builder.command_count());
        self.tracks[track].command_index = target_index;
    }

    /// Resolves `offset` relative to the just-executed command (`Jump`) and
    /// hands it to `take_repeating_jump_to`.
    fn take_repeating_jump(
        &mut self,
        track: usize,
        offset: i16,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        let jump_command_index = self.tracks[track].command_index - 1;
        let Some(target_index) = self.resolve_jump_target(track, offset)? else {
            return Ok(());
        };
        self.take_repeating_jump_to(track, jump_command_index, target_index, builder);
        Ok(())
    }

    /// Keeps independent F1 tracks looping until every active song track has
    /// reached the requested loop count or ended.
    fn take_finite_track_end_loop(
        &mut self,
        track: usize,
        offset: i16,
    ) -> Result<(), MdxConvertError> {
        let Some(limit) = self.song_loop.loop_count else {
            return Ok(());
        };
        let command_index = self.tracks[track].command_index - 1;
        let Some(target_index) = self.resolve_jump_target(track, offset)? else {
            return Ok(());
        };
        if offset >= 0 {
            self.tracks[track].command_index = target_index;
            return Ok(());
        }
        let count = self.song_loop.track_loop_counts.entry(track).or_insert(0);
        *count = count.saturating_add(1);

        let all_tracks_reached_limit = self.tracks.iter().enumerate().all(|(track, state)| {
            !state.active
                || state.sync_wait
                || self
                    .song_loop
                    .track_loop_counts
                    .get(&track)
                    .is_some_and(|count| *count >= limit)
        });
        if all_tracks_reached_limit {
            self.song_loop.loop_complete = true;
        } else if target_index == command_index {
            self.tracks[track].active = false;
        } else {
            self.tracks[track].command_index = target_index;
        }
        Ok(())
    }

    /// Marks a loop track as arrived, then lets it continue from its target
    /// while the other participating tracks reach the shared boundary.
    fn park_at_synchronized_f1(
        &mut self,
        track: usize,
        offset: i16,
    ) -> Result<(), MdxConvertError> {
        let command_index = self.tracks[track].command_index - 1;
        let Some(target_index) = self.resolve_jump_target(track, offset)? else {
            self.song_loop.synchronized_f1_tracks.remove(&track);
            self.tracks[track].active = false;
            return Ok(());
        };
        self.song_loop
            .synchronized_f1_targets
            .entry(track)
            .or_insert(target_index);
        self.song_loop.synchronized_f1_arrivals.insert(track);
        self.tracks[track].command_index = target_index;
        self.tracks[track].active = target_index != command_index;
        Ok(())
    }

    /// Returns whether every synchronized F1 track has arrived and all other
    /// tracks have finished.
    fn synchronized_f1_barrier_ready(&self) -> bool {
        let loop_tracks = &self.song_loop.synchronized_f1_tracks;
        !loop_tracks.is_empty()
            && loop_tracks
                .iter()
                .all(|track| self.song_loop.synchronized_f1_arrivals.contains(track))
            && !self
                .tracks
                .iter()
                .enumerate()
                .any(|(track, state)| !loop_tracks.contains(&track) && state.active)
    }

    /// Starts a loop pass or finishes playback after the requested number of
    /// passes once all loop tracks and one-shot tracks reach the boundary.
    fn complete_synchronized_f1_barrier(&mut self, builder: &mut VgmBuilder) -> bool {
        if !self.synchronized_f1_barrier_ready() {
            return false;
        }

        if let Some(limit) = self.song_loop.loop_count {
            self.song_loop.f1_loop_passes_completed =
                self.song_loop.f1_loop_passes_completed.saturating_add(1);
            if self.song_loop.f1_loop_passes_completed >= limit {
                self.song_loop.loop_complete = true;
                return false;
            }
        } else if self.song_loop.loop_index.is_some() {
            self.song_loop.loop_complete = true;
            return false;
        } else {
            self.song_loop.loop_index = Some(builder.command_count());
        }

        for &track in self.song_loop.synchronized_f1_targets.keys() {
            self.tracks[track].active = true;
        }
        self.song_loop.synchronized_f1_arrivals.clear();
        true
    }

    /// Emits the necessary wait commands to the VGM builder to account for the passage
    /// of one MDX tick, taking into consideration both the sample rate and any pending
    /// PCM data writes. Ensures that PCM bytes are spread evenly across the tick's
    /// samples to maintain accurate playback timing.
    fn emit_wait(&mut self, builder: &mut VgmBuilder) {
        let (tick_microseconds, samples) = self.timing.advance_tick();

        if !self.pcm_output.has_pcm {
            Self::emit_wait_chunks(builder, samples);
            return;
        }

        let pcm_bytes_due = self.pcm_output.mck_scheduler.advance(tick_microseconds);
        if pcm_bytes_due == 0 {
            Self::emit_wait_chunks(builder, samples);
            return;
        }

        // Spread the due OKIM6258 data writes evenly across this tick's
        // samples (splitting `samples` into `pcm_bytes_due` near-equal
        // segments) so each byte lands close to its real playback position
        // without resorting to a wait-1-sample-per-byte command stream.
        let mut emitted_samples = 0;
        let first_byte_at_zero = self.pcm_output.mck_scheduler.take_initial_event();
        let start_index = if first_byte_at_zero {
            self.emit_pcm_byte(builder);
            1
        } else {
            0
        };
        for byte_index in start_index..pcm_bytes_due {
            // Place each byte at its fractional position in the tick. Using
            // the cumulative target avoids dropping the remainder on every
            // tick and produces the 5/6-sample cadence of a 7812 Hz stream.
            let target_index = if first_byte_at_zero {
                byte_index
            } else {
                byte_index + 1
            };
            let target_samples = target_index * samples / pcm_bytes_due;
            Self::emit_wait_chunks(builder, target_samples - emitted_samples);
            emitted_samples = target_samples;
            self.emit_pcm_byte(builder);
        }
        Self::emit_wait_chunks(builder, samples - emitted_samples);
    }

    /// Emits wait commands in chunks of up to 65535 samples to the VGM builder.
    /// This is used internally by `emit_wait` to handle large numbers of samples
    /// without exceeding the maximum wait command size.
    fn emit_wait_chunks(builder: &mut VgmBuilder, samples: u32) {
        let mut remaining = samples;
        while remaining > 0 {
            let chunk = remaining.min(u32::from(u16::MAX)) as u16;
            builder.add_vgm_command(WaitSamples(chunk));
            remaining -= u32::from(chunk);
        }
    }

    /// Advances the global fadeout counter and reapplies FM and mixed PCM
    /// attenuation when the level changes.
    fn advance_fadeout(&mut self, builder: &mut VgmBuilder) {
        if !self.fadeout.advance() {
            return;
        }
        for track in 0..8 {
            if self.tracks[track].fm.voice_selected {
                self.emit_volume(track, builder);
            }
        }
        if self.pcm_uses_mixer() {
            for track in 8..self.tracks.len().min(16) {
                let gain = self.pcm_channel_gain(track);
                self.pcm_output.set_channel_gain(track - 8, gain);
            }
        }
    }

    /// Applies the legacy PCM1 clock/divider selection for `0xed` F0-F4.
    /// OKIM6258 clock bytes are written to registers `0x08`-`0x0b`;
    /// libvgm commits the new clock when register `0x0b` is written. The
    /// following `0x0c` write selects the divider.
    fn set_legacy_pcm_rate(&mut self, mode: u8, builder: &mut VgmBuilder) {
        let Some((byte_rate_units, clock_bytes, divider_value)) = (match mode {
            0 => Some((
                4_000_000 * MCK_RATE_DENOMINATOR / 2_048,
                [0x00, 0x09, 0x3d, 0x00],
                0,
            )), // 4 MHz / 1024
            1 => Some((
                4_000_000 * MCK_RATE_DENOMINATOR / 1_536,
                [0x00, 0x09, 0x3d, 0x00],
                1,
            )), // 4 MHz / 768
            2 => Some((
                8_000_000 * MCK_RATE_DENOMINATOR / 2_048,
                [0x00, 0x12, 0x7a, 0x00],
                0,
            )), // 8 MHz / 1024
            3 => Some((
                8_000_000 * MCK_RATE_DENOMINATOR / 1_536,
                [0x00, 0x12, 0x7a, 0x00],
                1,
            )), // 8 MHz / 768
            4 => Some((
                8_000_000 * MCK_RATE_DENOMINATOR / 1_024,
                [0x00, 0x12, 0x7a, 0x00],
                2,
            )), // 8 MHz / 512
            _ => None,
        }) else {
            return;
        };
        self.pcm_output.mck_scheduler.set_byte_rate(byte_rate_units);
        for (register, value) in (0x08..=0x0b).zip(clock_bytes) {
            builder.add_vgm_command((Instance::Primary, Okim6258Spec { register, value }));
        }
        builder.add_vgm_command((
            Instance::Primary,
            Okim6258Spec {
                register: 0x0c,
                value: divider_value,
            },
        ));
    }

    /// Produces one raw or mixed ADPCM byte and writes it to OKIM6258 register 1.
    fn emit_pcm_byte(&mut self, builder: &mut VgmBuilder) {
        let mix_pcm = !(matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(self.adpcm_mode, AdpcmMode::Through));
        let byte = self
            .pcm_output
            .next_adpcm_byte(mix_pcm, self.package.borrow());
        builder.add_vgm_command((
            Instance::Primary,
            Okim6258Spec {
                register: 1,
                value: byte,
            },
        ));
    }

    /// Appends the final PCM stop write, if applicable, without
    /// finalizing `builder` into a `VgmDocument`. Stopping ADPCM playback
    /// (by writing 0x01 to the control register) is skipped when a native
    /// VGM loop point was set: the player never reaches this stop write on
    /// subsequent loops (it jumps back to `song_loop_index` first), but it
    /// would still execute once on the very first pass, permanently
    /// leaving the chip stopped since no later `Okim6258Write` re-triggers
    /// register 0.
    ///
    /// Shared by [`finalize_with_pcm`](Self::finalize_with_pcm) and
    /// [`MdxVgmGenerator`], which has no use for a complete `VgmDocument`
    /// and therefore never calls `VgmBuilder::finalize()`.
    fn emit_closing_commands(&self, builder: &mut VgmBuilder) {
        if self.pcm_output.has_pcm && self.song_loop.loop_index.is_none() {
            builder.add_vgm_command((
                Instance::Primary,
                Okim6258Spec {
                    register: 0,
                    value: 0x01,
                },
            ));
        }
    }

    /// Finalizes the document, stopping ADPCM playback first (if it was
    /// ever started); see [`emit_closing_commands`](Self::emit_closing_commands).
    fn finalize_with_pcm(&mut self, mut builder: VgmBuilder) -> VgmDocument {
        self.emit_closing_commands(&mut builder);
        self.finalize(builder)
    }

    /// Wraps a conversion error with the current MDX position, if available.
    fn conversion_failure(&self, error: MdxConvertError) -> ConversionFailure {
        ConversionFailure {
            error,
            position: self.check_state.as_ref().and_then(|state| state.position),
        }
    }

    /// Adds available MDX coordinates, reporting a playback-check limit hit when recorded.
    fn diagnostic_error(&self, error: MdxConvertError) -> MdxPlaybackCheckError {
        let position = self.check_state.as_ref().and_then(|state| state.position);
        let track = position.map(|(track, _)| track);
        let command_index = position.map(|(_, command)| command);
        match self.check_state.as_ref().and_then(|state| state.limit_hit) {
            Some(resource) => MdxPlaybackCheckError::LimitExceeded {
                resource,
                track,
                command_index,
            },
            None => MdxPlaybackCheckError::Conversion {
                error,
                track,
                command_index,
            },
        }
    }
}

/// Execution budgets for a bounded lazy playback check.
#[derive(Debug, Clone, Copy)]
pub struct MdxPlaybackCheckLimits {
    /// Maximum playback ticks, including ticks spent waiting for synchronization.
    pub max_ticks: u64,
    /// Maximum MDX commands executed across all tracks and repeats.
    pub max_commands: u64,
}

impl Default for MdxPlaybackCheckLimits {
    fn default() -> Self {
        Self {
            max_ticks: 100_000,
            max_commands: 1_000_000,
        }
    }
}

/// A playback failure or an incomplete check, with zero-based MDX coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdxPlaybackCheckError {
    /// Playback returned an error.
    Conversion {
        /// Original conversion error.
        error: MdxConvertError,
        /// Track being processed, if available.
        track: Option<usize>,
        /// Command being processed, if available.
        command_index: Option<usize>,
    },
    /// Execution stopped before completion because a budget was exhausted.
    LimitExceeded {
        /// Exhausted budget (ticks or MDX commands).
        resource: &'static str,
        /// Track being processed, if available.
        track: Option<usize>,
        /// Command being processed, if available.
        command_index: Option<usize>,
    },
}

impl fmt::Display for MdxPlaybackCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (track, command_index) = match self {
            Self::Conversion {
                error,
                track,
                command_index,
            } => {
                write!(f, "{error}")?;
                (track, command_index)
            }
            Self::LimitExceeded {
                resource,
                track,
                command_index,
            } => {
                write!(f, "playback check incomplete: {resource} limit exceeded")?;
                (track, command_index)
            }
        };
        if let Some(track) = track {
            write!(f, " (track {track}")?;
            if let Some(command_index) = command_index {
                write!(f, ", MDX command {command_index}")?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl Error for MdxPlaybackCheckError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conversion { error, .. } => Some(error),
            Self::LimitExceeded { .. } => None,
        }
    }
}

impl From<MdxConvertError> for MdxPlaybackCheckError {
    fn from(error: MdxConvertError) -> Self {
        Self::Conversion {
            error,
            track: None,
            command_index: None,
        }
    }
}

/// Checks finite playback using the lazy generator without retaining VGM output.
///
/// PDX lookup and loading remain the caller's responsibility. This checks only
/// executed paths and errors reported by playback, not missing or malformed PCM
/// payloads that playback treats as silence. Coordinates refer to MDX commands,
/// not source-text lines. Limits also apply within a tick, preventing command-only
/// repeats from blocking the caller indefinitely.
/// This check uses `limits` rather than [`MdxToVgmOptions::max_ticks`] or
/// [`MdxToVgmOptions::max_commands_per_tick`].
///
/// # Errors
///
/// Returns a conversion error, invalid finite-loop/budget options, or an explicit
/// incomplete-check error when an execution limit is reached.
pub fn check_playback(
    package: &MdxPackage,
    options: MdxToVgmOptions,
    limits: MdxPlaybackCheckLimits,
) -> Result<(), MdxPlaybackCheckError> {
    let invalid = |error| MdxPlaybackCheckError::Conversion {
        error,
        track: None,
        command_index: None,
    };
    if options.loop_count.is_none() || limits.max_ticks == 0 || limits.max_commands == 0 {
        return Err(invalid(MdxConvertError::InvalidOptions(
            "playback checking requires finite loops and nonzero execution limits",
        )));
    }
    let mut generator = MdxVgmGenerator::new(
        package,
        MdxToVgmOptions {
            max_ticks: None,
            max_commands_per_tick: None,
            ..options
        },
        false,
    );
    generator.playback.check_state = Some(PlaybackCheckState {
        limits: Some(limits),
        ticks: 0,
        commands: 0,
        position: None,
        limit_hit: None,
    });
    loop {
        let result = generator.run_step();
        generator.builder.drain_commands().for_each(drop);
        match result {
            Ok(false) => return Ok(()),
            Ok(true) => {}
            Err(error) => {
                return Err(generator.playback.diagnostic_error(error));
            }
        }
    }
}

#[derive(Default)]
struct PlaybackCheckState {
    limits: Option<MdxPlaybackCheckLimits>,
    ticks: u64,
    commands: u64,
    position: Option<(usize, usize)>,
    limit_hit: Option<&'static str>,
}

/// Lazily drives an [`MdxPackage`] into VGM commands, one MDX tick at a
/// time, so conversion work happens only as a [`VgmStream`](crate::vgm::VgmStream)
/// or [`VgmCallbackStream`](crate::vgm::VgmCallbackStream) built from it is
/// actually iterated, rather than all at once up front. Construct one via
/// [`to_vgm_stream_generator`].
///
/// Does not retain previously produced commands: `builder` is reused as
/// per-tick scratch space and drained via [`VgmBuilder::drain_commands`]
/// into `pending` after every step, so memory use stays bounded to at most
/// one tick's worth of commands rather than growing with the length of the
/// song (see [`SongLoopState::mark_native_loop`], which is `false` for this
/// path).
struct MdxVgmGenerator<P: Borrow<MdxPackage>> {
    playback: PlaybackState<P>,
    options: MdxToVgmOptions,
    /// Scratch space that one tick's worth of commands is appended to,
    /// then immediately drained into `pending`; never allowed to grow
    /// across ticks.
    builder: VgmBuilder,
    /// Commands produced by the most recent tick(s) that have not yet been
    /// handed out via `next_command`.
    pending: VecDeque<VgmCommand>,
    initialized: bool,
    finished: bool,
}

impl<P: Borrow<MdxPackage>> MdxVgmGenerator<P> {
    /// `mark_native_loop` is `true` for the eager [`to_vgm_document`] path
    /// (which needs a fixed native VGM loop point) and `false` for the
    /// streaming [`to_vgm_stream_generator`] path (which just keeps
    /// repeating indefinitely); see [`SongLoopState::mark_native_loop`].
    fn new(package: P, mut options: MdxToVgmOptions, mark_native_loop: bool) -> Self {
        options.normalize();
        let pcm_mode = MdxPcmMode::from_track_count(package.borrow().mdx.header.track_count());
        let mut playback = PlaybackState::new(
            package,
            pcm_mode,
            options.adpcm_mode,
            options.loop_count,
            mark_native_loop,
        );
        playback.max_commands_per_tick = options.max_commands_per_tick;
        playback.pcm_output.max_decoded_pcm_samples = options.max_decoded_pcm_samples;
        Self {
            playback,
            options,
            builder: VgmBuilder::new(),
            pending: VecDeque::new(),
            initialized: false,
            finished: false,
        }
    }

    /// Accumulates eager output with any earlier pass charged to the same limit.
    fn run_eager_steps(&mut self, retained_commands: usize) -> Result<(), MdxConvertError> {
        loop {
            let playing = self.run_step()?;
            check_output_command_limit(
                self.options.max_output_commands,
                self.builder.command_count(),
                retained_commands,
            )?;
            if !playing {
                return Ok(());
            }
        }
    }

    /// Runs one more step (the one-time initialization, or one MDX tick),
    /// appending its commands to `self.builder`. Returns `false` once the
    /// song (and its closing commands) have been fully emitted.
    fn run_step(&mut self) -> Result<bool, MdxConvertError> {
        if self.finished {
            return Ok(false);
        }
        if !self.initialized {
            self.initialized = true;
            if self.playback.pcm_output.has_pcm {
                self.builder.register_chip(
                    Chip::Okim6258,
                    Instance::Primary,
                    self.options.okim6258_clock,
                );
            }
            self.playback.emit_initialization(&mut self.builder);
            if self.playback.pcm_output.has_pcm {
                self.builder.add_vgm_command((
                    Instance::Primary,
                    Okim6258Spec {
                        register: 0,
                        value: 0x02,
                    },
                ));
            }
        }

        match self
            .playback
            .step(&mut self.builder, self.options.max_ticks)?
        {
            StepOutcome::Finished => {
                self.playback.emit_closing_commands(&mut self.builder);
                // `VgmBuilder::finalize()` is never called in the lazy path (it
                // has no use for a complete `VgmDocument`), but `VgmStream`
                // still relies on an explicit `EndOfData` command to signal
                // end-of-stream/looping, so append one here to match what
                // `finalize()` would have guaranteed.
                self.builder.add_vgm_command(EndOfData {});
                self.finished = true;
                Ok(false)
            }
            StepOutcome::Continue => Ok(true),
        }
    }
}

impl<P: Borrow<MdxPackage>> fmt::Debug for MdxVgmGenerator<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MdxVgmGenerator").finish_non_exhaustive()
    }
}

/// Implements the `VgmCommandGenerator` trait for `MdxVgmGenerator`, allowing it to
/// produce a stream of VGM commands based on the MDX playback state.
impl<P: Borrow<MdxPackage>> VgmCommandGenerator for MdxVgmGenerator<P> {
    fn next_command(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        loop {
            if let Some(command) = self.pending.pop_front() {
                return Ok(Some(command));
            }
            if self.finished {
                return Ok(None);
            }
            self.run_step()?;
            self.pending.extend(self.builder.drain_commands());
        }
    }
}

/// Emits the necessary YM2151 register writes to configure a tone on the specified channel.
fn emit_tone(builder: &mut VgmBuilder, channel: u8, tone: &MdxTone) {
    let registers = [0x40, 0x60, 0x80, 0xa0, 0xc0, 0xe0];
    let slot_offsets = [0x00, 0x08, 0x10, 0x18];
    let operators = &tone.operators;
    for (register, values) in [
        (
            registers[0],
            operators.map(|operator| operator.dt1 << 4 | operator.ml),
        ),
        (registers[1], operators.map(|operator| operator.ol)),
        (
            registers[2],
            operators.map(|operator| operator.ks << 6 | operator.ar),
        ),
        (
            registers[3],
            operators.map(|operator| operator.ame << 7 | operator.dr),
        ),
        (
            registers[4],
            operators.map(|operator| operator.dt2 << 6 | operator.sr),
        ),
        (
            registers[5],
            operators.map(|operator| operator.sl << 4 | operator.rr),
        ),
    ] {
        for (operator, value) in values.into_iter().enumerate() {
            write_ym2151(builder, register + slot_offsets[operator] + channel, value);
        }
    }
}

/// Writes a value to a YM2151 register via the VGM builder.
fn write_ym2151(builder: &mut VgmBuilder, register: u8, value: u8) {
    builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
}

/// Builds a lazy generator; playback errors are returned by `next_command`.
impl From<(MdxPackage, MdxToVgmOptions)> for Box<dyn VgmCommandGenerator> {
    fn from((package, options): (MdxPackage, MdxToVgmOptions)) -> Self {
        Box::new(MdxVgmGenerator::new(package, options, false))
    }
}

/// Builds a lazy stream; playback errors are returned during iteration.
impl From<(MdxPackage, MdxToVgmOptions)> for VgmStream {
    fn from(input: (MdxPackage, MdxToVgmOptions)) -> Self {
        Self::from_generator(input.into())
    }
}

/// Converts the whole song, returning any playback error.
impl TryFrom<(&MdxPackage, &MdxToVgmOptions)> for VgmDocument {
    type Error = MdxConvertError;

    fn try_from((package, options): (&MdxPackage, &MdxToVgmOptions)) -> Result<Self, Self::Error> {
        to_vgm_document(package, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdx::command::MdxRest;
    use crate::mdx::document::MdxBuilder;

    #[test]
    fn decoded_pcm_cache_shares_aliased_pdx_ranges() {
        let payload = [0x12, 0x34, 0x56, 0x78];
        let mut builder = crate::mdx::pdx::PdxBuilder::new();
        builder.set_sample(0, 0, payload.to_vec()).unwrap();
        builder.set_sample(1, 0, vec![0x80]).unwrap();
        let mut bytes = builder.finalize().to_bytes();
        let entry: [u8; 8] = bytes[..8].try_into().unwrap();
        for bank in 0..2 {
            for note in 0..96 {
                let offset = (bank * 96 + note) * 8;
                bytes[offset..offset + 8].copy_from_slice(&entry);
            }
        }
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(crate::mdx::pdx::PdxDocument::parse(&bytes).unwrap()),
        };
        assert_eq!(package.pdx.as_ref().unwrap().banks.len(), 2);
        let expected = decode_pcm8a_with_pcm16_15khz(Pcm8aFormat::Adpcm, &payload, false).unwrap();
        let mut output = PcmOutputState::new(true, AdpcmMode::Resample);
        for bank in 0..2 {
            for note in 0..96 {
                assert_eq!(
                    output
                        .decode_sample(&package, bank, note, Pcm8aFormat::Adpcm, false)
                        .unwrap(),
                    Some((0, expected.len()))
                );
            }
        }
        assert_eq!(output.samples, expected);
        assert_eq!(output.sample_ranges.len(), 1);
    }

    #[test]
    fn decoded_pcm_cache_preserves_format_and_scaling() {
        let payload = [0x12, 0x34, 0x56, 0x78];
        let mut builder = crate::mdx::pdx::PdxBuilder::new();
        builder.set_sample(0, 0, payload.to_vec()).unwrap();
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(builder.finalize()),
        };
        let mut output = PcmOutputState::new(true, AdpcmMode::Through);
        for (format, pcm16_is_15khz) in [
            (Pcm8aFormat::Adpcm, false),
            (Pcm8aFormat::Pcm8, false),
            (Pcm8aFormat::Pcm16, false),
            (Pcm8aFormat::Pcm16, true),
        ] {
            let expected = decode_pcm8a_with_pcm16_15khz(format, &payload, pcm16_is_15khz).unwrap();
            let start = output.samples.len();
            let range = output
                .decode_sample(&package, 0, 0, format, pcm16_is_15khz)
                .unwrap();
            assert_eq!(range, Some((start, expected.len())));
            assert_eq!(&output.samples[start..], expected);
        }
        let count = output.samples.len();
        for format in [Pcm8aFormat::Adpcm, Pcm8aFormat::Pcm8] {
            let range = output.decode_sample(&package, 0, 0, format, false).unwrap();
            assert_eq!(
                output.decode_sample(&package, 0, 0, format, true).unwrap(),
                range
            );
        }
        assert_eq!(output.samples.len(), count);
        assert_eq!(output.sample_ranges.len(), 4);
    }

    #[test]
    fn decoded_pcm_cache_distinguishes_source_start_and_length() {
        let payload = [0x12, 0x34, 0x56, 0x78];
        let mut builder = crate::mdx::pdx::PdxBuilder::new();
        builder.set_sample(0, 0, payload.to_vec()).unwrap();
        builder.set_sample(0, 1, payload.to_vec()).unwrap();
        let mut pdx = builder.finalize();
        let source = pdx.entry(0, 0).unwrap();
        pdx.banks[0].entries[2] = Some(crate::mdx::pdx::PdxSample { size: 2, ..source });
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(pdx),
        };
        let mut output = PcmOutputState::new(true, AdpcmMode::Lpf);
        for (note, bytes) in [(0, &payload[..]), (1, &payload[..]), (2, &payload[..2])] {
            let expected = decode_pcm8a_with_pcm16_15khz(Pcm8aFormat::Adpcm, bytes, false).unwrap();
            let start = output.samples.len();
            assert_eq!(
                output
                    .decode_sample(&package, 0, note, Pcm8aFormat::Adpcm, false)
                    .unwrap(),
                Some((start, expected.len()))
            );
            assert_eq!(&output.samples[start..], expected);
        }
        assert_eq!(output.sample_ranges.len(), 3);
    }

    #[test]
    fn decoded_pcm_cache_does_not_cache_invalid_samples() {
        let mut builder = crate::mdx::pdx::PdxBuilder::new();
        builder
            .set_sample(0, 0, vec![0x12, 0x34, 0x56, 0x78])
            .unwrap();
        let mut pdx = builder.finalize();
        let source = pdx.entry(0, 0).unwrap();
        pdx.banks[0].entries[1] = Some(crate::mdx::pdx::PdxSample {
            start: u32::MAX,
            ..source
        });
        pdx.banks[0].entries[2] = Some(crate::mdx::pdx::PdxSample { size: 3, ..source });
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(pdx),
        };
        let mut output = PcmOutputState::new(true, AdpcmMode::Resample);
        output.max_decoded_pcm_samples = Some(0);
        for (bank, note) in [(0, 1), (0, 2), (0, 3), (0, 96), (1, 0)] {
            assert_eq!(
                output
                    .decode_sample(&package, bank, note, Pcm8aFormat::Pcm16, false)
                    .unwrap(),
                None
            );
        }
        assert!(output.samples.is_empty());
        assert!(output.sample_ranges.is_empty());
        output.max_decoded_pcm_samples = Some(2);
        assert_eq!(
            output
                .decode_sample(&package, 0, 0, Pcm8aFormat::Pcm16, false)
                .unwrap(),
            Some((0, 2))
        );
        assert_eq!(output.sample_ranges.len(), 1);
    }

    #[test]
    fn decoded_pcm_cache_limit_boundaries_and_disable() {
        assert_eq!(
            MdxToVgmOptions::default().max_decoded_pcm_samples,
            Some(32_000_000)
        );
        let mut builder = crate::mdx::pdx::PdxBuilder::new();
        builder
            .set_sample(0, 0, vec![0x12, 0x34, 0x56, 0x78])
            .unwrap();
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(builder.finalize()),
        };
        for (format, scaling, length) in [
            (Pcm8aFormat::Adpcm, false, 8),
            (Pcm8aFormat::Pcm8, false, 4),
            (Pcm8aFormat::Pcm16, false, 2),
            (Pcm8aFormat::Pcm16, true, 2),
        ] {
            for limit in [0, length - 1] {
                let mut output = PcmOutputState::new(true, AdpcmMode::Resample);
                output.max_decoded_pcm_samples = Some(limit);
                assert_eq!(
                    output.decode_sample(&package, 0, 0, format, scaling),
                    Err(MdxConvertError::DecodedPcmSampleLimitExceeded {
                        max_decoded_pcm_samples: limit,
                    })
                );
                assert!(output.samples.is_empty());
                assert!(output.sample_ranges.is_empty());
            }
            for limit in [Some(length), Some(length + 1), None] {
                let mut output = PcmOutputState::new(true, AdpcmMode::Resample);
                output.max_decoded_pcm_samples = limit;
                assert_eq!(
                    output
                        .decode_sample(&package, 0, 0, format, scaling)
                        .unwrap(),
                    Some((0, length as usize))
                );
                assert_eq!(output.samples.len(), length as usize);
            }
        }
    }

    #[test]
    fn decoded_pcm_cache_limit_counts_overlaps_but_not_shared_ranges() {
        let mut builder = crate::mdx::pdx::PdxBuilder::new();
        builder
            .set_sample(0, 0, vec![0x12, 0x34, 0x56, 0x78])
            .unwrap();
        let mut pdx = builder.finalize();
        let source = pdx.entry(0, 0).unwrap();
        pdx.banks[0].entries[1] = Some(crate::mdx::pdx::PdxSample {
            start: source.start + 1,
            size: 3,
        });
        pdx.banks[0].entries[2] = Some(source);
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(pdx),
        };
        let mut output = PcmOutputState::new(true, AdpcmMode::Resample);
        output.max_decoded_pcm_samples = Some(8);
        assert_eq!(
            output
                .decode_sample(&package, 0, 0, Pcm8aFormat::Adpcm, false)
                .unwrap(),
            Some((0, 8))
        );
        assert_eq!(
            output
                .decode_sample(&package, 0, 2, Pcm8aFormat::Adpcm, false)
                .unwrap(),
            Some((0, 8))
        );
        assert!(
            output
                .decode_sample(&package, 0, 0, Pcm8aFormat::Pcm8, false)
                .is_err()
        );
        output.max_decoded_pcm_samples = Some(13);
        let samples = output.samples.clone();
        assert_eq!(
            output.decode_sample(&package, 0, 1, Pcm8aFormat::Adpcm, false),
            Err(MdxConvertError::DecodedPcmSampleLimitExceeded {
                max_decoded_pcm_samples: 13
            })
        );
        assert_eq!(output.samples, samples);
        assert_eq!(output.sample_ranges.len(), 1);
        output.max_decoded_pcm_samples = Some(14);
        assert_eq!(
            output
                .decode_sample(&package, 0, 1, Pcm8aFormat::Adpcm, false)
                .unwrap(),
            Some((8, 6))
        );
        assert_eq!(output.samples.len(), 14);
        assert_eq!(output.sample_ranges.len(), 2);
    }

    #[test]
    fn conversion_output_limit_stops_pcm_before_tick_limit() {
        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(0, crate::mdx::command::MdxTempo { value: 1 })
            .add_mdx_command(0, MdxRest { ticks: 100 });
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: Some(crate::mdx::pdx::PdxBuilder::new().finalize()),
        };
        let options = MdxToVgmOptions {
            max_output_commands: Some(100),
            max_ticks: Some(100),
            ..Default::default()
        };
        let expected = MdxConvertError::OutputCommandLimitExceeded {
            max_output_commands: 100,
        };
        let mut generator = MdxVgmGenerator::new(&package, options, true);
        assert_eq!(generator.run_eager_steps(0).unwrap_err(), expected);
        assert_eq!(generator.playback.timing.elapsed_ticks, 1);
        assert!(generator.builder.command_count() > 100);
        assert!(generator.builder.command_count() < 2_000);
        assert_eq!(
            to_vgm_document_with_diagnostics(&package, &options).unwrap_err(),
            MdxPlaybackCheckError::Conversion {
                error: expected,
                track: Some(0),
                command_index: Some(1),
            }
        );
    }

    #[test]
    fn conversion_output_limit_counts_finalization() {
        let package = MdxPackage {
            mdx: MdxBuilder::new().finalize().unwrap(),
            pdx: Some(crate::mdx::pdx::PdxBuilder::new().finalize()),
        };
        let options = MdxToVgmOptions {
            max_output_commands: None,
            ..Default::default()
        };
        let mut generator = MdxVgmGenerator::new(&package, options, true);
        generator.run_eager_steps(0).unwrap();
        let step_count = u32::try_from(generator.builder.command_count()).unwrap();
        let document = to_vgm_document(&package, &options).unwrap();
        assert_eq!(document.commands.len(), step_count as usize + 1);
        assert_eq!(
            to_vgm_document(
                &package,
                &MdxToVgmOptions {
                    max_output_commands: Some(step_count),
                    ..options
                }
            )
            .unwrap_err(),
            MdxConvertError::OutputCommandLimitExceeded {
                max_output_commands: step_count,
            }
        );
        assert!(
            to_vgm_document(
                &package,
                &MdxToVgmOptions {
                    max_output_commands: Some(step_count + 1),
                    ..options
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn conversion_output_limit_counts_eager_f1_fallback() {
        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(0, MdxRest { ticks: 2 })
            .add_mdx_command(
                0,
                MdxCommand::EndOfTrackLoop(crate::mdx::command::MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: 0,
                }),
            );
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        let options = MdxToVgmOptions {
            max_output_commands: None,
            ..Default::default()
        };
        let mut generator = MdxVgmGenerator::new(&package, options, true);
        generator.run_eager_steps(0).unwrap();
        assert!(generator.playback.song_loop.track_end_loop_seen);
        assert!(generator.playback.song_loop.loop_index.is_none());
        let first_pass_count = u32::try_from(generator.builder.command_count()).unwrap();
        let document = to_vgm_document(&package, &options).unwrap();
        let count = u32::try_from(document.commands.len()).unwrap();
        assert!(count > first_pass_count);
        assert!(document.header.loop_offset > 0);
        for max_output_commands in [first_pass_count, count - 1] {
            assert_eq!(
                to_vgm_document(
                    &package,
                    &MdxToVgmOptions {
                        max_output_commands: Some(max_output_commands),
                        ..options
                    }
                )
                .unwrap_err(),
                MdxConvertError::OutputCommandLimitExceeded {
                    max_output_commands,
                }
            );
        }
        assert!(
            to_vgm_document(
                &package,
                &MdxToVgmOptions {
                    max_output_commands: Some(count),
                    ..options
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn conversion_output_limit_handles_large_retained_counts() {
        let expected = MdxConvertError::OutputCommandLimitExceeded {
            max_output_commands: u32::MAX,
        };
        assert_eq!(
            check_output_command_limit(Some(u32::MAX), 1, u32::MAX as usize).unwrap_err(),
            expected
        );
        assert_eq!(
            check_output_command_limit(Some(u32::MAX), 1, usize::MAX).unwrap_err(),
            expected
        );
        assert!(check_output_command_limit(None, usize::MAX, usize::MAX).is_ok());
    }

    #[test]
    fn conversion_command_limit_survives_f1_boundaries_and_generator_calls() {
        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(
                0,
                crate::mdx::command::MdxOpmRegisterWrite {
                    register: 0x1a,
                    value: 1,
                },
            )
            .add_mdx_command(
                0,
                MdxCommand::EndOfTrackLoop(crate::mdx::command::MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: -6,
                }),
            );
        let bytes = builder.finalize().unwrap().to_bytes().unwrap();
        let package = MdxPackage::parse(&bytes, None).unwrap();
        let options = MdxToVgmOptions {
            loop_count: Some(4),
            max_ticks: Some(1),
            max_commands_per_tick: Some(5),
            ..Default::default()
        };
        let expected = MdxConvertError::CommandLimitExceeded {
            max_commands_per_tick: 5,
        };
        assert_eq!(to_vgm_document(&package, &options).unwrap_err(), expected);
        let mut generator = MdxVgmGenerator::new(package, options, false);
        assert!(generator.next_command().unwrap().is_some());
        let error = (0..64)
            .find_map(|_| generator.next_command().err())
            .expect("F1 passes retain the command count between generator calls");
        assert_eq!(
            error.source().unwrap().downcast_ref::<MdxConvertError>(),
            Some(&expected)
        );
        assert_eq!(generator.playback.timing.elapsed_ticks, 0);
        assert_eq!(generator.playback.timing.commands_since_tick, 5);
    }

    #[test]
    fn conversion_command_limit_default_and_unlimited_counter() {
        assert_eq!(
            MdxToVgmOptions::default().max_commands_per_tick,
            Some(100_000)
        );
        let mut playback = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
        playback.timing.commands_since_tick = u32::MAX;
        playback
            .process_commands(8, &mut VgmBuilder::new())
            .unwrap();
        assert_eq!(playback.timing.commands_since_tick, u32::MAX);
        playback.timing.advance_tick();
        assert_eq!(playback.timing.commands_since_tick, 0);
    }

    #[test]
    fn conversion_tick_limit_default_duration_matches_tempo() {
        let mut timing = PlaybackTimingState::new();
        timing.tempo = 216;
        let (microseconds, _) = timing.advance_tick();
        assert_eq!(MdxToVgmOptions::default().max_ticks, Some(100_000));
        assert_eq!(u64::from(microseconds) * 100_000, 1_024_000_000);
    }

    #[test]
    fn conversion_tick_counter_does_not_wrap_when_unlimited() {
        let mut timing = PlaybackTimingState::new();
        timing.elapsed_ticks = u32::MAX;
        timing.advance_tick();
        assert_eq!(timing.elapsed_ticks, u32::MAX);
    }

    fn playback_state(pcm_mode: MdxPcmMode, adpcm_mode: AdpcmMode) -> PlaybackState<MdxPackage> {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(8, MdxRest { ticks: 1 });
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        PlaybackState::new(package, pcm_mode, adpcm_mode, None, false)
    }

    #[test]
    fn pitch_register_writes_validate_supported_range() {
        for pitch in [0, 0x17ff] {
            let mut playback = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
            let mut builder = VgmBuilder::new();
            playback.write_pitch(0, &mut builder, pitch).unwrap();
            assert_eq!(builder.command_count(), 2);
            assert_eq!(playback.tracks[0].fm.last_written_pitch, Some(pitch));
        }
        for pitch in [0x1800, 0x1fff, 0x2000, u16::MAX] {
            let mut playback = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
            let mut builder = VgmBuilder::new();
            assert_eq!(
                playback.write_pitch(0, &mut builder, pitch),
                Err(MdxConvertError::PitchOutOfRange { track: 0, pitch })
            );
            assert_eq!(builder.command_count(), 0);
            assert_eq!(playback.tracks[0].fm.last_written_pitch, None);
        }
    }

    #[test]
    fn generator_drains_each_batch_before_advancing_and_ends_once() {
        for has_pcm in [false, true] {
            let mut builder = MdxBuilder::new();
            for value in [0x11, 0x22, 0x33] {
                builder
                    .add_mdx_command(
                        0,
                        crate::mdx::command::MdxOpmRegisterWrite {
                            register: 0x1a,
                            value,
                        },
                    )
                    .add_mdx_command(0, MdxRest { ticks: 1 });
            }
            let pdx = if has_pcm {
                builder
                    .add_mdx_command(
                        8,
                        crate::mdx::command::MdxNote {
                            note: 0x80,
                            length: 1,
                        },
                    )
                    .add_mdx_command(8, MdxRest { ticks: 4 });
                let mut pdx_builder = crate::mdx::pdx::PdxBuilder::new();
                pdx_builder
                    .set_sample(0, 0, vec![0x11, 0x22, 0x33])
                    .unwrap();
                Some(pdx_builder.finalize())
            } else {
                None
            };
            let package = MdxPackage {
                mdx: builder.finalize().unwrap(),
                pdx,
            };
            let options = MdxToVgmOptions {
                loop_count: Some(1),
                ..Default::default()
            };
            let mut expected = to_vgm_document(&package, &options).unwrap().commands;
            let end = expected
                .iter()
                .position(|command| matches!(command, VgmCommand::EndOfData(_)))
                .unwrap();
            expected.truncate(end + 1);
            let mut generator = MdxVgmGenerator::new(package, options, false);
            let mut commands = vec![generator.next_command().unwrap().unwrap()];
            assert!(!generator.pending.is_empty());
            assert_eq!(generator.builder.command_count(), 0);
            let first_cursor = generator.playback.tracks[0].command_index;
            assert_eq!(first_cursor, 2);
            let pending_capacity = generator.pending.capacity();

            while !generator.pending.is_empty() {
                commands.push(generator.next_command().unwrap().unwrap());
                assert_eq!(generator.playback.tracks[0].command_index, first_cursor);
                assert_eq!(generator.builder.command_count(), 0);
            }
            for _command in 0..1024 {
                let Some(command) = generator.next_command().unwrap() else {
                    break;
                };
                commands.push(command);
                assert_eq!(generator.pending.capacity(), pending_capacity);
                assert_eq!(generator.builder.command_count(), 0);
            }

            assert!(generator.finished);
            assert!(generator.pending.is_empty());
            assert_eq!(commands, expected, "PCM present: {has_pcm}");
            assert_eq!(
                commands
                    .iter()
                    .filter(|command| matches!(command, VgmCommand::EndOfData(_)))
                    .count(),
                1
            );
            if has_pcm {
                assert!(
                    matches!(&commands[commands.len() - 2], VgmCommand::Okim6258Write(_, spec) if spec.register == 0 && spec.value == 1)
                );
            }
            for _attempt in 0..3 {
                assert_eq!(generator.next_command().unwrap(), None);
            }
        }
    }

    #[test]
    fn jump_source_map_is_initialized_once_and_only_when_needed() {
        let mut state = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
        let mut builder = VgmBuilder::new();
        assert!(state.jump_source_map.is_none());

        for _tick in 0..4 {
            if matches!(
                state.step(&mut builder, None).unwrap(),
                StepOutcome::Finished
            ) {
                break;
            }
        }
        assert!(state.finished());
        assert!(state.jump_source_map.is_none());

        state.tracks[8].command_index = 1;
        assert_eq!(state.resolve_jump_target(8, 0).unwrap(), Some(1));
        let cached = state
            .jump_source_map
            .as_ref()
            .expect("first jump builds source map");
        assert_eq!(cached, &state.package.mdx.sourcemap().unwrap());
        let cached_ptr = cached.as_ptr();
        for _repeat in 0..100 {
            assert_eq!(state.resolve_jump_target(8, 0).unwrap(), Some(1));
            assert_eq!(cached_ptr, state.jump_source_map.as_ref().unwrap().as_ptr());
        }
    }

    #[test]
    fn jump_resolution_requires_exact_boundaries_on_each_track() {
        let commands = vec![
            MdxCommand::Rest(MdxRest { ticks: 1 }),
            MdxCommand::OpmRegisterWrite(crate::mdx::command::MdxOpmRegisterWrite {
                register: 0x1a,
                value: 0x55,
            }),
            MdxCommand::Note(crate::mdx::command::MdxNote {
                note: 0x80,
                length: 1,
            }),
            MdxCommand::Jump(crate::mdx::command::MdxRelativeOffset {
                opcode: 0xf1,
                offset: -9,
            }),
        ];
        let mut builder = MdxBuilder::new();
        for track in [1, 8, 15] {
            builder.set_track(track, commands.clone());
        }
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        let mut state = PlaybackState::new(
            package,
            MdxPcmMode::Pcm8a,
            AdpcmMode::Through,
            Some(1),
            false,
        );

        for track in [1, 8, 15] {
            state.tracks[track].command_index = 4;
            for (offset, expected) in [
                (-9, Some(0)),
                (-8, Some(1)),
                (-5, Some(2)),
                (-3, Some(3)),
                (0, Some(4)),
                (-7, None),
                (-6, None),
                (-4, None),
                (-2, None),
                (-1, None),
                (1, None),
                (2, None),
                (i16::MIN, None),
                (i16::MAX, None),
            ] {
                assert_eq!(
                    state.resolve_jump_target(track, offset).unwrap(),
                    expected,
                    "track {track}, offset {offset}"
                );
            }
        }
    }

    #[test]
    fn jump_resolution_uses_document_edits_before_playback() {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(1, MdxRest { ticks: 1 });
        let mut mdx = builder.finalize().unwrap();
        mdx.header.title = "A longer title before playback".to_string();
        mdx.tracks[0] = vec![MdxCommand::Rest(MdxRest { ticks: 1 }); 64];
        mdx.tracks[1].insert(
            1,
            MdxCommand::OpmRegisterWrite(crate::mdx::command::MdxOpmRegisterWrite {
                register: 0x1a,
                value: 0x55,
            }),
        );
        mdx.tracks[1].insert(
            2,
            MdxCommand::Jump(crate::mdx::command::MdxRelativeOffset {
                opcode: 0xf1,
                offset: -6,
            }),
        );
        let package = MdxPackage { mdx, pdx: None };
        let mut state = PlaybackState::new(
            package,
            MdxPcmMode::LegacyAdpcm,
            AdpcmMode::Through,
            Some(1),
            false,
        );
        state.tracks[1].command_index = 3;

        assert_eq!(state.resolve_jump_target(1, -6).unwrap(), Some(1));
        assert_eq!(state.resolve_jump_target(1, -7).unwrap(), Some(0));
        assert_eq!(state.resolve_jump_target(1, -5).unwrap(), None);
        assert_eq!(state.resolve_jump_target(1, 0).unwrap(), Some(3));
    }

    #[test]
    fn mck_scheduler_preserves_fractional_pcm_byte_rate() {
        let mut scheduler =
            MckScheduler::new(pcm_mixer::PCM8_MASTER_SAMPLE_RATE * (MCK_RATE_DENOMINATOR / 2));
        let bytes_due = (0..768).map(|_| scheduler.advance(22_272)).sum::<u32>();

        assert_eq!(bytes_due, 133_632);
    }

    #[test]
    fn mck_scheduler_handles_maximum_tick_at_legacy_rates() {
        for ((numerator, denominator), expected) in [
            ((4_000_000, 2_048), 127),
            ((4_000_000, 1_536), 170),
            ((8_000_000, 2_048), 255),
            ((8_000_000, 1_536), 340),
            ((8_000_000, 1_024), 510),
        ] {
            let mut scheduler = MckScheduler::new(numerator * MCK_RATE_DENOMINATOR / denominator);
            assert_eq!(scheduler.advance(65_280), expected);
        }
    }

    #[test]
    fn synchronized_f1_barrier_preserves_pcm_loop_phase() {
        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(0, MdxRest { ticks: 5 })
            .add_mdx_command(
                0,
                MdxCommand::EndOfTrackLoop(crate::mdx::command::MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: 0,
                }),
            )
            .add_mdx_command(
                8,
                MdxCommand::Note(crate::mdx::command::MdxNote {
                    note: 0x80,
                    length: 1,
                }),
            )
            .add_mdx_command(8, MdxRest { ticks: 1 })
            .add_mdx_command(
                8,
                MdxCommand::EndOfTrackLoop(crate::mdx::command::MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: 0,
                }),
            );
        let mut mdx = builder.finalize().unwrap();
        for (track, loop_index) in [(0, 1), (8, 2)] {
            let source_map = mdx.sourcemap().unwrap();
            let target = source_map[track][0].0;
            let (loop_offset, loop_length) = source_map[track][loop_index];
            let relative =
                i32::try_from(target).unwrap() - i32::try_from(loop_offset + loop_length).unwrap();
            let MdxCommand::EndOfTrackLoop(command) = &mut mdx.tracks[track][loop_index] else {
                unreachable!("expected F1 loop marker");
            };
            command.offset = i16::try_from(relative).unwrap();
        }
        let package = MdxPackage { mdx, pdx: None };
        let mut playback = PlaybackState::new(
            package,
            MdxPcmMode::Pcm8a,
            AdpcmMode::Through,
            Some(2),
            false,
        );
        let mut vgm_builder = VgmBuilder::new();

        while playback.song_loop.f1_loop_passes_completed == 0 {
            playback.step(&mut vgm_builder, None).unwrap();
        }

        assert_eq!(playback.tracks[8].command_index, 1);
        assert_eq!(playback.tracks[8].wait_ticks, 1);
    }

    #[test]
    fn synchronized_f1_barrier_preserves_early_fm_loop_phase() {
        let mut builder = MdxBuilder::new();
        for (track, ticks) in [(0, 5), (1, 8)] {
            builder
                .add_mdx_command(track, MdxRest { ticks })
                .add_mdx_command(
                    track,
                    MdxCommand::EndOfTrackLoop(crate::mdx::command::MdxRelativeOffset {
                        opcode: 0xf1,
                        offset: 0,
                    }),
                );
        }
        let mut mdx = builder.finalize().unwrap();
        for track in 0..2 {
            let source_map = mdx.sourcemap().unwrap();
            let target = source_map[track][0].0;
            let (loop_offset, loop_length) = source_map[track][1];
            let relative =
                i32::try_from(target).unwrap() - i32::try_from(loop_offset + loop_length).unwrap();
            let MdxCommand::EndOfTrackLoop(command) = &mut mdx.tracks[track][1] else {
                unreachable!("expected F1 loop marker");
            };
            command.offset = i16::try_from(relative).unwrap();
        }
        let package = MdxPackage { mdx, pdx: None };
        let mut playback = PlaybackState::new(
            package,
            MdxPcmMode::Pcm8a,
            AdpcmMode::Through,
            Some(2),
            false,
        );
        let mut vgm_builder = VgmBuilder::new();

        while playback.song_loop.f1_loop_passes_completed == 0 {
            playback.step(&mut vgm_builder, None).unwrap();
        }

        assert!(playback.tracks[0].active);
        assert_eq!(playback.tracks[0].command_index, 1);
        assert_eq!(playback.tracks[0].wait_ticks, 2);
    }

    #[test]
    fn pcm_key_on_and_live_volume_follow_embedded_fadeout() {
        let mut playback = playback_state(MdxPcmMode::Pcm8a, AdpcmMode::Through);
        playback.fadeout.level = 3;
        playback.tracks[8].fm.volume = 8;

        playback.begin_pcm_key_on(8, 0x80).unwrap();

        assert_eq!(playback.pcm_output.channels[0].gain, 12);

        playback.tracks[8].fm.volume = 0x80;
        playback.apply_live_pcm_gain(8);

        assert_eq!(playback.pcm_output.channels[0].gain, 64);
    }

    #[test]
    fn fadeout_level_change_reapplies_gain_to_mixed_pcm_channels() {
        let mut playback = playback_state(MdxPcmMode::Pcm8a, AdpcmMode::Resample);
        playback.fadeout.seen = true;
        playback.fadeout.level = 2;
        playback.fadeout.counter = -1;
        playback.tracks[8].fm.volume = 8;
        playback.pcm_output.channels[0].gain = 16;

        playback.advance_fadeout(&mut VgmBuilder::new());

        assert_eq!(playback.fadeout.level, 3);
        assert_eq!(playback.pcm_output.channels[0].gain, 12);
    }

    /// Checks that legacy pass-through key-on does not populate decoded PCM storage.
    #[test]
    fn legacy_adpcm_through_key_on_does_not_decode_samples() {
        let mut mdx_builder = MdxBuilder::new();
        mdx_builder.add_mdx_command(8, MdxRest { ticks: 1 });
        let mut pdx_builder = crate::mdx::pdx::PdxBuilder::new();
        pdx_builder
            .set_sample(0, 0, vec![0x12, 0x34, 0x56])
            .unwrap();
        let package = MdxPackage {
            mdx: mdx_builder.finalize().unwrap(),
            pdx: Some(pdx_builder.finalize()),
        };
        let mut playback = PlaybackState::new(
            package,
            MdxPcmMode::LegacyAdpcm,
            AdpcmMode::Through,
            Some(1),
            false,
        );

        playback.begin_pcm_key_on(8, 0x80).unwrap();

        assert!(playback.pcm_output.samples.is_empty());
        assert!(playback.pcm_output.sample_ranges.is_empty());
        let mut builder = VgmBuilder::new();
        for _byte in 0..4 {
            playback.emit_pcm_byte(&mut builder);
        }
        let bytes = builder
            .drain_commands()
            .map(|command| match command {
                VgmCommand::Okim6258Write(_, spec) if spec.register == 1 => spec.value,
                _ => panic!("expected an OKIM6258 data write"),
            })
            .collect::<Vec<_>>();
        assert_eq!(bytes, [0x12, 0x34, 0x56, 0x80]);
    }

    /// Checks raw PDX references, ties, retriggers, bank changes, and missing samples.
    #[test]
    fn legacy_adpcm_through_preserves_raw_sample_lifecycle() {
        let mut mdx_builder = MdxBuilder::new();
        mdx_builder.add_mdx_command(8, MdxRest { ticks: 1 });
        let mut pdx_builder = crate::mdx::pdx::PdxBuilder::new();
        pdx_builder
            .set_sample(0, 0, vec![0x12, 0x34, 0x56])
            .unwrap();
        pdx_builder.set_sample(0, 1, vec![0xab, 0xcd]).unwrap();
        pdx_builder.set_sample(1, 0, vec![0xef, 0x01]).unwrap();
        let package = MdxPackage {
            mdx: mdx_builder.finalize().unwrap(),
            pdx: Some(pdx_builder.finalize()),
        };
        let mut playback = PlaybackState::new(
            package,
            MdxPcmMode::LegacyAdpcm,
            AdpcmMode::Through,
            Some(1),
            false,
        );
        playback.begin_pcm_key_on(8, 0x80).unwrap();
        let source = playback
            .package
            .pdx
            .as_ref()
            .unwrap()
            .sample_bytes(0, 0)
            .unwrap();
        let reference = playback.pcm_output.raw_sample.as_ref().unwrap();
        assert_eq!(
            playback
                .package
                .pcm_sample_bytes(reference)
                .unwrap()
                .as_ptr(),
            source.as_ptr()
        );
        assert!(
            playback
                .pcm_output
                .has_pending_output(playback.pcm_mode, playback.adpcm_mode)
        );
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x12
        );

        playback.tracks[8].fm.key_off_disabled = true;
        playback.stop_pcm_channel(8);
        playback.begin_pcm_key_on(8, 0x80).unwrap();
        assert_eq!(playback.pcm_output.raw_position, 1);
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x34
        );
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x56
        );
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x80
        );
        playback.begin_pcm_key_on(8, 0x80).unwrap();
        assert_eq!(playback.pcm_output.raw_position, 4);
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x80
        );
        playback.tracks[8].fm.key_off_disabled = false;
        playback.stop_pcm_channel(8);
        playback.begin_pcm_key_on(8, 0x80).unwrap();
        assert_eq!(playback.pcm_output.raw_position, 0);
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x12
        );

        for (bank, note, expected) in [
            (0, 0x81, vec![0xab, 0xcd]),
            (1, 0x80, vec![0xef, 0x01]),
            (0, 0x82, vec![]),
            (2, 0x80, vec![]),
        ] {
            playback.tracks[8].fm.key_off_disabled = true;
            playback.stop_pcm_channel(8);
            playback.tracks[8].pcm.bank = bank;
            playback.begin_pcm_key_on(8, note).unwrap();
            assert_eq!(playback.pcm_output.raw_position, 0);
            assert_eq!(playback.pcm_output.raw_length, expected.len());
            for byte in expected {
                assert_eq!(
                    playback
                        .pcm_output
                        .next_adpcm_byte(false, &playback.package),
                    byte
                );
            }
            assert!(
                !playback
                    .pcm_output
                    .has_pending_output(playback.pcm_mode, playback.adpcm_mode)
            );
            assert_eq!(
                playback
                    .pcm_output
                    .next_adpcm_byte(false, &playback.package),
                0x80
            );
            assert!(playback.pcm_output.samples.is_empty());
            assert!(playback.pcm_output.sample_ranges.is_empty());
        }

        playback.tracks[8].pcm.bank = 0;
        playback.begin_pcm_key_on(8, 0x80).unwrap();
        playback
            .process_commands(8, &mut VgmBuilder::new())
            .unwrap();
        assert!(playback.pcm_output.raw_sample.is_none());
        assert_eq!(playback.pcm_output.raw_position, 0);
        assert_eq!(playback.pcm_output.raw_length, 0);
        assert_eq!(
            playback
                .pcm_output
                .next_adpcm_byte(false, &playback.package),
            0x80
        );

        let mut without_pdx = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
        without_pdx.begin_pcm_key_on(8, 0x80).unwrap();
        assert_eq!(without_pdx.pcm_output.raw_length, 0);
        assert!(without_pdx.pcm_output.channels[0].block_key.is_none());
        assert_eq!(
            without_pdx
                .pcm_output
                .next_adpcm_byte(false, &without_pdx.package),
            0x80
        );
    }

    /// Checks that all mixed playback modes still decode and cache their PCM samples.
    #[test]
    fn mixed_pcm_key_on_still_decodes_and_reuses_cached_samples() {
        let mut mdx_builder = MdxBuilder::new();
        mdx_builder.add_mdx_command(8, MdxRest { ticks: 1 });
        let mut pdx_builder = crate::mdx::pdx::PdxBuilder::new();
        pdx_builder
            .set_sample(0, 0, vec![0x12, 0x34, 0x56])
            .unwrap();
        let package = MdxPackage {
            mdx: mdx_builder.finalize().unwrap(),
            pdx: Some(pdx_builder.finalize()),
        };
        let expected =
            decode_pcm8a_with_pcm16_15khz(Pcm8aFormat::Adpcm, &[0x12, 0x34, 0x56], false).unwrap();
        for (pcm_mode, adpcm_mode) in [
            (MdxPcmMode::LegacyAdpcm, AdpcmMode::Resample),
            (MdxPcmMode::LegacyAdpcm, AdpcmMode::Lpf),
            (MdxPcmMode::Pcm8a, AdpcmMode::Through),
            (MdxPcmMode::Pcm8a, AdpcmMode::Resample),
            (MdxPcmMode::Pcm8a, AdpcmMode::Lpf),
        ] {
            let mut playback =
                PlaybackState::new(package.clone(), pcm_mode, adpcm_mode, Some(1), false);
            for _trigger in 0..2 {
                playback.begin_pcm_key_on(8, 0x80).unwrap();
                assert_eq!(playback.pcm_output.samples, expected);
                assert_eq!(playback.pcm_output.sample_ranges.len(), 1);
                assert_eq!(
                    playback.pcm_output.channels[0].block_length,
                    expected.len() as u32
                );
                assert!(playback.pcm_output.raw_sample.is_none());
            }
        }
    }

    #[test]
    fn legacy_adpcm_through_does_not_apply_fadeout_to_pcm_gain() {
        let mut playback = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
        playback.fadeout.seen = true;
        playback.fadeout.level = 2;
        playback.fadeout.counter = -1;
        playback.tracks[8].fm.volume = 8;

        playback.begin_pcm_key_on(8, 0x80).unwrap();
        playback.advance_fadeout(&mut VgmBuilder::new());

        assert_eq!(playback.fadeout.level, 3);
        assert_eq!(playback.pcm_output.channels[0].gain, 16);
    }
}
