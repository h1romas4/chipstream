//! Conversion from the parsed MML AST to soundlog's typed MDX document.

use std::fmt;

use soundlog::mdx::command::{
    MdxAdpcmOrNoiseFrequency, MdxCommand, MdxEndOfTrack, MdxGate, MdxKeyOffDisable, MdxKeyOnDelay,
    MdxLfoDelay, MdxLfoWaveform, MdxLoopStart, MdxNote, MdxOpmLfo, MdxOpmRegisterWrite, MdxPan,
    MdxPitchLfo, MdxRelativeOffset, MdxRest, MdxSignedWord, MdxSyncSend, MdxTempo,
    MdxVoiceOrPcmBank, MdxVolume, MdxVolumeDown, MdxVolumeLfo, MdxVolumeUp,
};
use soundlog::mdx::document::{MdxBuilder, MdxDocument};
use soundlog::mdx::tone::{MdxOperator, MdxTone};

use super::mml::{Accidental, MmlCommand, MmlDocument, MmlLength, MmlVoice};

const TICKS_PER_WHOLE: u16 = 192;
const FIRST_NOTE: u16 = 0x80;
const LAST_NOTE: u16 = 0xdf;

/// An error raised while lowering MML AST nodes into MDX commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// A track uses a channel that cannot be represented in MDX.
    InvalidChannel(char),
    /// A voice does not contain the required 47 parameters.
    InvalidVoice { number: u8, parameter_count: usize },
    /// A parsed value cannot be represented by the target MDX type.
    InvalidValue { command: &'static str, value: i64 },
    /// The source command has no soundlog MDX equivalent.
    UnsupportedCommand(&'static str),
    /// The soundlog builder rejected the generated document.
    Builder(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannel(channel) => write!(formatter, "invalid MDX channel {channel:?}"),
            Self::InvalidVoice {
                number,
                parameter_count,
            } => write!(
                formatter,
                "voice @{number} has {parameter_count} parameters; expected 47"
            ),
            Self::InvalidValue { command, value } => {
                write!(
                    formatter,
                    "{command} value {value} cannot be represented in MDX"
                )
            }
            Self::UnsupportedCommand(command) => {
                write!(formatter, "MML command {command} has no MDX mapping")
            }
            Self::Builder(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile a parsed MML document into a typed soundlog MDX document.
///
/// The compiler preserves the MML title, PDX filename, voice definitions, and
/// channel order. Track indices follow MXDRV's layout: `A` through `H` map to
/// indices `0` through `7`, and `P` maps to index `8`.
///
/// # Errors
///
/// Returns [`CompileError`] when a channel, voice, value, or command cannot be
/// represented by soundlog's MDX model.
pub fn compile(document: &MmlDocument) -> Result<MdxDocument, CompileError> {
    let mut builder = MdxBuilder::new();
    let mut tracks: [Vec<MdxCommand>; 9] = std::array::from_fn(|_| Vec::new());
    let mut track_states: [TrackState; 9] = std::array::from_fn(|_| TrackState::default());
    if let Some(title) = &document.title {
        builder.set_title(title);
    }
    builder.set_pdx_name(document.pcm_file.as_deref());

    for voice in &document.voices {
        builder.append_tone(compile_voice(voice)?);
    }

    for track in &document.tracks {
        let track_index = channel_index(track.channel)?;
        tracks[track_index].extend(compile_track(
            &track.commands,
            &mut track_states[track_index],
        )?);
    }

    for (track_index, mut commands) in tracks.into_iter().enumerate() {
        if commands.is_empty() {
            commands.push(MdxEndOfTrack.into());
        }
        builder.set_track(track_index, commands);
    }

    builder
        .finalize()
        .map_err(|error| CompileError::Builder(error.to_string()))
}

/// Convert one MML voice definition into an MDX tone.
fn compile_voice(voice: &MmlVoice) -> Result<MdxTone, CompileError> {
    if voice.values.len() != 47 {
        return Err(CompileError::InvalidVoice {
            number: voice.number,
            parameter_count: voice.values.len(),
        });
    }

    let operators = [0, 2, 1, 3].map(|index| {
        let values = &voice.values[index * 11..index * 11 + 11];
        MdxOperator {
            ar: values[0],
            dr: values[1],
            sr: values[2],
            rr: values[3],
            sl: values[4],
            ol: values[5],
            ks: values[6],
            ml: values[7],
            dt1: values[8],
            dt2: values[9],
            ame: values[10],
        }
    });

    Ok(MdxTone {
        voice_number: voice.number,
        con: voice.values[44],
        fl: voice.values[45],
        op: voice.values[46],
        operators,
    })
}

#[derive(Debug, Clone)]
struct TrackState {
    octave: u8,
    default_length: MmlLength,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            octave: 4,
            default_length: MmlLength::Denominator(4),
        }
    }
}

/// Compile one track while maintaining its octave and default note length.
fn compile_track(
    commands: &[MmlCommand],
    state: &mut TrackState,
) -> Result<Vec<MdxCommand>, CompileError> {
    compile_commands(commands, state)
}

/// Lower MML commands recursively into their soundlog MDX representations.
fn compile_commands(
    commands: &[MmlCommand],
    state: &mut TrackState,
) -> Result<Vec<MdxCommand>, CompileError> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < commands.len() {
        let command = &commands[index];
        let note_is_legato = matches!(commands.get(index + 1), Some(MmlCommand::Legato));
        let mut consumed = 1;
        if matches!(commands.get(index + 1), Some(MmlCommand::Portamento)) {
            if let Some(target) = commands.get(index + 2) {
                if let (Some(source_note), Some(target_note)) = (
                    command_note_number(command, state.octave)?,
                    command_note_number(target, state.octave)?,
                ) {
                    let offset = i32::from(target_note) - i32::from(source_note);
                    let offset =
                        i16::try_from(offset * 341).map_err(|_| CompileError::InvalidValue {
                            command: "portamento",
                            value: i64::from(offset * 341),
                        })?;
                    output.push(MdxCommand::Portamento(MdxSignedWord {
                        opcode: 0xf2,
                        offset,
                    }));
                    consumed = 3;
                }
            }
        }
        match command {
            MmlCommand::OpmTempo(value) => {
                output.push(MdxTempo { value: *value }.into());
            }
            MmlCommand::Tempo(value) => {
                output.push(
                    MdxTempo {
                        value: tempo_value(*value)?,
                    }
                    .into(),
                );
            }
            MmlCommand::VoiceSelect(value) => {
                output.push(MdxVoiceOrPcmBank { value: *value }.into());
            }
            MmlCommand::Directive(_) => {}
            MmlCommand::Ignore => break,
            MmlCommand::Repeat { body, count } => {
                let body_commands = compile_commands(body, state)?;
                let body_length = command_bytes(&body_commands);
                output.push(
                    MdxLoopStart {
                        count: checked_u8("repeat", *count as i64)?,
                        reserved: 0,
                    }
                    .into(),
                );
                output.extend(body_commands);
                output.push(MdxCommand::LoopEnd(MdxRelativeOffset {
                    opcode: 0xf5,
                    offset: -(body_length as i16 + 3),
                }));
            }
            MmlCommand::Note {
                name,
                accidental,
                length,
            } => {
                if note_is_legato {
                    output.push(MdxKeyOffDisable.into());
                }
                output.extend(compile_note(
                    note_number(state.octave, *name, *accidental)?,
                    note_ticks(
                        length.map(MmlLength::Denominator).as_ref(),
                        &state.default_length,
                    )?,
                )?);
            }
            MmlCommand::ExtendedNote {
                name,
                accidental,
                length,
            } => {
                if note_is_legato {
                    output.push(MdxKeyOffDisable.into());
                }
                output.extend(compile_note(
                    note_number(state.octave, *name, *accidental)?,
                    length_ticks(length)?,
                )?);
            }
            MmlCommand::NumericNote { note, length } => {
                if note_is_legato {
                    output.push(MdxKeyOffDisable.into());
                }
                output.extend(compile_note(
                    *note as u16,
                    note_ticks(length.as_ref(), &state.default_length)?,
                )?);
            }
            MmlCommand::Rest { length } => output.extend(compile_rest(note_ticks(
                length.map(MmlLength::Denominator).as_ref(),
                &state.default_length,
            )?)),
            MmlCommand::ExtendedRest(length) => output.extend(compile_rest(length_ticks(length)?)),
            MmlCommand::Octave(value) => state.octave = *value,
            MmlCommand::OctaveDown => state.octave = state.octave.saturating_sub(1),
            MmlCommand::OctaveUp => state.octave = state.octave.saturating_add(1),
            MmlCommand::DefaultLength(value) => state.default_length = value.clone(),
            MmlCommand::Gate(value) => output.push(MdxGate { value: *value }.into()),
            MmlCommand::FineGate(value) => output.push(
                MdxGate {
                    value: checked_u8("@q", 256 - i64::from(*value))?,
                }
                .into(),
            ),
            MmlCommand::Portamento => output.push(MdxCommand::Portamento(MdxSignedWord {
                opcode: 0xf2,
                offset: 0,
            })),
            MmlCommand::Legato => {}
            MmlCommand::Volume(value) => output.push(MdxVolume { value: *value }.into()),
            MmlCommand::FineVolume(value) => output.push(
                MdxVolume {
                    value: checked_u8("@v", 128 + i64::from(*value))?,
                }
                .into(),
            ),
            MmlCommand::VolumeDown => output.push(MdxVolumeDown.into()),
            MmlCommand::VolumeUp => output.push(MdxVolumeUp.into()),
            MmlCommand::Pan(value) => output.push(MdxPan::from_raw(*value).into()),
            MmlCommand::LoopStart => output.push(
                MdxLoopStart {
                    count: 0,
                    reserved: 0,
                }
                .into(),
            ),
            MmlCommand::Detune(value) => output.push(MdxCommand::Detune(MdxSignedWord {
                opcode: 0xf3,
                offset: *value,
            })),
            MmlCommand::LoopEscape => output.push(MdxCommand::LoopEscape(MdxRelativeOffset {
                opcode: 0xf4,
                offset: 0,
            })),
            MmlCommand::RegisterWrite { register, value } => output.push(
                MdxOpmRegisterWrite {
                    register: *register,
                    value: *value,
                }
                .into(),
            ),
            MmlCommand::KeyOnDelay(value) => output.push(MdxKeyOnDelay { value: *value }.into()),
            MmlCommand::NoiseFrequency(value) => output.push(
                MdxAdpcmOrNoiseFrequency {
                    value: 0x80 | *value,
                }
                .into(),
            ),
            MmlCommand::PcmFrequency(value) => {
                output.push(MdxAdpcmOrNoiseFrequency { value: *value & 0x07 }.into())
            }
            MmlCommand::SyncSend(channel) => output.push(
                MdxSyncSend {
                    value: sync_channel_value(*channel)?,
                }
                .into(),
            ),
            MmlCommand::SyncWait => output.push(soundlog::mdx::command::MdxSyncWait.into()),
            MmlCommand::PitchLfo {
                waveform,
                period,
                amplitude,
            } => output.push(
                MdxPitchLfo::Configure {
                    waveform: MdxLfoWaveform::from_raw(*waveform),
                    frequency: checked_u16("pitch LFO period", u32::from(*period) * 2)?,
                    amplitude: checked_i16("pitch LFO amplitude", u32::from(*amplitude) * 128)?,
                }
                .into(),
            ),
            MmlCommand::PitchLfoOn => output.push(MdxPitchLfo::SetEnabled { enabled: true }.into()),
            MmlCommand::PitchLfoOff => {
                output.push(MdxPitchLfo::SetEnabled { enabled: false }.into())
            }
            MmlCommand::VolumeLfo {
                waveform,
                period,
                amplitude,
            } => output.push(
                MdxVolumeLfo::Configure {
                    waveform: MdxLfoWaveform::from_raw(*waveform),
                    frequency: checked_u16("volume LFO period", u32::from(*period) * 2)?,
                    amplitude: checked_u16("volume LFO amplitude", u32::from(*amplitude) * 32)?,
                }
                .into(),
            ),
            MmlCommand::VolumeLfoOn => {
                output.push(MdxVolumeLfo::SetEnabled { enabled: true }.into())
            }
            MmlCommand::VolumeLfoOff => {
                output.push(MdxVolumeLfo::SetEnabled { enabled: false }.into())
            }
            MmlCommand::LfoDelay(value) => output.push(MdxLfoDelay { value: *value }.into()),
            MmlCommand::OpmLfo {
                waveform,
                lfrq,
                pmd,
                amd,
                pms,
                ams,
                key_sync,
            } => output.push(
                MdxOpmLfo::Configure {
                    control: ((*key_sync & 1) << 6) | (*waveform & 3),
                    lfrq: *lfrq,
                    pmd: 0x80 | *pmd,
                    amd: *amd,
                    pms_ams: ((*pms & 7) << 4) | (*ams & 0xf),
                }
                .into(),
            ),
            MmlCommand::OpmLfoOn => output.push(MdxOpmLfo::SetEnabled { enabled: true }.into()),
            MmlCommand::OpmLfoOff => output.push(MdxOpmLfo::SetEnabled { enabled: false }.into()),
        }
        index += consumed;
    }
    Ok(output)
}

/// Return the note number represented by a note command at the current octave.
fn command_note_number(command: &MmlCommand, octave: u8) -> Result<Option<u16>, CompileError> {
    match command {
        MmlCommand::Note {
            name, accidental, ..
        }
        | MmlCommand::ExtendedNote {
            name, accidental, ..
        } => Ok(Some(note_number(octave, *name, *accidental)?)),
        MmlCommand::NumericNote { note, .. } => Ok(Some(*note as u16)),
        _ => Ok(None),
    }
}

/// Encode a note, splitting it when its duration exceeds one MDX note command.
fn compile_note(note: u16, ticks: u16) -> Result<Vec<MdxCommand>, CompileError> {
    let opcode = FIRST_NOTE + note;
    if !(FIRST_NOTE..=LAST_NOTE).contains(&opcode) {
        return Err(CompileError::InvalidValue {
            command: "note",
            value: note as i64,
        });
    }
    let mut commands = Vec::new();
    let mut remaining = ticks;
    while remaining > 0 {
        let length = remaining.min(0x100);
        commands.push(
            MdxNote::new(opcode as u8, length)
                .ok_or(CompileError::InvalidValue {
                    command: "note length",
                    value: length as i64,
                })?
                .into(),
        );
        remaining -= length;
    }
    Ok(commands)
}

/// Encode a rest, splitting it when its duration exceeds one MDX rest command.
fn compile_rest(ticks: u16) -> Vec<MdxCommand> {
    let mut commands = Vec::new();
    let mut remaining = ticks;
    while remaining > 0 {
        let length = remaining.min(0x80);
        commands.push(
            MdxRest::new(length)
                .expect("length is clamped to the MDX rest range")
                .into(),
        );
        remaining -= length;
    }
    commands
}

/// Convert an MML note name and accidental at an octave into an MDX note number.
fn note_number(
    octave: u8,
    name: char,
    accidental: Option<Accidental>,
) -> Result<u16, CompileError> {
    let base = match name {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => {
            return Err(CompileError::InvalidValue {
                command: "note name",
                value: name as i64,
            });
        }
    };
    let modifier = match accidental {
        Some(Accidental::Sharp) => 1,
        Some(Accidental::Flat) => -1,
        None => 0,
    };
    let absolute = i32::from(octave) * 12 + base + modifier - 3;
    if !(0..=95).contains(&absolute) {
        return Err(CompileError::InvalidValue {
            command: "note",
            value: i64::from(absolute),
        });
    }
    Ok(absolute as u16)
}

/// Convert an optional note length to ticks, using the track default when absent.
fn note_ticks(length: Option<&MmlLength>, default_length: &MmlLength) -> Result<u16, CompileError> {
    length
        .map(length_ticks)
        .unwrap_or_else(|| length_ticks(default_length))
}

/// Convert a parsed MML length expression into MDX ticks.
fn length_ticks(length: &MmlLength) -> Result<u16, CompileError> {
    match length {
        MmlLength::Denominator(value) => ticks_from_denominator(*value, "note length"),
        MmlLength::Ticks(value) => Ok(*value),
        MmlLength::Sum(values) => values.iter().try_fold(0_u16, |total, value| {
            total
                .checked_add(length_ticks(value)?)
                .ok_or(CompileError::InvalidValue {
                    command: "note length",
                    value: i64::from(u16::MAX),
                })
        }),
    }
}

/// Convert a BPM-style MML tempo into the OPM tempo byte used by MDX.
fn tempo_value(bpm: u32) -> Result<u8, CompileError> {
    if bpm == 0 {
        return Err(CompileError::InvalidValue {
            command: "tempo",
            value: 0,
        });
    }
    let decrement = 78_125 / (16_u64 * u64::from(bpm));
    Ok(256_u64.saturating_sub(decrement).min(u64::from(u8::MAX)) as u8)
}

/// Convert a denominator-based length into ticks for a whole note of 192 ticks.
fn ticks_from_denominator(value: u16, command: &'static str) -> Result<u16, CompileError> {
    if value == 0 {
        return Err(CompileError::InvalidValue { command, value: 0 });
    }
    Ok(TICKS_PER_WHOLE / value)
}

/// Convert a signed integer to an MDX unsigned byte value.
fn checked_u8(command: &'static str, value: i64) -> Result<u8, CompileError> {
    u8::try_from(value).map_err(|_| CompileError::InvalidValue { command, value })
}

/// Convert a scaled MML value to an MDX unsigned 16-bit value.
fn checked_u16(command: &'static str, value: u32) -> Result<u16, CompileError> {
    u16::try_from(value).map_err(|_| CompileError::InvalidValue {
        command,
        value: i64::from(value),
    })
}

/// Convert a scaled MML value to an MDX signed 16-bit value.
fn checked_i16(command: &'static str, value: u32) -> Result<i16, CompileError> {
    i16::try_from(value).map_err(|_| CompileError::InvalidValue {
        command,
        value: i64::from(value),
    })
}

/// Map an MML track channel to its zero-based MDX track index.
fn channel_index(channel: char) -> Result<usize, CompileError> {
    match channel {
        'A'..='H' => Ok(channel as usize - 'A' as usize),
        'P' => Ok(8),
        _ => Err(CompileError::InvalidChannel(channel)),
    }
}

/// Map an MML synchronization channel to the value used by MDX.
fn sync_channel_value(channel: char) -> Result<u8, CompileError> {
    match channel {
        'A'..='H' => Ok(channel as u8 - b'A'),
        'P'..='W' => Ok(channel as u8 - b'P' + 8),
        '0'..='9' => Ok(channel as u8 - b'0'),
        _ => Err(CompileError::InvalidChannel(channel)),
    }
}

/// Return the serialized byte length of a sequence of MDX commands.
fn command_bytes(commands: &[MdxCommand]) -> usize {
    commands
        .iter()
        .filter_map(MdxCommand::to_mdx_bytes)
        .map(|bytes| bytes.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdx::parse;
    use soundlog::mdx::command::{MdxEndOfTrack, MdxTempo};

    #[test]
    fn compiles_metadata_voice_and_basic_track() {
        let source = "#title \"Test\"\n#pcmfile \"test.pdx\"\nA o4 l4 c4\n";
        let document = parse(source).unwrap();
        let compiled = compile(&document).unwrap();

        assert_eq!(compiled.header.title, "Test");
        assert_eq!(compiled.header.pdx_name.as_deref(), Some("test.pdx"));
        assert_eq!(compiled.tracks[0].len(), 2);
        assert!(matches!(compiled.tracks[0][0], MdxCommand::Note(_)));
        assert!(matches!(
            compiled.tracks[0][1],
            MdxCommand::EndOfTrack(MdxEndOfTrack)
        ));
    }

    #[test]
    fn includes_end_of_track_for_empty_standard_tracks() {
        let compiled = compile(&parse("").unwrap()).unwrap();

        assert_eq!(compiled.tracks.len(), 9);
        assert!(
            compiled
                .tracks
                .iter()
                .all(|track| matches!(track.as_slice(), [MdxCommand::EndOfTrack(_)]))
        );
    }

    #[test]
    fn compiles_voice_parameters_into_soundlog_tone() {
        let source = "@1={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\n";
        let compiled = compile(&parse(source).unwrap()).unwrap();
        let tone = &compiled.tone_bank.tones[0];

        assert_eq!(tone.voice_number, 1);
        assert_eq!(tone.operators[0].ar, 1);
        assert_eq!(tone.operators[3].ame, 44);
        assert_eq!((tone.con, tone.fl, tone.op), (45, 46, 47));
    }

    #[test]
    fn converts_bpm_tempo_to_mdx_tempo() {
        let compiled = compile(&parse("A t30 t4882\n").unwrap()).unwrap();

        assert!(matches!(
            compiled.tracks[0][0],
            MdxCommand::Tempo(MdxTempo { value: 94 })
        ));
        assert!(matches!(
            compiled.tracks[0][1],
            MdxCommand::Tempo(MdxTempo { value: 255 })
        ));
    }

    #[test]
    fn places_key_off_disable_before_legato_notes() {
        let compiled = compile(&parse("A c&d e\n").unwrap()).unwrap();
        let commands = &compiled.tracks[0];

        assert!(matches!(commands[0], MdxCommand::KeyOffDisable(_)));
        assert!(matches!(commands[1], MdxCommand::Note(_)));
        assert!(matches!(commands[2], MdxCommand::Note(_)));
        assert!(matches!(commands[3], MdxCommand::Note(_)));
        assert!(matches!(commands[4], MdxCommand::EndOfTrack(_)));
    }
}
