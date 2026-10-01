//! MXDRV MML parsing and syntax tree types.
//!
//! The [`parse`] function converts an MML source string into an [`MmlDocument`]
//! containing metadata, voice definitions, and per-channel command streams.
//! The [`format_tree`] function renders that typed document for inspection;
//! neither parsing nor formatting produces an MDX document.

use std::fmt;

use pest::Parser;
use pest::error::{ErrorVariant, LineColLocation};
use pest_derive::Parser;

#[cfg(feature = "source-map")]
use crate::diagnostic::Diagnostic;
use crate::source::Span;

#[derive(Parser)]
#[grammar = "mdx/mml.pest"]
struct MmlParser;

/// Parsed MXDRV MML metadata, voice definitions, and channel commands.
///
/// This AST owns its values without retaining source text or locations. Parsing
/// and compilation are separate; see [`parse`] and [`super::compile::compile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlDocument {
    /// The last `#title` value, or `None` when not specified.
    pub title: Option<String>,
    /// The last `#pcmfile` PDX filename, or `None` when not specified.
    pub pcm_file: Option<String>,
    /// Voice definitions in source order.
    pub voices: Vec<MmlVoice>,
    /// Tracks in first-seen channel order, with repeated channel lines merged.
    pub tracks: Vec<MmlTrack>,
}

/// A channel's typed command sequence, including nested repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlTrack {
    /// The MXDRV channel name: `A` through `H`, or `P` through `W`.
    pub channel: char,
    /// Commands in source order after merging lines for this channel.
    pub commands: Vec<MmlCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One-based MML source coordinates, with columns counted in characters.
pub struct SourcePosition {
    /// One-based source line number.
    pub line_number: usize,
    /// One-based Unicode scalar-value column, not a byte or UTF-16 offset.
    pub column: usize,
}

impl SourcePosition {
    pub(crate) fn from_span(source: &str, span: Span) -> Self {
        let (line_number, column) = pest::Position::new(source, span.start())
            .expect("source span must start at a UTF-8 boundary")
            .line_col();
        Self {
            line_number,
            column,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A command's source range and the locations of its nested repeat body.
pub struct CommandSource {
    /// The command range, or the opening bracket for a repeat.
    pub position: Span,
    /// The closing bracket and optional repeat count.
    pub end_position: Option<Span>,
    /// Nested command ranges for repeats; empty for non-repeat commands.
    pub body: Vec<CommandSource>,
}

#[derive(Debug, Default)]
pub(super) struct MmlSourceMap {
    tracks: Vec<Vec<CommandSource>>,
    #[cfg(feature = "source-map")]
    pub title: Option<Span>,
    #[cfg(feature = "source-map")]
    pub pcm_file: Option<Span>,
    #[cfg(feature = "source-map")]
    pub voices: Vec<super::frontend::VoiceSource>,
    #[cfg(feature = "source-map")]
    pub arguments: Vec<super::frontend::ArgumentSource>,
}

impl std::ops::Deref for MmlSourceMap {
    type Target = Vec<Vec<CommandSource>>;

    fn deref(&self) -> &Self::Target {
        &self.tracks
    }
}

impl std::ops::DerefMut for MmlSourceMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tracks
    }
}

type RepeatFrame = (Vec<MmlCommand>, Span, Vec<CommandSource>);

impl CommandSource {
    /// Bounding range of a command, including both delimiters for repeats.
    ///
    /// A multiline repeat's bounding range may include interleaved lines for
    /// other channels; [`Self::body`] identifies its actual nested commands.
    ///
    /// # Panics
    ///
    /// Panics if manually supplied delimiter ranges have an end before the start.
    #[cfg(feature = "source-map")]
    pub fn full_span(&self) -> Span {
        Span::new(
            self.position.start(),
            self.end_position.unwrap_or(self.position).end(),
        )
        .expect("command ranges are ordered")
    }

    fn new(position: Span) -> Self {
        Self {
            position,
            end_position: None,
            body: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedCommand {
    Command(MmlCommand, Option<Span>),
    RepeatStart(Span),
    RepeatEnd { count: u16, position: Span },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTrack {
    channel: char,
    commands: Vec<ParsedCommand>,
}

/// A numbered MXDRV FM voice definition with raw OPM parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlVoice {
    /// The voice number following `@`.
    pub number: u8,
    /// Parameters in source order: four groups of 11 operator values and 3 globals.
    pub values: Vec<u8>,
}

/// A typed MXDRV MML command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmlCommand {
    /// Set the OPM tempo using `@t`.
    OpmTempo(u8),
    /// Select a voice using `@N`.
    VoiceSelect(u8),
    /// A parser directive or preserved block comment.
    Directive(String),
    /// Repeat a sequence of commands.
    Repeat {
        /// Commands inside the repeat block.
        body: Vec<MmlCommand>,
        /// Number of repetitions.
        count: u16,
    },
    /// Set tempo using `t` in quarter notes per second.
    Tempo(u32),
    /// Play a named note.
    Note {
        /// Note name from `a` through `g`.
        name: char,
        /// Optional sharp or flat modifier.
        accidental: Option<Accidental>,
        /// Optional note-length denominator.
        length: Option<u16>,
    },
    /// Play a note with an extended length expression.
    ExtendedNote {
        /// Note name from `a` through `g`.
        name: char,
        /// Optional sharp or flat modifier.
        accidental: Option<Accidental>,
        /// Extended note length.
        length: MmlLength,
    },
    /// Play a note by numeric note number.
    NumericNote {
        /// Numeric note number.
        note: u8,
        /// Optional note length.
        length: Option<MmlLength>,
    },
    /// Rest for an optional denominator-based length.
    Rest {
        /// Optional rest length.
        length: Option<u16>,
    },
    /// Rest with an extended length expression.
    ExtendedRest(MmlLength),
    /// Set the octave.
    Octave(u8),
    /// Decrease the octave.
    OctaveDown,
    /// Increase the octave.
    OctaveUp,
    /// Set the default note length.
    DefaultLength(MmlLength),
    /// Re-apply the current default note length.
    DefaultLengthReset,
    /// Set the gate value.
    Gate(u8),
    /// Set the fine gate value using `@q`.
    FineGate(u16),
    /// Enable portamento for the next note.
    Portamento,
    /// Enable legato for the next note.
    Legato,
    /// Set volume.
    Volume(u8),
    /// Set fine volume using `@v`.
    FineVolume(u8),
    /// Decrease volume.
    VolumeDown,
    /// Increase volume.
    VolumeUp,
    /// Set stereo pan.
    Pan(u8),
    /// Mark the beginning of a loop.
    LoopStart,
    /// Set detune.
    Detune(i16),
    /// Escape from the current loop.
    LoopEscape,
    /// Write an OPM register.
    RegisterWrite {
        /// Register number.
        register: u8,
        /// Value written to the register.
        value: u8,
    },
    /// Set key-on delay.
    KeyOnDelay(u8),
    /// Set noise frequency.
    NoiseFrequency(u8),
    /// Send a synchronization event to another channel value.
    SyncSend(u8),
    /// Wait for a synchronization event.
    SyncWait,
    /// Configure pitch LFO.
    PitchLfo {
        /// LFO waveform number.
        waveform: u8,
        /// LFO period.
        period: u16,
        /// LFO amplitude.
        amplitude: i16,
    },
    /// Enable pitch LFO.
    PitchLfoOn,
    /// Disable pitch LFO.
    PitchLfoOff,
    /// Configure volume LFO.
    VolumeLfo {
        /// LFO waveform number.
        waveform: u8,
        /// LFO period.
        period: u16,
        /// LFO amplitude.
        amplitude: i16,
    },
    /// Enable volume LFO.
    VolumeLfoOn,
    /// Disable volume LFO.
    VolumeLfoOff,
    /// Set LFO delay.
    LfoDelay(u8),
    /// Configure OPM LFO.
    OpmLfo {
        /// OPM LFO waveform.
        waveform: u8,
        /// LFO frequency.
        lfrq: u8,
        /// Phase modulation depth.
        pmd: u8,
        /// Amplitude modulation depth.
        amd: u8,
        /// Phase modulation sensitivity.
        pms: u8,
        /// Amplitude modulation sensitivity.
        ams: u8,
        /// Key-sync setting.
        key_sync: u8,
    },
    /// Enable OPM LFO.
    OpmLfoOn,
    /// Disable OPM LFO.
    OpmLfoOff,
    /// Ignore the remainder of the channel input.
    Ignore,
    /// Set PCM frequency.
    PcmFrequency(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A note-length expression.
pub enum MmlLength {
    /// A conventional denominator, such as `4` for a quarter note.
    Denominator(u16),
    /// A length expressed directly in ticks using `%N`.
    Ticks(u16),
    /// A sum of multiple length expressions joined by `^`.
    Sum(Vec<MmlLength>),
    /// A length expression containing additive or subtractive adjustments.
    Adjusted {
        /// Base length before adjustments.
        base: Box<MmlLength>,
        /// Adjustments as `(add, length)`, where `false` means subtract.
        adjustments: Vec<(bool, MmlLength)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A note accidental.
pub enum Accidental {
    /// Raise the note by one semitone.
    Sharp,
    /// Lower the note by one semitone.
    Flat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// An error encountered while parsing or validating MML.
pub enum ParseError {
    /// The input does not match the MML grammar.
    Syntax(String),
    /// A numeric argument is outside the command's supported range.
    InvalidValue {
        /// Command associated with the invalid value.
        command: &'static str,
        /// Parsed numeric value.
        value: i32,
        /// Inclusive lower bound.
        min: i32,
        /// Inclusive upper bound.
        max: i32,
        /// One-based source line containing the value.
        line_number: usize,
        /// One-based source column containing the value.
        column: usize,
        /// One-based exclusive end column of the value.
        end_column: usize,
        /// Source line containing the value.
        line_text: String,
    },
}

impl From<pest::error::Error<Rule>> for ParseError {
    fn from(error: pest::error::Error<Rule>) -> Self {
        Self::Syntax(error.to_string())
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => formatter.write_str(error),
            Self::InvalidValue {
                command,
                value,
                min,
                max,
                line_number,
                column,
                end_column,
                line_text,
            } => write!(
                formatter,
                "MML value error at line {line_number}, columns {column}-{end_column}\n  {line_text}\n  {}{}\n  reason: {command} value {value} is outside the range {min}..={max}",
                " ".repeat(column.saturating_sub(1)),
                "^".repeat(end_column.saturating_sub(*column).max(1)),
            ),
        }
    }
}

impl std::error::Error for ParseError {}

pub(super) enum ParseFailure {
    Syntax(Box<pest::error::Error<Rule>>),
    Repeat {
        span: Span,
        message: &'static str,
        expected: &'static str,
    },
    Value {
        span: Span,
        command: &'static str,
        value: i32,
        min: i32,
        max: i32,
    },
    Overflow {
        span: Span,
        command: &'static str,
    },
    SourceTooLarge,
}

impl ParseFailure {
    fn into_legacy(self, source: &str) -> ParseError {
        match self {
            Self::Syntax(error) => ParseError::Syntax(format_syntax_error(&error, source)),
            Self::Repeat {
                span,
                message,
                expected,
            } => ParseError::Syntax(format_repeat_error(source, span, message, expected)),
            Self::Value {
                span,
                command,
                value,
                min,
                max,
            } => {
                let position = SourcePosition::from_span(source, span);
                ParseError::InvalidValue {
                    command,
                    value,
                    min,
                    max,
                    line_number: position.line_number,
                    column: position.column,
                    end_column: position.column + span.text(source).unwrap_or("").chars().count(),
                    line_text: source
                        .lines()
                        .nth(position.line_number - 1)
                        .unwrap_or("")
                        .to_owned(),
                }
            }
            Self::Overflow { span, command } => {
                let position = SourcePosition::from_span(source, span);
                let end_column = position.column + span.text(source).unwrap_or("").chars().count();
                let line_text = source.lines().nth(position.line_number - 1).unwrap_or("");
                ParseError::Syntax(format!(
                    "MML value error at line {}, columns {}-{end_column}\n  {line_text}\n  {}{}\n  reason: {command} integer is too large to represent",
                    position.line_number,
                    position.column,
                    " ".repeat(position.column - 1),
                    "^".repeat(end_column.saturating_sub(position.column).max(1)),
                ))
            }
            Self::SourceTooLarge => {
                ParseError::Syntax("MML source exceeds the 32-bit byte range".to_owned())
            }
        }
    }

    #[cfg(feature = "source-map")]
    pub(super) fn into_diagnostic(self) -> Diagnostic {
        match self {
            Self::Syntax(error) => {
                let span = match error.location {
                    pest::error::InputLocation::Pos(offset) => Span::new(offset, offset),
                    pest::error::InputLocation::Span((start, end)) => Span::new(start, end),
                };
                let message = match error.variant {
                    ErrorVariant::ParsingError { positives, .. } => format!(
                        "unexpected input; expected {}",
                        positives
                            .iter()
                            .map(describe_rule)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    ErrorVariant::CustomError { message } => message,
                };
                Diagnostic::error("mmlx.syntax", message, span)
            }
            Self::Repeat { span, message, .. } => {
                Diagnostic::error("mmlx.repeat", message, Some(span))
            }
            Self::Value {
                span,
                command,
                value,
                min,
                max,
            } => Diagnostic::error(
                "mmlx.invalid-value",
                format!("{command} value {value} is outside the range {min}..={max}"),
                Some(span),
            ),
            Self::Overflow { span, command } => Diagnostic::error(
                "mmlx.integer-overflow",
                format!("{command} integer is too large to represent"),
                Some(span),
            ),
            Self::SourceTooLarge => Diagnostic::error(
                "mmlx.source-too-large",
                "MML source exceeds the 32-bit byte range",
                None,
            ),
        }
    }
}

/// Parse an MXDRV MML source string.
///
/// Whitespace and comments accepted by the grammar are ignored. The returned
/// document retains metadata, voice definitions, tracks, and typed commands;
/// it does not compile the commands into MDX bytes.
///
/// # Errors
///
/// Returns [`ParseError::Syntax`] when the source does not match the grammar or
/// [`ParseError::InvalidValue`] when a parsed value violates a command range.
///
/// # Examples
///
/// ```
/// let source = "#title \"Example\"\nA c4 d4 e4";
/// let document = mmlx::mdx::parse(source).expect("valid MML source");
///
/// assert_eq!(document.title.as_deref(), Some("Example"));
/// assert_eq!(document.tracks[0].channel, 'A');
/// ```
pub fn parse(source: &str) -> Result<MmlDocument, ParseError> {
    parse_internal(source, None).map_err(|error| error.into_legacy(source))
}

pub(super) fn parse_with_positions(
    source: &str,
) -> Result<(MmlDocument, MmlSourceMap), ParseError> {
    let mut positions = MmlSourceMap::default();
    let document =
        parse_internal(source, Some(&mut positions)).map_err(|error| error.into_legacy(source))?;
    Ok((document, positions))
}

pub(super) fn parse_internal(
    source: &str,
    mut positions: Option<&mut MmlSourceMap>,
) -> Result<MmlDocument, ParseFailure> {
    if source.len() > u32::MAX as usize {
        return Err(ParseFailure::SourceTooLarge);
    }
    let mut document = MmlDocument {
        title: None,
        pcm_file: None,
        voices: Vec::new(),
        tracks: Vec::new(),
    };
    let mut parsed_tracks = Vec::new();

    let document_pair = MmlParser::parse(Rule::document, source)
        .map_err(|error| ParseFailure::Syntax(Box::new(error)))?
        .next()
        .expect("document rule must produce a pair");
    for pair in document_pair.into_inner() {
        let Some(line) = pair.into_inner().next() else {
            continue;
        };
        #[cfg(feature = "source-map")]
        if let Some(positions) = &mut positions {
            capture_syntax(&line, positions);
        }
        match line.as_rule() {
            Rule::title => document.title = Some(parse_string(line)),
            Rule::pcmfile => document.pcm_file = Some(parse_string(line)),
            Rule::voice => document.voices.push(parse_voice(line)?),
            Rule::track => {
                append_parsed_tracks(&mut parsed_tracks, parse_track(line, positions.is_some())?)
            }
            Rule::blank | Rule::comment | Rule::block_comment => {}
            rule => unreachable!("unexpected line rule: {rule:?}"),
        }
    }

    for track in parsed_tracks {
        let mut track_positions = Vec::new();
        document.tracks.push(MmlTrack {
            channel: track.channel,
            commands: assemble_repeats(
                track.commands,
                source,
                positions.as_ref().map(|_| &mut track_positions),
            )?,
        });
        if let Some(positions) = &mut positions {
            positions.push(track_positions);
        }
    }
    Ok(document)
}

/// Append parsed source lines to their channel's existing event track.
fn append_parsed_tracks(tracks: &mut Vec<ParsedTrack>, incoming: Vec<ParsedTrack>) {
    for track in incoming {
        if let Some(existing) = tracks
            .iter_mut()
            .find(|existing: &&mut ParsedTrack| existing.channel == track.channel)
        {
            existing.commands.extend(track.commands);
        } else {
            tracks.push(track);
        }
    }
}

/// Assemble flat loop events into nested repeat commands within one channel.
fn assemble_repeats(
    commands: Vec<ParsedCommand>,
    _source: &str,
    mut positions: Option<&mut Vec<CommandSource>>,
) -> Result<Vec<MmlCommand>, ParseFailure> {
    let mut output = Vec::new();
    let mut stack: Vec<RepeatFrame> = Vec::new();

    for command in commands {
        match command {
            ParsedCommand::RepeatStart(position) => stack.push((Vec::new(), position, Vec::new())),
            ParsedCommand::RepeatEnd { count, position } => {
                let Some((body, start_position, body_positions)) = stack.pop() else {
                    return Err(ParseFailure::Repeat {
                        span: position,
                        message: "unexpected ']'",
                        expected: "repeat start '['",
                    });
                };
                let repeat_source = positions.as_ref().map(|_| CommandSource {
                    position: start_position,
                    end_position: Some(position),
                    body: body_positions,
                });
                append_assembled_command(
                    &mut output,
                    &mut stack,
                    MmlCommand::Repeat { body, count },
                    positions.as_deref_mut(),
                    repeat_source,
                );
            }
            ParsedCommand::Command(command, position) => append_assembled_command(
                &mut output,
                &mut stack,
                command,
                positions.as_deref_mut(),
                position.map(CommandSource::new),
            ),
        }
    }

    if let Some((_, position, _)) = stack.last() {
        return Err(ParseFailure::Repeat {
            span: *position,
            message: "repeat is not terminated",
            expected: "repeat end ']'",
        });
    }
    Ok(output)
}

/// Format a repeat-structure error with its source line, marker, and expectation.
fn format_repeat_error(source: &str, span: Span, message: &str, expected: &str) -> String {
    let position = SourcePosition::from_span(source, span);
    let end_column = position.column + 1;
    let line_text = source
        .lines()
        .nth(position.line_number.saturating_sub(1))
        .unwrap_or("");
    format!(
        "MML syntax error at line {}, columns {}-{end_column}: {message}\n  {line_text}\n  {}^\n  expected: {expected}",
        position.line_number,
        position.column,
        " ".repeat(position.column.saturating_sub(1)),
    )
}

/// Append a command to the innermost open repeat, or to the top-level output.
fn append_assembled_command(
    output: &mut Vec<MmlCommand>,
    stack: &mut [RepeatFrame],
    command: MmlCommand,
    positions: Option<&mut Vec<CommandSource>>,
    position: Option<CommandSource>,
) {
    if let Some((body, _, body_positions)) = stack.last_mut() {
        body.push(command);
        if let Some(position) = position {
            body_positions.push(position);
        }
    } else {
        output.push(command);
        if let (Some(positions), Some(position)) = (positions, position) {
            positions.push(position);
        }
    }
}

/// Format a parsed MML document as a readable syntax tree.
///
/// This uses the typed AST produced by [`parse`]. The original Pest pairs and
/// source trivia are not retained after parsing, so whitespace and comments
/// are not included in the formatted tree.
///
/// # Examples
///
/// ```
/// let document = mmlx::mdx::parse("A c4 d4").expect("valid MML source");
/// let tree = mmlx::mdx::format_tree(&document);
///
/// assert!(tree.contains("MmlDocument"));
/// ```
pub fn format_tree(document: &MmlDocument) -> String {
    format!("{document:#?}")
}

/// Convert a PEST error into a compact, source-oriented diagnostic.
fn format_syntax_error(error: &pest::error::Error<Rule>, source: &str) -> String {
    let (line_number, column, end_column) = match error.line_col {
        LineColLocation::Pos((line, column)) => (line, column, column + 1),
        LineColLocation::Span((line, column), (end_line, end_column)) => {
            let end_column = if line == end_line {
                end_column.max(column + 1)
            } else {
                column + 1
            };
            (line, column, end_column)
        }
    };
    let line_text = source
        .lines()
        .nth(line_number.saturating_sub(1))
        .unwrap_or("");
    let actual = line_text
        .chars()
        .nth(column.saturating_sub(1))
        .map(|character| format!("{character:?}"))
        .unwrap_or_else(|| "<end of input>".to_owned());
    let expected = match &error.variant {
        ErrorVariant::ParsingError { positives, .. } if positives.len() > 8 => {
            "a valid MML command or end of line".to_owned()
        }
        ErrorVariant::ParsingError { positives, .. } => positives
            .iter()
            .map(describe_rule)
            .collect::<Vec<_>>()
            .join(", "),
        ErrorVariant::CustomError { message } => message.clone(),
    };
    let expected = if expected.is_empty() {
        "a valid MML command".to_owned()
    } else {
        expected
    };

    format!(
        "MML syntax error at line {line_number}, columns {column}-{end_column}: unexpected {actual}\n  {line_text}\n  {}{}\n  expected: {expected}",
        " ".repeat(column.saturating_sub(1)),
        "^".repeat(end_column.saturating_sub(column).max(1)),
    )
}

/// Turn a grammar rule into a user-facing expectation description.
fn describe_rule(rule: &Rule) -> String {
    match rule {
        Rule::number => "unsigned integer".to_owned(),
        other => format!("{other:?}"),
    }
}

fn numeric_span(pair: &pest::iterators::Pair<'_, Rule>, byte_offset: usize) -> Span {
    let width = pair.as_str()[byte_offset..]
        .bytes()
        .enumerate()
        .take_while(|(index, byte)| byte.is_ascii_digit() || (*index == 0 && *byte == b'-'))
        .count();
    let start = pair.as_span().start() + byte_offset;
    Span::new(start, start + width).expect("source length was checked before parsing")
}

fn format_numeric_overflow(
    pair: &pest::iterators::Pair<'_, Rule>,
    character_offset: usize,
    command: &'static str,
) -> ParseFailure {
    ParseFailure::Overflow {
        span: numeric_span(pair, character_offset),
        command,
    }
}

/// Return a structured error when a value is outside an inclusive range.
fn invalid_value_at(
    pair: &pest::iterators::Pair<'_, Rule>,
    character_offset: usize,
    command: &'static str,
    value: i32,
    min: i32,
    max: i32,
) -> ParseFailure {
    ParseFailure::Value {
        span: numeric_span(pair, character_offset),
        command,
        value,
        min,
        max,
    }
}

/// Return an error when a source value is outside an inclusive range.
fn validate_range(
    pair: &pest::iterators::Pair<'_, Rule>,
    character_offset: usize,
    command: &'static str,
    value: i32,
    min: i32,
    max: i32,
) -> Result<(), ParseFailure> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(invalid_value_at(
            pair,
            character_offset,
            command,
            value,
            min,
            max,
        ))
    }
}

/// Parse an integer from a grammar pair and validate its semantic range.
fn validate_pair_range(
    pair: &pest::iterators::Pair<'_, Rule>,
    command: &'static str,
    min: i32,
    max: i32,
) -> Result<i32, ParseFailure> {
    let value = pair
        .as_str()
        .parse::<i32>()
        .map_err(|_| format_numeric_overflow(pair, 0, command))?;
    validate_range(pair, 0, command, value, min, max)?;
    Ok(value)
}

/// Convert a `voice` grammar pair into a voice definition.
fn parse_voice(voice: pest::iterators::Pair<'_, Rule>) -> Result<MmlVoice, ParseFailure> {
    let mut numbers = voice
        .clone()
        .into_inner()
        .filter(|pair| matches!(pair.as_rule(), Rule::voice_number | Rule::number))
        .map(|number| {
            validate_pair_range(&number, "voice parameter", 0, 255).map(|value| value as u8)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter();
    let number = numbers.next().expect("voice must have a number");
    let values = numbers.collect();
    Ok(MmlVoice { number, values })
}

/// Extract the quoted string from a metadata grammar pair.
fn parse_string(line: pest::iterators::Pair<'_, Rule>) -> String {
    line.into_inner()
        .find(|pair| pair.as_rule() == Rule::string)
        .map(|pair| pair.as_str()[1..pair.as_str().len() - 1].to_owned())
        .expect("metadata line must contain a string")
}

/// Convert a track line into one typed track per channel in its prefix.
fn parse_track(
    line: pest::iterators::Pair<'_, Rule>,
    capture_positions: bool,
) -> Result<Vec<ParsedTrack>, ParseFailure> {
    let mut children = line.into_inner();
    let channels = children
        .next()
        .expect("track must have a channel")
        .as_str()
        .chars()
        .collect::<Vec<_>>();
    let commands = children
        .filter(|pair| pair.as_rule() != Rule::block_comment)
        .map(|pair| parse_parsed_command(pair, capture_positions))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(channels
        .into_iter()
        .map(|channel| ParsedTrack {
            channel,
            commands: commands.clone(),
        })
        .collect())
}

/// Convert one grammar pair into an intermediate command, validating its values.
fn parse_parsed_command(
    pair: pest::iterators::Pair<'_, Rule>,
    capture_positions: bool,
) -> Result<ParsedCommand, ParseFailure> {
    Ok(match pair.as_rule() {
        Rule::repeat_start => ParsedCommand::RepeatStart(source_span(&pair)),
        Rule::repeat_end => {
            let position = source_span(&pair);
            let count = pair
                .into_inner()
                .next()
                .map(|count| validate_pair_range(&count, "repeat", 2, 255))
                .transpose()?
                .unwrap_or(2) as u16;
            ParsedCommand::RepeatEnd { count, position }
        }
        _ => {
            validate_command_pair(&pair)?;
            let position = capture_positions.then(|| source_span(&pair));
            ParsedCommand::Command(parse_command(pair), position)
        }
    })
}

fn source_span(pair: &pest::iterators::Pair<'_, Rule>) -> Span {
    let span = pair.as_span();
    let end = if matches!(pair.as_rule(), Rule::title | Rule::pcmfile | Rule::voice) {
        span.end()
    } else if let Some(child) = pair.clone().into_inner().next_back() {
        source_span(&child).end()
    } else if matches!(
        pair.as_rule(),
        Rule::repeat_end | Rule::rest | Rule::default_length
    ) {
        span.start() + 1
    } else {
        span.end()
    };
    Span::new(span.start(), end).expect("source length was checked before parsing")
}

#[cfg(feature = "source-map")]
fn capture_syntax(pair: &pest::iterators::Pair<'_, Rule>, sources: &mut MmlSourceMap) {
    use super::frontend::{ArgumentSource, VoiceSource};

    fn numeric_ranges(pair: pest::iterators::Pair<'_, Rule>, output: &mut Vec<Span>) {
        if matches!(pair.as_rule(), Rule::number | Rule::signed_number) {
            output.push(source_span(&pair));
        } else {
            for child in pair.into_inner() {
                numeric_ranges(child, output);
            }
        }
    }

    match pair.as_rule() {
        Rule::title => sources.title = Some(source_span(pair)),
        Rule::pcmfile => sources.pcm_file = Some(source_span(pair)),
        Rule::voice => {
            let mut ranges = Vec::new();
            numeric_ranges(pair.clone(), &mut ranges);
            sources.voices.push(VoiceSource {
                span: source_span(pair),
                number: ranges.remove(0),
                parameters: ranges,
            });
        }
        Rule::track => {
            for command in pair.clone().into_inner().skip(1) {
                let mut ranges = Vec::new();
                numeric_ranges(command.clone(), &mut ranges);
                sources
                    .arguments
                    .extend(ranges.into_iter().map(|span| ArgumentSource {
                        command: source_span(&command),
                        span,
                    }));
            }
        }
        _ => {}
    }
}

/// Validate numeric arguments before converting them into the AST's narrow integer types.
fn validate_command_pair(pair: &pest::iterators::Pair<'_, Rule>) -> Result<(), ParseFailure> {
    let children = pair.clone().into_inner().collect::<Vec<_>>();
    let check = |index, command, min, max| {
        validate_pair_range(&children[index], command, min, max).map(|_| ())
    };

    match pair.as_rule() {
        Rule::tempo => check(0, "tempo", 19, 4882)?,
        Rule::opm_tempo => check(0, "@t", 0, 255)?,
        Rule::voice_select => check(0, "voice", 0, 255)?,
        Rule::note => {
            for child in &children {
                if child.as_rule() == Rule::length_expression {
                    validate_length_pair(child)?;
                }
            }
        }
        Rule::numeric_note => {
            check(0, "note number", 0, 95)?;
            if let Some(length) = children.get(1) {
                validate_length_pair(length)?;
            }
        }
        Rule::rest | Rule::default_length => {
            for child in &children {
                validate_length_pair(child)?;
            }
        }
        Rule::octave => check(0, "octave", 0, 8)?,
        Rule::gate => check(0, "q", 1, 8)?,
        Rule::fine_gate => check(0, "@q", 1, 256)?,
        Rule::volume => check(0, "v", 0, 15)?,
        Rule::fine_volume => check(0, "@v", 0, 127)?,
        Rule::pan => check(0, "p", 0, 3)?,
        Rule::detune => check(0, "D", -32767, 32767)?,
        Rule::register_write => {
            check(0, "register", 0, 255)?;
            check(1, "register value", 0, 255)?;
        }
        Rule::key_on_delay => check(0, "k", 0, 255)?,
        Rule::noise_frequency => check(0, "w", 0, 31)?,
        Rule::sync_send => {
            if children[0]
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            {
                check(0, "sync channel", 0, 255)?;
            }
        }
        Rule::pitch_lfo | Rule::volume_lfo => {
            check(0, "LFO waveform", 0, 7)?;
            check(1, "LFO period", 0, u16::MAX as i32)?;
            check(2, "LFO amplitude", i16::MIN as i32, i16::MAX as i32)?;
        }
        Rule::lfo_delay => check(0, "MD", 0, 255)?,
        Rule::opm_lfo => {
            check(0, "OPM LFO waveform", 0, 3)?;
            for index in 1..6 {
                check(index, "OPM LFO value", 0, 255)?;
            }
            check(6, "OPM LFO key sync", 0, 1)?;
        }
        Rule::pcm_frequency => check(0, "PCM frequency", 0, 12)?,
        _ => {}
    }
    Ok(())
}

/// Validate every scalar in a length expression before parsing it as `u16`.
fn validate_length_pair(pair: &pest::iterators::Pair<'_, Rule>) -> Result<(), ParseFailure> {
    let expression = if pair.as_rule() == Rule::duration {
        pair.clone()
            .into_inner()
            .next()
            .expect("duration has a length expression")
    } else {
        pair.clone()
    };
    let mut total = 0_i32;
    let mut last_value_offset = 0;
    let children = expression.clone().into_inner().collect::<Vec<_>>();
    let has_adjustment = children
        .iter()
        .any(|child| child.as_rule() == Rule::length_operator && child.as_str() == "~");

    for term in children
        .iter()
        .filter(|child| child.as_rule() == Rule::length_term)
    {
        let mut term_children = term.clone().into_inner();
        let first = term_children.next().expect("length term has a value");
        let (ticks, digits) = if first.as_rule() == Rule::length_ticks {
            (
                true,
                term_children.next().expect("tick length has a number"),
            )
        } else {
            (false, first)
        };
        let dots = term_children
            .filter(|child| child.as_rule() == Rule::length_dot)
            .count();
        let value_offset = digits.as_span().start() - expression.as_span().start();
        last_value_offset = value_offset;
        let command = if ticks {
            "note tick length"
        } else {
            "note length"
        };
        let max = if ticks { i32::from(u16::MAX) } else { 256 };
        let mut value = digits
            .as_str()
            .parse::<i32>()
            .map_err(|_| format_numeric_overflow(&expression, value_offset, command))?;
        validate_range(&expression, value_offset, command, value, 1, max)?;
        let mut term_total = value;
        for _ in 0..dots {
            value = if ticks {
                value / 2
            } else {
                value.checked_mul(2).ok_or_else(|| {
                    invalid_value_at(&expression, value_offset, command, i32::MAX, 1, max)
                })?
            };
            validate_range(&expression, value_offset, command, value, 1, max)?;
            term_total = term_total.checked_add(value).ok_or_else(|| {
                invalid_value_at(
                    &expression,
                    value_offset,
                    "note length",
                    i32::MAX,
                    1,
                    i32::from(u16::MAX),
                )
            })?;
        }
        total = total.checked_add(term_total).ok_or_else(|| {
            invalid_value_at(
                &expression,
                value_offset,
                "note length",
                i32::MAX,
                1,
                i32::from(u16::MAX),
            )
        })?;
    }

    if !has_adjustment {
        validate_range(
            &expression,
            last_value_offset,
            "note length",
            total,
            1,
            i32::from(u16::MAX),
        )?;
    }
    Ok(())
}

/// Convert one command grammar pair into its typed AST representation.
fn parse_command(pair: pest::iterators::Pair<'_, Rule>) -> MmlCommand {
    match pair.as_rule() {
        Rule::block_comment => MmlCommand::Directive(pair.as_str().to_owned()),
        Rule::tempo => MmlCommand::Tempo(
            pair.into_inner()
                .next()
                .expect("tempo must have a number")
                .as_str()
                .parse()
                .expect("number is valid"),
        ),
        Rule::opm_tempo => MmlCommand::OpmTempo(parse_single_u16(pair) as u8),
        Rule::voice_select => MmlCommand::VoiceSelect(parse_single_u16(pair) as u8),
        Rule::note => {
            let mut children = pair.into_inner();
            let name = children
                .next()
                .expect("note must have a name")
                .as_str()
                .chars()
                .next()
                .unwrap();
            let mut accidental = None;
            let mut length = None;
            for child in children {
                match child.as_rule() {
                    Rule::accidental => {
                        accidental = Some(match child.as_str() {
                            "+" => Accidental::Sharp,
                            "-" => Accidental::Flat,
                            _ => unreachable!(),
                        })
                    }
                    Rule::number => length = Some(child.as_str().parse().expect("number is valid")),
                    Rule::length_expression => match parse_length(child) {
                        MmlLength::Denominator(value) => length = Some(value),
                        length => {
                            return MmlCommand::ExtendedNote {
                                name,
                                accidental,
                                length,
                            };
                        }
                    },
                    _ => unreachable!("unexpected note rule: {:?}", child.as_rule()),
                }
            }
            MmlCommand::Note {
                name,
                accidental,
                length,
            }
        }
        Rule::numeric_note => {
            let mut children = pair.into_inner();
            let note = parse_pair_u16(children.next().expect("numeric note has a value")) as u8;
            let length = children.next().map(parse_duration);
            MmlCommand::NumericNote { note, length }
        }
        Rule::rest => {
            let length = pair.into_inner().next();
            match length {
                Some(length) if length.as_rule() == Rule::length_expression => {
                    match parse_length(length) {
                        MmlLength::Denominator(value) => MmlCommand::Rest {
                            length: Some(value),
                        },
                        length => MmlCommand::ExtendedRest(length),
                    }
                }
                Some(length) => MmlCommand::Rest {
                    length: Some(length.as_str().parse().expect("number is valid")),
                },
                None => MmlCommand::Rest { length: None },
            }
        }
        Rule::octave => MmlCommand::Octave(
            pair.into_inner()
                .next()
                .expect("octave must have a number")
                .as_str()
                .parse()
                .expect("number is valid"),
        ),
        Rule::default_length => match pair.into_inner().next() {
            Some(length) => MmlCommand::DefaultLength(parse_length(length)),
            None => MmlCommand::DefaultLengthReset,
        },
        Rule::octave_down => MmlCommand::OctaveDown,
        Rule::octave_up => MmlCommand::OctaveUp,
        Rule::gate => MmlCommand::Gate(parse_single_u16(pair) as u8),
        Rule::fine_gate => MmlCommand::FineGate(parse_single_u16(pair)),
        Rule::portamento => MmlCommand::Portamento,
        Rule::legato => MmlCommand::Legato,
        Rule::volume => MmlCommand::Volume(parse_single_u16(pair) as u8),
        Rule::fine_volume => MmlCommand::FineVolume(parse_single_u16(pair) as u8),
        Rule::volume_down => MmlCommand::VolumeDown,
        Rule::volume_up => MmlCommand::VolumeUp,
        Rule::pan => MmlCommand::Pan(parse_single_u16(pair) as u8),
        Rule::loop_start => MmlCommand::LoopStart,
        Rule::detune => MmlCommand::Detune(parse_signed_i16(pair)),
        Rule::loop_escape => MmlCommand::LoopEscape,
        Rule::register_write => {
            let values = parse_u16_values(pair);
            MmlCommand::RegisterWrite {
                register: values[0] as u8,
                value: values[1] as u8,
            }
        }
        Rule::key_on_delay => MmlCommand::KeyOnDelay(parse_single_u16(pair) as u8),
        Rule::noise_frequency => MmlCommand::NoiseFrequency(parse_single_u16(pair) as u8),
        Rule::sync_send => {
            let channel = pair
                .into_inner()
                .next()
                .expect("sync send has a channel")
                .as_str();
            let value = match channel.as_bytes().first().copied() {
                Some(b'A'..=b'H') => channel.as_bytes()[0] - b'A',
                Some(b'P'..=b'W') => channel.as_bytes()[0] - b'P' + 8,
                Some(b'0'..=b'9') => channel.parse().expect("sync channel is valid"),
                _ => unreachable!("sync channel is valid"),
            };
            MmlCommand::SyncSend(value)
        }
        Rule::sync_wait => MmlCommand::SyncWait,
        Rule::pitch_lfo => {
            let mut values = pair.into_inner();
            let waveform = parse_pair_u16(values.next().expect("pitch LFO waveform")) as u8;
            let period = parse_pair_u16(values.next().expect("pitch LFO period"));
            let amplitude = values
                .next()
                .expect("pitch LFO amplitude")
                .as_str()
                .parse()
                .expect("pitch LFO amplitude is valid");
            MmlCommand::PitchLfo {
                waveform,
                period,
                amplitude,
            }
        }
        Rule::pitch_lfo_on => MmlCommand::PitchLfoOn,
        Rule::pitch_lfo_off => MmlCommand::PitchLfoOff,
        Rule::volume_lfo => {
            let mut values = pair.into_inner();
            let waveform = parse_pair_u16(values.next().expect("volume LFO waveform")) as u8;
            let period = parse_pair_u16(values.next().expect("volume LFO period"));
            let amplitude = values
                .next()
                .expect("volume LFO amplitude")
                .as_str()
                .parse()
                .expect("volume LFO amplitude is valid");
            MmlCommand::VolumeLfo {
                waveform,
                period,
                amplitude,
            }
        }
        Rule::volume_lfo_on => MmlCommand::VolumeLfoOn,
        Rule::volume_lfo_off => MmlCommand::VolumeLfoOff,
        Rule::lfo_delay => MmlCommand::LfoDelay(parse_single_u16(pair) as u8),
        Rule::opm_lfo => {
            let values = parse_u16_values(pair);
            MmlCommand::OpmLfo {
                waveform: values[0] as u8,
                lfrq: values[1] as u8,
                pmd: values[2] as u8,
                amd: values[3] as u8,
                pms: values[4] as u8,
                ams: values[5] as u8,
                key_sync: values[6] as u8,
            }
        }
        Rule::opm_lfo_on => MmlCommand::OpmLfoOn,
        Rule::opm_lfo_off => MmlCommand::OpmLfoOff,
        Rule::ignore => MmlCommand::Ignore,
        Rule::pcm_frequency => MmlCommand::PcmFrequency(parse_single_u16(pair) as u8),
        rule => unreachable!("unexpected command rule: {rule:?}"),
    }
}

/// Parse a grammar pair containing an unsigned integer.
fn parse_pair_u16(pair: pest::iterators::Pair<'_, Rule>) -> u16 {
    pair.as_str().parse().expect("number is valid")
}

/// Parse the one unsigned integer nested in a command pair.
fn parse_single_u16(pair: pest::iterators::Pair<'_, Rule>) -> u16 {
    parse_pair_u16(
        pair.into_inner()
            .next()
            .expect("command has a numeric argument"),
    )
}

/// Parse all unsigned integer children of a command pair.
fn parse_u16_values(pair: pest::iterators::Pair<'_, Rule>) -> Vec<u16> {
    pair.into_inner().map(parse_pair_u16).collect()
}

/// Parse the signed integer nested in a command pair.
fn parse_signed_i16(pair: pest::iterators::Pair<'_, Rule>) -> i16 {
    pair.into_inner()
        .next()
        .expect("command has a signed argument")
        .as_str()
        .parse()
        .expect("signed number is valid")
}

/// Convert either a simple length or an extended expression into the AST.
fn parse_length(pair: pest::iterators::Pair<'_, Rule>) -> MmlLength {
    match pair.as_rule() {
        Rule::length_expression => parse_length_expression(pair),
        Rule::dotted_length => parse_dotted_length(pair.as_str()),
        _ => MmlLength::Denominator(parse_pair_u16(pair)),
    }
}

/// Convert a dotted denominator into the equivalent sum of note lengths.
fn parse_dotted_length(source: &str) -> MmlLength {
    let source = source.trim();
    let number = source.trim_end_matches('.');
    let denominator: u16 = number.parse().expect("number is valid");
    let dot_count = source.len() - number.len();
    let lengths = (0..=dot_count)
        .map(|shift| MmlLength::Denominator(denominator << shift))
        .collect();
    MmlLength::Sum(lengths)
}

/// Extract and parse the length expression used by a duration argument.
fn parse_duration(pair: pest::iterators::Pair<'_, Rule>) -> MmlLength {
    parse_length(pair.into_inner().next().expect("duration has a value"))
}

/// Parse an extended length expression from its grammar pairs.
fn parse_length_expression(pair: pest::iterators::Pair<'_, Rule>) -> MmlLength {
    let mut children = pair.into_inner();
    let base = parse_length_term(children.next().expect("length expression has a base"));
    let mut operators = Vec::new();
    let mut terms = Vec::new();
    while let Some(operator) = children.next() {
        operators.push(operator.as_str().chars().next().unwrap());
        terms.push(parse_length_term(
            children
                .next()
                .expect("length operator has a following term"),
        ));
    }

    if operators.is_empty() {
        base
    } else if operators.iter().all(|operator| *operator == '^') {
        let mut lengths = vec![base];
        lengths.extend(terms);
        MmlLength::Sum(lengths)
    } else {
        MmlLength::Adjusted {
            base: Box::new(base),
            adjustments: operators
                .into_iter()
                .zip(terms)
                .map(|(operator, term)| (operator == '^', term))
                .collect(),
        }
    }
}

/// Parse one duration term, including optional dots and `%N` tick notation.
fn parse_length_term(pair: pest::iterators::Pair<'_, Rule>) -> MmlLength {
    let mut children = pair.into_inner();
    let first = children.next().expect("length term has a value");
    let mut value = if first.as_rule() == Rule::length_ticks {
        MmlLength::Ticks(parse_pair_u16(
            children.next().expect("tick length has a number"),
        ))
    } else {
        MmlLength::Denominator(parse_pair_u16(first))
    };
    let dots = children.count();
    if dots == 0 {
        return value;
    }
    let mut lengths = vec![value.clone()];
    for _ in 0..dots {
        value = match value {
            MmlLength::Denominator(value) => MmlLength::Denominator(value * 2),
            MmlLength::Ticks(value) => MmlLength::Ticks(value / 2),
            _ => unreachable!("length term is scalar"),
        };
        lengths.push(value.clone());
    }
    MmlLength::Sum(lengths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_positions_preserve_ast_and_nested_multiline_channel_mapping() {
        let source = "AB @42 [r4\nA [c4]2\nAB d4]3\n";
        let (document, positions) = parse_with_positions(source).unwrap();
        assert_eq!(document, parse(source).unwrap());
        assert_eq!(positions.len(), 2);
        assert_eq!(
            SourcePosition::from_span(source, positions[0][0].position),
            SourcePosition {
                line_number: 1,
                column: 4
            }
        );
        assert_eq!(
            SourcePosition::from_span(source, positions[0][1].position),
            SourcePosition {
                line_number: 1,
                column: 8
            }
        );
        assert_eq!(
            positions[0][1]
                .end_position
                .map(|span| SourcePosition::from_span(source, span)),
            Some(SourcePosition {
                line_number: 3,
                column: 6
            })
        );
        assert_eq!(
            SourcePosition::from_span(source, positions[0][1].body[1].body[0].position),
            SourcePosition {
                line_number: 2,
                column: 4
            }
        );
        assert_eq!(
            SourcePosition::from_span(source, positions[1][1].body[1].position),
            SourcePosition {
                line_number: 3,
                column: 4
            }
        );
    }

    #[test]
    fn parses_metadata_and_track_commands() {
        let document =
            parse("#title \"Wapico\"\n#pcmfile \"wapico.pdx\"\nA t120 o4 l8 c+8 d r4\n").unwrap();

        assert_eq!(document.title.as_deref(), Some("Wapico"));
        assert_eq!(document.pcm_file.as_deref(), Some("wapico.pdx"));
        assert!(document.voices.is_empty());
        assert_eq!(document.tracks.len(), 1);
        assert_eq!(document.tracks[0].channel, 'A');
        assert_eq!(document.tracks[0].commands.len(), 6);
        assert_eq!(document.tracks[0].commands[1], MmlCommand::Octave(4));
        assert_eq!(
            document.tracks[0].commands[3],
            MmlCommand::Note {
                name: 'c',
                accidental: Some(Accidental::Sharp),
                length: Some(8),
            }
        );
    }

    #[test]
    fn expands_multiple_standard_channels() {
        let document = parse("ABCDEFGH c\n").unwrap();

        assert_eq!(document.tracks.len(), 8);
        assert_eq!(
            document
                .tracks
                .iter()
                .map(|track| track.channel)
                .collect::<Vec<_>>(),
            vec!['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H']
        );
        assert!(
            document
                .tracks
                .iter()
                .all(|track| track.commands.len() == 1)
        );
    }

    #[test]
    fn displays_the_parsed_document_tree() {
        let document = parse("A c4\n").unwrap();
        let tree = format_tree(&document);

        assert!(tree.contains("MmlDocument"), "{tree}");
        assert!(tree.contains("tracks"), "{tree}");
        assert!(tree.contains("Note"), "{tree}");
    }

    #[test]
    fn parses_tick_lengths_as_default_and_explicit_lengths() {
        let document = parse("A l%192 c%36\n").unwrap();

        assert_eq!(
            document.tracks[0].commands,
            vec![
                MmlCommand::DefaultLength(MmlLength::Ticks(192)),
                MmlCommand::ExtendedNote {
                    name: 'c',
                    accidental: None,
                    length: MmlLength::Ticks(36),
                },
            ]
        );
    }

    #[test]
    fn accepts_documented_volume_and_noise_limits() {
        let document = parse("A v15 w31\n").unwrap();

        assert_eq!(document.tracks[0].commands[0], MmlCommand::Volume(15));
        assert_eq!(
            document.tracks[0].commands[1],
            MmlCommand::NoiseFrequency(31)
        );
    }

    #[test]
    fn accepts_case_insensitive_pseudo_commands_but_uppercase_channels_only() {
        let document = parse("#TiTle \"Wapico\"\n#PCMFILE \"wapico.pdx\"\nA c4\n").unwrap();

        assert_eq!(document.title.as_deref(), Some("Wapico"));
        assert_eq!(document.pcm_file.as_deref(), Some("wapico.pdx"));
        assert!(parse("#title \"Wapico\"\na c4\n").is_err());
    }

    #[test]
    fn accepts_mml2mdx_style_command_spacing() {
        let document = parse("  \tA t 120 o 4 l 8 c+ 8 y 1 , 2 ; trailing comment\n").unwrap();

        assert_eq!(document.tracks.len(), 1);
        assert_eq!(document.tracks[0].commands.len(), 5);
        assert!(matches!(
            document.tracks[0].commands.last(),
            Some(MmlCommand::RegisterWrite {
                register: 1,
                value: 2
            })
        ));
    }

    #[test]
    fn parses_multidigit_sync_channel() {
        let document = parse("A S10\n").unwrap();

        assert_eq!(document.tracks[0].commands, vec![MmlCommand::SyncSend(10)]);
    }

    #[test]
    fn parses_bare_default_length_reset() {
        let document = parse("A l8 l c\n").unwrap();

        assert!(matches!(
            document.tracks[0].commands.as_slice(),
            [
                MmlCommand::DefaultLength(MmlLength::Denominator(8)),
                MmlCommand::DefaultLengthReset,
                MmlCommand::Note { .. }
            ]
        ));
    }

    #[test]
    fn enforces_reference_tempo_range() {
        assert!(matches!(
            parse("A t18\n"),
            Err(ParseError::InvalidValue {
                command: "tempo",
                value: 18,
                min: 19,
                max: 4882,
                ..
            })
        ));
        assert!(matches!(
            parse("A t4883\n"),
            Err(ParseError::InvalidValue {
                command: "tempo",
                value: 4883,
                min: 19,
                max: 4882,
                ..
            })
        ));
        assert!(parse("A t19 t4882\n").is_ok());
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(parse("A z\n").is_err());
    }

    #[test]
    fn reports_repeat_structure_errors_at_their_source_locations() {
        assert_eq!(
            parse("A c4\nB ]\n").unwrap_err().to_string(),
            "MML syntax error at line 2, columns 3-4: unexpected ']'\n  B ]\n    ^\n  expected: repeat start '['"
        );
        assert_eq!(
            parse("A c4\nB [c4\n").unwrap_err().to_string(),
            "MML syntax error at line 2, columns 3-4: repeat is not terminated\n  B [c4\n    ^\n  expected: repeat end ']'"
        );
    }

    #[test]
    fn explains_numeric_syntax_errors() {
        let error = parse("A @t\n").unwrap_err().to_string();

        assert!(error.contains("line 1, columns"));
        assert!(error.contains("A @t"));
        assert!(error.contains("expected: unsigned integer"));

        assert!(matches!(
            parse("A @t256\n"),
            Err(ParseError::InvalidValue {
                command: "@t",
                value: 256,
                min: 0,
                max: 255,
                ..
            })
        ));
        assert!(matches!(
            parse("A w32\n"),
            Err(ParseError::InvalidValue {
                command: "w",
                value: 32,
                min: 0,
                max: 31,
                ..
            })
        ));

        let error = parse("A t5000\n").unwrap_err().to_string();

        assert_eq!(
            error,
            "MML value error at line 1, columns 4-8\n  A t5000\n     ^^^^\n  reason: tempo value 5000 is outside the range 19..=4882"
        );

        let error = parse("A c4\nB t5000\n").unwrap_err().to_string();

        assert!(error.contains("MML value error at line 2, columns 4-8"));
        assert!(error.contains("  B t5000\n     ^^^^"));

        let error = parse("A c4^300\n").unwrap_err().to_string();

        assert!(error.contains("MML value error at line 1, columns 6-9"));
        assert!(error.contains("  A c4^300\n       ^^^"));
    }

    #[test]
    fn validates_numeric_ranges_after_parsing() {
        assert!(parse("A c0004\n").is_ok());

        let cases = [
            ("A c1000\n", "note length", 1000, 1, 256),
            ("A @256\n", "voice", 256, 0, 255),
            ("A n96\n", "note number", 96, 0, 95),
            ("A o9\n", "octave", 9, 0, 8),
            ("A q0\n", "q", 0, 1, 8),
            ("A v16\n", "v", 16, 0, 15),
            ("A @v128\n", "@v", 128, 0, 127),
            ("A p4\n", "p", 4, 0, 3),
            ("A y256,0\n", "register", 256, 0, 255),
            ("A F13\n", "PCM frequency", 13, 0, 12),
            ("A [c]256\n", "repeat", 256, 2, 255),
        ];

        for (source, command, value, min, max) in cases {
            assert!(
                matches!(
                    parse(source),
                    Err(ParseError::InvalidValue {
                        command: actual_command,
                        value: actual_value,
                        min: actual_min,
                        max: actual_max,
                        ..
                    }) if actual_command == command
                        && actual_value == value
                        && actual_min == min
                        && actual_max == max
                ),
                "unexpected result for {source:?}"
            );
        }

        let error = parse("A t999999999999999999999999999999999999999999999999999999999999\n")
            .unwrap_err()
            .to_string();

        assert!(error.contains("MML value error at line 1, columns 4-"));
        assert!(
            error.contains(
                "  A t999999999999999999999999999999999999999999999999999999999999\n     ^"
            )
        );

        let error = parse("A c%999999999999999999999999999999999999999999999999999999999999\n")
            .unwrap_err()
            .to_string();

        assert!(error.contains("MML value error at line 1, columns 5-"));
        assert!(error.contains(
            "  A c%999999999999999999999999999999999999999999999999999999999999\n      ^"
        ));
    }

    #[test]
    fn parses_voice_definition_and_block_comments() {
        let document = parse(
            "@1 = {\n  /* OP1 */\n  28, 4, 0, 5, 1, 37, 2, 1, 7, 0, 0,\n  /* OP2 */\n  22, 9, 1, 2, 1, 47, 2, 12, 0, 0, 0,\n  /* OP3 */\n  29, 4, 3, 6, 1, 37, 1, 3, 3, 0, 0,\n  /* OP4 */\n  15, 7, 0, 5, 10, 0, 2, 1, 0, 0, 1,\n  /* CON FL OP */\n  2, 7, 15\n}\n/* ignored */\nA c4\n",
        )
        .unwrap();

        assert_eq!(document.voices.len(), 1);
        assert_eq!(document.voices[0].number, 1);
        assert_eq!(document.voices[0].values.len(), 47);
        assert_eq!(document.tracks.len(), 1);
    }

    #[test]
    fn accepts_block_comments_between_voice_and_track_tokens() {
        let source = r#"@/*a*/1/*b*/=/*c*/{
/*d*/28/*e*/,/*f*/4,0,5,1,37,2,1,7,0,0,
22,9,1,2,1,47,2,12,0,0,0,
29,4,3,6,1,37,1,3,3,0,0,
15,7,0,5,10,0,2,1,0,0,1,
2,7,15/*g*/
}
A t/*h*/120 c4/*i*/d4
"#;
        let document = parse(source).unwrap();

        assert_eq!(document.voices.len(), 1);
        assert_eq!(document.voices[0].number, 1);
        assert_eq!(document.voices[0].values.len(), 47);
        assert_eq!(document.tracks[0].commands.len(), 3);
    }

    #[test]
    fn accepts_block_comments_inside_length_expressions_and_metadata() {
        let source = r#"#title/*before value*/"Song /*literal*/"/*after value*/
A c4/*before add*/^/*after add*/8~/*before ticks*/%/*before tick count*/24
"#;
        let document = parse(source).unwrap();

        assert_eq!(document.title.as_deref(), Some("Song /*literal*/"));
        assert_eq!(
            document.tracks[0].commands,
            vec![MmlCommand::ExtendedNote {
                name: 'c',
                accidental: None,
                length: MmlLength::Adjusted {
                    base: Box::new(MmlLength::Denominator(4)),
                    adjustments: vec![
                        (true, MmlLength::Denominator(8)),
                        (false, MmlLength::Ticks(24)),
                    ],
                },
            }]
        );
    }

    #[test]
    fn accepts_line_comments_at_voice_token_boundaries() {
        let source = r#"@; marker
1; voice number
=; equals
{; opening brace
28; before comma
,; after comma
4,0,5,1,37,2,1,7,0,0,
22,9,1,2,1,47,2,12,0,0,0,
29,4,3,6,1,37,1,3,3,0,0,
15,7,0,5,10,0,2,1,0,0,1,
2,7,15; before closing brace
}
A c4
"#;
        let document = parse(source).unwrap();

        assert_eq!(document.voices.len(), 1);
        assert_eq!(document.voices[0].number, 1);
        assert_eq!(document.voices[0].values.len(), 47);
    }

    #[test]
    fn accepts_semicolon_comments_at_line_ends() {
        let source = r#"#title "Song"; title comment
#pcmfile "song.pdx"; pcm comment
@1 = {
28,4,0,5,1,37,2,1,7,0,0,
22,9,1,2,1,47,2,12,0,0,0,
29,4,3,6,1,37,1,3,3,0,0,
15,7,0,5,10,0,2,1,0,0,1,
2,7,15
}; voice comment
A c4; track comment
"#;
        let document = parse(source).unwrap();

        assert_eq!(document.title.as_deref(), Some("Song"));
        assert_eq!(document.pcm_file.as_deref(), Some("song.pdx"));
        assert_eq!(document.voices.len(), 1);
        assert_eq!(document.voices[0].values.len(), 47);
        assert_eq!(document.tracks[0].commands.len(), 1);
    }

    #[test]
    fn parses_all_documented_mxdrv_commands() {
        let document = parse(
            "A @t200 @1 o4 < > l4 n12,8 r%24 q4 @q12 _ & v8 @v100 ( ) p3 L D-12 [c4 / d4]2 y1,2 k3 w4 S0 W MP0,4,5 MPON MPOF MA0,4,5 MAON MAOF MD3 MH0,1,2,3,4,5,1 MHON MHOF ! F4\n",
        )
        .unwrap();

        assert_eq!(document.tracks.len(), 1);
        assert!(document.tracks[0].commands.len() >= 30);
        assert!(matches!(
            document.tracks[0]
                .commands
                .iter()
                .find(|command| matches!(command, MmlCommand::Repeat { .. })),
            Some(MmlCommand::Repeat { count: 2, .. })
        ));
    }

    #[test]
    fn rejects_incomplete_voice_definition() {
        let source = "@1 = { 28, 4, 0, 5, 1, 37, 2, 1, 7, 0, 0 }\n";

        assert!(parse(source).is_err());
    }
}
