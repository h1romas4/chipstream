//! MXDRV MML parsing and syntax tree types.
//!
//! The [`parse`] function converts an MML source string into an [`MmlDocument`]
//! containing metadata, voice definitions, and per-channel command streams.

use std::fmt;

use pest::Parser;
use pest::error::{ErrorVariant, LineColLocation};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "mdx/mml.pest"]
struct MmlParser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlDocument {
    pub title: Option<String>,
    pub pcm_file: Option<String>,
    pub voices: Vec<MmlVoice>,
    pub tracks: Vec<MmlTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlTrack {
    pub channel: char,
    pub commands: Vec<MmlCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlVoice {
    pub number: u8,
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
    /// Send a synchronization event to another channel.
    SyncSend(char),
    /// Wait for a synchronization event.
    SyncWait,
    /// Configure pitch LFO.
    PitchLfo {
        /// LFO waveform number.
        waveform: u8,
        /// LFO period.
        period: u16,
        /// LFO amplitude.
        amplitude: u16,
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
        amplitude: u16,
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
        value: i64,
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
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
            } => write!(
                formatter,
                "{command} value {value} is outside the range {min}..={max}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

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
pub fn parse(source: &str) -> Result<MmlDocument, ParseError> {
    let mut document = MmlDocument {
        title: None,
        pcm_file: None,
        voices: Vec::new(),
        tracks: Vec::new(),
    };

    let document_pair = MmlParser::parse(Rule::document, source)
        .map_err(|error| ParseError::Syntax(format_syntax_error(&error, source)))?
        .next()
        .expect("document rule must produce a pair");
    for pair in document_pair.into_inner() {
        let Some(line) = pair.into_inner().next() else {
            continue;
        };
        match line.as_rule() {
            Rule::title => document.title = Some(parse_string(line)),
            Rule::pcmfile => document.pcm_file = Some(parse_string(line)),
            Rule::voice => document.voices.push(parse_voice(line)),
            Rule::track => document.tracks.push(parse_track(line)),
            Rule::blank | Rule::comment | Rule::block_comment => {}
            rule => unreachable!("unexpected line rule: {rule:?}"),
        }
    }

    validate_document(&document)?;
    Ok(document)
}

/// Format a parsed MML document as a readable syntax tree.
///
/// This uses the typed AST produced by [`parse`]. The original Pest pairs and
/// source trivia are not retained after parsing, so whitespace and comments
/// are not included in the formatted tree.
pub fn format_tree(document: &MmlDocument) -> String {
    format!("{document:#?}")
}

/// Convert a PEST error into a compact, source-oriented diagnostic.
fn format_syntax_error(error: &pest::error::Error<Rule>, source: &str) -> String {
    let (line_number, column) = match error.line_col {
        LineColLocation::Pos((line, column)) | LineColLocation::Span((line, column), _) => {
            (line, column)
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
    let numeric_context = describe_numeric_context(line_text, column);
    let expected = match numeric_context {
        Some(description) => description.to_owned(),
        None => match &error.variant {
            ErrorVariant::ParsingError { positives, .. } if positives.len() > 8 => {
                "a valid MML command or end of line".to_owned()
            }
            ErrorVariant::ParsingError { positives, .. } => positives
                .iter()
                .map(describe_rule)
                .collect::<Vec<_>>()
                .join(", "),
            ErrorVariant::CustomError { message } => message.clone(),
        },
    };
    let expected = if expected.is_empty() {
        "a valid MML command".to_owned()
    } else {
        expected
    };

    format!(
        "MML syntax error at line {line_number}, column {column}: unexpected {actual}\n  {line_text}\n  {}^\n  expected: {expected}",
        " ".repeat(column.saturating_sub(1)),
    )
}

/// Describe the numeric argument expected at a syntax-error location.
fn describe_numeric_context(line: &str, column: usize) -> Option<&'static str> {
    let characters = line.chars().collect::<Vec<_>>();
    let position = column.saturating_sub(1).min(characters.len());
    let mut number_start = position;
    while number_start > 0 && characters[number_start - 1].is_ascii_digit() {
        number_start -= 1;
    }
    if number_start == position {
        return None;
    }
    let prefix = characters[..number_start].iter().collect::<String>();
    if prefix.ends_with("@t") {
        Some("OPM tempo: 0..=255, up to 3 digits")
    } else if prefix.ends_with("@v") {
        Some("fine volume: 0..=127, up to 3 digits")
    } else if prefix.ends_with("@q") {
        Some("fine gate: 1..=256, up to 3 digits")
    } else if prefix.ends_with("MD") {
        Some("LFO delay: numeric argument")
    } else if prefix.ends_with('F') {
        Some("PCM frequency: 0..=12, up to 2 digits")
    } else if prefix.ends_with('n') {
        Some("note number: 0..=95, up to 2 digits")
    } else if prefix.ends_with('l') {
        Some("default note length: up to 3 digits")
    } else if prefix.ends_with('q') {
        Some("gate: 1..=8, 1 digit")
    } else if prefix.ends_with('v') {
        Some("volume: 0..=15, up to 2 digits")
    } else if prefix.ends_with('p') {
        Some("pan: 0..=3, 1 digit")
    } else if prefix.ends_with('w') {
        Some("noise frequency: 0..=311, up to 3 digits")
    } else if prefix.ends_with('o') {
        Some("octave: 0..=8, 1 digit")
    } else {
        None
    }
}

/// Turn a grammar rule into a user-facing expectation description.
fn describe_rule(rule: &Rule) -> String {
    match rule {
        Rule::byte_number => "byte number (0..=255, up to 3 digits)".to_owned(),
        Rule::repeat_count => "repeat count (2..=255, up to 3 digits)".to_owned(),
        Rule::note_number => "note number (0..=95, up to 2 digits)".to_owned(),
        Rule::note_length_number => "note length (up to 3 digits)".to_owned(),
        Rule::fine_volume_number => "fine volume (0..=127, up to 3 digits)".to_owned(),
        Rule::volume_number => "volume (0..=15, up to 2 digits)".to_owned(),
        Rule::pcm_frequency_number => "PCM frequency (0..=12, up to 2 digits)".to_owned(),
        Rule::octave_number => "octave (0..=8, 1 digit)".to_owned(),
        Rule::gate_number => "gate (1..=8, 1 digit)".to_owned(),
        Rule::pan_number => "pan (0..=3, 1 digit)".to_owned(),
        Rule::noise_number => "noise frequency (0..=31, up to 2 digits)".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Validate every command in a parsed document, including nested repeats.
fn validate_document(document: &MmlDocument) -> Result<(), ParseError> {
    for track in &document.tracks {
        for command in &track.commands {
            validate_command(command)?;
        }
    }
    Ok(())
}

/// Validate command-specific ranges and recursively validate repeat bodies.
fn validate_command(command: &MmlCommand) -> Result<(), ParseError> {
    match command {
        MmlCommand::Tempo(value) => validate_range("tempo", *value as i64, 19, 4882)?,
        MmlCommand::Repeat { body, count } => {
            validate_range("repeat", *count as i64, 2, 255)?;
            for command in body {
                validate_command(command)?;
            }
        }
        MmlCommand::Note {
            length: Some(length),
            ..
        }
        | MmlCommand::Rest {
            length: Some(length),
        } => validate_range("note length", *length as i64, 1, 256)?,
        MmlCommand::ExtendedNote { length, .. }
        | MmlCommand::ExtendedRest(length)
        | MmlCommand::DefaultLength(length)
        | MmlCommand::NumericNote {
            length: Some(length),
            ..
        } => validate_length(length)?,
        MmlCommand::FineGate(value) => validate_range("@q", *value as i64, 1, 256)?,
        MmlCommand::Detune(value) => validate_range("D", *value as i64, -32767, 32767)?,
        MmlCommand::PitchLfo { waveform, .. } | MmlCommand::VolumeLfo { waveform, .. } => {
            validate_range("LFO waveform", *waveform as i64, 0, 2)?;
        }
        MmlCommand::OpmLfo {
            waveform, key_sync, ..
        } => {
            validate_range("OPM LFO waveform", *waveform as i64, 0, 3)?;
            validate_range("OPM LFO key sync", *key_sync as i64, 0, 1)?;
        }
        _ => {}
    }
    Ok(())
}

/// Validate each component and the total value of a length expression.
fn validate_length(length: &MmlLength) -> Result<(), ParseError> {
    let total = match length {
        MmlLength::Denominator(value) => {
            validate_range("note length", *value as i64, 1, 256)?;
            *value as i64
        }
        MmlLength::Ticks(value) => {
            validate_range("note tick length", *value as i64, 1, 256)?;
            *value as i64
        }
        MmlLength::Sum(values) => values.iter().try_fold(0_i64, |total, value| {
            validate_length(value)?;
            Ok::<_, ParseError>(total + length_value(value))
        })?,
    };
    validate_range("note length", total, 1, 256)
}

/// Calculate the total numeric value represented by a length expression.
fn length_value(length: &MmlLength) -> i64 {
    match length {
        MmlLength::Denominator(value) | MmlLength::Ticks(value) => *value as i64,
        MmlLength::Sum(values) => values.iter().map(length_value).sum(),
    }
}

/// Return a structured error when a value is outside an inclusive range.
fn validate_range(command: &'static str, value: i64, min: i64, max: i64) -> Result<(), ParseError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ParseError::InvalidValue {
            command,
            value,
            min,
            max,
        })
    }
}

/// Convert a `voice` grammar pair into a voice definition.
fn parse_voice(voice: pest::iterators::Pair<'_, Rule>) -> MmlVoice {
    let mut numbers = voice
        .into_inner()
        .filter(|pair| matches!(pair.as_rule(), Rule::voice_number | Rule::number))
        .map(|number| number.as_str().parse().expect("voice value is valid"));
    let number = numbers.next().expect("voice must have a number");
    let values = numbers.collect();
    MmlVoice { number, values }
}

/// Extract the quoted string from a metadata grammar pair.
fn parse_string(line: pest::iterators::Pair<'_, Rule>) -> String {
    line.into_inner()
        .find(|pair| pair.as_rule() == Rule::string)
        .map(|pair| pair.as_str()[1..pair.as_str().len() - 1].to_owned())
        .expect("metadata line must contain a string")
}

/// Convert a `track` grammar pair into a channel and typed commands.
fn parse_track(line: pest::iterators::Pair<'_, Rule>) -> MmlTrack {
    let mut children = line.into_inner();
    let channel = children
        .next()
        .expect("track must have a channel")
        .as_str()
        .chars()
        .next()
        .expect("channel must not be empty");
    let commands = children
        .filter(|pair| pair.as_rule() != Rule::block_comment)
        .map(parse_command)
        .collect();
    MmlTrack { channel, commands }
}

/// Convert one command grammar pair into its typed AST representation.
fn parse_command(pair: pest::iterators::Pair<'_, Rule>) -> MmlCommand {
    match pair.as_rule() {
        Rule::block_comment => MmlCommand::Directive(pair.as_str().to_owned()),
        Rule::repeat => parse_repeat(pair),
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
                    Rule::note_length_number => {
                        length = Some(child.as_str().parse().expect("number is valid"))
                    }
                    _ => unreachable!("unexpected note rule: {:?}", child.as_rule()),
                }
            }
            MmlCommand::Note {
                name,
                accidental,
                length,
            }
        }
        Rule::complex_note => {
            let mut children = pair.into_inner();
            let name = parse_note_name(children.next().expect("note has a name"));
            let next = children.next().expect("note has a length");
            let (accidental, length_pair) = if next.as_rule() == Rule::accidental {
                (
                    parse_accidental(Some(next)),
                    children.next().expect("note has a length"),
                )
            } else {
                (None, next)
            };
            let length = parse_length(length_pair);
            MmlCommand::ExtendedNote {
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
        Rule::rest => MmlCommand::Rest {
            length: pair
                .into_inner()
                .next()
                .map(|number| number.as_str().parse().expect("number is valid")),
        },
        Rule::complex_rest => MmlCommand::ExtendedRest(parse_length(
            pair.into_inner().next().expect("rest has a length"),
        )),
        Rule::octave => MmlCommand::Octave(
            pair.into_inner()
                .next()
                .expect("octave must have a number")
                .as_str()
                .parse()
                .expect("number is valid"),
        ),
        Rule::default_length => MmlCommand::DefaultLength(parse_length(
            pair.into_inner().next().expect("length must have a value"),
        )),
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
        Rule::sync_send => MmlCommand::SyncSend(
            pair.into_inner()
                .next()
                .expect("sync send has a channel")
                .as_str()
                .chars()
                .next()
                .expect("channel is not empty"),
        ),
        Rule::sync_wait => MmlCommand::SyncWait,
        Rule::pitch_lfo => {
            let values = parse_u16_values(pair);
            MmlCommand::PitchLfo {
                waveform: values[0] as u8,
                period: values[1],
                amplitude: values[2],
            }
        }
        Rule::pitch_lfo_on => MmlCommand::PitchLfoOn,
        Rule::pitch_lfo_off => MmlCommand::PitchLfoOff,
        Rule::volume_lfo => {
            let values = parse_u16_values(pair);
            MmlCommand::VolumeLfo {
                waveform: values[0] as u8,
                period: values[1],
                amplitude: values[2],
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

/// Extract the single note letter from a note-name grammar pair.
fn parse_note_name(pair: pest::iterators::Pair<'_, Rule>) -> char {
    pair.as_str()
        .chars()
        .next()
        .expect("note name is not empty")
}

/// Convert an optional accidental grammar pair into an AST accidental.
fn parse_accidental(pair: Option<pest::iterators::Pair<'_, Rule>>) -> Option<Accidental> {
    pair.map(|pair| match pair.as_str() {
        "+" => Accidental::Sharp,
        "-" => Accidental::Flat,
        _ => unreachable!(),
    })
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
    if pair.as_rule() == Rule::length_expression {
        parse_length_expression(pair.as_str())
    } else {
        MmlLength::Denominator(parse_pair_u16(pair))
    }
}

/// Extract and parse the length expression used by a duration argument.
fn parse_duration(pair: pest::iterators::Pair<'_, Rule>) -> MmlLength {
    parse_length(pair.into_inner().next().expect("duration has a value"))
}

/// Parse the `^`-separated textual form of an extended length expression.
fn parse_length_expression(source: &str) -> MmlLength {
    let lengths = source
        .split('^')
        .map(|part| {
            let part = part.trim();
            if let Some(ticks) = part.strip_prefix('%') {
                MmlLength::Ticks(ticks.parse().expect("tick length is valid"))
            } else {
                MmlLength::Denominator(part.parse().expect("denominator is valid"))
            }
        })
        .collect::<Vec<_>>();
    if lengths.len() == 1 {
        lengths.into_iter().next().unwrap()
    } else {
        MmlLength::Sum(lengths)
    }
}

/// Convert a repeat grammar pair, including its nested command body.
fn parse_repeat(pair: pest::iterators::Pair<'_, Rule>) -> MmlCommand {
    let mut children = pair.into_inner();
    let mut body = Vec::new();
    let mut count: Option<u16> = None;
    for child in children.by_ref() {
        if child.as_rule() == Rule::repeat_count {
            count = Some(child.as_str().parse().expect("repeat count is valid"));
        } else {
            body.push(parse_command(child));
        }
    }
    MmlCommand::Repeat {
        body,
        count: count.expect("repeat must have a count"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn enforces_reference_tempo_range() {
        assert!(matches!(
            parse("A t18\n"),
            Err(ParseError::InvalidValue {
                command: "tempo",
                value: 18,
                min: 19,
                max: 4882,
            })
        ));
        assert!(matches!(
            parse("A t4883\n"),
            Err(ParseError::InvalidValue {
                command: "tempo",
                value: 4883,
                min: 19,
                max: 4882,
            })
        ));
        assert!(parse("A t19 t4882\n").is_ok());
    }

    #[test]
    fn rejects_unknown_command() {
        assert!(parse("A z\n").is_err());
    }

    #[test]
    fn explains_numeric_syntax_errors() {
        let error = parse("A @t999\n").unwrap_err().to_string();

        assert!(error.contains("line 1, column"));
        assert!(error.contains("A @t999"));
        assert!(error.contains("expected: OPM tempo: 0..=255, up to 3 digits"));
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
