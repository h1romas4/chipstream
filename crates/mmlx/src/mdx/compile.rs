//! Conversion from the parsed MML AST to soundlog's typed MDX document.
//!
//! The compiler lowers channel commands, voice definitions, and document
//! metadata into soundlog's MDX model. It reports [`CompileError`] when source
//! constructs cannot be represented by that model; it does not serialize the
//! resulting document to bytes.

use std::fmt;

use soundlog::mdx::command::{
    MdxAdpcmOrNoiseFrequency, MdxCommand, MdxEndOfTrack, MdxGate, MdxKeyOffDisable, MdxKeyOnDelay,
    MdxLfoDelay, MdxLfoWaveform, MdxLoopStart, MdxNote, MdxOpmLfo, MdxOpmRegisterWrite, MdxPan,
    MdxPitchLfo, MdxRelativeOffset, MdxRest, MdxSignedWord, MdxSyncSend, MdxTempo,
    MdxVoiceOrPcmBank, MdxVolume, MdxVolumeDown, MdxVolumeLfo, MdxVolumeUp,
};
use soundlog::mdx::document::{MdxBuilder, MdxDocument};
use soundlog::mdx::tone::{MdxOperator, MdxTone};

use crate::source::Span;

use super::mml::{
    Accidental, CommandSource, MmlCommand, MmlDocument, MmlLength, MmlSourceMap, MmlVoice,
};

/// Compile a parsed MML document into a typed soundlog MDX document.
///
/// The compiler preserves the MML title, PDX filename, voice definitions, and
/// channel order. Track indices follow MXDRV's layout: `A` through `H` map to
/// indices `0` through `7`, and `P` through `W` map to indices `8` through `15`.
///
/// # Errors
///
/// Returns [`CompileError`] when a channel, voice, value, or command cannot be
/// represented by soundlog's MDX model.
///
/// # Examples
///
/// ```
/// let parsed = mmlx::mdx::parse("#title \"Example\"\nA c4 d4 e4")
///     .expect("valid MML source");
/// let document = mmlx::mdx::compile(&parsed).expect("supported MML commands");
/// let mdx_bytes = document.to_bytes().expect("valid MDX layout");
///
/// assert!(!mdx_bytes.is_empty());
/// ```
pub fn compile(document: &MmlDocument) -> Result<MdxDocument, CompileError> {
    compile_internal(document, None)
}

type MdxSourceMap = [Vec<Option<Span>>; 16];
type CompiledCommands = (Vec<MdxCommand>, Option<Vec<Option<Span>>>);

/// Source-location input and results for one MDX compilation.
///
/// Borrows the syntax map associated with the input AST without copying source
/// text. On success, the output map addresses finalized MDX track commands;
/// synthetic commands have no span. On failure, only the error span is intended
/// for diagnostics, and the output map may be incomplete.
///
/// Ordinary compilation passes no context to [`compile_internal`] and does not
/// allocate source maps.
pub(super) struct CompileSourceContext<'a> {
    /// Syntax ranges corresponding to the input AST.
    sources: &'a MmlSourceMap,
    /// Primary source spans indexed by finalized MDX track and command.
    pub output_map: MdxSourceMap,
    /// The responsible source range on failure, or `None` if unavailable.
    pub error_span: Option<Span>,
}

impl<'a> CompileSourceContext<'a> {
    /// Create an empty context without allocating heap storage.
    ///
    /// Output vectors allocate as mapped commands are emitted during compilation.
    #[cfg(feature = "source-map")]
    pub fn new(sources: &'a MmlSourceMap) -> Self {
        Self {
            sources,
            output_map: std::array::from_fn(|_| Vec::new()),
            error_span: None,
        }
    }
}

struct CommandSourceContext<'a, 'error> {
    sources: &'a [CommandSource],
    error_span: &'error mut Option<Span>,
}

const TICKS_PER_WHOLE: u16 = 192;
const FIRST_NOTE: u16 = 0x80;
const LAST_NOTE: u16 = 0xdf;

/// An error raised while lowering MML AST nodes into MDX commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// A track uses a channel that cannot be represented in MDX.
    InvalidChannel(char),
    /// A voice does not contain the required 47 parameters.
    InvalidVoice {
        /// The MML voice number following `@`.
        number: u8,
        /// The supplied parameter count, rather than the required 47.
        parameter_count: usize,
    },
    /// A parsed value cannot be represented by the target MDX type.
    InvalidValue {
        /// The command or generated field whose value is invalid.
        command: &'static str,
        /// The source or computed value that could not be represented.
        value: i32,
    },
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

/// Lower a parsed MML document into MDX, optionally collecting source locations.
///
/// The context supplies syntax ranges and records finalized output locations
/// and the responsible error span. Passing `None` avoids allocating source maps.
pub(super) fn compile_internal(
    document: &MmlDocument,
    mut source_context: Option<&mut CompileSourceContext<'_>>,
) -> Result<MdxDocument, CompileError> {
    let mut builder = MdxBuilder::new();
    let mut tracks: [Vec<MdxCommand>; 16] = std::array::from_fn(|_| Vec::new());
    let mut track_states: [TrackState; 16] = std::array::from_fn(|_| TrackState::default());
    let track_count = if document
        .tracks
        .iter()
        .any(|track| matches!(track.channel, 'P'..='W'))
    {
        16
    } else {
        9
    };
    if let Some(title) = &document.title {
        builder.set_title(title);
    }
    builder.set_pdx_name(document.pcm_file.as_deref());

    #[cfg(feature = "source-map")]
    let mut voice_sources = source_context
        .as_ref()
        .map(|context| context.sources.voices.iter());
    for voice in &document.voices {
        if let Some(context) = &mut source_context {
            #[cfg(feature = "source-map")]
            {
                context.error_span = voice_sources
                    .as_mut()
                    .and_then(Iterator::next)
                    .map(|voice| voice.span);
            }
            #[cfg(not(feature = "source-map"))]
            {
                context.error_span = None;
            }
        }
        builder.append_tone(compile_voice(voice)?);
    }

    for (source_index, track) in document.tracks.iter().enumerate() {
        let track_index = channel_index(track.channel)?;
        let base_offset = command_bytes(&tracks[track_index]);
        let (commands, command_positions) = compile_track(
            &track.commands,
            &mut track_states[track_index],
            base_offset,
            source_context.as_mut().map(|context| CommandSourceContext {
                sources: context.sources.get(source_index).map_or(&[], Vec::as_slice),
                error_span: &mut context.error_span,
            }),
        )?;
        tracks[track_index].extend(commands);
        if let (Some(context), Some(command_positions)) = (&mut source_context, command_positions) {
            context.output_map[track_index].extend(command_positions);
        }
    }

    if let Some(context) = &mut source_context {
        context.error_span = None;
    }
    for (track_index, mut commands) in tracks.into_iter().take(track_count).enumerate() {
        if let Some(loop_start) = track_states[track_index].loop_start {
            let track_length = command_bytes(&commands);
            let mut position = 0_usize;
            let starts_with_duration = commands.iter().any(|command| {
                let at_loop_start = position == loop_start;
                position += command.to_mdx_bytes().map_or(0, |bytes| bytes.len());
                at_loop_start && matches!(command, MdxCommand::Note(_) | MdxCommand::Rest(_))
            });
            let mut rest_position = 0_usize;
            let starts_with_rest = commands.iter().any(|command| {
                let at_loop_start = rest_position == loop_start;
                rest_position += command.to_mdx_bytes().map_or(0, |bytes| bytes.len());
                at_loop_start && matches!(command, MdxCommand::Rest(_))
            });
            // Resolve the loop marker's byte offset to its command index.
            let loop_start_index = commands
                .iter()
                .scan(0_usize, |position, command| {
                    let current = *position;
                    *position += command.to_mdx_bytes().map_or(0, |bytes| bytes.len());
                    Some((current, command))
                })
                .position(|(position, _)| position == loop_start);
            // A later finite repeat changes how legacy loop targets are encoded.
            let has_finite_repeat_after_loop = loop_start_index.is_some_and(|index| {
                commands[index + 1..]
                    .iter()
                    .any(|command| matches!(command, MdxCommand::LoopStart(_)))
            });
            let loop_adjustment = if has_finite_repeat_after_loop
                && ((track_index < 8 && starts_with_duration)
                    || (track_index >= 8 && starts_with_rest))
            {
                1
            } else {
                3
            };
            let offset = i32::try_from(loop_start).unwrap_or(i32::MAX)
                - i32::try_from(track_length).unwrap_or(i32::MAX)
                - loop_adjustment;
            let offset = i16::try_from(offset).map_err(|_| CompileError::InvalidValue {
                command: "loop start",
                value: offset,
            })?;
            commands.push(MdxCommand::EndOfTrackLoop(MdxRelativeOffset {
                opcode: 0xf1,
                offset,
            }));
        } else if commands.is_empty() {
            commands.push(MdxEndOfTrack.into());
        }
        builder.set_track(track_index, commands);
    }

    let document = builder
        .finalize()
        .map_err(|error| CompileError::Builder(error.to_string()))?;
    if let Some(context) = source_context {
        if matches!(document.tracks[0].first(), Some(MdxCommand::PcmMode(_))) {
            context.output_map[0].insert(0, None);
        }
        for (track, commands) in document.tracks.iter().enumerate() {
            context.output_map[track].resize(commands.len(), None);
        }
    }
    Ok(document)
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
    loop_start: Option<usize>,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            octave: 4,
            default_length: MmlLength::Denominator(4),
            loop_start: None,
        }
    }
}

/// Compile one track while maintaining its octave and default note length.
fn compile_track(
    commands: &[MmlCommand],
    state: &mut TrackState,
    base_offset: usize,
    source_context: Option<CommandSourceContext<'_, '_>>,
) -> Result<CompiledCommands, CompileError> {
    compile_commands(commands, state, base_offset, source_context)
}

/// Lower MML commands recursively into their soundlog MDX representations.
fn compile_commands(
    commands: &[MmlCommand],
    state: &mut TrackState,
    base_offset: usize,
    mut source_context: Option<CommandSourceContext<'_, '_>>,
) -> Result<CompiledCommands, CompileError> {
    let mut output = Vec::new();
    let sources = source_context.as_ref().map(|context| context.sources);
    let mut output_positions = source_context.as_ref().map(|_| Vec::new());
    let mut index = 0;
    let mut last_note_output: Option<(usize, u16, u16)> = None;
    while index < commands.len() {
        let command = &commands[index];
        let command_source = sources.and_then(|sources| sources.get(index));
        let position = command_source.map(|source| source.position);
        if let Some(context) = &mut source_context {
            *context.error_span = position;
        }
        let note_is_legato = matches!(commands.get(index + 1), Some(MmlCommand::Legato));
        let mut consumed = 1;
        if matches!(commands.get(index + 1), Some(MmlCommand::Portamento))
            && let Some(target) = commands.get(index + 2)
            && let (Some(source_note), Some(target_note)) = (
                command_note_number(command, state.octave)?,
                command_note_number(target, state.octave).inspect_err(|_| {
                    if let Some(context) = &mut source_context {
                        *context.error_span = sources
                            .and_then(|sources| sources.get(index + 2))
                            .map(|source| source.position);
                    }
                })?,
            )
        {
            let ticks = command_note_ticks(command, state)?.ok_or(CompileError::InvalidValue {
                command: "portamento",
                value: 0,
            })?;
            let offset = portamento_offset(source_note, target_note, ticks)?;
            output.push(MdxCommand::Portamento(MdxSignedWord {
                opcode: 0xf2,
                offset,
            }));
            consumed = 3;
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
                let body_base = base_offset + command_bytes(&output);
                let (mut body_commands, body_positions) = compile_commands(
                    body,
                    state,
                    body_base,
                    source_context.as_mut().map(|context| CommandSourceContext {
                        sources: command_source.map_or(&[], |source| source.body.as_slice()),
                        error_span: context.error_span,
                    }),
                )?;
                if let Some(context) = &mut source_context {
                    *context.error_span = position;
                }
                let body_length = command_bytes(&body_commands);
                patch_repeat_escape_offsets(&mut body_commands, body_length)?;
                if let Some(positions) = &mut output_positions {
                    positions.push(position);
                    positions
                        .extend(body_positions.unwrap_or_else(|| vec![None; body_commands.len()]));
                    positions.push(command_source.and_then(|source| source.end_position));
                }
                output.push(
                    MdxLoopStart {
                        count: checked_u8("repeat", i32::from(*count))?,
                        reserved: 0,
                    }
                    .into(),
                );
                output.extend(body_commands);
                output.push(MdxCommand::LoopEnd(MdxRelativeOffset {
                    opcode: 0xf5,
                    offset: checked_signed_i16("repeat", -(body_length as i32 + 3))?,
                }));
            }
            MmlCommand::Note {
                name,
                accidental,
                length,
            } => {
                let note_output_start = output.len();
                let note = note_number(state.octave, *name, *accidental)?;
                let ticks = note_ticks(
                    length.map(MmlLength::Denominator).as_ref(),
                    &state.default_length,
                )?;
                if note_is_legato {
                    output.push(MdxKeyOffDisable.into());
                }
                output.extend(compile_note(note, ticks)?);
                last_note_output = Some((note_output_start, note, ticks));
            }
            MmlCommand::ExtendedNote {
                name,
                accidental,
                length,
            } => {
                let note_output_start = output.len();
                let note = note_number(state.octave, *name, *accidental)?;
                let ticks = length_ticks(length)?;
                if note_is_legato {
                    output.push(MdxKeyOffDisable.into());
                }
                output.extend(compile_note(note, ticks)?);
                last_note_output = Some((note_output_start, note, ticks));
            }
            MmlCommand::NumericNote { note, length } => {
                let note_output_start = output.len();
                let ticks = note_ticks(length.as_ref(), &state.default_length)?;
                if note_is_legato {
                    output.push(MdxKeyOffDisable.into());
                }
                output.extend(compile_note(*note as u16, ticks)?);
                last_note_output = Some((note_output_start, *note as u16, ticks));
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
            MmlCommand::DefaultLengthReset => {}
            MmlCommand::Gate(value) => output.push(MdxGate { value: *value }.into()),
            MmlCommand::FineGate(value) => output.push(
                MdxGate {
                    value: checked_u8("@q", 256 - i32::from(*value))?,
                }
                .into(),
            ),
            MmlCommand::Portamento => {
                if let Some((note_output_start, source_note, ticks)) = last_note_output {
                    let mut target_index = index + 1;
                    let mut target_octave = state.octave;
                    while let Some(target) = commands.get(target_index) {
                        match target {
                            MmlCommand::Octave(value) => {
                                target_octave = *value;
                                target_index += 1;
                            }
                            MmlCommand::OctaveDown => {
                                target_octave = target_octave.saturating_sub(1);
                                target_index += 1;
                            }
                            MmlCommand::OctaveUp => {
                                target_octave = target_octave.saturating_add(1);
                                target_index += 1;
                            }
                            _ => break,
                        }
                    }
                    if let Some(target) = commands.get(target_index) {
                        if let Some(target_note) = command_note_number(target, target_octave)
                            .inspect_err(|_| {
                                if let Some(context) = &mut source_context {
                                    *context.error_span = sources
                                        .and_then(|sources| sources.get(target_index))
                                        .map(|source| source.position);
                                }
                            })?
                        {
                            state.octave = target_octave;
                            let offset = portamento_offset(source_note, target_note, ticks)?;
                            output.insert(
                                note_output_start,
                                MdxCommand::Portamento(MdxSignedWord {
                                    opcode: 0xf2,
                                    offset,
                                }),
                            );
                            if let Some(positions) = &mut output_positions {
                                positions.insert(note_output_start, position);
                            }
                            last_note_output = Some((note_output_start + 1, source_note, ticks));
                            consumed = target_index - index + 1;
                        } else {
                            output.push(MdxCommand::Portamento(MdxSignedWord {
                                opcode: 0xf2,
                                offset: 0,
                            }));
                        }
                    } else {
                        output.push(MdxCommand::Portamento(MdxSignedWord {
                            opcode: 0xf2,
                            offset: 0,
                        }));
                    }
                } else {
                    output.push(MdxCommand::Portamento(MdxSignedWord {
                        opcode: 0xf2,
                        offset: 0,
                    }));
                }
            }
            MmlCommand::Legato => {}
            MmlCommand::Volume(value) => output.push(MdxVolume { value: *value }.into()),
            MmlCommand::FineVolume(value) => output.push(
                MdxVolume {
                    value: checked_u8("@v", 255 - i32::from(*value))?,
                }
                .into(),
            ),
            MmlCommand::VolumeDown => output.push(MdxVolumeDown.into()),
            MmlCommand::VolumeUp => output.push(MdxVolumeUp.into()),
            MmlCommand::Pan(value) => output.push(MdxPan::from_raw(*value).into()),
            MmlCommand::LoopStart => {
                state.loop_start = Some(base_offset + command_bytes(&output));
            }
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
                output.push(MdxAdpcmOrNoiseFrequency { value: *value }.into())
            }
            MmlCommand::SyncSend(value) => output.push(MdxSyncSend { value: *value }.into()),
            MmlCommand::SyncWait => output.push(soundlog::mdx::command::MdxSyncWait.into()),
            MmlCommand::PitchLfo {
                waveform,
                period,
                amplitude,
            } => {
                let (frequency, amplitude) = pitch_lfo_values(*waveform, *period, *amplitude)?;
                output.push(
                    MdxPitchLfo::Configure {
                        waveform: MdxLfoWaveform::from_raw(*waveform),
                        frequency,
                        amplitude,
                    }
                    .into(),
                );
            }
            MmlCommand::PitchLfoOn => output.push(MdxPitchLfo::SetEnabled { enabled: true }.into()),
            MmlCommand::PitchLfoOff => {
                output.push(MdxPitchLfo::SetEnabled { enabled: false }.into())
            }
            MmlCommand::VolumeLfo {
                waveform,
                period,
                amplitude,
            } => {
                let (frequency, amplitude) = volume_lfo_values(*waveform, *period, *amplitude)?;
                output.push(
                    MdxVolumeLfo::Configure {
                        waveform: MdxLfoWaveform::from_raw(*waveform),
                        frequency,
                        amplitude,
                    }
                    .into(),
                );
            }
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
                    pmd: 0x80 | (*pmd & 0x7f),
                    amd: *amd,
                    pms_ams: ((*pms & 0xf) << 4) | (*ams & 0xf),
                }
                .into(),
            ),
            MmlCommand::OpmLfoOn => output.push(MdxOpmLfo::SetEnabled { enabled: true }.into()),
            MmlCommand::OpmLfoOff => output.push(MdxOpmLfo::SetEnabled { enabled: false }.into()),
        }
        if let Some(positions) = &mut output_positions {
            positions.resize(output.len(), position);
        }
        index += consumed;
    }
    Ok((output, output_positions))
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

/// Return the duration of a note used as the source of a portamento.
fn command_note_ticks(
    command: &MmlCommand,
    state: &TrackState,
) -> Result<Option<u16>, CompileError> {
    match command {
        MmlCommand::Note { length, .. } => note_ticks(
            length.map(MmlLength::Denominator).as_ref(),
            &state.default_length,
        )
        .map(Some),
        MmlCommand::ExtendedNote { length, .. } => length_ticks(length).map(Some),
        MmlCommand::NumericNote { length, .. } => {
            note_ticks(length.as_ref(), &state.default_length).map(Some)
        }
        _ => Ok(None),
    }
}

/// Calculate the MDX portamento rate from a source note to a target note.
fn portamento_offset(source_note: u16, target_note: u16, ticks: u16) -> Result<i16, CompileError> {
    if ticks == 0 {
        return Err(CompileError::InvalidValue {
            command: "portamento",
            value: 0,
        });
    }
    let delta = i32::from(target_note) - i32::from(source_note);
    let offset = (16_384 * delta) / i32::from(ticks);
    i16::try_from(offset).map_err(|_| CompileError::InvalidValue {
        command: "portamento",
        value: offset,
    })
}

/// Encode a note, splitting it when its duration exceeds one MDX note command.
fn compile_note(note: u16, ticks: u16) -> Result<Vec<MdxCommand>, CompileError> {
    let opcode = FIRST_NOTE + note;
    if !(FIRST_NOTE..=LAST_NOTE).contains(&opcode) {
        return Err(CompileError::InvalidValue {
            command: "note",
            value: i32::from(note),
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
                    value: i32::from(length),
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
                value: name as i32,
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
            value: absolute,
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
    let ticks = match length {
        MmlLength::Denominator(value) => ticks_from_denominator(*value, "note length"),
        MmlLength::Ticks(value) => Ok(*value),
        MmlLength::Sum(values) => values.iter().try_fold(0_u16, |total, value| {
            total
                .checked_add(length_ticks(value)?)
                .ok_or(CompileError::InvalidValue {
                    command: "note length",
                    value: i32::from(u16::MAX),
                })
        }),
        MmlLength::Adjusted { base, adjustments } => {
            adjustments
                .iter()
                .try_fold(length_ticks(base)?, |total, (add, value)| {
                    let value = length_ticks(value)?;
                    if *add {
                        total.checked_add(value)
                    } else {
                        total.checked_sub(value)
                    }
                    .ok_or(CompileError::InvalidValue {
                        command: "note length",
                        value: i32::from(u16::MAX),
                    })
                })
        }
    }?;
    if ticks == 0 {
        return Err(CompileError::InvalidValue {
            command: "note length",
            value: 0,
        });
    }
    Ok(ticks)
}

/// Convert a BPM-style MML tempo into the OPM tempo byte used by MDX.
fn tempo_value(bpm: u32) -> Result<u8, CompileError> {
    if bpm == 0 {
        return Err(CompileError::InvalidValue {
            command: "tempo",
            value: 0,
        });
    }
    if bpm > 78_125 / 16 {
        return Ok(u8::MAX);
    }
    let decrement = 78_125 / (16 * bpm);
    Ok(256_u32.saturating_sub(decrement).min(u32::from(u8::MAX)) as u8)
}

/// Convert a denominator-based length into ticks for a whole note of 192 ticks.
fn ticks_from_denominator(value: u16, command: &'static str) -> Result<u16, CompileError> {
    if value == 0 {
        return Err(CompileError::InvalidValue { command, value: 0 });
    }
    Ok(TICKS_PER_WHOLE / value)
}

/// Convert a signed integer to an MDX unsigned byte value.
fn checked_u8(command: &'static str, value: i32) -> Result<u8, CompileError> {
    u8::try_from(value).map_err(|_| CompileError::InvalidValue { command, value })
}

/// Convert a scaled MML value to an MDX unsigned 16-bit value.
fn checked_u16(command: &'static str, value: u32) -> Result<u16, CompileError> {
    u16::try_from(value).map_err(|_| CompileError::InvalidValue {
        command,
        value: i32::try_from(value).unwrap_or(i32::MAX),
    })
}

/// Convert an MML pitch LFO tuple using MXDRV's waveform-specific scaling.
fn pitch_lfo_values(waveform: u8, period: u16, depth: i16) -> Result<(u16, i16), CompileError> {
    if period == 0 {
        return Err(CompileError::InvalidValue {
            command: "pitch LFO period",
            value: 0,
        });
    }
    let period = i32::from(period);
    let depth = i32::from(depth);
    let (frequency, amplitude) = match waveform & 0x03 {
        0 => (period * 4, depth * 128 / period),
        1 => (period * 2, depth * 256),
        _ => (period * 2, depth * 256 / period),
    };
    Ok((
        checked_u16(
            "pitch LFO period",
            u32::try_from(frequency).unwrap_or(u32::MAX),
        )?,
        i16::try_from(amplitude).map_err(|_| CompileError::InvalidValue {
            command: "pitch LFO amplitude",
            value: amplitude,
        })?,
    ))
}

/// Convert an MML volume LFO tuple using MXDRV's waveform-specific scaling.
fn volume_lfo_values(waveform: u8, period: u16, depth: i16) -> Result<(u16, u16), CompileError> {
    if period == 0 {
        return Err(CompileError::InvalidValue {
            command: "volume LFO period",
            value: 0,
        });
    }
    let period = u32::from(period);
    let depth = i32::from(depth);
    let (frequency, amplitude) = match waveform & 0x03 {
        0 => (period * 4, depth * 16),
        1 => (period * 2, depth * 256),
        _ => (period * 2, depth * 16),
    };
    let amplitude = i16::try_from(amplitude).map_err(|_| CompileError::InvalidValue {
        command: "volume LFO amplitude",
        value: amplitude,
    })? as u16;
    Ok((checked_u16("volume LFO period", frequency)?, amplitude))
}

/// Convert an unscaled signed integer to an MDX signed 16-bit value.
fn checked_signed_i16(command: &'static str, value: i32) -> Result<i16, CompileError> {
    i16::try_from(value).map_err(|_| CompileError::InvalidValue { command, value })
}

/// Map an MML track channel to its zero-based MDX track index.
fn channel_index(channel: char) -> Result<usize, CompileError> {
    match channel {
        'A'..='H' => Ok(channel as usize - 'A' as usize),
        'P'..='W' => Ok(channel as usize - 'P' as usize + 8),
        _ => Err(CompileError::InvalidChannel(channel)),
    }
}

/// Map an MML synchronization channel to the value used by MDX.
/// Return the serialized byte length of a sequence of MDX commands.
fn command_bytes(commands: &[MdxCommand]) -> usize {
    commands
        .iter()
        .filter_map(MdxCommand::to_mdx_bytes)
        .map(|bytes| bytes.len())
        .sum()
}

/// Patch escape offsets for the outermost repeat body.
fn patch_repeat_escape_offsets(
    commands: &mut [MdxCommand],
    body_length: usize,
) -> Result<(), CompileError> {
    let mut position = 0_usize;
    let mut nested_depth = 0_usize;
    for command in commands {
        let command_length = command.to_mdx_bytes().map_or(0, |bytes| bytes.len());
        match command {
            MdxCommand::LoopStart(_) => nested_depth += 1,
            MdxCommand::LoopEnd(_) => nested_depth = nested_depth.saturating_sub(1),
            MdxCommand::LoopEscape(command) if nested_depth == 0 => {
                let offset = i32::try_from(body_length).unwrap_or(i32::MAX)
                    - i32::try_from(position).unwrap_or(i32::MAX)
                    - 2;
                command.offset = checked_signed_i16("repeat escape", offset)?;
            }
            _ => {}
        }
        position = position.saturating_add(command_length);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "source-map")]
    use super::super::mml::parse_with_sources;
    use super::*;
    #[cfg(feature = "source-map")]
    use crate::frontend::SourceFile;

    #[test]
    fn ordinary_lowering_does_not_allocate_output_positions() {
        let document = super::super::parse("A [c4]2").unwrap();
        let (_, positions) = compile_commands(
            &document.tracks[0].commands,
            &mut TrackState::default(),
            0,
            None,
        )
        .unwrap();
        assert_eq!(positions, None);
    }

    #[cfg(feature = "source-map")]
    #[test]
    fn compile_error_locations_cover_notes_lengths_repeats_and_lookahead() {
        for (source, line, column) in [
            ("A a>>>>>>>>>a", 1, 13),
            ("AB r4\nA [r4 [>>>>>>>>a]2]3", 2, 16),
            ("A o0 c", 1, 6),
            ("A c193", 1, 3),
            ("A l193 c", 1, 8),
            ("A o8 c_b", 1, 8),
            ("A c_>>>>>>>>>a", 1, 14),
        ] {
            let document = super::super::parse(source).unwrap();
            let original = compile(&document).unwrap_err();
            let (located_document, sources) = parse_with_sources(source).unwrap();
            let mut context = CompileSourceContext::new(&sources);
            assert_eq!(
                compile_internal(&located_document, Some(&mut context)).unwrap_err(),
                original
            );
            assert_eq!(
                context
                    .error_span
                    .and_then(|span| SourceFile::new(source).unwrap().position(span.start()))
                    .map(|position| (position.line_number, position.column)),
                Some((line, column)),
                "{source}"
            );
        }
        let (mut document, mut sources) = parse_with_sources("A r4").unwrap();
        document.tracks[0].commands = vec![MmlCommand::Rest { length: Some(4) }; 65_536];
        let rest_source = sources[0][0].clone();
        sources[0].resize(65_536, rest_source);
        let mut context = CompileSourceContext::new(&sources);
        let result = compile_internal(&document, Some(&mut context));
        assert!(matches!(result, Err(CompileError::Builder(_))));
        assert_eq!(context.error_span, None);
    }

    #[cfg(feature = "source-map")]
    #[test]
    fn diagnostic_mapping_preserves_compilation_and_maps_split_notes_and_repeats() {
        for source in [
            "AB @42 c%600& r%300 d4",
            "AB @42 [r4\nA [c4]2\nAB d4]3\n",
            "A @42 c4_d4 e4",
            "A @42 c4_>d4 e4",
            "A L r4 c4\nA d4",
            "AP @42 c%600& d4",
            "P [c4 [d4]2]3\nA @42 e4",
            "A r4 ! c4",
        ] {
            let (document, sources) = parse_with_sources(source).unwrap();
            let mut context = CompileSourceContext::new(&sources);
            let diagnosed = compile_internal(&document, Some(&mut context)).unwrap();
            let normal = compile(&document).unwrap();
            assert_eq!(
                diagnosed.to_bytes().unwrap(),
                normal.to_bytes().unwrap(),
                "{source}"
            );
            for (track, commands) in diagnosed.tracks.iter().enumerate() {
                for (index, command) in commands.iter().enumerate() {
                    if matches!(command, MdxCommand::Note(_)) {
                        let position = SourceFile::new(source)
                            .unwrap()
                            .position(context.output_map[track][index].unwrap().start())
                            .unwrap();
                        let character = source
                            .lines()
                            .nth(position.line_number - 1)
                            .unwrap()
                            .chars()
                            .nth(position.column - 1)
                            .unwrap();
                        assert!(matches!(character, 'a'..='g'), "{source}: {position:?}");
                    }
                }
            }
        }
    }
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
        assert_eq!(tempo_value(u32::MAX), Ok(u8::MAX));
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

    #[test]
    fn rejects_note_and_rest_lengths_that_resolve_to_zero_ticks() {
        for source in ["A c193\n", "A r193\n", "A c4~4\n", "A r4~4\n"] {
            let document = parse(source).unwrap();
            assert!(matches!(
                compile(&document),
                Err(CompileError::InvalidValue {
                    command: "note length",
                    value: 0
                })
            ));
        }
    }
}
