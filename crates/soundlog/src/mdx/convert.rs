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
    MdxCommand, MdxExtended2Command, MdxExtendedCommand, MdxOpmLfo, MdxPitchLfo, MdxVolumeLfo,
};
use crate::mdx::package::MdxPackage;
use crate::mdx::pcm::{AdpcmEncoder, Pcm8aFormat, decode_pcm8a};
use crate::mdx::pcm_mixer::{self, PcmChannelState};
use crate::mdx::tone::MdxTone;
use crate::vgm::command::{Instance, WaitSamples};
use crate::vgm::{VgmBuilder, VgmDocument};
use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;

/// Number of microseconds in one second, used as the fixed-point time scale
/// for sample and PCM-byte remainder calculations.
const MICROSECONDS_PER_SECOND: u32 = 1_000_000;
/// MXDRV tempo used when an MDX stream has not issued a tempo command yet.
const DEFAULT_TEMPO: u8 = 200;
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

impl MdxPcmMode {
    fn from_track_count(track_count: usize) -> Self {
        if track_count == 16 {
            Self::Pcm8a
        } else {
            Self::LegacyAdpcm
        }
    }
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
    /// VGM output sample rate in Hz.
    pub sample_rate: u32,
    /// Total number of playthroughs of the whole song's repeat.
    ///
    /// This only affects the song-level repeat: a backward `Jump`, or a
    /// `LoopEnd` whose count is encoded as `0` (the file's "loop forever"
    /// marker). It does not override the counts of ordinary nested repeat
    /// blocks (`LoopStart`/`LoopEnd` with a nonzero count), since those are
    /// authored per track and overriding them independently across tracks
    /// would desynchronize them from each other.
    ///
    /// `None` (the default) preserves the file's own "loop forever" intent
    /// by encoding a native VGM loop point instead of repeating internally.
    /// `Some(1)` plays the song once with no repeat.
    pub loop_count: Option<u32>,
    /// Enables compatibility handling for MXDRV16y-style files, which encode
    /// the true FM channel via raw `0xFE` register-`0x08` writes instead of
    /// trusting the track index, and which may contain a zero-length
    /// "infinite loop" placeholder that must be escaped via its own offset
    /// rather than treated as the song's repeat point.
    ///
    /// This does not attempt to auto-detect such files (unlike the
    /// reference, which infers it from the tone bank layout); callers must
    /// opt in explicitly.
    pub mxdrv16y: bool,
}

impl Default for MdxToVgmOptions {
    fn default() -> Self {
        Self {
            ym2151_clock: 4_000_000,
            okim6258_clock: pcm_mixer::PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ,
            sample_rate: 44_100,
            loop_count: None,
            mxdrv16y: false,
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
        if self.sample_rate == 0 {
            return Err(MdxConvertError::InvalidOptions(
                "sample rate must not be zero",
            ));
        }
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
///     mdx: MdxBuilder::new().finalize(),
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
        .set_sample_rate(options.sample_rate)
        .register_chip(Chip::Ym2151, Instance::Primary, options.ym2151_clock);

    while generator.run_step()? {}
    Ok(generator.playback.finalize_with_pcm(generator.builder))
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
    /// MXDRV16y trap: escape the empty infinite loop via its own offset.
    Escape,
    /// No active loop; continue past this command without jumping.
    Fallthrough,
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
    /// Whether an FM note is currently keyed on.
    key_on: bool,
    /// Currently selected FM voice number, or the PCM bank for PCM tracks.
    voice: u8,
    /// Set once a voice-select command has run at least once, mirroring
    /// the reference's `voiceNo != 0xff` check used by MXDRV16y channel
    /// remapping to decide whether a voice needs reapplying.
    voice_selected: bool,
    /// Set by a voice-select command; applying the tone (and register
    /// `0x20`'s CON/FL bits, via `con_fl`) is deferred to the next key-on,
    /// mirroring `_applyPendingFmState`'s "voice update pending" flag.
    voice_pending: bool,
    /// Set by a pan command; writing register `0x20` is deferred to the
    /// next key-on, mirroring `_applyPendingFmState`'s "pan update
    /// pending" flag. Note that register `0x20` (pan combined with CON/FL)
    /// is therefore never written for a track that has no pan command.
    pan_pending: bool,
    /// CON/FL of the currently selected tone (`con | fl << 3`), used for
    /// register `0x20` and to resolve the key-on slot fallback.
    con_fl: u8,
    /// Raw key-on slot mask (tone `op` byte, shifted) combined with the FM
    /// channel. A zero mask (top 5 bits clear) falls back to the
    /// algorithm's default carrier slots, resolved at key-on time.
    key_on_slot: u8,
    /// Physical YM2151 channel currently used by this track.
    fm_channel: u8,
    /// FM pan bits written to YM2151 register `0x20`.
    pan: u8,
    /// Current MDX volume value, using the MDX signed attenuation encoding.
    volume: u8,
    /// Gate ratio or signed gate adjustment used to calculate key-off timing.
    gate: i8,
    /// Whether the next note should preserve the current sound instead of
    /// scheduling a key-off.
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
    /// Last pitch actually written to the OPM registers, used to avoid
    /// redundant writes (mirrors `writePitchIfChanged` in the reference).
    last_written_pitch: Option<u16>,
    /// Accumulated pitch-bend offset in the fixed-point representation used
    /// by the playback simulation.
    bend_offset: i32,
    /// Per-tick pitch-bend increment from the portamento command.
    bend_delta: i32,
    /// Whether portamento applies this tick; cleared before each new batch
    /// of commands is processed, so it only affects the note it precedes.
    portamento_active: bool,
    /// Whether this track is blocked at a synchronization wait.
    sync_wait: bool,
    /// Nested loop stack containing remaining counts and command indices.
    loop_stack: Vec<(u32, usize)>,
    /// Per-channel PMS/AMS sensitivity, applied to register `0x38+ch`.
    pms_ams: u8,
    /// Whether the OPM's hardware LFO waveform position is reset on the
    /// next key-on (register `0x01` LFO_RESET pulse).
    opm_lfo_reset_pending: bool,
    /// Number of ticks to delay pitch and volume LFO activation after key-on.
    lfo_delay: u8,
    /// Remaining ticks in the current LFO delay.
    lfo_delay_counter: u8,
    /// Whether the pitch LFO is enabled.
    pitch_lfo_enabled: bool,
    /// Selected pitch LFO waveform and mode.
    pitch_lfo_type: u8,
    /// Configured pitch LFO period in ticks.
    pitch_lfo_length: u16,
    /// Effective pitch LFO period after waveform-specific adjustment.
    pitch_lfo_length_cooked: u16,
    /// Remaining ticks in the current pitch LFO period.
    pitch_lfo_length_counter: u16,
    /// Initial pitch LFO step in fixed-point units.
    pitch_lfo_delta_start: i32,
    /// Current pitch LFO step in fixed-point units.
    pitch_lfo_delta: i32,
    /// Initial pitch LFO offset in fixed-point units.
    pitch_lfo_offset_start: i32,
    /// Current pitch LFO offset in fixed-point units.
    pitch_lfo_offset: i32,
    /// Whether the volume LFO is enabled.
    volume_lfo_enabled: bool,
    /// Selected volume LFO waveform and mode.
    volume_lfo_type: u8,
    /// Configured volume LFO period in ticks.
    volume_lfo_length: u16,
    /// Remaining ticks in the current volume LFO period.
    volume_lfo_length_counter: u16,
    /// Initial volume LFO step.
    volume_lfo_delta_start: u16,
    /// Current volume LFO step.
    volume_lfo_delta: u16,
    /// Waveform-adjusted volume LFO step used by the update logic.
    volume_lfo_delta_cooked: u16,
    /// Current volume LFO attenuation offset.
    volume_lfo_offset: u16,
    /// PDX bank selector for tracks >= 8, set by `0xfd`.
    pcm_bank: u8,
    /// Q16.16 resampler rate step for tracks >= 8, set by `0xed` on a
    /// PCM8A track (`0x10000` is 1.0x playback speed).
    pcm_rate_step: u32,
    /// Sample data format selected by the last `0xed` on a PCM8A track.
    pcm_data_kind: Pcm8aFormat,
    /// Raw ADPCM pan value (0=center, 1=left, 2=right, 3=mute), routed to
    /// the NJU72342 amplifier in the reference rather than a chip register.
    pcm_pan: u8,
}

struct PlaybackState<P: Borrow<MdxPackage>> {
    /// MDX/PDX package being consumed by the playback simulation.
    package: P,
    /// MDX PCM command semantics selected from the header's track layout.
    pcm_mode: MdxPcmMode,
    /// Per-track command cursors and playback state for the MDX tracks.
    tracks: Vec<TrackState>,
    /// True for files with PCM8/PCM8A tracks (>= 9 MDX tracks); gates all
    /// PCM8 mixer/VGM-stream output.
    has_pcm: bool,
    /// Per-channel ADPCM/PCM playback state for tracks 8-15.
    pcm_channels: [PcmChannelState; 8],
    /// Persistent re-encoder state for the whole song's mixed PCM8 output.
    pcm_encoder: AdpcmEncoder,
    /// Q at `MICROSECONDS_PER_SECOND` scale, tracking fractional OKIM6258
    /// data-register writes owed across tick boundaries (mirrors
    /// `sample_remainder` but driven by `PCM8_STREAM_BYTE_RATE_HZ` instead
    /// of the VGM sample rate).
    pcm_output_remainder: u32,
    /// Current MDX tempo, used to derive the duration of one playback tick.
    tempo: u8,
    /// Fractional VGM samples owed after converting elapsed tick time at the
    /// configured output sample rate, scaled by `MICROSECONDS_PER_SECOND`.
    sample_remainder: u32,
    /// Optional finite repeat limit for an unconditional whole-song repeat.
    loop_count: Option<u32>,
    /// Shadow of OPM register `0x0f` (noise enable + frequency), needed to
    /// preserve the noise-enable bit when only the frequency is updated.
    opm_reg_0f: u8,
    /// Shadow of OPM register `0x1b` (CT1/CT2 + LFO waveform), needed to
    /// preserve the CT bits when only the LFO waveform is updated.
    opm_reg_1b: u8,
    /// Shared LFO random generator seed (mirrors the single global PRNG
    /// used by all tracks in the reference implementation).
    lfo_rand_seed: u16,
    /// VGM command index recorded the first time an unconditional repeat
    /// (infinite `LoopEnd`, or a backward whole-song `Jump`) reaches a given
    /// `(track, target_command_index)`, keyed by that pair.
    song_loop_starts: HashMap<(usize, usize), usize>,
    /// How many times a `Jump`-based repeat has been taken so far, keyed by
    /// the jump command's own `(track, command_index)`. Only used when
    /// `loop_count` overrides the (otherwise unconditional) repeat.
    jump_repeat_counts: HashMap<(usize, usize), u32>,
    /// VGM command index to use as the native loop point, once an
    /// unconditional repeat has been seen a second time.
    song_loop_index: Option<usize>,
    /// Set once a native loop point has been established; conversion stops
    /// here instead of looping the repeat internally forever.
    song_loop_complete: bool,
    /// Enables MXDRV16y compatibility handling; see `MdxToVgmOptions::mxdrv16y`.
    mxdrv16y: bool,
    /// Whether an unconditional ("loop forever") repeat should stop and
    /// record a fixed native VGM loop point (`true`, needed by
    /// [`to_vgm_document`] to produce a finite `VgmDocument` with a valid
    /// loop header) or simply keep repeating indefinitely, producing new
    /// commands for each pass without ever finishing (`false`, used by the
    /// streaming [`MdxVgmGenerator`] path, which never retains enough
    /// history to rewind to a remembered position anyway).
    mark_native_loop: bool,
}

impl<P: Borrow<MdxPackage>> PlaybackState<P> {
    fn new(
        package: P,
        pcm_mode: MdxPcmMode,
        loop_count: Option<u32>,
        mxdrv16y: bool,
        mark_native_loop: bool,
    ) -> Self {
        let has_pcm = package.borrow().drives_okim6258();
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
                key_on: false,
                voice: 0,
                voice_selected: false,
                voice_pending: false,
                pan_pending: false,
                con_fl: 0,
                key_on_slot: 0,
                fm_channel: track as u8,
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
                sync_wait: false,
                loop_stack: Vec::new(),
                pms_ams: 0,
                opm_lfo_reset_pending: false,
                lfo_delay: 0,
                lfo_delay_counter: 0,
                pitch_lfo_enabled: false,
                pitch_lfo_type: 0,
                pitch_lfo_length: 0,
                pitch_lfo_length_cooked: 0,
                pitch_lfo_length_counter: 0,
                pitch_lfo_delta_start: 0,
                pitch_lfo_delta: 0,
                pitch_lfo_offset_start: 0,
                pitch_lfo_offset: 0,
                volume_lfo_enabled: false,
                volume_lfo_type: 0,
                volume_lfo_length: 0,
                volume_lfo_length_counter: 0,
                volume_lfo_delta_start: 0,
                volume_lfo_delta: 0,
                volume_lfo_delta_cooked: 0,
                volume_lfo_offset: 0,
                pcm_bank: 0,
                pcm_rate_step: 0x10000,
                pcm_data_kind: Pcm8aFormat::Adpcm,
                pcm_pan: 0,
            })
            .collect();
        Self {
            package,
            pcm_mode,
            tracks,
            has_pcm,
            pcm_channels: Default::default(),
            pcm_encoder: AdpcmEncoder::default(),
            pcm_output_remainder: 0,
            tempo: DEFAULT_TEMPO,
            sample_remainder: 0,
            loop_count,
            opm_reg_0f: 0,
            opm_reg_1b: 0,
            lfo_rand_seed: 0x1234,
            song_loop_starts: HashMap::new(),
            jump_repeat_counts: HashMap::new(),
            song_loop_index: None,
            song_loop_complete: false,
            mxdrv16y,
            mark_native_loop,
        }
    }

    /// Checks if the playback has finished, either due to the song loop being complete
    /// or all tracks being inactive.
    fn finished(&self) -> bool {
        self.song_loop_complete || self.tracks.iter().all(|track| !track.active)
    }

    /// Runs one tick of playback, appending any resulting commands to
    /// `builder`. Shared by the eager [`to_vgm_document`] loop and
    /// [`MdxVgmGenerator`], which drives the same steps lazily.
    fn step(
        &mut self,
        builder: &mut VgmBuilder,
        sample_rate: u32,
    ) -> Result<StepOutcome, MdxConvertError> {
        if self.finished() {
            return Ok(StepOutcome::Finished);
        }
        self.process_tick(builder)?;
        if self.finished() {
            return Ok(StepOutcome::Finished);
        }
        self.emit_wait(builder, sample_rate);
        Ok(StepOutcome::Continue)
    }

    /// Finalizes the document, applying the native VGM loop point detected
    /// from an unconditional repeat, if any (see `take_repeating_jump`).
    fn finalize(&self, mut builder: VgmBuilder) -> VgmDocument {
        if let Some(loop_index) = self.song_loop_index {
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
        for track_index in 0..self.tracks.len() {
            if !self.tracks[track_index].active {
                continue;
            }
            if track_index < 8 {
                self.update_fm_tick(track_index, builder);
            }
            self.process_key_off(track_index, builder);
            self.process_key_on_delay(track_index, builder)?;
            if self.tracks[track_index].wait_ticks > 0 {
                self.tracks[track_index].wait_ticks -= 1;
            }
            // Sync-wait only blocks new command processing; envelope, pitch
            // and LFO updates above still run while a track waits.
            if self.tracks[track_index].sync_wait {
                continue;
            }
            if self.tracks[track_index].wait_ticks > 0 {
                continue;
            }
            // Portamento only affects the note it immediately precedes.
            self.tracks[track_index].portamento_active = false;
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
            if self.tracks[track].key_on {
                write_ym2151(builder, 0x08, self.tracks[track].fm_channel);
                self.tracks[track].key_on = false;
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
        let hold = self.tracks[track].key_off_disabled;
        let state = &mut self.pcm_channels[channel];
        if hold {
            state.hold = true;
        } else {
            state.block = None;
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
        let bank = usize::from(self.tracks[track].pcm_bank);
        let note_index = usize::from(note_index);
        let format = self.tracks[track].pcm_data_kind;
        let tie = self.tracks[track].key_off_disabled;
        let channel = track - 8;

        let format_key = match format {
            Pcm8aFormat::Adpcm => 0u8,
            Pcm8aFormat::Pcm16 => 1u8,
            Pcm8aFormat::Pcm8 => 2u8,
        };
        let block_key = (bank, note_index, format_key);
        let rate_step = self.tracks[track].pcm_rate_step;
        let gain = pcm_mixer::pcm8_gain(self.tracks[track].volume);
        let same_block = tie && self.pcm_channels[channel].block_key == Some(block_key);
        if !same_block {
            let samples = self.decode_pcm_samples(bank, note_index, format);
            let state = &mut self.pcm_channels[channel];
            state.block = samples;
            state.block_key = state.block.as_ref().map(|_| block_key);
            state.pos_in_block = 0;
            state.rate_counter = 0;
        }
        let state = &mut self.pcm_channels[channel];
        state.rate_step = rate_step;
        state.gain = gain;
        state.hold = false;
    }

    /// Applies a live volume change to a currently-playing ADPCM/PCM
    /// channel (`track` >= 8), mirroring `pcm8SetVolume`'s behavior of
    /// updating a channel's gain immediately rather than only at the next
    /// key-on.
    fn apply_live_pcm_gain(&mut self, track: usize) {
        let gain = pcm_mixer::pcm8_gain(self.tracks[track].volume);
        self.pcm_channels[track - 8].gain = gain;
    }

    /// Decodes one PDX sample for the channel that is starting playback.
    /// The decoded block is owned only by that channel; there is no song-wide
    /// sample cache, so memory is bounded by the eight active channels.
    fn decode_pcm_samples(
        &mut self,
        bank: usize,
        note: usize,
        format: Pcm8aFormat,
    ) -> Option<Box<[i16]>> {
        let bytes = self
            .package
            .borrow()
            .pdx
            .as_ref()?
            .sample_bytes(bank, note)?;
        let decoded = decode_pcm8a(format, bytes).ok()?;
        Some(decoded.into_boxed_slice())
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
            match command {
                MdxCommand::Rest(command) => {
                    self.tracks[track].wait_ticks = command.ticks;
                    // Rest always (re)starts the key-off countdown and
                    // cancels any pending tie, regardless of prior state.
                    self.tracks[track].key_off_ticks = command.ticks;
                    self.tracks[track].key_off_disabled = false;
                }
                MdxCommand::Note(command) if track < 8 => {
                    let note = i32::from(command.note - 0x80) + self.tracks[track].transpose;
                    let note = note.clamp(0, 127) as u16;
                    let pitch = (note << 6)
                        .saturating_add(5)
                        .saturating_add_signed(self.tracks[track].detune);
                    self.tracks[track].note_pitch = Some(pitch);
                    self.write_pitch(track, builder, pitch);
                    if self.tracks[track].key_on_delay == 0 {
                        self.begin_key_on(track, builder)?;
                    } else {
                        self.tracks[track].key_on_delay_counter = self.tracks[track].key_on_delay;
                        self.tracks[track].key_on_pending = true;
                    }
                    self.tracks[track].wait_ticks = command.length;
                    if !self.tracks[track].key_off_disabled {
                        let gate = i16::from(self.tracks[track].gate);
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
                    self.tracks[track].key_off_disabled = false;
                }
                MdxCommand::Note(command) => {
                    // PCM key-on: `note` is an 0x80-based index into the
                    // track's current PDX bank rather than a pitch.
                    self.begin_pcm_key_on(track, command.note);
                    self.tracks[track].wait_ticks = command.length;
                    if !self.tracks[track].key_off_disabled {
                        let gate = i16::from(self.tracks[track].gate);
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
                    self.tracks[track].key_off_disabled = false;
                }
                MdxCommand::Tempo(command) => {
                    self.tempo = command.value.max(1);
                }
                MdxCommand::OpmRegisterWrite(command) => {
                    if command.register == 0x0f {
                        self.opm_reg_0f = command.value;
                    } else if command.register == 0x1b {
                        self.opm_reg_1b = command.value;
                    }
                    if self.mxdrv16y && track < 8 && command.register == 0x08 {
                        // Some MXDRV16y files encode the real FM channel via
                        // a raw key-on register write rather than trusting
                        // the track index; reapply the voice to the new
                        // channel if one was already selected.
                        let new_channel = command.value & 0x07;
                        if new_channel != self.tracks[track].fm_channel {
                            self.tracks[track].fm_channel = new_channel;
                            if self.tracks[track].voice_selected {
                                self.tracks[track].voice_pending = true;
                            }
                        }
                    }
                    write_ym2151(builder, command.register, command.value);
                }
                MdxCommand::VoiceOrPcmBank(command) if track < 8 => {
                    self.tracks[track].voice = command.value;
                    self.tracks[track].voice_selected = true;
                    self.tracks[track].voice_pending = true;
                }
                MdxCommand::VoiceOrPcmBank(command) => {
                    self.tracks[track].pcm_bank = command.value;
                }
                MdxCommand::EndOfTrack(_) => {
                    // The reference clears the key-on flag without sending
                    // an explicit key-off; the note's own gate/key-off
                    // countdown is expected to have already released it.
                    self.tracks[track].active = false;
                    self.tracks[track].key_on = false;
                }
                MdxCommand::Pan(command) if track < 8 => {
                    self.tracks[track].pan = match command.value {
                        1 => 0x40,
                        2 => 0x80,
                        3 => 0xc0,
                        _ => 0,
                    };
                    self.tracks[track].pan_pending = true;
                }
                MdxCommand::Pan(command) => {
                    // libvgm uses OKIM6258 register 0x02 for the X68000 pan
                    // extension (0=center, 1=left, 2=right, 3=mute).
                    let pan = command.value & 0x03;
                    self.tracks[track].pcm_pan = pan;
                    builder.add_vgm_command((
                        Instance::Primary,
                        Okim6258Spec {
                            register: 0x02,
                            value: pan,
                        },
                    ));
                }
                MdxCommand::Volume(command) if track < 8 => {
                    self.tracks[track].volume = command.value;
                    self.emit_volume(track, builder);
                }
                MdxCommand::Volume(command) => {
                    self.tracks[track].volume = command.value;
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
                MdxCommand::Gate(command) => self.tracks[track].gate = command.value as i8,
                MdxCommand::KeyOffDisable(_) => self.tracks[track].key_off_disabled = true,
                MdxCommand::KeyOnDelay(command) => {
                    self.tracks[track].key_on_delay = command.value;
                    self.tracks[track].key_on_delay_counter = 0;
                    self.tracks[track].key_on_pending = false;
                }
                MdxCommand::Detune(command) => self.tracks[track].detune = command.offset,
                MdxCommand::Portamento(command) => {
                    self.tracks[track].bend_delta = i32::from(command.offset) << 8;
                    self.tracks[track].portamento_active = true;
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
                        self.tracks[track].pcm_rate_step = rate_step;
                        self.tracks[track].pcm_data_kind = data_kind;
                    }
                }
                MdxCommand::OpmLfo(command) => self.apply_opm_lfo(track, command, builder),
                MdxCommand::PitchLfo(command) => self.apply_pitch_lfo(track, command),
                MdxCommand::VolumeLfo(command) => self.apply_volume_lfo(track, command),
                MdxCommand::LfoDelay(command) => self.tracks[track].lfo_delay = command.value,
                MdxCommand::PcmMode(_) => {}
                MdxCommand::Extended(command) => match command {
                    // None of the E7 sub-commands other than PCM8 direct
                    // drive (deferred) perform any FM action in the
                    // reference implementation; they only consume bytes.
                    MdxExtendedCommand::Fadeout { .. }
                    | MdxExtendedCommand::Pcm8DirectDrive { .. }
                    | MdxExtendedCommand::KeyOff { .. }
                    | MdxExtendedCommand::ChannelControl { .. }
                    | MdxExtendedCommand::AddNoteLength { .. }
                    | MdxExtendedCommand::SetFlag { .. }
                    | MdxExtendedCommand::Error
                    | MdxExtendedCommand::Unknown(_) => {}
                },
                MdxCommand::Extended2(command) => match command {
                    MdxExtended2Command::Transpose { value } => {
                        self.tracks[track].transpose = i32::from(value)
                    }
                    MdxExtended2Command::RelativeTranspose { value } => {
                        let next = self.tracks[track].transpose + i32::from(value);
                        self.tracks[track].transpose = next.clamp(-127, 127);
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
                MdxCommand::LoopEnd(command) => {
                    // The jump target is the position saved at `LoopStart`,
                    // not recomputed from this command's own offset
                    // (mirrors the reference's `pc = loopStack[sp]`, which
                    // stays correct even if a malformed file's encoded
                    // offset does not actually point back to the loop
                    // start).
                    let this_command_index = self.tracks[track].command_index - 1;
                    let mxdrv16y = self.mxdrv16y;
                    // `remaining == 0` is the file's "loop forever" marker.
                    // It can only be reached when `loop_count` is `None`,
                    // since `LoopStart` otherwise overrides it with a finite
                    // count. Resolved via a native VGM loop point rather
                    // than looping forever internally.
                    let action = match self.tracks[track].loop_stack.last_mut() {
                        Some((remaining, start_index)) if *remaining == 0 => {
                            // MXDRV16y "DD1_00" trap: `F6 00 00` immediately
                            // followed by `F5` (an empty, unconditionally
                            // "infinite" loop body) is a placeholder, not a
                            // real repeat point. Escape via the F5's own
                            // offset instead of looping on the spot.
                            if mxdrv16y && *start_index == this_command_index {
                                LoopEndAction::Escape
                            } else {
                                LoopEndAction::SongLoop(*start_index)
                            }
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
                        LoopEndAction::Escape => self.jump_relative(track, command.offset),
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
        let voice = self.tracks[track].voice;
        let tone = self
            .package
            .borrow()
            .mdx
            .tone_bank
            .tones
            .iter()
            .find(|tone| tone.voice_number == voice)
            .ok_or(MdxConvertError::MissingTone { voice })?;
        let fm_channel = self.tracks[track].fm_channel;
        self.tracks[track].con_fl = tone.con | (tone.fl << 3);
        // Loading a voice always re-arms the pan-pending flag too (even
        // without a new Pan command), so register 0x20's CON/FL bits get
        // refreshed for the new algorithm at the next key-on. Mirrors
        // `_setVoice`'s unconditional `flags |= 0x04`.
        self.tracks[track].pan_pending = true;
        // Store the tone's own key-on slot mask combined with the channel;
        // a zero mask falls back to the algorithm default at key-on time.
        self.tracks[track].key_on_slot = ((tone.op & 0x0f) << 3) | fm_channel;
        emit_tone(builder, fm_channel, tone);
        self.emit_volume(track, builder);
        Ok(())
    }

    /// Resolves the key-on slot mask, falling back to the algorithm's
    /// default carrier slots when the tone did not specify its own mask.
    fn resolved_key_on_slot(&self, track: usize) -> u8 {
        let state = &self.tracks[track];
        if state.key_on_slot & 0xf8 == 0 {
            CARRIER_KEYON_SLOTS[(state.con_fl & 0x07) as usize] | state.fm_channel
        } else {
            state.key_on_slot
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
        if self.tracks[track].voice_pending {
            self.emit_voice(track, builder)?;
            self.tracks[track].voice_pending = false;
        }
        if self.tracks[track].pan_pending {
            let fm_channel = self.tracks[track].fm_channel;
            let value = self.tracks[track].pan | (self.tracks[track].con_fl & 0x3f);
            write_ym2151(builder, 0x20 + fm_channel, value);
            self.tracks[track].pan_pending = false;
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
        let already_on = self.tracks[track].key_on;
        if !already_on && self.tracks[track].lfo_delay > 0 {
            self.start_lfo_delay(track);
        }
        self.tracks[track].bend_offset = 0;
        if !already_on {
            self.reset_opm_lfo_if_needed(track, builder);
            let slot = self.resolved_key_on_slot(track);
            write_ym2151(builder, 0x08, slot);
            self.tracks[track].key_on = true;
        }
        self.tracks[track].key_on_pending = false;
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
        if self.tracks[track].key_on_delay_counter == 0 || !self.tracks[track].key_on_pending {
            return Ok(());
        }
        self.tracks[track].key_on_delay_counter -= 1;
        if self.tracks[track].key_on_delay_counter == 0 {
            self.begin_key_on(track, builder)?;
        }
        Ok(())
    }

    /// Per-tick FM modulation: pitch bend accumulation, pitch/volume LFO
    /// updates (gated by LFO delay), and the resulting register writes.
    fn update_fm_tick(&mut self, track: usize, builder: &mut VgmBuilder) {
        let prev_volume_lfo_offset = self.tracks[track].volume_lfo_offset;
        // Updates the FM state for the current tick, including pitch bend
        // accumulation, LFO updates (if the LFO delay has elapsed), and
        // register writes for pitch and volume changes.
        if self.tracks[track].portamento_active && self.tracks[track].key_on_delay_counter == 0 {
            self.tracks[track].bend_offset = self.tracks[track]
                .bend_offset
                .wrapping_add(self.tracks[track].bend_delta);
        }
        // Determines whether the LFO updates should be skipped for this tick based on
        // the LFO delay and key-on delay counters.
        let mut skip_lfo = false;
        if self.tracks[track].lfo_delay > 0 {
            if self.tracks[track].key_on_delay_counter != 0 {
                skip_lfo = true;
            } else if self.tracks[track].lfo_delay_counter > 0 {
                self.tracks[track].lfo_delay_counter -= 1;
                if self.tracks[track].lfo_delay_counter == 0 {
                    if self.tracks[track].pitch_lfo_enabled {
                        self.reset_pitch_lfo(track);
                    }
                    if self.tracks[track].volume_lfo_enabled {
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
        if self.tracks[track].note_pitch.is_some() {
            self.update_pitch(track, builder);
        }
        // Emits the volume register write if the volume LFO offset has changed.
        if self.tracks[track].volume_lfo_offset != prev_volume_lfo_offset {
            self.emit_volume(track, builder);
        }
    }

    /// Writes the pitch registers only if the computed pitch changed
    /// (mirrors `writePitchIfChanged` in the reference).
    fn update_pitch(&mut self, track: usize, builder: &mut VgmBuilder) {
        let Some(note_pitch) = self.tracks[track].note_pitch else {
            return;
        };
        let bend = self.tracks[track].bend_offset >> 16;
        let lfo = self.tracks[track].pitch_lfo_offset >> 16;
        let pitch = i32::from(note_pitch)
            .saturating_add(bend)
            .saturating_add(lfo)
            .clamp(0, 0x17ff) as u16;
        if self.tracks[track].last_written_pitch == Some(pitch) {
            return;
        }
        self.write_pitch(track, builder, pitch);
    }

    /// Writes the computed pitch to the YM2151 registers for the specified track.
    /// Updates the last written pitch to avoid redundant writes.
    fn write_pitch(&mut self, track: usize, builder: &mut VgmBuilder, pitch: u16) {
        let fm_channel = self.tracks[track].fm_channel;
        let pitch_register = pitch << 2;
        let key_fraction = pitch_register as u8;
        let key_code = YM2151_KEYCODE_TABLE[((pitch_register >> 8) & 0x7f) as usize];
        write_ym2151(builder, 0x30 + fm_channel, key_fraction);
        write_ym2151(builder, 0x28 + fm_channel, key_code);
        self.tracks[track].last_written_pitch = Some(pitch);
    }

    /// Decreases the volume of the specified track, taking into account the
    /// FM volume encoding. If the volume is in the lower 7 bits, it is
    /// decremented by 1 unless it is already at the minimum. If the volume
    /// is in the upper 7 bits (indicating attenuation), it is incremented
    /// by 1 unless it is already at the maximum.
    fn volume_down(&mut self, track: usize) {
        let volume = self.tracks[track].volume;
        self.tracks[track].volume = if volume & 0x80 == 0 {
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
        let volume = self.tracks[track].volume;
        self.tracks[track].volume = if volume & 0x80 == 0 {
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
            .find(|tone| tone.voice_number == self.tracks[track].voice)
        else {
            return;
        };
        let base_attenuation = if self.tracks[track].volume & 0x80 != 0 {
            u16::from(self.tracks[track].volume & 0x7f)
        } else {
            u16::from(FM_VOLUME_TABLE[self.tracks[track].volume.min(15) as usize])
        };
        let lfo_attenuation = self.tracks[track].volume_lfo_offset >> 8;
        let attenuation = base_attenuation + lfo_attenuation;
        let carrier_mask = CARRIER_TL_SLOTS[(tone.con & 0x07) as usize];
        let fm_channel = self.tracks[track].fm_channel;
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
                let fm_channel = self.tracks[track].fm_channel;
                let value = if enabled {
                    self.tracks[track].pms_ams
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
                self.tracks[track].opm_lfo_reset_pending = control & 0x40 != 0;
                // Preserve the CT1/CT2 bits (0xc0) already held in register
                // 0x1b; only the waveform/enable bits are updated here.
                let masked = (control & !0x40) | (self.opm_reg_1b & 0xc0);
                self.opm_reg_1b = masked;
                self.tracks[track].pms_ams = pms_ams;
                let fm_channel = self.tracks[track].fm_channel;
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
                    self.tracks[track].pitch_lfo_enabled = true;
                } else {
                    self.tracks[track].pitch_lfo_enabled = false;
                    self.tracks[track].pitch_lfo_offset = 0;
                }
            }
            MdxPitchLfo::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                self.tracks[track].pitch_lfo_enabled = true;
                let wave_type = waveform & 0x03;
                let mode = wave_type << 1;
                self.tracks[track].pitch_lfo_type = wave_type + 1;
                self.tracks[track].pitch_lfo_length = frequency;

                let mut cooked = frequency;
                if mode != 0x02 {
                    cooked >>= 1;
                    if mode == 0x06 {
                        cooked = 1;
                    }
                }
                self.tracks[track].pitch_lfo_length_cooked = cooked;

                let mut delta = i32::from(amplitude) << 8;
                let mut wave_check = waveform;
                if wave_check >= 0x04 {
                    delta <<= 8;
                    wave_check &= 0x03;
                }
                self.tracks[track].pitch_lfo_delta_start = delta;
                self.tracks[track].pitch_lfo_offset_start =
                    if wave_check == 0x02 { delta } else { 0 };

                self.tracks[track].pitch_lfo_length_counter =
                    self.tracks[track].pitch_lfo_length_cooked;
                self.tracks[track].pitch_lfo_delta = self.tracks[track].pitch_lfo_delta_start;
                self.tracks[track].pitch_lfo_offset = self.tracks[track].pitch_lfo_offset_start;
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
                    self.tracks[track].volume_lfo_enabled = true;
                } else {
                    self.tracks[track].volume_lfo_enabled = false;
                    self.tracks[track].volume_lfo_offset = 0;
                }
            }
            MdxVolumeLfo::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                self.tracks[track].volume_lfo_enabled = true;
                let mode = waveform << 1;
                self.tracks[track].volume_lfo_type = waveform + 1;
                self.tracks[track].volume_lfo_length = frequency;
                self.tracks[track].volume_lfo_delta_start = amplitude;

                let mut cooked = i32::from(amplitude as i16);
                if mode & 0x02 == 0 {
                    cooked = cooked.wrapping_mul(i32::from(frequency as i16));
                }
                cooked = cooked.wrapping_neg();
                if cooked < 0 {
                    cooked = 0;
                }
                self.tracks[track].volume_lfo_delta_cooked = cooked as u16;

                self.tracks[track].volume_lfo_length_counter = self.tracks[track].volume_lfo_length;
                self.tracks[track].volume_lfo_delta = self.tracks[track].volume_lfo_delta_start;
                self.tracks[track].volume_lfo_offset = self.tracks[track].volume_lfo_delta_cooked;
            }
        }
    }

    /// Resets the pitch LFO for the specified track to its initial state.
    fn reset_pitch_lfo(&mut self, track: usize) {
        self.tracks[track].pitch_lfo_length_counter = self.tracks[track].pitch_lfo_length_cooked;
        self.tracks[track].pitch_lfo_delta = self.tracks[track].pitch_lfo_delta_start;
        self.tracks[track].pitch_lfo_offset = self.tracks[track].pitch_lfo_offset_start;
    }

    /// Resets the volume LFO for the specified track to its initial state.
    fn reset_volume_lfo(&mut self, track: usize) {
        self.tracks[track].volume_lfo_length_counter = self.tracks[track].volume_lfo_length;
        self.tracks[track].volume_lfo_delta = self.tracks[track].volume_lfo_delta_start;
        self.tracks[track].volume_lfo_offset = self.tracks[track].volume_lfo_delta_cooked;
    }

    /// Starts the LFO delay for the specified track. Initializes the delay counter
    /// and resets the pitch and volume LFO offsets. If the delay counter reaches zero,
    /// the pitch and volume LFOs are reset immediately.
    fn start_lfo_delay(&mut self, track: usize) {
        self.tracks[track].lfo_delay_counter = self.tracks[track].lfo_delay;
        self.tracks[track].pitch_lfo_offset = 0;
        self.tracks[track].volume_lfo_offset = 0;
        self.tracks[track].lfo_delay_counter = self.tracks[track].lfo_delay_counter.wrapping_sub(1);
        if self.tracks[track].lfo_delay_counter == 0 {
            if self.tracks[track].pitch_lfo_enabled {
                self.reset_pitch_lfo(track);
            }
            if self.tracks[track].volume_lfo_enabled {
                self.reset_volume_lfo(track);
            }
        }
    }

    /// Resets the OPM LFO for the specified track if a reset is pending.
    /// Writes the necessary commands to the YM2151 registers to perform the reset.
    fn reset_opm_lfo_if_needed(&self, track: usize, builder: &mut VgmBuilder) {
        if !self.tracks[track].opm_lfo_reset_pending {
            return;
        }
        write_ym2151(builder, 0x01, 0x02);
        write_ym2151(builder, 0x01, 0x00);
    }

    /// Updates the pitch LFO for the specified track based on its type, delta, and length counter.
    /// Handles sawtooth, square, and triangle waveforms, updating the internal offset and
    /// length counter accordingly.
    fn update_pitch_lfo(&mut self, track: usize) {
        if !self.tracks[track].pitch_lfo_enabled || self.tracks[track].pitch_lfo_type == 0 {
            return;
        }
        match self.tracks[track].pitch_lfo_type {
            1 => {
                // Sawtooth: ramp, then flip sign at the end of each period.
                self.tracks[track].pitch_lfo_offset = self.tracks[track]
                    .pitch_lfo_offset
                    .wrapping_add(self.tracks[track].pitch_lfo_delta);
                self.tracks[track].pitch_lfo_length_counter =
                    self.tracks[track].pitch_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].pitch_lfo_length_counter == 0 {
                    self.tracks[track].pitch_lfo_length_counter =
                        self.tracks[track].pitch_lfo_length;
                    self.tracks[track].pitch_lfo_offset =
                        self.tracks[track].pitch_lfo_offset.wrapping_neg();
                }
            }
            2 => {
                // Square: hold at delta, flip sign at the end of each period.
                self.tracks[track].pitch_lfo_offset = self.tracks[track].pitch_lfo_delta;
                self.tracks[track].pitch_lfo_length_counter =
                    self.tracks[track].pitch_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].pitch_lfo_length_counter == 0 {
                    self.tracks[track].pitch_lfo_length_counter =
                        self.tracks[track].pitch_lfo_length;
                    self.tracks[track].pitch_lfo_delta =
                        self.tracks[track].pitch_lfo_delta.wrapping_neg();
                }
            }
            3 => {
                // Triangle: ramp continuously, flip sign at each period end.
                self.tracks[track].pitch_lfo_offset = self.tracks[track]
                    .pitch_lfo_offset
                    .wrapping_add(self.tracks[track].pitch_lfo_delta);
                self.tracks[track].pitch_lfo_length_counter =
                    self.tracks[track].pitch_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].pitch_lfo_length_counter == 0 {
                    self.tracks[track].pitch_lfo_length_counter =
                        self.tracks[track].pitch_lfo_length;
                    self.tracks[track].pitch_lfo_delta =
                        self.tracks[track].pitch_lfo_delta.wrapping_neg();
                }
            }
            4 => {
                // Random: reload with a new random offset each period.
                self.tracks[track].pitch_lfo_length_counter =
                    self.tracks[track].pitch_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].pitch_lfo_length_counter == 0 {
                    let random = i32::from(self.next_lfo_rand() as i16);
                    self.tracks[track].pitch_lfo_offset =
                        random.wrapping_mul(self.tracks[track].pitch_lfo_delta);
                    self.tracks[track].pitch_lfo_length_counter =
                        self.tracks[track].pitch_lfo_length;
                }
            }
            _ => {}
        }
    }

    /// Updates the volume LFO for the specified track based on its type, delta, and length counter.
    /// Handles sawtooth, square, and triangle waveforms, updating the internal offset and
    /// length counter accordingly.
    fn update_volume_lfo(&mut self, track: usize) {
        if !self.tracks[track].volume_lfo_enabled || self.tracks[track].volume_lfo_type == 0 {
            return;
        }
        match self.tracks[track].volume_lfo_type {
            1 => {
                // Sawtooth: ramp, then reset to the cooked baseline.
                self.tracks[track].volume_lfo_offset = self.tracks[track]
                    .volume_lfo_offset
                    .wrapping_add(self.tracks[track].volume_lfo_delta);
                self.tracks[track].volume_lfo_length_counter =
                    self.tracks[track].volume_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].volume_lfo_length_counter == 0 {
                    self.tracks[track].volume_lfo_length_counter =
                        self.tracks[track].volume_lfo_length;
                    self.tracks[track].volume_lfo_offset =
                        self.tracks[track].volume_lfo_delta_cooked;
                }
            }
            2 => {
                // Square: step at each period end, then flip sign.
                self.tracks[track].volume_lfo_length_counter =
                    self.tracks[track].volume_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].volume_lfo_length_counter == 0 {
                    self.tracks[track].volume_lfo_length_counter =
                        self.tracks[track].volume_lfo_length;
                    self.tracks[track].volume_lfo_offset = self.tracks[track]
                        .volume_lfo_offset
                        .wrapping_add(self.tracks[track].volume_lfo_delta);
                    self.tracks[track].volume_lfo_delta =
                        self.tracks[track].volume_lfo_delta.wrapping_neg();
                }
            }
            3 => {
                // Triangle: ramp continuously, flip sign at each period end.
                self.tracks[track].volume_lfo_offset = self.tracks[track]
                    .volume_lfo_offset
                    .wrapping_add(self.tracks[track].volume_lfo_delta);
                self.tracks[track].volume_lfo_length_counter =
                    self.tracks[track].volume_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].volume_lfo_length_counter == 0 {
                    self.tracks[track].volume_lfo_length_counter =
                        self.tracks[track].volume_lfo_length;
                    self.tracks[track].volume_lfo_delta =
                        self.tracks[track].volume_lfo_delta.wrapping_neg();
                }
            }
            4 => {
                // Random: reload with a new random offset each period.
                self.tracks[track].volume_lfo_length_counter =
                    self.tracks[track].volume_lfo_length_counter.wrapping_sub(1);
                if self.tracks[track].volume_lfo_length_counter == 0 {
                    let random = i32::from(self.next_lfo_rand() as i16);
                    let delta = i32::from(self.tracks[track].volume_lfo_delta as i16);
                    self.tracks[track].volume_lfo_offset = random.wrapping_mul(delta) as u16;
                    self.tracks[track].volume_lfo_length_counter =
                        self.tracks[track].volume_lfo_length;
                }
            }
            _ => {}
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
        let target_offset = (current_offset as i64)
            .saturating_add(current_length as i64)
            .saturating_add(i64::from(offset));
        self.package.borrow().mdx.sourcemap()[track]
            .iter()
            .position(|(offset, _)| *offset as i64 == target_offset)
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
    fn take_repeating_jump_to(
        &mut self,
        track: usize,
        jump_command_index: usize,
        target_index: usize,
        builder: &mut VgmBuilder,
    ) {
        if let Some(limit) = self.loop_count {
            let count = self
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
        if !self.mark_native_loop {
            self.tracks[track].command_index = target_index;
            return;
        }
        let key = (track, target_index);
        if let Some(&loop_index) = self.song_loop_starts.get(&key) {
            self.song_loop_index.get_or_insert(loop_index);
            self.song_loop_complete = true;
            return;
        }
        self.song_loop_starts.insert(key, builder.command_count());
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
    fn emit_wait(&mut self, builder: &mut VgmBuilder, sample_rate: u32) {
        let tick_microseconds = self.tick_microseconds();
        let sample_accumulator = u64::from(self.sample_remainder)
            + u64::from(tick_microseconds) * u64::from(sample_rate);
        let samples = (sample_accumulator / u64::from(MICROSECONDS_PER_SECOND)) as u32;
        self.sample_remainder = (sample_accumulator % u64::from(MICROSECONDS_PER_SECOND)) as u32;

        if !self.has_pcm {
            Self::emit_wait_chunks(builder, samples);
            return;
        }

        let pcm_accumulator = u64::from(self.pcm_output_remainder)
            + u64::from(tick_microseconds) * u64::from(pcm_mixer::PCM8_STREAM_BYTE_RATE_HZ);
        let pcm_bytes_due = (pcm_accumulator / u64::from(MICROSECONDS_PER_SECOND)) as u32;
        self.pcm_output_remainder = (pcm_accumulator % u64::from(MICROSECONDS_PER_SECOND)) as u32;
        if pcm_bytes_due == 0 {
            Self::emit_wait_chunks(builder, samples);
            return;
        }

        // Spread the due OKIM6258 data writes evenly across this tick's
        // samples (splitting `samples` into `pcm_bytes_due` near-equal
        // segments) so each byte lands close to its real playback position
        // without resorting to a wait-1-sample-per-byte command stream.
        let mut remaining_samples = samples;
        let mut remaining_bytes = pcm_bytes_due;
        while remaining_bytes > 0 {
            let chunk = remaining_samples / remaining_bytes;
            Self::emit_wait_chunks(builder, chunk);
            remaining_samples -= chunk;
            remaining_bytes -= 1;
            self.emit_pcm_byte(builder);
        }
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
        256 * u32::from(256u16 - u16::from(self.tempo))
    }

    /// Mixes and re-encodes one ADPCM byte (2 samples) from the 8 PCM8
    /// channels and writes it directly to the OKIM6258 data register (1).
    fn emit_pcm_byte(&mut self, builder: &mut VgmBuilder) {
        let byte = pcm_mixer::mix_and_encode_byte(&mut self.pcm_channels, &mut self.pcm_encoder);
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
        if self.has_pcm && self.song_loop_index.is_none() {
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
/// song (see [`PlaybackState::mark_native_loop`], which is `false` for this
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
    /// repeating indefinitely); see [`PlaybackState::mark_native_loop`].
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
            options.loop_count,
            options.mxdrv16y,
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
            if self.playback.has_pcm {
                self.builder.register_chip(
                    Chip::Okim6258,
                    Instance::Primary,
                    self.options.okim6258_clock,
                );
            }
            self.playback.emit_initialization(&mut self.builder);
            if self.playback.has_pcm {
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
            .step(&mut self.builder, self.options.sample_rate)?
        {
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
