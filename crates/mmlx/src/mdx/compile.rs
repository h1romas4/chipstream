//! Conversion from the parsed MML AST to soundlog's typed MDX document.

use std::fmt;

use soundlog::mdx::command::{
    MdxAdpcmOrNoiseFrequency, MdxCommand, MdxGate, MdxKeyOffDisable, MdxKeyOnDelay, MdxLfoDelay,
    MdxLfoWaveform, MdxLoopStart, MdxNote, MdxOpmLfo, MdxOpmRegisterWrite, MdxPan, MdxPitchLfo,
    MdxRelativeOffset, MdxRest, MdxSignedWord, MdxSyncSend, MdxTempo, MdxVoiceOrPcmBank, MdxVolume,
    MdxVolumeDown, MdxVolumeLfo, MdxVolumeUp,
};
use soundlog::mdx::document::{MdxBuilder, MdxDocument};
use soundlog::mdx::tone::{MdxOperator, MdxTone};

use super::mml::{Accidental, MmlCommand, MmlDocument, MmlLength, MmlVoice};

const TICKS_PER_WHOLE: u16 = 192;
const DEFAULT_LENGTH: u16 = 4;
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
    if let Some(title) = &document.title {
        builder.set_title(title);
    }
    builder.set_pdx_name(document.pcm_file.as_deref());

    for voice in &document.voices {
        builder.append_tone(compile_voice(voice)?);
    }

    for track in &document.tracks {
        let track_index = channel_index(track.channel)?;
        let commands = compile_track(&track.commands)?;
        builder.set_track(track_index, commands);
    }

    builder
        .finalize()
        .map_err(|error| CompileError::Builder(error.to_string()))
}

fn compile_voice(voice: &MmlVoice) -> Result<MdxTone, CompileError> {
    if voice.values.len() != 47 {
        return Err(CompileError::InvalidVoice {
            number: voice.number,
            parameter_count: voice.values.len(),
        });
    }

    let operators = std::array::from_fn(|index| {
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

#[derive(Debug, Clone, Copy)]
struct TrackState {
    octave: u8,
    default_length: u16,
}

fn compile_track(commands: &[MmlCommand]) -> Result<Vec<MdxCommand>, CompileError> {
    let mut state = TrackState {
        octave: 4,
        default_length: DEFAULT_LENGTH,
    };
    compile_commands(commands, &mut state)
}

fn compile_commands(
    commands: &[MmlCommand],
    state: &mut TrackState,
) -> Result<Vec<MdxCommand>, CompileError> {
    let mut output = Vec::new();
    for command in commands {
        match command {
            MmlCommand::OpmTempo(value) | MmlCommand::Tempo(value) => {
                output.push(MdxTempo { value: *value }.into());
            }
            MmlCommand::VoiceSelect(value) => {
                output.push(MdxVoiceOrPcmBank { value: *value }.into());
            }
            MmlCommand::Directive(_) | MmlCommand::Ignore => {}
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
            } => output.extend(compile_note(
                note_number(state.octave, *name, *accidental)?,
                note_ticks(
                    length.map(MmlLength::Denominator).as_ref(),
                    state.default_length,
                )?,
            )?),
            MmlCommand::ExtendedNote {
                name,
                accidental,
                length,
            } => output.extend(compile_note(
                note_number(state.octave, *name, *accidental)?,
                length_ticks(length, state.default_length)?,
            )?),
            MmlCommand::NumericNote { note, length } => output.extend(compile_note(
                *note as u16,
                note_ticks(length.as_ref(), state.default_length)?,
            )?),
            MmlCommand::Rest { length } => output.extend(compile_rest(note_ticks(
                length.map(MmlLength::Denominator).as_ref(),
                state.default_length,
            )?)),
            MmlCommand::ExtendedRest(length) => {
                output.extend(compile_rest(length_ticks(length, state.default_length)?))
            }
            MmlCommand::Octave(value) => state.octave = *value,
            MmlCommand::OctaveDown => state.octave = state.octave.saturating_sub(1),
            MmlCommand::OctaveUp => state.octave = state.octave.saturating_add(1),
            MmlCommand::DefaultLength(value) => state.default_length = *value,
            MmlCommand::Gate(value) => output.push(MdxGate { value: *value }.into()),
            MmlCommand::FineGate(value) => output.push(
                MdxGate {
                    value: checked_u8("@q", *value as i64)?,
                }
                .into(),
            ),
            MmlCommand::Portamento => output.push(MdxCommand::Portamento(MdxSignedWord {
                opcode: 0xf2,
                offset: 0,
            })),
            MmlCommand::Legato => output.push(MdxKeyOffDisable.into()),
            MmlCommand::Volume(value) | MmlCommand::FineVolume(value) => {
                output.push(MdxVolume { value: *value }.into())
            }
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
            MmlCommand::NoiseFrequency(value) | MmlCommand::PcmFrequency(value) => {
                output.push(MdxAdpcmOrNoiseFrequency { value: *value }.into())
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
                    frequency: *period,
                    amplitude: i16::try_from(*amplitude).map_err(|_| {
                        CompileError::InvalidValue {
                            command: "pitch LFO amplitude",
                            value: *amplitude as i64,
                        }
                    })?,
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
                    frequency: *period,
                    amplitude: *amplitude,
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
                    control: ((*key_sync & 1) << 7) | (*waveform & 3),
                    lfrq: *lfrq,
                    pmd: *pmd,
                    amd: *amd,
                    pms_ams: ((*pms & 7) << 4) | (*ams & 3),
                }
                .into(),
            ),
            MmlCommand::OpmLfoOn => output.push(MdxOpmLfo::SetEnabled { enabled: true }.into()),
            MmlCommand::OpmLfoOff => output.push(MdxOpmLfo::SetEnabled { enabled: false }.into()),
        }
    }
    Ok(output)
}

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

fn note_ticks(length: Option<&MmlLength>, default_length: u16) -> Result<u16, CompileError> {
    length
        .map(|length| length_ticks(length, default_length))
        .unwrap_or_else(|| ticks_from_denominator(default_length, "default length"))
}

fn length_ticks(length: &MmlLength, default_length: u16) -> Result<u16, CompileError> {
    match length {
        MmlLength::Denominator(value) => ticks_from_denominator(*value, "note length"),
        MmlLength::Ticks(value) => Ok(*value),
        MmlLength::Sum(values) => values.iter().try_fold(0_u16, |total, value| {
            total
                .checked_add(length_ticks(value, default_length)?)
                .ok_or(CompileError::InvalidValue {
                    command: "note length",
                    value: i64::from(u16::MAX),
                })
        }),
    }
}

fn ticks_from_denominator(value: u16, command: &'static str) -> Result<u16, CompileError> {
    if value == 0 {
        return Err(CompileError::InvalidValue { command, value: 0 });
    }
    Ok(TICKS_PER_WHOLE / value)
}

fn checked_u8(command: &'static str, value: i64) -> Result<u8, CompileError> {
    u8::try_from(value).map_err(|_| CompileError::InvalidValue { command, value })
}

fn channel_index(channel: char) -> Result<usize, CompileError> {
    match channel {
        'A'..='H' => Ok(channel as usize - 'A' as usize),
        'P' => Ok(8),
        _ => Err(CompileError::InvalidChannel(channel)),
    }
}

fn sync_channel_value(channel: char) -> Result<u8, CompileError> {
    match channel {
        'A'..='H' => Ok(channel as u8 - b'A'),
        'P'..='W' => Ok(channel as u8 - b'P' + 8),
        '0'..='9' => Ok(channel as u8 - b'0'),
        _ => Err(CompileError::InvalidChannel(channel)),
    }
}

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
    use soundlog::mdx::command::MdxEndOfTrack;

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
    fn compiles_voice_parameters_into_soundlog_tone() {
        let source = "@1={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\n";
        let compiled = compile(&parse(source).unwrap()).unwrap();
        let tone = &compiled.tone_bank.tones[0];

        assert_eq!(tone.voice_number, 1);
        assert_eq!(tone.operators[0].ar, 1);
        assert_eq!(tone.operators[3].ame, 44);
        assert_eq!((tone.con, tone.fl, tone.op), (45, 46, 47));
    }
}
