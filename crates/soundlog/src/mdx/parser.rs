//! MDX command parsing utilities.
//!
//! This module decodes one command at a time from an MDX track byte stream.
//! It dispatches on the opcode and delegates operand validation to the typed
//! command specifications in [`crate::mdx::command`].
//!
//! The returned length always includes the opcode and its operands, allowing
//! callers to advance through a track without maintaining parser state here.

use crate::ParseError;
use crate::binutil::read_u8_at;
use crate::mdx::command::{
    MdxAdpcmOrNoiseFrequency, MdxCommand, MdxCommandSpec, MdxEndOfTrack, MdxExtended2Command,
    MdxExtendedCommand, MdxGate, MdxKeyOffDisable, MdxKeyOnDelay, MdxLfoDelay, MdxLoopStart,
    MdxNote, MdxOpmLfo, MdxOpmRegisterWrite, MdxPan, MdxPcmMode, MdxPitchLfo, MdxRelativeOffset,
    MdxRest, MdxSignedWord, MdxSyncSend, MdxSyncWait, MdxTempo, MdxVoiceOrPcmBank, MdxVolume,
    MdxVolumeDown, MdxVolumeLfo, MdxVolumeUp,
};

/// A type alias for a function that parses a byte command from the MDX stream.
///
/// The function takes the byte slice, the offset of the operands, and the opcode.
/// It returns the parsed command and the number of bytes consumed, or a `ParseError`.
type ByteCommandParser<T> = fn(&[u8], usize, u8) -> Result<(T, usize), ParseError>;

/// Parse one MDX track command.
///
/// `offset` points to the command opcode. The returned length includes the
/// opcode byte and all command operands.
pub fn parse_mdx_command(bytes: &[u8], offset: usize) -> Result<(MdxCommand, usize), ParseError> {
    let opcode = read_u8_at(bytes, offset)?;
    let operands = offset + 1;

    match opcode {
        0x00..=0x7f => {
            let (command, length) = MdxRest::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::Rest(command), length))
        }
        0x80..=0xdf => {
            let (command, length) = MdxNote::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::Note(command), length))
        }
        0xe8 => parse_no_operand(bytes, operands, opcode, MdxPcmMode, MdxCommand::PcmMode),
        0xe9 => parse_byte_command(
            bytes,
            operands,
            opcode,
            MdxLfoDelay::parse,
            MdxCommand::LfoDelay,
        ),
        0xea => parse_opm_lfo(bytes, operands, opcode),
        0xeb => parse_volume_lfo(bytes, operands, opcode),
        0xec => parse_pitch_lfo(bytes, operands, opcode),
        0xed => parse_byte_command(
            bytes,
            operands,
            opcode,
            MdxAdpcmOrNoiseFrequency::parse,
            MdxCommand::AdpcmOrNoiseFrequency,
        ),
        0xee => parse_no_operand(bytes, operands, opcode, MdxSyncWait, MdxCommand::SyncWait),
        0xef => parse_byte_command(
            bytes,
            operands,
            opcode,
            MdxSyncSend::parse,
            MdxCommand::SyncSend,
        ),
        0xf0 => parse_byte_command(
            bytes,
            operands,
            opcode,
            MdxKeyOnDelay::parse,
            MdxCommand::KeyOnDelay,
        ),
        0xf1 => parse_end_or_jump(bytes, operands),
        0xf2 => parse_signed_word(bytes, operands, opcode, MdxCommand::Portamento),
        0xf3 => parse_signed_word(bytes, operands, opcode, MdxCommand::Detune),
        0xf4 => parse_relative(bytes, operands, opcode, MdxCommand::LoopEscape),
        0xf5 => parse_relative(bytes, operands, opcode, MdxCommand::LoopEnd),
        0xf6 => {
            let (command, length) = MdxLoopStart::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::LoopStart(command), length))
        }
        0xf7 => parse_no_operand(
            bytes,
            operands,
            opcode,
            MdxKeyOffDisable,
            MdxCommand::KeyOffDisable,
        ),
        0xf8 => {
            let (command, length) = MdxGate::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::Gate(command), length))
        }
        0xf9 => parse_no_operand(bytes, operands, opcode, MdxVolumeUp, MdxCommand::VolumeUp),
        0xfa => parse_no_operand(
            bytes,
            operands,
            opcode,
            MdxVolumeDown,
            MdxCommand::VolumeDown,
        ),
        0xfb => parse_byte_command(
            bytes,
            operands,
            opcode,
            MdxVolume::parse,
            MdxCommand::Volume,
        ),
        0xfc => {
            let (command, length) = MdxPan::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::Pan(command), length))
        }
        0xfd => parse_byte_command(
            bytes,
            operands,
            opcode,
            MdxVoiceOrPcmBank::parse,
            MdxCommand::VoiceOrPcmBank,
        ),
        0xfe => {
            let (command, length) = MdxOpmRegisterWrite::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::OpmRegisterWrite(command), length))
        }
        0xff => {
            let (command, length) = MdxTempo::parse(bytes, operands, opcode)?;
            Ok((MdxCommand::Tempo(command), length))
        }
        0xe6 => parse_extended2(bytes, operands, opcode),
        0xe7 => parse_extended(bytes, operands, opcode),
        _ => Ok((
            MdxCommand::Raw(crate::mdx::command::MdxRawCommand {
                opcode,
            }),
            1,
        )),
    }
}

/// Parses a byte command from the MDX stream.
///
/// `parse` is a function that knows how to parse the specific byte command.
/// `map` is a function that maps the parsed command to an `MdxCommand` variant.
fn parse_byte_command<T, F>(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
    parse: ByteCommandParser<T>,
    map: F,
) -> Result<(MdxCommand, usize), ParseError>
where
    F: FnOnce(T) -> MdxCommand,
{
    let (command, length) = parse(bytes, offset, opcode)?;
    Ok((map(command), length))
}

/// Parses a no-operand command from the MDX stream.
///
/// `_spec` is a type that implements `MdxCommandSpec` and knows how to parse the command.
/// `map` is a function that maps the parsed command to an `MdxCommand` variant.
fn parse_no_operand<T, F>(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
    _spec: T,
    map: F,
) -> Result<(MdxCommand, usize), ParseError>
where
    T: MdxCommandSpec,
    F: FnOnce(T) -> MdxCommand,
{
    let (command, length) = T::parse(bytes, offset, opcode)?;
    Ok((map(command), length))
}

/// Parses an OPM LFO command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_opm_lfo(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
) -> Result<(MdxCommand, usize), ParseError> {
    let (command, length) = MdxOpmLfo::parse(bytes, offset, opcode)?;
    Ok((MdxCommand::OpmLfo(command), length))
}

/// Parses a volume LFO command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_volume_lfo(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
) -> Result<(MdxCommand, usize), ParseError> {
    let (command, length) = MdxVolumeLfo::parse(bytes, offset, opcode)?;
    Ok((MdxCommand::VolumeLfo(command), length))
}

/// Parses a pitch LFO command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_pitch_lfo(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
) -> Result<(MdxCommand, usize), ParseError> {
    let (command, length) = MdxPitchLfo::parse(bytes, offset, opcode)?;
    Ok((MdxCommand::PitchLfo(command), length))
}

/// Parses an extended command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_extended(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
) -> Result<(MdxCommand, usize), ParseError> {
    let (command, length) = MdxExtendedCommand::parse(bytes, offset, opcode)?;
    Ok((MdxCommand::Extended(command), length))
}

/// Parses an extended2 command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_extended2(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
) -> Result<(MdxCommand, usize), ParseError> {
    let (command, length) = MdxExtended2Command::parse(bytes, offset, opcode)?;
    Ok((MdxCommand::Extended2(command), length))
}

/// Parses a signed word command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_signed_word<F>(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
    map: F,
) -> Result<(MdxCommand, usize), ParseError>
where
    F: FnOnce(MdxSignedWord) -> MdxCommand,
{
    let (command, length) = MdxSignedWord::parse(bytes, offset, opcode)?;
    Ok((map(command), length))
}

/// Parses a relative offset command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_relative<F>(
    bytes: &[u8],
    offset: usize,
    opcode: u8,
    map: F,
) -> Result<(MdxCommand, usize), ParseError>
where
    F: FnOnce(MdxRelativeOffset) -> MdxCommand,
{
    let (command, length) = MdxRelativeOffset::parse(bytes, offset, opcode)?;
    Ok((map(command), length))
}

/// Parses an end-of-track or jump command from the MDX stream.
///
/// Returns the parsed command and the number of bytes consumed.
fn parse_end_or_jump(bytes: &[u8], offset: usize) -> Result<(MdxCommand, usize), ParseError> {
    let first_operand = read_u8_at(bytes, offset)?;
    if first_operand == 0 {
        let (command, length) = MdxEndOfTrack::parse(bytes, offset, 0xf1)?;
        return Ok((MdxCommand::EndOfTrack(command), length));
    }

    parse_relative(bytes, offset, 0xf1, MdxCommand::Jump)
}
