//! XGM v1 parsing, construction, and serialization.

use super::command::{XgmCommand, XgmRegisterWrite};
use super::header::{XGM_SAMPLE_COUNT, XgmHeader};
pub use super::header::{XgmSample, XgmTiming};
use crate::ParseError;
use crate::binutil::{read_slice, read_u16_le_at, read_u32_le_at};
use crate::meta::{Gd3, Gd3ParseLimits};

const HEADER_SIZE: usize = 0x104;
const MAX_SAMPLE_DATA_BYTES: usize = u16::MAX as usize * 256;

/// Resource limits for parsing untrusted XGM files.
///
/// `None` disables a limit; zero permits no items or bytes. The sample bank is
/// additionally bounded by the XGM format itself to less than 16 MiB.
#[derive(Debug, Clone, Copy)]
pub struct XgmParseLimits {
    /// Maximum shared sample bank length in bytes. Default: the XGM format maximum.
    pub max_sample_data_bytes: Option<usize>,
    /// Maximum length of each defined sample in bytes. Default: the XGM format maximum.
    pub max_sample_bytes: Option<usize>,
    /// Maximum number of defined samples. Default: 63.
    pub max_samples: Option<usize>,
    /// Maximum number of tracks. Default: 256.
    pub max_tracks: Option<usize>,
    /// Maximum total commands across all tracks. Default: 1,000,000.
    pub max_commands: Option<usize>,
    /// Maximum GD3 chunk size in bytes, including its 12-byte header. Default: 16 KiB.
    pub max_gd3_bytes: Option<usize>,
    /// Maximum total GD3 bytes across all tracks, including headers. Default: 256 KiB.
    pub max_total_gd3_bytes: Option<usize>,
}

impl Default for XgmParseLimits {
    fn default() -> Self {
        Self {
            max_sample_data_bytes: Some(MAX_SAMPLE_DATA_BYTES),
            max_sample_bytes: Some(MAX_SAMPLE_DATA_BYTES),
            max_samples: Some(XGM_SAMPLE_COUNT),
            max_tracks: Some(256),
            max_commands: Some(1_000_000),
            max_gd3_bytes: Gd3ParseLimits::default().max_gd3_bytes,
            max_total_gd3_bytes: Some(256 * 1024),
        }
    }
}

/// One music block and its optional GD3 metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct XgmTrack {
    /// Commands in file order. Loop targets are command indices.
    pub commands: Vec<XgmCommand>,
    /// Per-track metadata, when present.
    pub gd3: Option<Gd3>,
}

/// A complete XGM v1 file, including its shared sample bank and tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct XgmDocument {
    /// Timing and sample table.
    pub header: XgmHeader,
    /// Shared signed 8-bit PCM data.
    pub sample_data: Vec<u8>,
    /// Music blocks. Multiple tracks share the same sample bank.
    pub tracks: Vec<XgmTrack>,
}

impl XgmDocument {
    /// Parse an uncompressed XGM v1 file with [`XgmParseLimits::default`].
    ///
    /// For compatibility with files whose GD3 length is zero despite containing
    /// metadata, the eleven UTF-16 fields are read to their terminators.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with_limits(bytes, XgmParseLimits::default())
    }

    /// Parse an uncompressed XGM v1 file with caller-selected resource limits.
    ///
    /// Limits are checked before copying sample or metadata data, and before
    /// retaining each track or command.
    pub fn parse_with_limits(bytes: &[u8], limits: XgmParseLimits) -> Result<Self, ParseError> {
        let header = read_slice(bytes, 0, HEADER_SIZE)?;
        if &header[..4] != b"XGM " {
            return Err(ParseError::InvalidIdent(header[..4].try_into().unwrap()));
        }
        if header[0x102] != 1 {
            return Err(ParseError::UnsupportedVersion(u32::from(header[0x102])));
        }
        let flags = header[0x103];
        if flags & !7 != 0 {
            return Err(invalid("unsupported XGM header flags"));
        }
        let timing = if flags & 1 == 0 {
            XgmTiming::Ntsc
        } else {
            XgmTiming::Pal
        };
        let sample_len = usize::from(read_u16_le_at(bytes, 0x100)?) * 256;
        check_limit(
            "sample data bytes",
            sample_len,
            limits.max_sample_data_bytes,
        )?;
        let mut xgm_header = XgmHeader::new(timing);
        let mut sample_count = 0;
        for (index, sample) in xgm_header.samples.iter_mut().enumerate() {
            let offset = read_u16_le_at(bytes, 4 + index * 4)?;
            let length = read_u16_le_at(bytes, 6 + index * 4)?;
            if offset != u16::MAX {
                sample_count += 1;
                check_limit("sample count", sample_count, limits.max_samples)?;
                check_limit(
                    "sample length",
                    usize::from(length) * 256,
                    limits.max_sample_bytes,
                )?;
                *sample = Some(XgmSample {
                    offset: u32::from(offset) * 256,
                    length: u32::from(length) * 256,
                });
            }
        }
        let sample_data = read_slice(bytes, HEADER_SIZE, sample_len)?.to_vec();
        let mut pos = HEADER_SIZE + sample_len;
        let mut tracks = Vec::new();
        let mut command_count = 0usize;
        let mut total_gd3_bytes = 0usize;
        loop {
            check_limit("track count", tracks.len() + 1, limits.max_tracks)?;
            let len = read_u32_le_at(bytes, pos)? as usize;
            pos = pos
                .checked_add(4)
                .ok_or_else(|| invalid("XGM track position overflow"))?;
            let music = read_slice(bytes, pos, len)?;
            pos = pos
                .checked_add(len)
                .ok_or_else(|| invalid("XGM track length overflow"))?;
            let commands = parse_commands(music, &mut command_count, limits.max_commands)?;
            let gd3 = if flags & 2 != 0 {
                let metadata_len = read_u32_le_at(bytes, pos + 8)? as usize;
                let total = if metadata_len == 0 {
                    let mut end = pos + 12;
                    check_limit("GD3 bytes", 12, limits.max_gd3_bytes)?;
                    check_total_gd3(total_gd3_bytes, 12, limits.max_total_gd3_bytes)?;
                    for _ in 0..11 {
                        loop {
                            let next = end
                                .checked_add(2)
                                .ok_or_else(|| invalid("XGM GD3 length overflow"))?;
                            check_limit("GD3 bytes", next - pos, limits.max_gd3_bytes)?;
                            check_total_gd3(
                                total_gd3_bytes,
                                next - pos,
                                limits.max_total_gd3_bytes,
                            )?;
                            let code = read_u16_le_at(bytes, end)?;
                            end = next;
                            if code == 0 {
                                break;
                            }
                        }
                    }
                    end - pos
                } else {
                    metadata_len
                        .checked_add(12)
                        .ok_or_else(|| invalid("XGM GD3 length overflow"))?
                };
                check_limit("GD3 bytes", total, limits.max_gd3_bytes)?;
                total_gd3_bytes =
                    check_total_gd3(total_gd3_bytes, total, limits.max_total_gd3_bytes)?;
                let mut raw = read_slice(bytes, pos, total)?.to_vec();
                if metadata_len == 0 {
                    let len = u32::try_from(total - 12)
                        .map_err(|_| invalid("XGM GD3 length overflow"))?;
                    raw[8..12].copy_from_slice(&len.to_le_bytes());
                }
                let metadata = Gd3::parse_with_limits(
                    &raw,
                    Gd3ParseLimits {
                        max_gd3_bytes: limits.max_gd3_bytes,
                    },
                )?;
                pos = pos
                    .checked_add(total)
                    .ok_or_else(|| invalid("XGM GD3 position overflow"))?;
                Some(metadata)
            } else {
                None
            };
            tracks.push(XgmTrack { commands, gd3 });
            if flags & 4 == 0 || pos == bytes.len() {
                break;
            }
        }
        if pos != bytes.len() {
            return Err(invalid("unexpected trailing data after XGM track"));
        }
        let document = Self {
            header: xgm_header,
            sample_data,
            tracks,
        };
        document.validate()?;
        Ok(document)
    }

    /// Validate and serialize the document as an XGM v1 file.
    ///
    /// The document can also be serialized with `Vec::<u8>::try_from` or
    /// `try_into()`, using either an owned or borrowed document.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ParseError> {
        self.validate()?;
        let mut output = Vec::new();
        output.extend_from_slice(b"XGM ");
        for sample in self.header.samples {
            let (offset, length) = match sample {
                Some(sample) => ((sample.offset / 256) as u16, (sample.length / 256) as u16),
                None => (u16::MAX, 1),
            };
            output.extend_from_slice(&offset.to_le_bytes());
            output.extend_from_slice(&length.to_le_bytes());
        }
        output.extend_from_slice(&((self.sample_data.len() / 256) as u16).to_le_bytes());
        output.push(1);
        let flags = u8::from(self.header.timing == XgmTiming::Pal)
            | (u8::from(self.tracks[0].gd3.is_some()) << 1)
            | (u8::from(self.tracks.len() > 1) << 2);
        output.push(flags);
        output.extend_from_slice(&self.sample_data);
        for track in &self.tracks {
            let music = serialize_commands(&track.commands)?;
            let len =
                u32::try_from(music.len()).map_err(|_| invalid("XGM music block too large"))?;
            output.extend_from_slice(&len.to_le_bytes());
            output.extend_from_slice(&music);
            if let Some(gd3) = &track.gd3 {
                output.extend_from_slice(&gd3.to_bytes());
            }
        }
        Ok(output)
    }

    /// Check sample ranges, track metadata, commands, and loop targets.
    pub fn validate(&self) -> Result<(), ParseError> {
        if !self.sample_data.len().is_multiple_of(256)
            || self.sample_data.len() / 256 > u16::MAX as usize
        {
            return Err(invalid(
                "XGM sample data must be 256-byte aligned and fit in the header",
            ));
        }
        for sample in self.header.samples.iter().flatten() {
            let offset = sample.offset as usize;
            let length = sample.length as usize;
            if !offset.is_multiple_of(256)
                || length == 0
                || !length.is_multiple_of(256)
                || offset / 256 >= u16::MAX as usize
                || length / 256 > u16::MAX as usize
                || offset
                    .checked_add(length)
                    .is_none_or(|end| end > self.sample_data.len())
            {
                return Err(invalid("invalid XGM sample range"));
            }
        }
        let Some(first) = self.tracks.first() else {
            return Err(invalid("XGM must contain at least one track"));
        };
        if self
            .tracks
            .iter()
            .any(|track| track.gd3.is_some() != first.gd3.is_some())
        {
            return Err(invalid("all XGM tracks must agree on GD3 presence"));
        }
        for track in &self.tracks {
            if !matches!(
                track.commands.last(),
                Some(XgmCommand::End | XgmCommand::Loop(_))
            ) {
                return Err(invalid("XGM track must end with an end or loop command"));
            }
            serialize_commands(&track.commands)?;
            for command in &track.commands {
                if let XgmCommand::Pcm { sample_id, .. } = command
                    && (1..=63).contains(sample_id)
                    && self.header.samples[usize::from(*sample_id - 1)].is_none()
                {
                    return Err(invalid(format!("XGM sample {sample_id} is not defined")));
                }
            }
        }
        Ok(())
    }
}

/// Validate and serialize an owned XGM document.
impl TryFrom<XgmDocument> for Vec<u8> {
    type Error = ParseError;

    fn try_from(document: XgmDocument) -> Result<Self, Self::Error> {
        document.to_bytes()
    }
}

/// Validate and serialize a borrowed XGM document.
impl TryFrom<&XgmDocument> for Vec<u8> {
    type Error = ParseError;

    fn try_from(document: &XgmDocument) -> Result<Self, Self::Error> {
        document.to_bytes()
    }
}

/// Incrementally construct a document with a shared sample bank.
pub struct XgmBuilder {
    document: XgmDocument,
}

impl XgmBuilder {
    /// Start an empty document. Add at least one track before finalizing.
    pub fn new(timing: XgmTiming) -> Self {
        Self {
            document: XgmDocument {
                header: XgmHeader::new(timing),
                sample_data: Vec::new(),
                tracks: Vec::new(),
            },
        }
    }

    /// Append an aligned signed PCM sample and assign it to ID 1–63.
    pub fn set_sample(&mut self, id: u8, data: &[u8]) -> Result<&mut Self, ParseError> {
        if !(1..=63).contains(&id) || data.is_empty() || !data.len().is_multiple_of(256) {
            return Err(invalid(
                "XGM sample ID must be 1–63 and data must be nonempty and 256-byte aligned",
            ));
        }
        let offset = self.document.sample_data.len();
        let end = offset
            .checked_add(data.len())
            .ok_or_else(|| invalid("XGM sample bank overflow"))?;
        if end / 256 > u16::MAX as usize || offset / 256 >= u16::MAX as usize {
            return Err(invalid("XGM sample bank exceeds the format limit"));
        }
        self.document.sample_data.extend_from_slice(data);
        self.document.header.samples[usize::from(id - 1)] = Some(XgmSample {
            offset: offset as u32,
            length: data.len() as u32,
        });
        Ok(self)
    }

    /// Append a music track.
    pub fn add_track(&mut self, track: XgmTrack) -> &mut Self {
        self.document.tracks.push(track);
        self
    }

    /// Validate and return the document.
    pub fn finalize(self) -> Result<XgmDocument, ParseError> {
        self.document.validate()?;
        Ok(self.document)
    }
}

fn parse_commands(
    bytes: &[u8],
    command_count: &mut usize,
    max_commands: Option<usize>,
) -> Result<Vec<XgmCommand>, ParseError> {
    let mut commands = Vec::new();
    let mut offsets = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let next_count = command_count
            .checked_add(1)
            .ok_or_else(|| invalid("XGM command count overflow"))?;
        check_limit("command count", next_count, max_commands)?;
        *command_count = next_count;
        offsets.push(pos);
        let opcode = bytes[pos];
        pos += 1;
        let count = usize::from(opcode & 15) + 1;
        let command = match opcode {
            0 => XgmCommand::Frame,
            0x10..=0x1f | 0x40..=0x4f => {
                let data = read_slice(bytes, pos, count)?.to_vec();
                pos += count;
                if opcode < 0x20 {
                    XgmCommand::Psg(data)
                } else {
                    XgmCommand::Ym2612Key(data)
                }
            }
            0x20..=0x3f => {
                let data = read_slice(bytes, pos, count * 2)?;
                pos += count * 2;
                let writes = data
                    .chunks_exact(2)
                    .map(|pair| XgmRegisterWrite {
                        register: pair[0],
                        value: pair[1],
                    })
                    .collect();
                if opcode < 0x30 {
                    XgmCommand::Ym2612Port0(writes)
                } else {
                    XgmCommand::Ym2612Port1(writes)
                }
            }
            0x50..=0x5f => {
                let sample_id = *read_slice(bytes, pos, 1)?.first().unwrap();
                pos += 1;
                XgmCommand::Pcm {
                    channel: opcode & 3,
                    priority: (opcode >> 2) & 3,
                    sample_id,
                }
            }
            0x7e => {
                let data = read_slice(bytes, pos, 3)?;
                pos += 3;
                XgmCommand::Loop(
                    usize::from(data[0])
                        | (usize::from(data[1]) << 8)
                        | (usize::from(data[2]) << 16),
                )
            }
            0x7f => XgmCommand::End,
            _ => {
                return Err(ParseError::UnknownOpcode {
                    opcode,
                    offset: pos - 1,
                });
            }
        };
        commands.push(command);
    }
    for command in &mut commands {
        if let XgmCommand::Loop(target) = command {
            *target = offsets
                .binary_search(target)
                .map_err(|_| invalid("XGM loop target is not a command boundary"))?;
        }
    }
    Ok(commands)
}

fn serialize_commands(commands: &[XgmCommand]) -> Result<Vec<u8>, ParseError> {
    let mut output = Vec::new();
    let mut offsets = Vec::with_capacity(commands.len());
    let mut loops = Vec::new();
    for command in commands {
        offsets.push(output.len());
        match command {
            XgmCommand::Frame => output.push(0),
            XgmCommand::Psg(data) | XgmCommand::Ym2612Key(data) => {
                if !(1..=16).contains(&data.len()) {
                    return Err(invalid("XGM write group must contain 1–16 entries"));
                }
                let base = if matches!(command, XgmCommand::Psg(_)) {
                    0x10
                } else {
                    0x40
                };
                output.push(base | (data.len() as u8 - 1));
                output.extend_from_slice(data);
            }
            XgmCommand::Ym2612Port0(writes) | XgmCommand::Ym2612Port1(writes) => {
                if !(1..=16).contains(&writes.len()) {
                    return Err(invalid("XGM write group must contain 1–16 entries"));
                }
                let base = if matches!(command, XgmCommand::Ym2612Port0(_)) {
                    0x20
                } else {
                    0x30
                };
                output.push(base | (writes.len() as u8 - 1));
                for write in writes {
                    output.extend_from_slice(&[write.register, write.value]);
                }
            }
            XgmCommand::Pcm {
                channel,
                priority,
                sample_id,
            } => {
                if *channel > 3 || *priority > 3 || *sample_id > 63 {
                    return Err(invalid("invalid XGM PCM command"));
                }
                output.extend_from_slice(&[0x50 | (priority << 2) | channel, *sample_id]);
            }
            XgmCommand::Loop(target) => {
                output.push(0x7e);
                loops.push((output.len(), *target));
                output.extend_from_slice(&[0; 3]);
            }
            XgmCommand::End => output.push(0x7f),
        }
    }
    for (pos, target) in loops {
        let offset = *offsets
            .get(target)
            .ok_or_else(|| invalid("XGM loop target is out of range"))?;
        if offset > 0xff_ffff {
            return Err(invalid("XGM loop target exceeds 24 bits"));
        }
        output[pos..pos + 3].copy_from_slice(&offset.to_le_bytes()[..3]);
    }
    Ok(output)
}

/// Create an XGM data validation error.
fn invalid(message: impl Into<String>) -> ParseError {
    ParseError::DataInconsistency(message.into())
}

/// Reject a resource value that exceeds its configured limit.
fn check_limit(resource: &str, value: usize, limit: Option<usize>) -> Result<(), ParseError> {
    if let Some(limit) = limit
        && value > limit
    {
        return Err(invalid(format!(
            "XGM {resource} limit exceeded (maximum {limit})"
        )));
    }

    Ok(())
}

/// Add a GD3 chunk to the cumulative metadata budget.
fn check_total_gd3(
    current: usize,
    bytes: usize,
    limit: Option<usize>,
) -> Result<usize, ParseError> {
    let total = current
        .checked_add(bytes)
        .ok_or_else(|| invalid("XGM GD3 total length overflow"))?;
    check_limit("total GD3 bytes", total, limit)?;
    Ok(total)
}
