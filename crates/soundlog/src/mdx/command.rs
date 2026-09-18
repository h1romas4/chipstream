//! MDX command definitions.
//!
//! The command values in this module describe the byte-level MDX track
//! language. Parsing a command stream is kept separate so that track-relative
//! jumps and loop state do not leak into these data types.
//!
//! Responsibilities:
//! - Represent rests, notes, register writes, control flow, LFOs, and PCM
//!   mode changes as typed `MdxCommand` variants.
//! - Define command-specific operand types and their byte serialization.
//! - Keep command specifications independent from document-level track
//!   offsets, repeat handling, and playback state.

use crate::ParseError;
use crate::binutil::{read_i16_be_at, read_u8_at, read_u16_be_at};

/// Trait implemented by an individual MDX command specification.
pub(crate) trait MdxCommandSpec: Sized {
    fn opcode(&self) -> u8;
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>);
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>;
}

/// A typed MDX track command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdxCommand {
    Rest(MdxRest),
    Note(MdxNote),
    Tempo(MdxTempo),
    OpmRegisterWrite(MdxOpmRegisterWrite),
    VoiceOrPcmBank(MdxVoiceOrPcmBank),
    Pan(MdxPan),
    Volume(MdxVolume),
    VolumeDown(MdxVolumeDown),
    VolumeUp(MdxVolumeUp),
    Gate(MdxGate),
    KeyOffDisable(MdxKeyOffDisable),
    LoopStart(MdxLoopStart),
    LoopEnd(MdxRelativeOffset),
    LoopEscape(MdxRelativeOffset),
    Detune(MdxSignedWord),
    Portamento(MdxSignedWord),
    EndOfTrack(MdxEndOfTrack),
    Jump(MdxRelativeOffset),
    KeyOnDelay(MdxKeyOnDelay),
    SyncSend(MdxSyncSend),
    SyncWait(MdxSyncWait),
    AdpcmOrNoiseFrequency(MdxAdpcmOrNoiseFrequency),
    PitchLfo(MdxPitchLfo),
    VolumeLfo(MdxVolumeLfo),
    OpmLfo(MdxOpmLfo),
    LfoDelay(MdxLfoDelay),
    PcmMode(MdxPcmMode),
    Extended(MdxExtendedCommand),
    Extended2(MdxExtended2Command),
    Raw(MdxRawCommand),
}

impl MdxCommand {
    /// Serialize commands whose byte-level representation is currently defined.
    pub fn to_mdx_bytes(&self) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        match self {
            Self::Rest(command) => command.to_mdx_bytes(&mut bytes),
            Self::Note(command) => command.to_mdx_bytes(&mut bytes),
            Self::Tempo(command) => command.to_mdx_bytes(&mut bytes),
            Self::OpmRegisterWrite(command) => command.to_mdx_bytes(&mut bytes),
            Self::VoiceOrPcmBank(command) => command.to_mdx_bytes(&mut bytes),
            Self::Volume(command) => command.to_mdx_bytes(&mut bytes),
            Self::KeyOnDelay(command) => command.to_mdx_bytes(&mut bytes),
            Self::SyncSend(command) => command.to_mdx_bytes(&mut bytes),
            Self::AdpcmOrNoiseFrequency(command) => command.to_mdx_bytes(&mut bytes),
            Self::LfoDelay(command) => command.to_mdx_bytes(&mut bytes),
            Self::Pan(command) => command.to_mdx_bytes(&mut bytes),
            Self::Gate(command) => command.to_mdx_bytes(&mut bytes),
            Self::LoopStart(command) => command.to_mdx_bytes(&mut bytes),
            Self::LoopEnd(command) | Self::LoopEscape(command) | Self::Jump(command) => {
                command.to_mdx_bytes(&mut bytes)
            }
            Self::Detune(command) | Self::Portamento(command) => command.to_mdx_bytes(&mut bytes),
            Self::VolumeDown(command) => command.to_mdx_bytes(&mut bytes),
            Self::VolumeUp(command) => command.to_mdx_bytes(&mut bytes),
            Self::KeyOffDisable(command) => command.to_mdx_bytes(&mut bytes),
            Self::SyncWait(command) => command.to_mdx_bytes(&mut bytes),
            Self::PcmMode(command) => command.to_mdx_bytes(&mut bytes),
            Self::EndOfTrack(command) => command.to_mdx_bytes(&mut bytes),
            Self::Raw(command) => command.to_mdx_bytes(&mut bytes),
            Self::PitchLfo(command) => command.to_mdx_bytes(&mut bytes),
            Self::VolumeLfo(command) => command.to_mdx_bytes(&mut bytes),
            Self::OpmLfo(command) => command.to_mdx_bytes(&mut bytes),
            Self::Extended(command) => command.to_mdx_bytes(&mut bytes),
            Self::Extended2(command) => command.to_mdx_bytes(&mut bytes),
        }
        Some(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxVolumeDown;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxVolumeUp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxKeyOffDisable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxSyncWait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxPcmMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxEndOfTrack;

impl MdxCommandSpec for MdxEndOfTrack {
    fn opcode(&self) -> u8 {
        0xf1
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), 0x00]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        if read_u8_at(bytes, offset)? != 0 {
            return Err(ParseError::Other("invalid MDX end-of-track command".into()));
        }
        Ok((Self, 2))
    }
}

impl MdxCommandSpec for MdxVolumeDown {
    fn opcode(&self) -> u8 {
        0xfa
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((Self, 1))
    }
}

impl MdxCommandSpec for MdxVolumeUp {
    fn opcode(&self) -> u8 {
        0xf9
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((Self, 1))
    }
}

impl MdxCommandSpec for MdxKeyOffDisable {
    fn opcode(&self) -> u8 {
        0xf7
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((Self, 1))
    }
}

impl MdxCommandSpec for MdxSyncWait {
    fn opcode(&self) -> u8 {
        0xee
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((Self, 1))
    }
}

impl MdxCommandSpec for MdxPcmMode {
    fn opcode(&self) -> u8 {
        0xe8
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((Self, 1))
    }
}

/// A rest command (`0x00..=0x7f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxRest {
    pub ticks: u16,
}

impl MdxCommandSpec for MdxRest {
    fn opcode(&self) -> u8 {
        (self.ticks - 1) as u8
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        MdxRest::new(u16::from(opcode) + 1)
            .map(|command| (command, 1))
            .ok_or_else(|| ParseError::Other("invalid MDX rest opcode".into()))
    }
}

impl MdxRest {
    pub const fn new(ticks: u16) -> Option<Self> {
        if ticks == 0 || ticks > 0x80 {
            None
        } else {
            Some(Self { ticks })
        }
    }

    pub const fn opcode(self) -> u8 {
        (self.ticks - 1) as u8
    }
}

/// A note command (`0x80..=0xdf`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxNote {
    pub note: u8,
    pub length: u16,
}

impl MdxNote {
    pub const fn new(note: u8, length: u16) -> Option<Self> {
        if note < 0x80 || note > 0xdf || length == 0 || length > 0x100 {
            None
        } else {
            Some(Self { note, length })
        }
    }

    pub const fn length_byte(self) -> u8 {
        (self.length - 1) as u8
    }
}

impl MdxCommandSpec for MdxNote {
    fn opcode(&self) -> u8 {
        self.note
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.note);
        dest.push(self.length_byte());
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        let length = u16::from(read_u8_at(bytes, offset)?) + 1;
        MdxNote::new(opcode, length)
            .map(|command| (command, 2))
            .ok_or_else(|| ParseError::Other("invalid MDX note opcode".into()))
    }
}

/// Set tempo (`0xff tempo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxTempo {
    pub value: u8,
}

impl MdxCommandSpec for MdxTempo {
    fn opcode(&self) -> u8 {
        0xff
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Set track pan (`0xfc pan`).
///
/// The value is kept in its raw MDX representation. MXDRV interprets values
/// `1` and `2` in opposite left/right order for FM and ADPCM tracks, so the
/// track type is required before converting this value to a playback pan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxPan {
    /// Raw MDX pan value; its left/right meaning depends on the track type.
    pub value: u8,
}

impl MdxCommandSpec for MdxPan {
    fn opcode(&self) -> u8 {
        0xfc
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Set gate time (`0xf8 gate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxGate {
    pub value: u8,
}

impl MdxCommandSpec for MdxGate {
    fn opcode(&self) -> u8 {
        0xf8
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Select an FM voice or PCM bank (`0xfd value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxVoiceOrPcmBank {
    pub value: u8,
}

impl MdxCommandSpec for MdxVoiceOrPcmBank {
    fn opcode(&self) -> u8 {
        0xfd
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Set track volume (`0xfb value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxVolume {
    pub value: u8,
}

impl MdxCommandSpec for MdxVolume {
    fn opcode(&self) -> u8 {
        0xfb
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Set key-on delay (`0xf0 value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxKeyOnDelay {
    pub value: u8,
}

impl MdxCommandSpec for MdxKeyOnDelay {
    fn opcode(&self) -> u8 {
        0xf0
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Send a synchronization value (`0xef value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxSyncSend {
    pub value: u8,
}

impl MdxCommandSpec for MdxSyncSend {
    fn opcode(&self) -> u8 {
        0xef
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Set ADPCM or noise frequency (`0xed value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxAdpcmOrNoiseFrequency {
    pub value: u8,
}

impl MdxCommandSpec for MdxAdpcmOrNoiseFrequency {
    fn opcode(&self) -> u8 {
        0xed
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// Set LFO delay (`0xe9 value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxLfoDelay {
    pub value: u8,
}

impl MdxCommandSpec for MdxLfoDelay {
    fn opcode(&self) -> u8 {
        0xe9
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                value: read_u8_at(bytes, offset)?,
            },
            2,
        ))
    }
}

/// A signed big-endian 16-bit command operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxSignedWord {
    pub opcode: u8,
    pub offset: i16,
}

impl MdxCommandSpec for MdxSignedWord {
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.extend_from_slice(&self.offset.to_be_bytes());
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                opcode,
                offset: read_i16_be_at(bytes, offset)?,
            },
            3,
        ))
    }
}

/// A relative big-endian 16-bit track offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxRelativeOffset {
    pub opcode: u8,
    pub offset: i16,
}

impl MdxCommandSpec for MdxRelativeOffset {
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.extend_from_slice(&self.offset.to_be_bytes());
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                opcode,
                offset: read_i16_be_at(bytes, offset)?,
            },
            3,
        ))
    }
}

/// A repeat start (`0xf6 count 0x00`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxLoopStart {
    pub count: u8,
    pub reserved: u8,
}

impl MdxCommandSpec for MdxLoopStart {
    fn opcode(&self) -> u8 {
        0xf6
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.count, self.reserved]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                count: read_u8_at(bytes, offset)?,
                reserved: read_u8_at(bytes, offset + 1)?,
            },
            3,
        ))
    }
}

/// Write one OPM register (`0xfe register value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxOpmRegisterWrite {
    pub register: u8,
    pub value: u8,
}

impl MdxCommandSpec for MdxOpmRegisterWrite {
    fn opcode(&self) -> u8 {
        0xfe
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.register, self.value]);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Ok((
            Self {
                register: read_u8_at(bytes, offset)?,
                value: read_u8_at(bytes, offset + 1)?,
            },
            3,
        ))
    }
}

/// An LFO waveform value used by pitch and volume LFO configuration commands.
///
/// Values with bit 7 set are reserved for the separate enable/disable form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdxLfoWaveform {
    Sawtooth,
    Square,
    Triangle,
    RandomNoise,
    Unknown(u8),
}

impl MdxLfoWaveform {
    pub const fn from_raw(value: u8) -> Self {
        match value {
            0x00 => Self::Sawtooth,
            0x01 => Self::Square,
            0x02 => Self::Triangle,
            0x03 => Self::RandomNoise,
            value => Self::Unknown(value),
        }
    }

    pub const fn raw(self) -> u8 {
        match self {
            Self::Sawtooth => 0x00,
            Self::Square => 0x01,
            Self::Triangle => 0x02,
            Self::RandomNoise => 0x03,
            Self::Unknown(value) => value,
        }
    }

    pub const fn base(self) -> u8 {
        self.raw() & 0x03
    }

    pub const fn base_waveform(self) -> Self {
        match self.base() {
            0 => Self::Sawtooth,
            1 => Self::Square,
            2 => Self::Triangle,
            _ => Self::RandomNoise,
        }
    }

    pub const fn has_extended_amplitude(self) -> bool {
        self.raw() >= 0x04
    }
}

/// Pitch LFO configuration or state change (`0xec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdxPitchLfo {
    SetEnabled {
        enabled: bool,
    },
    Configure {
        waveform: MdxLfoWaveform,
        frequency: u16,
        amplitude: i16,
    },
}

impl MdxCommandSpec for MdxPitchLfo {
    fn opcode(&self) -> u8 {
        0xec
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        match self {
            Self::SetEnabled { enabled } => dest.push(0x80 | u8::from(*enabled)),
            Self::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                dest.push(waveform.raw());
                dest.extend_from_slice(&frequency.to_be_bytes());
                dest.extend_from_slice(&amplitude.to_be_bytes());
            }
        }
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let waveform = read_u8_at(bytes, offset)?;
        if waveform & 0x80 != 0 {
            return Ok((
                Self::SetEnabled {
                    enabled: waveform & 1 != 0,
                },
                2,
            ));
        }
        let waveform = MdxLfoWaveform::from_raw(waveform);
        Ok((
            Self::Configure {
                waveform,
                frequency: read_u16_be_at(bytes, offset + 1)?,
                amplitude: read_i16_be_at(bytes, offset + 3)?,
            },
            6,
        ))
    }
}

/// Volume LFO configuration or state change (`0xeb`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdxVolumeLfo {
    SetEnabled {
        enabled: bool,
    },
    Configure {
        waveform: MdxLfoWaveform,
        frequency: u16,
        amplitude: u16,
    },
}

impl MdxCommandSpec for MdxVolumeLfo {
    fn opcode(&self) -> u8 {
        0xeb
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        match self {
            Self::SetEnabled { enabled } => dest.push(0x80 | u8::from(*enabled)),
            Self::Configure {
                waveform,
                frequency,
                amplitude,
            } => {
                dest.push(waveform.raw());
                dest.extend_from_slice(&frequency.to_be_bytes());
                dest.extend_from_slice(&amplitude.to_be_bytes());
            }
        }
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let waveform = read_u8_at(bytes, offset)?;
        if waveform & 0x80 != 0 {
            return Ok((
                Self::SetEnabled {
                    enabled: waveform & 1 != 0,
                },
                2,
            ));
        }
        let waveform = MdxLfoWaveform::from_raw(waveform);
        Ok((
            Self::Configure {
                waveform,
                frequency: read_u16_be_at(bytes, offset + 1)?,
                amplitude: read_u16_be_at(bytes, offset + 3)?,
            },
            6,
        ))
    }
}

/// OPM LFO configuration or state change (`0xea`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdxOpmLfo {
    SetEnabled {
        enabled: bool,
    },
    Configure {
        control: u8,
        lfrq: u8,
        pmd: u8,
        amd: u8,
        pms_ams: u8,
    },
}

impl MdxCommandSpec for MdxOpmLfo {
    fn opcode(&self) -> u8 {
        0xea
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        match self {
            Self::SetEnabled { enabled } => dest.push(0x80 | u8::from(*enabled)),
            Self::Configure {
                control,
                lfrq,
                pmd,
                amd,
                pms_ams,
            } => dest.extend_from_slice(&[*control, *lfrq, *pmd, *amd, *pms_ams]),
        }
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let control = read_u8_at(bytes, offset)?;
        if control & 0x80 != 0 {
            return Ok((
                Self::SetEnabled {
                    enabled: control & 1 != 0,
                },
                2,
            ));
        }
        Ok((
            Self::Configure {
                control,
                lfrq: read_u8_at(bytes, offset + 1)?,
                pmd: read_u8_at(bytes, offset + 2)?,
                amd: read_u8_at(bytes, offset + 3)?,
                pms_ams: read_u8_at(bytes, offset + 4)?,
            },
            6,
        ))
    }
}

/// Extended MML command (`0xe7`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdxExtendedCommand {
    Error,
    Fadeout { value: u8 },
    Pcm8DirectDrive { data: [u8; 6] },
    KeyOff { flag: u8 },
    ChannelControl { channel: u8 },
    AddNoteLength { value: u8 },
    SetFlag { value: u8 },
    Unknown(MdxExtendedUnknownCommand),
}

/// An unrecognized `0xe7` extended command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxExtendedUnknownCommand {
    pub opcode: u8,
    pub operand: Option<u8>,
}

impl MdxCommandSpec for MdxExtendedCommand {
    fn opcode(&self) -> u8 {
        0xe7
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.subopcode()]);
        match self {
            Self::Error => {}
            Self::Fadeout { value }
            | Self::KeyOff { flag: value }
            | Self::ChannelControl { channel: value }
            | Self::AddNoteLength { value }
            | Self::SetFlag { value } => dest.push(*value),
            Self::Pcm8DirectDrive { data } => dest.extend_from_slice(data),
            Self::Unknown(command) => {
                if let Some(operand) = command.operand {
                    dest.push(operand);
                }
            }
        }
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let subopcode = read_u8_at(bytes, offset)?;
        let payload = offset + 1;
        let command = match subopcode {
            0x00 => Self::Error,
            0x01 => Self::Fadeout {
                value: read_u8_at(bytes, payload)?,
            },
            0x02 => Self::Pcm8DirectDrive {
                data: [
                    read_u8_at(bytes, payload)?,
                    read_u8_at(bytes, payload + 1)?,
                    read_u8_at(bytes, payload + 2)?,
                    read_u8_at(bytes, payload + 3)?,
                    read_u8_at(bytes, payload + 4)?,
                    read_u8_at(bytes, payload + 5)?,
                ],
            },
            0x03 => Self::KeyOff {
                flag: read_u8_at(bytes, payload)?,
            },
            0x04 => Self::ChannelControl {
                channel: read_u8_at(bytes, payload)?,
            },
            0x05 => Self::AddNoteLength {
                value: read_u8_at(bytes, payload)?,
            },
            0x06 => Self::SetFlag {
                value: read_u8_at(bytes, payload)?,
            },
            0x0a => Self::Unknown(MdxExtendedUnknownCommand {
                opcode: subopcode,
                operand: Some(read_u8_at(bytes, payload)?),
            }),
            _ => Self::Unknown(MdxExtendedUnknownCommand {
                opcode: subopcode,
                operand: None,
            }),
        };
        let length = if subopcode == 0x02 {
            8
        } else if subopcode == 0x00 || subopcode >= 0x07 && subopcode != 0x0a {
            2
        } else {
            3
        };
        Ok((command, length))
    }
}

impl MdxExtendedCommand {
    fn subopcode(&self) -> u8 {
        match self {
            Self::Error => 0x00,
            Self::Fadeout { .. } => 0x01,
            Self::Pcm8DirectDrive { .. } => 0x02,
            Self::KeyOff { .. } => 0x03,
            Self::ChannelControl { .. } => 0x04,
            Self::AddNoteLength { .. } => 0x05,
            Self::SetFlag { .. } => 0x06,
            Self::Unknown(command) => command.opcode,
        }
    }
}

/// Extended MML command 2 (`0xe6`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdxExtended2Command {
    Error,
    RelativeDetune { value: u16 },
    Transpose { value: i8 },
    RelativeTranspose { value: i8 },
    Unknown(MdxExtended2UnknownCommand),
}

/// An unrecognized `0xe6` extended command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxExtended2UnknownCommand {
    pub opcode: u8,
}

impl MdxCommandSpec for MdxExtended2Command {
    fn opcode(&self) -> u8 {
        0xe6
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.extend_from_slice(&[self.opcode(), self.subopcode()]);
        match self {
            Self::Error => {}
            Self::RelativeDetune { value } => dest.extend_from_slice(&value.to_be_bytes()),
            Self::Transpose { value } | Self::RelativeTranspose { value } => {
                dest.push(*value as u8)
            }
            Self::Unknown(_) => {}
        }
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let subopcode = read_u8_at(bytes, offset)?;
        let payload = offset + 1;
        let command = match subopcode {
            0x00 => Self::Error,
            0x01 => Self::RelativeDetune {
                value: read_u16_be_at(bytes, payload)?,
            },
            0x02 => Self::Transpose {
                value: read_u8_at(bytes, payload)? as i8,
            },
            0x03 => Self::RelativeTranspose {
                value: read_u8_at(bytes, payload)? as i8,
            },
            _ => Self::Unknown(MdxExtended2UnknownCommand { opcode: subopcode }),
        };
        let length = if subopcode == 0x00 {
            2
        } else if subopcode == 0x01 {
            4
        } else if subopcode == 0x02 || subopcode == 0x03 {
            3
        } else {
            2
        };
        Ok((command, length))
    }
}

impl MdxExtended2Command {
    fn subopcode(&self) -> u8 {
        match self {
            Self::Error => 0x00,
            Self::RelativeDetune { .. } => 0x01,
            Self::Transpose { .. } => 0x02,
            Self::RelativeTranspose { .. } => 0x03,
            Self::Unknown(command) => command.opcode,
        }
    }
}

/// An MDX command whose payload is preserved without interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxRawCommand {
    pub opcode: u8,
}

impl MdxRawCommand {
    pub fn new(opcode: u8) -> Option<Self> {
        if opcode >= 0xe0 {
            Some(Self { opcode })
        } else {
            None
        }
    }
}

impl MdxCommandSpec for MdxRawCommand {
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_mdx_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        Err(ParseError::Other(
            "MDX raw command length is not self-describing".into(),
        ))
    }
}

impl From<MdxRest> for MdxCommand {
    fn from(command: MdxRest) -> Self {
        Self::Rest(command)
    }
}

impl From<MdxNote> for MdxCommand {
    fn from(command: MdxNote) -> Self {
        Self::Note(command)
    }
}

impl From<MdxTempo> for MdxCommand {
    fn from(command: MdxTempo) -> Self {
        Self::Tempo(command)
    }
}

impl From<MdxOpmRegisterWrite> for MdxCommand {
    fn from(command: MdxOpmRegisterWrite) -> Self {
        Self::OpmRegisterWrite(command)
    }
}

impl From<MdxVoiceOrPcmBank> for MdxCommand {
    fn from(command: MdxVoiceOrPcmBank) -> Self {
        Self::VoiceOrPcmBank(command)
    }
}

impl From<MdxPan> for MdxCommand {
    fn from(command: MdxPan) -> Self {
        Self::Pan(command)
    }
}

impl From<MdxVolume> for MdxCommand {
    fn from(command: MdxVolume) -> Self {
        Self::Volume(command)
    }
}

impl From<MdxVolumeDown> for MdxCommand {
    fn from(command: MdxVolumeDown) -> Self {
        Self::VolumeDown(command)
    }
}

impl From<MdxVolumeUp> for MdxCommand {
    fn from(command: MdxVolumeUp) -> Self {
        Self::VolumeUp(command)
    }
}

impl From<MdxGate> for MdxCommand {
    fn from(command: MdxGate) -> Self {
        Self::Gate(command)
    }
}

impl From<MdxKeyOffDisable> for MdxCommand {
    fn from(command: MdxKeyOffDisable) -> Self {
        Self::KeyOffDisable(command)
    }
}

impl From<MdxLoopStart> for MdxCommand {
    fn from(command: MdxLoopStart) -> Self {
        Self::LoopStart(command)
    }
}

impl From<MdxEndOfTrack> for MdxCommand {
    fn from(command: MdxEndOfTrack) -> Self {
        Self::EndOfTrack(command)
    }
}

impl From<MdxKeyOnDelay> for MdxCommand {
    fn from(command: MdxKeyOnDelay) -> Self {
        Self::KeyOnDelay(command)
    }
}

impl From<MdxSyncSend> for MdxCommand {
    fn from(command: MdxSyncSend) -> Self {
        Self::SyncSend(command)
    }
}

impl From<MdxSyncWait> for MdxCommand {
    fn from(command: MdxSyncWait) -> Self {
        Self::SyncWait(command)
    }
}

impl From<MdxAdpcmOrNoiseFrequency> for MdxCommand {
    fn from(command: MdxAdpcmOrNoiseFrequency) -> Self {
        Self::AdpcmOrNoiseFrequency(command)
    }
}

impl From<MdxPitchLfo> for MdxCommand {
    fn from(command: MdxPitchLfo) -> Self {
        Self::PitchLfo(command)
    }
}

impl From<MdxVolumeLfo> for MdxCommand {
    fn from(command: MdxVolumeLfo) -> Self {
        Self::VolumeLfo(command)
    }
}

impl From<MdxOpmLfo> for MdxCommand {
    fn from(command: MdxOpmLfo) -> Self {
        Self::OpmLfo(command)
    }
}

impl From<MdxLfoDelay> for MdxCommand {
    fn from(command: MdxLfoDelay) -> Self {
        Self::LfoDelay(command)
    }
}

impl From<MdxPcmMode> for MdxCommand {
    fn from(command: MdxPcmMode) -> Self {
        Self::PcmMode(command)
    }
}

impl From<MdxExtendedCommand> for MdxCommand {
    fn from(command: MdxExtendedCommand) -> Self {
        Self::Extended(command)
    }
}

impl From<MdxExtended2Command> for MdxCommand {
    fn from(command: MdxExtended2Command) -> Self {
        Self::Extended2(command)
    }
}

impl From<MdxRawCommand> for MdxCommand {
    fn from(command: MdxRawCommand) -> Self {
        Self::Raw(command)
    }
}
