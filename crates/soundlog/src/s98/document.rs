//! Typed S98 documents and checked parsing and serialization.

use crate::ParseError;
use crate::binutil::{read_slice, read_u32_le_at, write_u32};

/// Supported S98 format versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S98Version {
    /// Original S98 format.
    V1,
    /// S98 version 2.
    V2,
    /// S98 version 3.
    V3,
}

/// Device identifier, including unrecognized on-disk values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum S98DeviceType {
    /// Disabled device.
    None,
    /// Yamaha YM2149 PSG.
    Ym2149,
    /// Yamaha YM2203.
    Ym2203,
    /// Yamaha YM2612.
    Ym2612,
    /// Yamaha YM2608.
    Ym2608,
    /// Yamaha YM2151.
    Ym2151,
    /// Yamaha YM2413.
    Ym2413,
    /// Yamaha YM3526.
    Ym3526,
    /// Yamaha YM3812.
    Ym3812,
    /// Yamaha YMF262.
    Ymf262,
    /// General Instrument AY-3-8910.
    Ay8910,
    /// Texas Instruments SN76489.
    Sn76489,
    /// An unrecognized on-disk identifier.
    Unknown(u32),
}

impl From<u32> for S98DeviceType {
    fn from(value: u32) -> Self {
        match value {
            0 => Self::None,
            1 => Self::Ym2149,
            2 => Self::Ym2203,
            3 => Self::Ym2612,
            4 => Self::Ym2608,
            5 => Self::Ym2151,
            6 => Self::Ym2413,
            7 => Self::Ym3526,
            8 => Self::Ym3812,
            9 => Self::Ymf262,
            15 => Self::Ay8910,
            16 => Self::Sn76489,
            value => Self::Unknown(value),
        }
    }
}

impl From<S98DeviceType> for u32 {
    fn from(value: S98DeviceType) -> Self {
        match value {
            S98DeviceType::None => 0,
            S98DeviceType::Ym2149 => 1,
            S98DeviceType::Ym2203 => 2,
            S98DeviceType::Ym2612 => 3,
            S98DeviceType::Ym2608 => 4,
            S98DeviceType::Ym2151 => 5,
            S98DeviceType::Ym2413 => 6,
            S98DeviceType::Ym3526 => 7,
            S98DeviceType::Ym3812 => 8,
            S98DeviceType::Ymf262 => 9,
            S98DeviceType::Ay8910 => 15,
            S98DeviceType::Sn76489 => 16,
            S98DeviceType::Unknown(value) => value,
        }
    }
}

/// Device table entry. Reserved bytes are retained when serializing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct S98Device {
    /// Device identifier.
    pub kind: S98DeviceType,
    /// External clock frequency in hertz.
    pub clock: u32,
    /// S98 channel mute bits.
    pub pan: u32,
    /// Reserved field.
    pub reserved: u32,
}

impl S98Device {
    /// Construct a device with no panning and a zero reserved field.
    pub const fn new(kind: S98DeviceType, clock: u32) -> Self {
        Self {
            kind,
            clock,
            pan: 0,
            reserved: 0,
        }
    }
}

/// The two S98 device write ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S98Port {
    /// Normal port.
    Normal,
    /// Extended port.
    Extended,
}

/// A dump command. The end marker is represented by the end of the command list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S98Command {
    /// Write a register on the indexed device.
    Write {
        /// Zero-based index in the effective device list.
        device: u8,
        /// Device port.
        port: S98Port,
        /// Register address.
        register: u8,
        /// Register value.
        value: u8,
    },
    /// Advance by a positive number of S98 synchronization ticks.
    Wait(u64),
}

/// Format version, timing, and explicit device entries.
///
/// Zero timing fields use the S98 defaults of 10/1000 seconds. An empty
/// device list denotes one YM2608 at 7,987,200 Hz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S98Header {
    /// S98 version.
    pub version: S98Version,
    /// Tick duration numerator, as stored.
    pub timer_numerator: u32,
    /// Tick duration denominator, as stored.
    pub timer_denominator: u32,
    /// Explicit device entries; an empty list selects the default device.
    pub devices: Vec<S98Device>,
}

impl S98Header {
    /// Construct a header using the default timing and device.
    pub fn new(version: S98Version) -> Self {
        Self {
            version,
            timer_numerator: 0,
            timer_denominator: 0,
            devices: Vec::new(),
        }
    }

    /// Return the effective devices, including the implicit legacy default.
    pub fn effective_devices(&self) -> Vec<S98Device> {
        if self.devices.is_empty() {
            vec![S98Device::new(S98DeviceType::Ym2608, 7_987_200)]
        } else {
            self.devices.clone()
        }
    }
}

/// Bounds applied when parsing an S98 document.
#[derive(Debug, Clone, Copy)]
pub struct S98ParseLimits {
    /// Maximum total file size. Default: 64 MiB.
    pub max_file_bytes: Option<usize>,
    /// Maximum retained command count. Default: 1,000,000.
    pub max_commands: Option<usize>,
    /// Maximum tag size in bytes. Default: 1 MiB.
    pub max_tag_bytes: Option<usize>,
}

impl Default for S98ParseLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: Some(64 * 1024 * 1024),
            max_commands: Some(1_000_000),
            max_tag_bytes: Some(1024 * 1024),
        }
    }
}

/// An S98 file, with its loop represented by a command index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S98Document {
    /// Header and device declarations.
    pub header: S98Header,
    /// Commands before the end marker.
    pub commands: Vec<S98Command>,
    /// Loop target, including `commands.len()` for the end marker.
    pub loop_index: Option<usize>,
    /// Raw tag bytes, excluding an optional NUL terminator.
    pub tag: Option<Vec<u8>>,
}

fn invalid(message: impl Into<String>) -> ParseError {
    ParseError::DataInconsistency(message.into())
}

fn limit(name: &str, value: usize, maximum: Option<usize>) -> Result<(), ParseError> {
    if maximum.is_some_and(|max| value > max) {
        return Err(invalid(format!("S98 {name} limit exceeded")));
    }
    Ok(())
}

impl S98Document {
    /// Decode the tag as text without modifying the original bytes.
    ///
    /// Version 3 tags must start with `[S98]`. That identifier and a following
    /// UTF-8 BOM, if present, are omitted from the returned text. A v3 tag
    /// with a BOM is decoded as UTF-8; all other tags are assumed to use
    /// Shift_JIS. S98 does not identify the encoding of tags without a BOM,
    /// so this assumption may not match the source file. Invalid text and
    /// invalid v3 identifiers return an error.
    pub fn tag_text(&self) -> Result<Option<String>, ParseError> {
        let Some(tag) = self.tag.as_deref() else {
            return Ok(None);
        };
        let body = if self.header.version == S98Version::V3 {
            let body = tag
                .strip_prefix(b"[S98]")
                .ok_or_else(|| invalid("invalid S98 tag identifier"))?;
            if let Some(utf8) = body.strip_prefix(&[0xef, 0xbb, 0xbf]) {
                let text = std::str::from_utf8(utf8)
                    .map_err(|error| invalid(format!("invalid S98 UTF-8 tag: {error}")))?;
                return Ok(Some(text.to_owned()));
            }
            body
        } else {
            tag
        };
        let text = crate::encoding::decode_shift_jis_strict(body)
            .map_err(|error| invalid(format!("invalid S98 Shift_JIS tag: {error}")))?;
        Ok(Some(text))
    }

    /// Parse an uncompressed S98 file using default resource limits.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        Self::parse_with_limits(bytes, S98ParseLimits::default())
    }

    /// Parse an uncompressed S98 file using caller-selected limits.
    pub fn parse_with_limits(bytes: &[u8], limits: S98ParseLimits) -> Result<Self, ParseError> {
        limit("file size", bytes.len(), limits.max_file_bytes)?;
        let head = read_slice(bytes, 0, 0x20)?;
        if &head[..3] != b"S98" {
            return Err(ParseError::InvalidIdent(head[..4].try_into().unwrap()));
        }
        let version = match head[3] {
            b'1' => S98Version::V1,
            b'2' => S98Version::V2,
            b'3' => S98Version::V3,
            other => return Err(ParseError::UnsupportedVersion(u32::from(other))),
        };
        if read_u32_le_at(bytes, 0x0c)? != 0 {
            return Err(invalid("compressed S98 files are not supported"));
        }
        let tag_offset = read_u32_le_at(bytes, 0x10)? as usize;
        let data_offset = read_u32_le_at(bytes, 0x14)? as usize;
        let loop_offset = read_u32_le_at(bytes, 0x18)? as usize;
        if data_offset < 0x20 || data_offset >= bytes.len() {
            return Err(invalid("S98 data offset is out of range"));
        }
        if tag_offset != 0
            && (tag_offset < 0x20 || tag_offset >= bytes.len() || tag_offset == data_offset)
        {
            return Err(invalid("S98 tag offset is out of range"));
        }
        let table_end = if tag_offset != 0 && tag_offset < data_offset {
            tag_offset
        } else {
            data_offset
        };
        let mut devices = Vec::new();
        let count = if version == S98Version::V3 {
            let count = read_u32_le_at(bytes, 0x1c)? as usize;
            if count > 64 || 0x20 + count * 16 > table_end {
                return Err(invalid("invalid S98 device table length"));
            }
            count
        } else {
            ((table_end - 0x20) / 16).min(64)
        };
        for i in 0..count {
            let pos = 0x20 + 16 * i;
            let kind = read_u32_le_at(bytes, pos)?;
            if version != S98Version::V3 && kind == 0 {
                break;
            }
            devices.push(S98Device {
                kind: kind.into(),
                clock: read_u32_le_at(bytes, pos + 4)?,
                pan: read_u32_le_at(bytes, pos + 8)?,
                reserved: read_u32_le_at(bytes, pos + 12)?,
            });
        }
        let tag = if tag_offset == 0 {
            None
        } else {
            let end = if tag_offset < data_offset {
                data_offset
            } else {
                bytes.len()
            };
            let raw = &bytes[tag_offset..end];
            let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            limit("tag size", len, limits.max_tag_bytes)?;
            Some(raw[..len].to_vec())
        };
        let dump_end = if tag_offset > data_offset {
            tag_offset
        } else {
            bytes.len()
        };
        let mut pos = data_offset;
        let mut commands = Vec::new();
        let mut loop_index = None;
        loop {
            if pos == loop_offset && loop_offset != 0 {
                loop_index = Some(commands.len());
            }
            let opcode = *bytes
                .get(pos)
                .filter(|_| pos < dump_end)
                .ok_or(ParseError::UnexpectedEof)?;
            pos += 1;
            if opcode == 0xfd {
                break;
            }
            limit("command count", commands.len() + 1, limits.max_commands)?;
            let command = match opcode {
                0xff => S98Command::Wait(1),
                0xfe => {
                    let mut value = 0u64;
                    let mut shift = 0;
                    loop {
                        let byte = *bytes
                            .get(pos)
                            .filter(|_| pos < dump_end)
                            .ok_or(ParseError::UnexpectedEof)?;
                        pos += 1;
                        if shift >= 64 || (shift == 63 && byte & 0x7e != 0) {
                            return Err(invalid("S98 wait value overflow"));
                        }
                        value |= u64::from(byte & 0x7f) << shift;
                        if byte & 0x80 == 0 {
                            break;
                        }
                        shift += 7;
                    }
                    S98Command::Wait(
                        value
                            .checked_add(2)
                            .ok_or_else(|| invalid("S98 wait value overflow"))?,
                    )
                }
                _ => {
                    if pos.checked_add(2).is_none_or(|end| end > dump_end) {
                        return Err(ParseError::UnexpectedEof);
                    }
                    let command = S98Command::Write {
                        device: opcode / 2,
                        port: if opcode & 1 == 0 {
                            S98Port::Normal
                        } else {
                            S98Port::Extended
                        },
                        register: bytes[pos],
                        value: bytes[pos + 1],
                    };
                    pos += 2;
                    command
                }
            };
            commands.push(command);
        }
        if loop_offset != 0 && loop_index.is_none() {
            return Err(invalid("S98 loop offset is not a command boundary"));
        }
        let document = Self {
            header: S98Header {
                version,
                timer_numerator: read_u32_le_at(bytes, 4)?,
                timer_denominator: read_u32_le_at(bytes, 8)?,
                devices,
            },
            commands,
            loop_index,
            tag,
        };
        document.validate()?;
        Ok(document)
    }

    /// Validate structural invariants, including device indices and loop target.
    pub fn validate(&self) -> Result<(), ParseError> {
        if self.header.devices.len() > 64 {
            return Err(invalid("S98 supports at most 64 devices"));
        }
        if self.header.devices.iter().any(|d| matches!(d.kind, S98DeviceType::Unknown(raw) if !matches!(S98DeviceType::from(raw), S98DeviceType::Unknown(_)))) {
            return Err(invalid("S98 device type must use its known variant"));
        }
        if self.header.version != S98Version::V3
            && self.header.devices.iter().any(|d| u32::from(d.kind) == 0)
        {
            return Err(invalid("legacy S98 device type zero terminates the table"));
        }
        if self.loop_index.is_some_and(|i| i > self.commands.len()) {
            return Err(invalid("S98 loop index is out of range"));
        }
        if self.tag.as_ref().is_some_and(|tag| tag.contains(&0)) {
            return Err(invalid("S98 tag contains a NUL byte"));
        }
        let count = self.header.devices.len().max(1);
        for command in &self.commands {
            match command {
                S98Command::Write { device, .. } if usize::from(*device) >= count => {
                    return Err(invalid("S98 write references an undeclared device"));
                }
                S98Command::Wait(0) => return Err(invalid("S98 wait must be positive")),
                _ => {}
            }
        }
        Ok(())
    }

    /// Validate and serialize with recalculated offsets.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ParseError> {
        self.validate()?;
        let legacy_terminator =
            usize::from(self.header.version != S98Version::V3 && !self.header.devices.is_empty());
        let size = 0x20 + (self.header.devices.len() + legacy_terminator) * 16;
        let mut out = vec![0u8; size];
        out[..4].copy_from_slice(match self.header.version {
            S98Version::V1 => b"S981",
            S98Version::V2 => b"S982",
            S98Version::V3 => b"S983",
        });
        write_u32(&mut out, 4, self.header.timer_numerator);
        write_u32(&mut out, 8, self.header.timer_denominator);
        if self.header.version == S98Version::V3 {
            write_u32(&mut out, 0x1c, self.header.devices.len() as u32);
        }
        for (i, device) in self.header.devices.iter().enumerate() {
            let pos = 0x20 + 16 * i;
            write_u32(&mut out, pos, device.kind.into());
            write_u32(&mut out, pos + 4, device.clock);
            write_u32(&mut out, pos + 8, device.pan);
            write_u32(&mut out, pos + 12, device.reserved);
        }
        write_u32(&mut out, 0x14, size as u32);
        let mut loop_offset = None;
        for (index, command) in self.commands.iter().enumerate() {
            if self.loop_index == Some(index) {
                loop_offset = Some(out.len());
            }
            match *command {
                S98Command::Write {
                    device,
                    port,
                    register,
                    value,
                } => {
                    out.extend_from_slice(&[
                        device * 2 + u8::from(port == S98Port::Extended),
                        register,
                        value,
                    ]);
                }
                S98Command::Wait(1) => out.push(0xff),
                S98Command::Wait(ticks) => {
                    out.push(0xfe);
                    let mut value = ticks - 2;
                    while value >= 128 {
                        out.push((value as u8 & 0x7f) | 0x80);
                        value >>= 7;
                    }
                    out.push(value as u8);
                }
            }
        }
        if self.loop_index == Some(self.commands.len()) {
            loop_offset = Some(out.len());
        }
        out.push(0xfd);
        if let Some(offset) = loop_offset {
            write_u32(
                &mut out,
                0x18,
                u32::try_from(offset)
                    .map_err(|_| invalid("S98 loop offset exceeds format limit"))?,
            );
        }
        if let Some(tag) = &self.tag {
            let offset = u32::try_from(out.len())
                .map_err(|_| invalid("S98 tag offset exceeds format limit"))?;
            write_u32(&mut out, 0x10, offset);
            out.extend_from_slice(tag);
            out.push(0);
        }
        Ok(out)
    }
}

impl TryFrom<&S98Document> for Vec<u8> {
    type Error = ParseError;
    fn try_from(value: &S98Document) -> Result<Self, Self::Error> {
        value.to_bytes()
    }
}

impl TryFrom<S98Document> for Vec<u8> {
    type Error = ParseError;
    fn try_from(value: S98Document) -> Result<Self, Self::Error> {
        value.to_bytes()
    }
}

/// Builder for S98 documents.
#[derive(Debug, Clone)]
pub struct S98Builder {
    document: S98Document,
}

impl S98Builder {
    /// Start a document with the specified header.
    pub fn new(header: S98Header) -> Self {
        Self {
            document: S98Document {
                header,
                commands: Vec::new(),
                loop_index: None,
                tag: None,
            },
        }
    }

    /// Append a command.
    pub fn push(&mut self, command: S98Command) -> &mut Self {
        self.document.commands.push(command);
        self
    }

    /// Set the loop target to the next command.
    pub fn mark_loop(&mut self) -> &mut Self {
        self.document.loop_index = Some(self.document.commands.len());
        self
    }

    /// Set raw metadata bytes without a terminating NUL.
    pub fn set_tag(&mut self, tag: Vec<u8>) -> &mut Self {
        self.document.tag = Some(tag);
        self
    }

    /// Validate and return the document.
    pub fn finalize(self) -> Result<S98Document, ParseError> {
        self.document.validate()?;
        Ok(self.document)
    }
}
