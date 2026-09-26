//! Conversion of MDX playback into VGM commands.
//!
//! This module drives parsed MDX tracks through the MXDRV timing and control
//! semantics needed to produce VGM output. It supports both eager document
//! conversion and lazy command generation for bounded-memory streaming.
//! The playback semantics and PCM8/PCM8A handling are informed by the
//! NanoDriveX (by Fujix), particularly its `src/mdx.cpp` and
//! `include/mdx.h` sources.
//!
//! Responsibilities:
//! - Translate OPM register writes, waits, loops, LFOs, and track control into
//!   VGM commands while preserving MDX playback timing.
//! - Decode and mix optional PCM8/PCM8A data through the OKIM6258 path.
//! - Expose conversion options and errors without leaking playback state into
//!   the public format model.

use crate::chip::{Chip, Okim6258Spec, Ym2151Spec};
use crate::mdx::command::{
    MdxCommand, MdxExtended2Command, MdxExtendedCommand, MdxLfoWaveform, MdxOpmLfo, MdxPan,
    MdxPitchLfo, MdxVolumeLfo,
};
use crate::mdx::package::{MdxPackage, MdxPcmReference};
use crate::mdx::pcm::{AdpcmEncoder, Pcm8aFormat, decode_pcm8a_with_pcm16_15khz};
use crate::mdx::pcm_mixer::{self, PcmChannelState, PcmOutputFilter};
use crate::mdx::tone::MdxTone;
use crate::vgm::command::{Instance, WaitSamples};
use crate::vgm::{VGM_SAMPLE_RATE, VgmBuilder, VgmDocument};
use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
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
/// NanoDriveX receives one output event per PCM byte from its MCK interrupt.
/// VGM has no MCK command, so this scheduler carries the fractional event
/// phase across MDX ticks and exposes the number of events due in each tick.
struct MckScheduler {
    byte_rate_hz: u32,
    time_remainder: u32,
    started: bool,
}

impl MckScheduler {
    fn new(byte_rate_hz: u32) -> Self {
        Self {
            byte_rate_hz,
            time_remainder: 0,
            started: false,
        }
    }

    fn set_byte_rate(&mut self, byte_rate_hz: u32) {
        self.byte_rate_hz = byte_rate_hz;
    }

    fn advance(&mut self, tick_microseconds: u32) -> u32 {
        let accumulator = self.time_remainder + tick_microseconds.saturating_mul(self.byte_rate_hz);
        let bytes_due = accumulator / MICROSECONDS_PER_SECOND;
        self.time_remainder = accumulator % MICROSECONDS_PER_SECOND;
        bytes_due
    }

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
/// PCM8A; `Lpf` additionally applies the NanoDriveX-style output filter
/// before re-encoding. For legacy ADPCM, `Through` passes the encoded source
/// bytes directly, while `Resample` and `Lpf` use the decoded mixer path.
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
    /// Resample and apply the NanoDriveX-style LPF/HPF.
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
    /// Total number of playthroughs of the whole song's repeat.
    ///
    /// This only affects the song-level repeat: a backward `Jump`, or a
    /// `LoopEnd` whose count is encoded as `0` (the file's "loop forever"
    /// marker). It does not override the counts of ordinary nested repeat
    /// blocks (`LoopStart`/`LoopEnd` with a nonzero count), since those are
    /// authored per track and overriding them independently across tracks
    /// would desynchronize them from each other.
    ///
    /// `None` (the default) encodes whole-song repeats as a native VGM loop
    /// point instead of repeating them internally. Per-track `F1` terminators
    /// are emitted once in eager conversion because VGM has only one global
    /// loop point and cannot represent independently phased track loops. For
    /// files with an embedded fadeout, track loops continue internally from
    /// their first `F1` until fadeout ends, and FM carrier total-level writes
    /// reflect the global attenuation. PCM channels routed through the mixer
    /// follow the same attenuation; legacy ADPCM `Through` bytes remain raw.
    /// `Some(1)` plays the song once with no whole-song repeat.
    pub loop_count: Option<u32>,
}

impl Default for MdxToVgmOptions {
    fn default() -> Self {
        Self {
            ym2151_clock: 4_000_000,
            okim6258_clock: pcm_mixer::PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ,
            adpcm_mode: AdpcmMode::default(),
            loop_count: None,
        }
    }
}

/// Errors returned while converting an MDX playback stream into VGM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdxConvertError {
    InvalidOptions(&'static str),
    UnsupportedCommand { track: usize, command: &'static str },
    MissingTone { voice: u8 },
}

impl fmt::Display for MdxConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MdxConvertError::InvalidOptions(reason) => write!(f, "invalid options: {reason}"),
            MdxConvertError::UnsupportedCommand { track, command } => {
                write!(f, "unsupported command `{command}` on track {track}")
            }
            MdxConvertError::MissingTone { voice } => write!(f, "missing tone for voice {voice}"),
        }
    }
}

impl Error for MdxConvertError {}

impl MdxToVgmOptions {
    /// Validates option combinations shared by both the eager
    /// [`to_vgm_document`] path and the lazy [`MdxVgmGenerator`].
    fn validate(&self) -> Result<(), MdxConvertError> {
        if self.loop_count == Some(0) {
            return Err(MdxConvertError::InvalidOptions(
                "loop count must be greater than zero",
            ));
        }
        Ok(())
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
) -> Result<Box<dyn crate::vgm::stream::VgmCommandGenerator>, MdxConvertError> {
    Ok(Box::new(MdxVgmGenerator::new(package, options, false)?))
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
    let mut generator = MdxVgmGenerator::new(package, *options, true)?;
    // Header fields with no bearing on the command stream itself (and thus
    // no counterpart in the lazy generator, which never produces a
    // `VgmDocument`); only needed here, for the serialized document.
    generator
        .builder
        .set_sample_rate(VGM_SAMPLE_RATE)
        .register_chip(Chip::Ym2151, Instance::Primary, options.ym2151_clock);

    while generator.run_step()? {}

    // MDX F1 terminators are independent per-track loops, while VGM has one
    // global loop point. Add a restartable second pass so the loop can begin
    // from a clean initialization instead of pointing into a short track's
    // first pass.
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
        let mut repeat = MdxVgmGenerator::new(package, repeat_options, true)?;
        while repeat.run_step()? {}
        for command in repeat.builder.take_commands() {
            generator.builder.add_vgm_command(command);
        }
        generator.playback.song_loop.loop_index = Some(loop_index);
    }

    let mut document = generator.playback.finalize_with_pcm(generator.builder);
    if package.pdx.is_some() {
        document.header.okim6258_flags.clock_divider = pcm_mixer::PCM8_OKIM6258_CLOCK_DIVIDER;
    }
    Ok(document)
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

struct PcmOutputState {
    /// Whether the package contains PCM playback for the OKIM6258 path.
    has_pcm: bool,
    /// Per-channel ADPCM/PCM playback state for tracks 8-15.
    channels: [PcmChannelState; 8],
    /// Single decoded PCM arena shared by all PCM channels.
    samples: Vec<i16>,
    /// Ranges in `samples` indexed by `(bank, note, format)`.
    sample_ranges: HashMap<(usize, usize, u8, bool), (usize, usize)>,
    /// Persistent re-encoder state for the whole song's mixed PCM8 output.
    encoder: AdpcmEncoder,
    /// Persistent NanoDriveX-style output filter state for the mixed PCM8 stream.
    filter: PcmOutputFilter,
    /// Raw PCM1 ADPCM payload used by the `Through` mode.
    raw_bytes: Vec<u8>,
    /// Next raw byte to emit in the `Through` mode.
    raw_position: usize,
    /// MCK-driven OKIM6258 data-register write scheduler.
    mck_scheduler: MckScheduler,
}

impl PcmOutputState {
    fn new(has_pcm: bool, adpcm_mode: AdpcmMode) -> Self {
        Self {
            has_pcm,
            channels: Default::default(),
            samples: Vec::new(),
            sample_ranges: HashMap::new(),
            encoder: AdpcmEncoder::default(),
            filter: PcmOutputFilter::new(matches!(adpcm_mode, AdpcmMode::Lpf)),
            raw_bytes: Vec::new(),
            raw_position: 0,
            mck_scheduler: MckScheduler::new(pcm_mixer::PCM8_STREAM_BYTE_RATE_HZ),
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
}

impl PlaybackTimingState {
    fn new() -> Self {
        Self {
            tempo: DEFAULT_TEMPO,
            sample_remainder: 0,
        }
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
    fn new(loop_count: Option<u32>, mark_native_loop: bool) -> Self {
        Self {
            loop_count,
            mark_native_loop,
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

struct PlaybackState<P: Borrow<MdxPackage>> {
    /// MDX/PDX package being consumed by the playback simulation.
    package: P,
    /// MDX PCM command semantics selected from the header's track layout.
    pcm_mode: MdxPcmMode,
    /// ADPCM processing mode selected by the caller.
    adpcm_mode: AdpcmMode,
    /// Per-track command cursors and playback state for the MDX tracks.
    tracks: Vec<TrackState>,
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
    /// Shared LFO random generator seed (mirrors the single global PRNG
    /// used by all tracks in the reference implementation).
    lfo_rand_seed: u16,
    /// Whole-song repeat detection and native VGM loop-point state.
    song_loop: SongLoopState,
    /// Embedded fadeout detection and attenuation progression.
    fadeout: FadeoutState,
}

impl<P: Borrow<MdxPackage>> PlaybackState<P> {
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
            tracks,
            pcm_output: PcmOutputState::new(has_pcm, adpcm_mode),
            timing: PlaybackTimingState::new(),
            opm_reg_0f: 0,
            opm_reg_1b: 0,
            lfo_rand_seed: 0x1234,
            song_loop: SongLoopState::new(loop_count, mark_native_loop),
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

    fn has_pending_pcm_output(&self) -> bool {
        let raw_pending = matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && self.pcm_output.raw_position < self.pcm_output.raw_bytes.len();
        let mixed_pending = !(matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(self.adpcm_mode, AdpcmMode::Through))
            && self
                .pcm_output
                .channels
                .iter()
                .any(|channel| channel.block_length != 0);
        raw_pending || mixed_pending
    }

    /// Runs one tick of playback, appending any resulting commands to
    /// `builder`. Shared by the eager [`to_vgm_document`] loop and
    /// [`MdxVgmGenerator`], which drives the same steps lazily.
    fn step(&mut self, builder: &mut VgmBuilder) -> Result<StepOutcome, MdxConvertError> {
        if self.finished() {
            return Ok(StepOutcome::Finished);
        }
        self.process_tick(builder)?;
        if self.finished() {
            return Ok(StepOutcome::Finished);
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
    fn process_tick(&mut self, builder: &mut VgmBuilder) -> Result<(), MdxConvertError> {
        self.advance_fadeout(builder);
        if self.fadeout.level >= FADEOUT_FINAL_LEVEL {
            return Ok(());
        }
        for track_index in 0..self.tracks.len() {
            if !self.tracks[track_index].active {
                continue;
            }
            if self.tracks[track_index].wait_ticks > 0 {
                self.tracks[track_index].wait_ticks -= 1;
            }
            if track_index < 8 {
                self.update_fm_tick(track_index, builder);
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
        Ok(())
    }

    /// Processes a key-off event for the specified track, handling both FM
    /// and ADPCM/PCM channels as appropriate. For FM channels (< 8), it
    /// sends the key-off command to the YM2151. For ADPCM/PCM channels (>= 8),
    /// it stops the PCM channel according to the track's key-off state.
    fn process_key_off(&mut self, track: usize, builder: &mut VgmBuilder) {
        let reached_zero = {
            let state = &mut self.tracks[track];
            if state.key_off_ticks == 0 {
                false
            } else {
                state.key_off_ticks -= 1;
                state.key_off_ticks == 0
            }
        };
        if !reached_zero {
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

    /// Stops the ADPCM/PCM channel mapped to `track` (>= 8), mirroring
    /// `stopPcm8Channel`: a tie (`key_off_disabled`) holds the channel at
    /// the end of its block instead of clearing it, so a following tied
    /// note is not retriggered.
    fn stop_pcm_channel(&mut self, track: usize) {
        let channel = track - 8;
        let hold = self.tracks[track].fm.key_off_disabled;
        let state = &mut self.pcm_output.channels[channel];
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

    /// Applies an ADPCM/PCM key-on for `track` (>= 8): resolves the note
    /// (`0x80`-based) against the track's current PDX bank and data format,
    /// decoding and caching the sample data as needed. Mirrors the
    /// reference's block-id lookup and its `adpcmHold` re-trigger guard.
    fn begin_pcm_key_on(&mut self, track: usize, note: u8) {
        let Some(note_index) = note.checked_sub(0x80) else {
            return;
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
        let same_block = self.pcm_output.channels[channel].hold
            && self.pcm_output.channels[channel].block_key == Some(block_key);
        if same_block {
            // F7 followed by the same PCM note is a held note, not a second
            // trigger. This is the NanoDriveX "WAPICO" compatibility case.
            return;
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
            self.pcm_output.raw_bytes = self
                .package
                .borrow()
                .pcm_sample_bytes(&reference)
                .unwrap_or_default()
                .to_vec();
            self.pcm_output.raw_position = 0;
        }
        let range = self.decode_pcm_samples(
            bank,
            note_index,
            format,
            format == Pcm8aFormat::Pcm16 && rate_step == 0x10000,
        );
        let state = &mut self.pcm_output.channels[channel];
        state.block_start = range.map_or(0, |(start, _)| start);
        state.block_length = range.map_or(0, |(_, length)| length as u32);
        state.block_key = range.map(|_| block_key);
        state.pos_in_block = 0;
        state.rate_counter = 0;
        let state = &mut self.pcm_output.channels[channel];
        state.rate_step = rate_step;
        state.gain = gain;
        state.hold = tie;
    }

    /// Applies a live volume change to a currently-playing ADPCM/PCM
    /// channel (`track` >= 8), mirroring `pcm8SetVolume`'s behavior of
    /// updating a channel's gain immediately rather than only at the next
    /// key-on.
    fn apply_live_pcm_gain(&mut self, track: usize) {
        let gain = self.pcm_channel_gain(track);
        self.pcm_output.channels[track - 8].gain = gain;
    }

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

    fn pcm_uses_mixer(&self) -> bool {
        !(matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(self.adpcm_mode, AdpcmMode::Through))
    }

    /// Decodes one PDX sample into the shared PCM arena and returns its range.
    /// Repeated `(bank, note, format, rate)` lookups share the existing range.
    fn decode_pcm_samples(
        &mut self,
        bank: usize,
        note: usize,
        format: Pcm8aFormat,
        pcm16_is_15khz: bool,
    ) -> Option<(usize, usize)> {
        let format_key = match format {
            Pcm8aFormat::Adpcm => 0u8,
            Pcm8aFormat::Pcm16 => 1u8,
            Pcm8aFormat::Pcm8 => 2u8,
        };
        let key = (bank, note, format_key, pcm16_is_15khz);
        if let Some(&range) = self.pcm_output.sample_ranges.get(&key) {
            return Some(range);
        }
        let bytes = self
            .package
            .borrow()
            .pdx
            .as_ref()?
            .sample_bytes(bank, note)?;
        let decoded = decode_pcm8a_with_pcm16_15khz(format, bytes, pcm16_is_15khz).ok()?;
        let start = self.pcm_output.samples.len();
        let length = decoded.len();
        self.pcm_output.samples.extend_from_slice(&decoded);
        let range = (start, length);
        self.pcm_output.sample_ranges.insert(key, range);
        Some(range)
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
            self.tracks[track].command_index += 1;
            if matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm) && track == 8 {
                self.pcm_output.raw_bytes.clear();
                self.pcm_output.raw_position = 0;
            }
            match command {
                MdxCommand::Rest(command) => {
                    self.tracks[track].wait_ticks = command.ticks;
                    // Rest always (re)starts the key-off countdown and
                    // cancels any pending tie, regardless of prior state.
                    self.tracks[track].key_off_ticks = command.ticks;
                    self.tracks[track].fm.key_off_disabled = false;
                    if track >= 8 {
                        // NanoDriveX clears the ADPCM hold at a rest while
                        // allowing the sample to continue to its own end.
                        self.pcm_output.channels[track - 8].hold = false;
                    }
                }
                MdxCommand::Note(command) if track < 8 => {
                    let note = i32::from(command.note - 0x80) + self.tracks[track].fm.transpose;
                    let note = note.clamp(0, 127) as u16;
                    let pitch = (note << 6)
                        .saturating_add(5)
                        .saturating_add_signed(self.tracks[track].fm.detune);
                    self.tracks[track].fm.note_pitch = Some(pitch);
                    self.write_pitch(track, builder, pitch);
                    if self.tracks[track].fm.key_on_delay == 0 {
                        self.begin_key_on(track, builder)?;
                    } else {
                        self.tracks[track].fm.key_on_delay_counter =
                            self.tracks[track].fm.key_on_delay;
                        self.tracks[track].fm.key_on_pending = true;
                    }
                    self.tracks[track].wait_ticks = command.length;
                    if !self.tracks[track].fm.key_off_disabled {
                        let gate = i16::from(self.tracks[track].fm.gate);
                        let raw_length = (command.length - 1).min(i16::MAX as u16) as i16;
                        let key_off = if gate >= 0 {
                            ((raw_length * gate) >> 3) + 1
                        } else {
                            (raw_length + gate).max(0) + 1
                        };
                        self.tracks[track].key_off_ticks = key_off as u16;
                    } else {
                        self.tracks[track].key_off_ticks = 0;
                    }
                    self.tracks[track].fm.key_off_disabled = false;
                }
                MdxCommand::Note(command) => {
                    // PCM key-on: `note` is an 0x80-based index into the
                    // track's current PDX bank rather than a pitch.
                    self.begin_pcm_key_on(track, command.note);
                    self.tracks[track].wait_ticks = command.length;
                    if !self.tracks[track].fm.key_off_disabled {
                        let gate = i16::from(self.tracks[track].fm.gate);
                        let raw_length = (command.length - 1).min(i16::MAX as u16) as i16;
                        let key_off = if gate >= 0 {
                            ((raw_length * gate) >> 3) + 1
                        } else {
                            (raw_length + gate).max(0) + 1
                        };
                        self.tracks[track].key_off_ticks = key_off as u16;
                    } else {
                        self.tracks[track].key_off_ticks = 0;
                    }
                    self.tracks[track].fm.key_off_disabled = false;
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
                        MdxPan::Right => 2,
                        MdxPan::Left => 1,
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
                    self.volume_down(track);
                    self.emit_volume(track, builder);
                }
                MdxCommand::VolumeDown(_) => {
                    self.volume_down(track);
                    self.apply_live_pcm_gain(track);
                }
                MdxCommand::VolumeUp(_) if track < 8 => {
                    self.volume_up(track);
                    self.emit_volume(track, builder);
                }
                MdxCommand::VolumeUp(_) => {
                    self.volume_up(track);
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
                MdxCommand::PitchLfo(command) => self.apply_pitch_lfo(track, command),
                MdxCommand::VolumeLfo(command) => self.apply_volume_lfo(track, command),
                MdxCommand::LfoDelay(command) => self.tracks[track].lfo.delay = command.value,
                MdxCommand::PcmMode(_) => {}
                MdxCommand::Extended(command) => match command {
                    MdxExtendedCommand::Fadeout { value } => {
                        self.fadeout.seen = true;
                        self.fadeout.speed = value;
                        self.fadeout.counter = i16::from(value);
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
                    // Relative detune is parsed but not applied in the
                    // reference implementation.
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
                    // The jump target is the position saved at `LoopStart`,
                    // not recomputed from this command's own offset
                    // (mirrors the reference's `pc = loopStack[sp]`, which
                    // stays correct even if a malformed file's encoded
                    // offset does not actually point back to the loop
                    // start).
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
                        // jumping, matching the reference's `sp == 0` guard.
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
                        // The reference's offset targets the corresponding
                        // 0xF5 opcode itself; add 2 to land past its
                        // operand and fully break out of the loop.
                        self.jump_relative(track, command.offset.saturating_add(2));
                    }
                }
                MdxCommand::Jump(command) => {
                    self.take_repeating_jump(track, command.offset, builder)
                }
                MdxCommand::EndOfTrackLoop(command) => {
                    self.song_loop.track_end_loop_seen = true;
                    if self.song_loop.mark_native_loop
                        && self.song_loop.loop_count.is_none()
                        && !self.fadeout.has_command
                    {
                        // F1 is a per-track terminator in MDX. A short PCM
                        // track can loop long before the rest of the song, so
                        // it must not become the global VGM loop point.
                        self.tracks[track].active = false;
                    } else if self.song_loop.mark_native_loop && self.song_loop.loop_count.is_none()
                    {
                        self.jump_relative(track, command.offset);
                    } else {
                        self.take_repeating_jump(track, command.offset, builder);
                    }
                }
                MdxCommand::Raw(_) => {}
            }
        }
        Ok(())
    }

    /// Applies the tone (operator registers only; register `0x20` is
    /// handled separately by `apply_pending_fm_state`), mirroring
    /// `_setVoice`.
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
        // Loading a voice always re-arms the pan-pending flag too (even
        // without a new Pan command), so register 0x20's CON/FL bits get
        // refreshed for the new algorithm at the next key-on. Mirrors
        // `_setVoice`'s unconditional `flags |= 0x04`.
        self.tracks[track].fm.pan_pending = true;
        // Store the tone's own key-on slot mask combined with the channel;
        // a zero mask falls back to the algorithm default at key-on time.
        self.tracks[track].fm.key_on_slot = ((tone.op & 0x0f) << 3) | fm_channel;
        emit_tone(builder, fm_channel, tone);
        self.emit_volume(track, builder);
        Ok(())
    }

    /// Resolves the key-on slot mask, falling back to the algorithm's
    /// default carrier slots when the tone did not specify its own mask.
    fn resolved_key_on_slot(&self, track: usize) -> u8 {
        let state = &self.tracks[track];
        if state.fm.key_on_slot & 0xf8 == 0 {
            CARRIER_KEYON_SLOTS[(state.fm.con_fl & 0x07) as usize] | state.fm.fm_channel
        } else {
            state.fm.key_on_slot
        }
    }

    /// Applies a pending voice select and/or pan change, mirroring
    /// `_applyPendingFmState`. Both are deferred from the command that set
    /// them until the next key-on.
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

    /// Applies FM key-on, mirroring `_applyPendingFmState`'s tie handling:
    /// an already-sounding note (tie/legato) is not retriggered, so its
    /// envelope and LFO delay continue uninterrupted.
    fn begin_key_on(
        &mut self,
        track: usize,
        builder: &mut VgmBuilder,
    ) -> Result<(), MdxConvertError> {
        self.apply_pending_fm_state(track, builder)?;
        let already_on = self.tracks[track].fm.key_on;
        if !already_on && self.tracks[track].lfo.delay > 0 {
            self.start_lfo_delay(track);
        }
        self.tracks[track].fm.bend_offset = 0;
        if !already_on {
            self.reset_opm_lfo_if_needed(track, builder);
            let slot = self.resolved_key_on_slot(track);
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
    fn update_fm_tick(&mut self, track: usize, builder: &mut VgmBuilder) {
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
        let mut skip_lfo = false;
        if self.tracks[track].lfo.delay > 0 {
            if self.tracks[track].fm.key_on_delay_counter != 0 {
                skip_lfo = true;
            } else if self.tracks[track].lfo.delay_counter > 0 {
                self.tracks[track].lfo.delay_counter -= 1;
                if self.tracks[track].lfo.delay_counter == 0 {
                    if self.tracks[track].lfo.pitch_enabled {
                        self.reset_pitch_lfo(track);
                    }
                    if self.tracks[track].lfo.volume_enabled {
                        self.reset_volume_lfo(track);
                    }
                }
                skip_lfo = true;
            }
        }
        if !skip_lfo {
            self.update_pitch_lfo(track);
            self.update_volume_lfo(track);
        }
        // Updates the pitch and volume for the current tick based on the LFO and pitch bend.
        if self.tracks[track].fm.note_pitch.is_some() {
            self.update_pitch(track, builder);
        }
        // Emits the volume register write if the volume LFO offset has changed.
        if self.tracks[track].lfo.volume_offset != prev_volume_lfo_offset {
            self.emit_volume(track, builder);
        }
    }

    /// Writes the pitch registers only if the computed pitch changed
    /// (mirrors `writePitchIfChanged` in the reference).
    fn update_pitch(&mut self, track: usize, builder: &mut VgmBuilder) {
        let Some(note_pitch) = self.tracks[track].fm.note_pitch else {
            return;
        };
        let bend = self.tracks[track].fm.bend_offset >> 16;
        let lfo = self.tracks[track].lfo.pitch_offset >> 16;
        let pitch = i32::from(note_pitch)
            .saturating_add(bend)
            .saturating_add(lfo)
            .clamp(0, 0x17ff) as u16;
        if self.tracks[track].fm.last_written_pitch == Some(pitch) {
            return;
        }
        self.write_pitch(track, builder, pitch);
    }

    /// Writes the computed pitch to the YM2151 registers for the specified track.
    /// Updates the last written pitch to avoid redundant writes.
    fn write_pitch(&mut self, track: usize, builder: &mut VgmBuilder, pitch: u16) {
        let fm_channel = self.tracks[track].fm.fm_channel;
        let pitch_register = pitch << 2;
        let key_fraction = pitch_register as u8;
        let key_code = YM2151_KEYCODE_TABLE[((pitch_register >> 8) & 0x7f) as usize];
        write_ym2151(builder, 0x30 + fm_channel, key_fraction);
        write_ym2151(builder, 0x28 + fm_channel, key_code);
        self.tracks[track].fm.last_written_pitch = Some(pitch);
    }

    /// Decreases the volume of the specified track, taking into account the
    /// FM volume encoding. If the volume is in the lower 7 bits, it is
    /// decremented by 1 unless it is already at the minimum. If the volume
    /// is in the upper 7 bits (indicating attenuation), it is incremented
    /// by 1 unless it is already at the maximum.
    fn volume_down(&mut self, track: usize) {
        let volume = self.tracks[track].fm.volume;
        self.tracks[track].fm.volume = if volume & 0x80 == 0 {
            volume.saturating_sub(1)
        } else if volume != 0xff {
            volume + 1
        } else {
            volume
        };
    }

    /// Increases the volume of the specified track, taking into account the
    /// FM volume encoding. If the volume is in the lower 7 bits, it is
    /// incremented by 1 unless it is already at the maximum. If the volume
    /// is in the upper 7 bits (indicating attenuation), it is decremented
    /// by 1 unless it is already at the minimum.
    fn volume_up(&mut self, track: usize) {
        let volume = self.tracks[track].fm.volume;
        self.tracks[track].fm.volume = if volume & 0x80 == 0 {
            if volume < 15 { volume + 1 } else { volume }
        } else if volume != 0x80 {
            volume - 1
        } else {
            volume
        };
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
    fn apply_pitch_lfo(&mut self, track: usize, command: MdxPitchLfo) {
        match command {
            MdxPitchLfo::SetEnabled { enabled } => {
                if enabled {
                    self.reset_pitch_lfo(track);
                    self.tracks[track].lfo.pitch_enabled = true;
                } else {
                    self.tracks[track].lfo.pitch_enabled = false;
                    self.tracks[track].lfo.pitch_offset = 0;
                }
            }
            MdxPitchLfo::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                self.tracks[track].lfo.pitch_enabled = true;
                let wave_type = waveform.base();
                let mode = wave_type << 1;
                self.tracks[track].lfo.pitch_type = Some(waveform.base_waveform());
                self.tracks[track].lfo.pitch_length = frequency;

                let mut cooked = frequency;
                if mode != 0x02 {
                    cooked >>= 1;
                    if mode == 0x06 {
                        cooked = 1;
                    }
                }
                self.tracks[track].lfo.pitch_length_cooked = cooked;

                let mut delta = i32::from(amplitude) << 8;
                let wave_check = waveform.base();
                if waveform.has_extended_amplitude() {
                    delta <<= 8;
                }
                self.tracks[track].lfo.pitch_delta_start = delta;
                self.tracks[track].lfo.pitch_offset_start =
                    if wave_check == 0x02 { delta } else { 0 };

                self.tracks[track].lfo.pitch_length_counter =
                    self.tracks[track].lfo.pitch_length_cooked;
                self.tracks[track].lfo.pitch_delta = self.tracks[track].lfo.pitch_delta_start;
                self.tracks[track].lfo.pitch_offset = self.tracks[track].lfo.pitch_offset_start;
            }
        }
    }

    /// Applies the volume LFO settings for the specified track, updating the internal
    /// state as necessary. Handles enabling/disabling the LFO and configuring its
    /// waveform, frequency, and amplitude.
    fn apply_volume_lfo(&mut self, track: usize, command: MdxVolumeLfo) {
        match command {
            MdxVolumeLfo::SetEnabled { enabled } => {
                if enabled {
                    self.reset_volume_lfo(track);
                    self.tracks[track].lfo.volume_enabled = true;
                } else {
                    self.tracks[track].lfo.volume_enabled = false;
                    self.tracks[track].lfo.volume_offset = 0;
                }
            }
            MdxVolumeLfo::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                self.tracks[track].lfo.volume_enabled = true;
                let mode = waveform.raw() << 1;
                self.tracks[track].lfo.volume_type = Some(waveform.base_waveform());
                self.tracks[track].lfo.volume_length = frequency;
                self.tracks[track].lfo.volume_delta_start = amplitude;

                let mut cooked = i32::from(amplitude as i16);
                if mode & 0x02 == 0 {
                    cooked = cooked.wrapping_mul(i32::from(frequency as i16));
                }
                cooked = cooked.wrapping_neg();
                if cooked < 0 {
                    cooked = 0;
                }
                self.tracks[track].lfo.volume_delta_cooked = cooked as u16;

                self.tracks[track].lfo.volume_length_counter = self.tracks[track].lfo.volume_length;
                self.tracks[track].lfo.volume_delta = self.tracks[track].lfo.volume_delta_start;
                self.tracks[track].lfo.volume_offset = self.tracks[track].lfo.volume_delta_cooked;
            }
        }
    }

    /// Resets the pitch LFO for the specified track to its initial state.
    fn reset_pitch_lfo(&mut self, track: usize) {
        self.tracks[track].lfo.pitch_length_counter = self.tracks[track].lfo.pitch_length_cooked;
        self.tracks[track].lfo.pitch_delta = self.tracks[track].lfo.pitch_delta_start;
        self.tracks[track].lfo.pitch_offset = self.tracks[track].lfo.pitch_offset_start;
    }

    /// Resets the volume LFO for the specified track to its initial state.
    fn reset_volume_lfo(&mut self, track: usize) {
        self.tracks[track].lfo.volume_length_counter = self.tracks[track].lfo.volume_length;
        self.tracks[track].lfo.volume_delta = self.tracks[track].lfo.volume_delta_start;
        self.tracks[track].lfo.volume_offset = self.tracks[track].lfo.volume_delta_cooked;
    }

    /// Starts the LFO delay for the specified track. Initializes the delay counter
    /// and resets the pitch and volume LFO offsets. If the delay counter reaches zero,
    /// the pitch and volume LFOs are reset immediately.
    fn start_lfo_delay(&mut self, track: usize) {
        self.tracks[track].lfo.delay_counter = self.tracks[track].lfo.delay;
        self.tracks[track].lfo.pitch_offset = 0;
        self.tracks[track].lfo.volume_offset = 0;
        self.tracks[track].lfo.delay_counter = self.tracks[track].lfo.delay_counter.wrapping_sub(1);
        if self.tracks[track].lfo.delay_counter == 0 {
            if self.tracks[track].lfo.pitch_enabled {
                self.reset_pitch_lfo(track);
            }
            if self.tracks[track].lfo.volume_enabled {
                self.reset_volume_lfo(track);
            }
        }
    }

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
    fn update_pitch_lfo(&mut self, track: usize) {
        if !self.tracks[track].lfo.pitch_enabled {
            return;
        }
        let Some(waveform) = self.tracks[track].lfo.pitch_type else {
            return;
        };
        match waveform {
            MdxLfoWaveform::Sawtooth => {
                // Sawtooth: ramp, then flip sign at the end of each period.
                self.tracks[track].lfo.pitch_offset = self.tracks[track]
                    .lfo
                    .pitch_offset
                    .wrapping_add(self.tracks[track].lfo.pitch_delta);
                self.tracks[track].lfo.pitch_length_counter =
                    self.tracks[track].lfo.pitch_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.pitch_length_counter == 0 {
                    self.tracks[track].lfo.pitch_length_counter =
                        self.tracks[track].lfo.pitch_length;
                    self.tracks[track].lfo.pitch_offset =
                        self.tracks[track].lfo.pitch_offset.wrapping_neg();
                }
            }
            MdxLfoWaveform::Square => {
                // Square: hold at delta, flip sign at the end of each period.
                self.tracks[track].lfo.pitch_offset = self.tracks[track].lfo.pitch_delta;
                self.tracks[track].lfo.pitch_length_counter =
                    self.tracks[track].lfo.pitch_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.pitch_length_counter == 0 {
                    self.tracks[track].lfo.pitch_length_counter =
                        self.tracks[track].lfo.pitch_length;
                    self.tracks[track].lfo.pitch_delta =
                        self.tracks[track].lfo.pitch_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::Triangle => {
                // Triangle: ramp continuously, flip sign at each period end.
                self.tracks[track].lfo.pitch_offset = self.tracks[track]
                    .lfo
                    .pitch_offset
                    .wrapping_add(self.tracks[track].lfo.pitch_delta);
                self.tracks[track].lfo.pitch_length_counter =
                    self.tracks[track].lfo.pitch_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.pitch_length_counter == 0 {
                    self.tracks[track].lfo.pitch_length_counter =
                        self.tracks[track].lfo.pitch_length;
                    self.tracks[track].lfo.pitch_delta =
                        self.tracks[track].lfo.pitch_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::RandomNoise => {
                // Random: reload with a new random offset each period.
                self.tracks[track].lfo.pitch_length_counter =
                    self.tracks[track].lfo.pitch_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.pitch_length_counter == 0 {
                    let random = i32::from(self.next_lfo_rand() as i16);
                    self.tracks[track].lfo.pitch_offset =
                        random.wrapping_mul(self.tracks[track].lfo.pitch_delta);
                    self.tracks[track].lfo.pitch_length_counter =
                        self.tracks[track].lfo.pitch_length;
                }
            }
            MdxLfoWaveform::Unknown(_) => {}
        }
    }

    /// Updates the volume LFO for the specified track based on its type, delta, and length counter.
    /// Handles sawtooth, square, and triangle waveforms, updating the internal offset and
    /// length counter accordingly.
    fn update_volume_lfo(&mut self, track: usize) {
        if !self.tracks[track].lfo.volume_enabled {
            return;
        }
        let Some(waveform) = self.tracks[track].lfo.volume_type else {
            return;
        };
        match waveform {
            MdxLfoWaveform::Sawtooth => {
                // Sawtooth: ramp, then reset to the cooked baseline.
                self.tracks[track].lfo.volume_offset = self.tracks[track]
                    .lfo
                    .volume_offset
                    .wrapping_add(self.tracks[track].lfo.volume_delta);
                self.tracks[track].lfo.volume_length_counter =
                    self.tracks[track].lfo.volume_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.volume_length_counter == 0 {
                    self.tracks[track].lfo.volume_length_counter =
                        self.tracks[track].lfo.volume_length;
                    self.tracks[track].lfo.volume_offset =
                        self.tracks[track].lfo.volume_delta_cooked;
                }
            }
            MdxLfoWaveform::Square => {
                // Square: step at each period end, then flip sign.
                self.tracks[track].lfo.volume_length_counter =
                    self.tracks[track].lfo.volume_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.volume_length_counter == 0 {
                    self.tracks[track].lfo.volume_length_counter =
                        self.tracks[track].lfo.volume_length;
                    self.tracks[track].lfo.volume_offset = self.tracks[track]
                        .lfo
                        .volume_offset
                        .wrapping_add(self.tracks[track].lfo.volume_delta);
                    self.tracks[track].lfo.volume_delta =
                        self.tracks[track].lfo.volume_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::Triangle => {
                // Triangle: ramp continuously, flip sign at each period end.
                self.tracks[track].lfo.volume_offset = self.tracks[track]
                    .lfo
                    .volume_offset
                    .wrapping_add(self.tracks[track].lfo.volume_delta);
                self.tracks[track].lfo.volume_length_counter =
                    self.tracks[track].lfo.volume_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.volume_length_counter == 0 {
                    self.tracks[track].lfo.volume_length_counter =
                        self.tracks[track].lfo.volume_length;
                    self.tracks[track].lfo.volume_delta =
                        self.tracks[track].lfo.volume_delta.wrapping_neg();
                }
            }
            MdxLfoWaveform::RandomNoise => {
                // Random: reload with a new random offset each period.
                self.tracks[track].lfo.volume_length_counter =
                    self.tracks[track].lfo.volume_length_counter.wrapping_sub(1);
                if self.tracks[track].lfo.volume_length_counter == 0 {
                    let random = i32::from(self.next_lfo_rand() as i16);
                    let delta = i32::from(self.tracks[track].lfo.volume_delta as i16);
                    self.tracks[track].lfo.volume_offset = random.wrapping_mul(delta) as u16;
                    self.tracks[track].lfo.volume_length_counter =
                        self.tracks[track].lfo.volume_length;
                }
            }
            MdxLfoWaveform::Unknown(_) => {}
        }
    }

    /// Shared LFO random generator (single global PRNG in the reference).
    /// Returns the next random value to be used for the volume LFO's random waveform.
    fn next_lfo_rand(&mut self) -> u16 {
        let value = u32::from(self.lfo_rand_seed)
            .wrapping_mul(0xc549)
            .wrapping_add(0x0c);
        self.lfo_rand_seed = value as u16;
        (value >> 8) as u16
    }

    /// Resolves a relative jump offset (as encoded by `LoopEnd`,
    /// `LoopEscape` or `Jump`) to an absolute command index, if the target
    /// lands exactly on a command boundary.
    fn resolve_jump_target(&self, track: usize, offset: i16) -> Option<usize> {
        let command_index = self.tracks[track].command_index;
        let (current_offset, current_length) = self.package.borrow().mdx.sourcemap()[track]
            .get(command_index.saturating_sub(1))
            .copied()?;
        let command_end = current_offset.checked_add(current_length)?;
        let target_offset = if offset >= 0 {
            command_end.checked_add(offset as usize)?
        } else {
            command_end.checked_sub(offset.unsigned_abs() as usize)?
        };
        self.package.borrow().mdx.sourcemap()[track]
            .iter()
            .position(|(offset, _)| *offset == target_offset)
    }

    /// Performs a relative jump for the specified track by the given offset.
    /// If the target offset corresponds to a valid command boundary, updates the
    /// track's command index accordingly.
    fn jump_relative(&mut self, track: usize, offset: i16) {
        if let Some(target_index) = self.resolve_jump_target(track, offset) {
            self.tracks[track].command_index = target_index;
        }
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
    fn take_repeating_jump(&mut self, track: usize, offset: i16, builder: &mut VgmBuilder) {
        let jump_command_index = self.tracks[track].command_index - 1;
        let Some(target_index) = self.resolve_jump_target(track, offset) else {
            return;
        };
        self.take_repeating_jump_to(track, jump_command_index, target_index, builder);
    }

    /// Emits the necessary wait commands to the VGM builder to account for the passage
    /// of one MDX tick, taking into consideration both the sample rate and any pending
    /// PCM data writes. Ensures that PCM bytes are spread evenly across the tick's
    /// samples to maintain accurate playback timing.
    fn emit_wait(&mut self, builder: &mut VgmBuilder) {
        let tick_microseconds = self.tick_microseconds();
        let sample_accumulator = self.timing.sample_remainder + tick_microseconds * VGM_SAMPLE_RATE;
        let samples = sample_accumulator / MICROSECONDS_PER_SECOND;
        self.timing.sample_remainder = sample_accumulator % MICROSECONDS_PER_SECOND;

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
            let target_samples =
                (u64::from(target_index) * u64::from(samples) / u64::from(pcm_bytes_due)) as u32;
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

    /// Wall-clock duration of one MDX tick at the current tempo, in
    /// microseconds; shared by the FM wait-sample clock and the PCM8
    /// mixer's own sample clock so both stay in sync with the same tempo.
    fn tick_microseconds(&self) -> u32 {
        256 * u32::from(256u16 - u16::from(self.timing.tempo))
    }

    /// Advances the global fadeout counter and reapplies FM and mixed PCM
    /// attenuation when the level changes.
    fn advance_fadeout(&mut self, builder: &mut VgmBuilder) {
        if !self.fadeout.seen || self.fadeout.level >= FADEOUT_FINAL_LEVEL {
            return;
        }
        if self.fadeout.counter >= 0 {
            self.fadeout.counter -= 2;
            return;
        }

        self.fadeout.level += 1;
        self.fadeout.counter = i16::from(self.fadeout.speed);
        for track in 0..8 {
            if self.tracks[track].fm.voice_selected {
                self.emit_volume(track, builder);
            }
        }
        if self.pcm_uses_mixer() {
            for track in 8..self.tracks.len().min(16) {
                let gain = self.pcm_channel_gain(track);
                self.pcm_output.channels[track - 8].gain = gain;
            }
        }
    }

    /// Applies NanoDriveX's legacy PCM1 clock/divider selection for `0xed`
    /// F0-F4. OKIM6258 clock bytes are written to registers `0x08`-`0x0b`;
    /// libvgm commits the new clock when register `0x0b` is written. The
    /// following `0x0c` write selects the divider.
    fn set_legacy_pcm_rate(&mut self, mode: u8, builder: &mut VgmBuilder) {
        let Some((byte_rate_hz, clock_bytes, divider_value)) = (match mode {
            0 => Some((1_953, [0x00, 0x09, 0x3d, 0x00], 0)), // 4 MHz / 1024
            1 => Some((2_604, [0x00, 0x09, 0x3d, 0x00], 1)), // 4 MHz / 768
            2 => Some((3_906, [0x00, 0x12, 0x7a, 0x00], 0)), // 8 MHz / 1024
            3 => Some((5_208, [0x00, 0x12, 0x7a, 0x00], 1)), // 8 MHz / 768
            4 => Some((
                pcm_mixer::PCM8_STREAM_BYTE_RATE_HZ,
                [0x00, 0x12, 0x7a, 0x00],
                2,
            )), // 8 MHz / 512
            _ => None,
        }) else {
            return;
        };
        self.pcm_output.mck_scheduler.set_byte_rate(byte_rate_hz);
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

    /// Mixes and re-encodes one ADPCM byte (2 samples) from the 8 PCM8
    /// channels and writes it directly to the OKIM6258 data register (1).
    fn emit_pcm_byte(&mut self, builder: &mut VgmBuilder) {
        let byte = if matches!(self.pcm_mode, MdxPcmMode::LegacyAdpcm)
            && matches!(self.adpcm_mode, AdpcmMode::Through)
        {
            let byte = self
                .pcm_output
                .raw_bytes
                .get(self.pcm_output.raw_position)
                .copied()
                .unwrap_or(0x80);
            self.pcm_output.raw_position = self.pcm_output.raw_position.saturating_add(1);
            byte
        } else {
            pcm_mixer::mix_and_encode_byte(
                &mut self.pcm_output.channels,
                &self.pcm_output.samples,
                &mut self.pcm_output.encoder,
                &mut self.pcm_output.filter,
            )
        };
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
}

/// Lazily drives an [`MdxPackage`] into VGM commands, one MDX tick at a
/// time, so conversion work happens only as a [`VgmStream`](crate::vgm::VgmStream)
/// or [`VgmCallbackStream`](crate::vgm::VgmCallbackStream) built from it is
/// actually iterated, rather than all at once up front. Construct one via
/// [`to_vgm_stream_generator`].
///
/// Does not retain previously produced commands: `builder` is reused as
/// per-tick scratch space and drained via [`VgmBuilder::take_commands`]
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
    pending: VecDeque<crate::vgm::command::VgmCommand>,
    initialized: bool,
    finished: bool,
}

impl<P: Borrow<MdxPackage>> MdxVgmGenerator<P> {
    /// `mark_native_loop` is `true` for the eager [`to_vgm_document`] path
    /// (which needs a fixed native VGM loop point) and `false` for the
    /// streaming [`to_vgm_stream_generator`] path (which just keeps
    /// repeating indefinitely); see [`SongLoopState::mark_native_loop`].
    fn new(
        package: P,
        options: MdxToVgmOptions,
        mark_native_loop: bool,
    ) -> Result<Self, MdxConvertError> {
        options.validate()?;
        let pcm_mode = MdxPcmMode::from_track_count(package.borrow().mdx.header.track_count());
        let playback = PlaybackState::new(
            package,
            pcm_mode,
            options.adpcm_mode,
            options.loop_count,
            mark_native_loop,
        );
        Ok(Self {
            playback,
            options,
            builder: VgmBuilder::new(),
            pending: VecDeque::new(),
            initialized: false,
            finished: false,
        })
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

        match self.playback.step(&mut self.builder)? {
            StepOutcome::Finished => {
                self.playback.emit_closing_commands(&mut self.builder);
                // `VgmBuilder::finalize()` is never called in the lazy path (it
                // has no use for a complete `VgmDocument`), but `VgmStream`
                // still relies on an explicit `EndOfData` command to signal
                // end-of-stream/looping, so append one here to match what
                // `finalize()` would have guaranteed.
                self.builder
                    .add_vgm_command(crate::vgm::command::EndOfData {});
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
impl<P: Borrow<MdxPackage>> crate::vgm::stream::VgmCommandGenerator for MdxVgmGenerator<P> {
    fn next_command(
        &mut self,
    ) -> Result<Option<crate::vgm::command::VgmCommand>, crate::binutil::ParseError> {
        loop {
            if let Some(command) = self.pending.pop_front() {
                return Ok(Some(command));
            }
            if self.finished {
                return Ok(None);
            }
            self.run_step()
                .map_err(|e| crate::binutil::ParseError::Other(e.to_string()))?;
            self.pending.extend(self.builder.take_commands());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdx::command::MdxRest;
    use crate::mdx::document::MdxBuilder;

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
    fn pcm_key_on_and_live_volume_follow_embedded_fadeout() {
        let mut playback = playback_state(MdxPcmMode::Pcm8a, AdpcmMode::Through);
        playback.fadeout.level = 3;
        playback.tracks[8].fm.volume = 8;

        playback.begin_pcm_key_on(8, 0x80);

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

    #[test]
    fn legacy_adpcm_through_does_not_apply_fadeout_to_pcm_gain() {
        let mut playback = playback_state(MdxPcmMode::LegacyAdpcm, AdpcmMode::Through);
        playback.fadeout.seen = true;
        playback.fadeout.level = 2;
        playback.fadeout.counter = -1;
        playback.tracks[8].fm.volume = 8;

        playback.begin_pcm_key_on(8, 0x80);
        playback.advance_fadeout(&mut VgmBuilder::new());

        assert_eq!(playback.fadeout.level, 3);
        assert_eq!(playback.pcm_output.channels[0].gain, 16);
    }
}
