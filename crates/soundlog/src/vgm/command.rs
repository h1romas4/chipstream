//! VGM command utilities
//!
//! This module defines the VGM command types, per-command parsing and
//! serialization helpers, and the `CommandSpec` trait implemented by
//! both generic commands and chip-specific command specifications.
//!
//! Responsibilities:
//! - Represent VGM command variants via `VgmCommand` and associated
//!   structs for each command spec.
//! - Provide `CommandSpec` implementations that can parse from a byte
//!   stream and serialize back to VGM bytes.
//! - Helpers to convert single commands to bytes (`command_to_vgm_bytes`)
//!   and compute per-command offsets/lengths used by `VgmDocument`.
//!
//! Most items are crate-visible; the module is intended for internal use.
//! The public surface exposes the command enum and associated specs so
//! other VGM components (parser, document, header) can interoperate.
use crate::binutil::{
    ParseError, read_i32_le_at, read_slice, read_u8_at, read_u24_be_at, read_u32_le_at,
};
use crate::chip;
use crate::vgm::document::VgmDocument;
use crate::vgm::header::{VGM_MAX_HEADER_SIZE, VgmHeader};

/// Chip instance identifier for VGM commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instance {
    Primary = 0x0,
    Secondary = 0x1,
}

/// Conversion between `usize` and `ChipId`.
impl From<usize> for Instance {
    fn from(v: usize) -> Self {
        match v {
            0 => Instance::Primary,
            1 => Instance::Secondary,
            _ => panic!("Invalid ChipId from usize: {}", v),
        }
    }
}

/// Conversion between `ChipId` and `usize`.
impl From<Instance> for usize {
    fn from(id: Instance) -> Self {
        id as usize
    }
}

#[derive(Debug, Clone, PartialEq)]
/// All supported VGM commands and per-chip write variants.
pub enum VgmCommand {
    AY8910StereoMask(Ay8910StereoMask),
    WaitSamples(WaitSamples),
    Wait735Samples(Wait735Samples),
    Wait882Samples(Wait882Samples),
    EndOfData(EndOfData),
    DataBlock(DataBlock),
    PcmRamWrite(PcmRamWrite),
    WaitNSample(WaitNSample),
    YM2612Port0Address2AWriteAndWaitN(Ym2612Port0Address2AWriteAndWaitN),
    SetupStreamControl(SetupStreamControl),
    SetStreamData(SetStreamData),
    SetStreamFrequency(SetStreamFrequency),
    StartStream(StartStream),
    StopStream(StopStream),
    StartStreamFastCall(StartStreamFastCall),
    SeekOffset(SeekOffset),
    Sn76489Write(Instance, chip::PsgSpec),
    Ym2413Write(Instance, chip::Ym2413Spec),
    Ym2612Write(Instance, chip::Ym2612Spec),
    Ym2151Write(Instance, chip::Ym2151Spec),
    SegaPcmWrite(Instance, chip::SegaPcmSpec),
    Rf5c68U8Write(Instance, chip::Rf5c68U8Spec),
    Rf5c68U16Write(Instance, chip::Rf5c68U16Spec),
    Ym2203Write(Instance, chip::Ym2203Spec),
    Ym2608Write(Instance, chip::Ym2608Spec),
    Ym2610bWrite(Instance, chip::Ym2610Spec),
    Ym3812Write(Instance, chip::Ym3812Spec),
    Ym3526Write(Instance, chip::Ym3526Spec),
    Y8950Write(Instance, chip::Y8950Spec),
    Ymf262Write(Instance, chip::Ymf262Spec),
    Ymf278bWrite(Instance, chip::Ymf278bSpec),
    Ymf271Write(Instance, chip::Ymf271Spec),
    Scc1Write(Instance, chip::Scc1Spec),
    Ymz280bWrite(Instance, chip::Ymz280bSpec),
    Rf5c164U8Write(Instance, chip::Rf5c164U8Spec),
    Rf5c164U16Write(Instance, chip::Rf5c164U16Spec),
    PwmWrite(Instance, chip::PwmSpec),
    Ay8910Write(Instance, chip::Ay8910Spec),
    GbDmgWrite(Instance, chip::GbDmgSpec),
    NesApuWrite(Instance, chip::NesApuSpec),
    MultiPcmWrite(Instance, chip::MultiPcmSpec),
    MultiPcmBankWrite(Instance, chip::MultiPcmBankSpec),
    Upd7759Write(Instance, chip::Upd7759Spec),
    Okim6258Write(Instance, chip::Okim6258Spec),
    Okim6295Write(Instance, chip::Okim6295Spec),
    K054539Write(Instance, chip::K054539Spec),
    Huc6280Write(Instance, chip::Huc6280Spec),
    C140Write(Instance, chip::C140Spec),
    K053260Write(Instance, chip::K053260Spec),
    PokeyWrite(Instance, chip::PokeySpec),
    QsoundWrite(Instance, chip::QsoundSpec),
    ScspWrite(Instance, chip::ScspSpec),
    WonderSwanWrite(Instance, chip::WonderSwanSpec),
    VsuWrite(Instance, chip::VsuSpec),
    Saa1099Write(Instance, chip::Saa1099Spec),
    Es5503Write(Instance, chip::Es5503Spec),
    Es5506BEWrite(Instance, chip::Es5506U8Spec),
    Es5506D6Write(Instance, chip::Es5506U16Spec),
    X1010Write(Instance, chip::X1010Spec),
    C352Write(Instance, chip::C352Spec),
    Ga20Write(Instance, chip::Ga20Spec),
    MikeyWrite(Instance, chip::MikeySpec),
    GameGearPsgWrite(Instance, chip::GameGearPsgSpec),
    ReservedU8Write(ReservedU8),
    ReservedU16Write(ReservedU16),
    ReservedU24Write(ReservedU24),
    ReservedU32Write(ReservedU32),
    UnknownCommand(UnknownSpec),
}

/// Trait for VGM command specifications.
pub(crate) trait CommandSpec {
    fn opcode(&self) -> u8;
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>);
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized;
}

/// AY8910 stereo mask
#[derive(Debug, Clone, PartialEq)]
pub struct Ay8910StereoMask(pub u8);

/// Wait n samples, n can range from 0 to 65535 (approx 1.49 seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct WaitSamples(pub u16);

/// wait 735 samples (60th of a second), a shortcut for 0x61 0xdf 0x02
#[derive(Debug, Clone, PartialEq)]
pub struct Wait735Samples;

/// wait 882 samples (50th of a second), a shortcut for 0x61 0x72 0x03
#[derive(Debug, Clone, PartialEq)]
pub struct Wait882Samples;

/// end of sound data
#[derive(Debug, Clone, PartialEq)]
pub struct EndOfData;

/// VGM command 0x67 specifies a data block.
#[derive(Debug, Clone, PartialEq)]
pub struct DataBlock {
    pub marker: u8,
    pub chip_id: u8,
    pub data_type: u8,
    pub size: u32,
    pub data: Vec<u8>,
}

/// VGM command 0x68 specifies a PCM RAM write.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmRamWrite {
    pub marker: u8,
    pub chip_type: u8,
    pub read_offset: u32,
    pub write_offset: u32,
    pub size: u32,
    pub data: Vec<u8>,
}

/// wait n+1 samples, n can range from 0 to 15.
#[derive(Debug, Clone, PartialEq)]
pub struct WaitNSample(pub u8);

/// YM2612 port 0 address 2A write from the data bank,
/// then wait n samples; n can range from 0 to 15.
/// Note that the wait is n, NOT n+1.
#[derive(Debug, Clone, PartialEq)]
pub struct Ym2612Port0Address2AWriteAndWaitN(pub u8);

/// DAC Stream Control Write: Setup Stream Control
#[derive(Debug, Clone, PartialEq)]
pub struct SetupStreamControl {
    pub stream_id: u8,
    pub chip_type: u8,
    pub write_port: u8,
    pub write_command: u8,
}

/// DAC Stream Control Write: Set Stream Data
#[derive(Debug, Clone, PartialEq)]
pub struct SetStreamData {
    pub stream_id: u8,
    pub data_bank_id: u8,
    pub step_size: u8,
    pub step_base: u8,
}

/// DAC Stream Control Write: Set Stream Frequency
#[derive(Debug, Clone, PartialEq)]
pub struct SetStreamFrequency {
    pub stream_id: u8,
    pub frequency: u32,
}

/// DAC Stream Control Write: Start Stream
#[derive(Debug, Clone, PartialEq)]
pub struct StartStream {
    pub stream_id: u8,
    pub data_start_offset: i32,
    pub length_mode: u8,
    pub data_length: u32,
}

/// DAC Stream Control Write: Stop Stream
#[derive(Debug, Clone, PartialEq)]
pub struct StopStream {
    pub stream_id: u8,
}

/// DAC Stream Control Write: Start Stream (fast call)
#[derive(Debug, Clone, PartialEq)]
pub struct StartStreamFastCall {
    pub stream_id: u8,
    pub block_id: u16,
    pub flags: u8,
}

/// one operand, reserved for future use
#[derive(Debug, Clone, PartialEq)]
pub struct ReservedU8 {
    pub opcode: u8,
    pub dd: u8,
}

/// two operands, reserved for future use (Note: was one operand only til v1.60)
#[derive(Debug, Clone, PartialEq)]
pub struct ReservedU16 {
    pub opcode: u8,
    pub dd1: u8,
    pub dd2: u8,
}

/// three operands, reserved for future use
#[derive(Debug, Clone, PartialEq)]
pub struct ReservedU24 {
    pub opcode: u8,
    pub dd1: u8,
    pub dd2: u8,
    pub dd3: u8,
}

/// three operands, reserved for future use
#[derive(Debug, Clone, PartialEq)]
pub struct ReservedU32 {
    pub opcode: u8,
    pub dd1: u8,
    pub dd2: u8,
    pub dd3: u8,
    pub dd4: u8,
}

/// Unknown command placeholder for opcodes that don't map to a known spec.
/// Only stores the opcode byte; payload (if any) is left uninterpreted.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownSpec {
    pub opcode: u8,
    pub offset: usize,
}

/// Seek to offset dddddddd (Intel byte order)
/// in PCM data bank of data block type 0 (YM2612).
#[derive(Debug, Clone, PartialEq)]
pub struct SeekOffset(pub u32);

impl CommandSpec for Ay8910StereoMask {
    fn opcode(&self) -> u8 {
        0x31
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.0);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let v = read_u8_at(bytes, offset)?;
        Ok((Ay8910StereoMask(v), 1))
    }
}

impl CommandSpec for WaitSamples {
    fn opcode(&self) -> u8 {
        0x61
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.0 & 0xFF) as u8);
        dest.push((self.0 >> 8) as u8);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let lo = read_u8_at(bytes, offset)?;
        let hi = read_u8_at(bytes, offset + 1)?;
        let val = ((hi as u16) << 8) | (lo as u16);
        Ok((WaitSamples(val), 2))
    }
}

impl CommandSpec for Wait735Samples {
    fn opcode(&self) -> u8 {
        0x62
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        Ok((Wait735Samples, 0))
    }
}

impl CommandSpec for Wait882Samples {
    fn opcode(&self) -> u8 {
        0x63
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        Ok((Wait882Samples, 0))
    }
}

impl CommandSpec for EndOfData {
    fn opcode(&self) -> u8 {
        0x66
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        Ok((EndOfData, 0))
    }
}

impl CommandSpec for DataBlock {
    fn opcode(&self) -> u8 {
        0x67
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        // 0x67 marker tt ss ss ss ss data...
        dest.push(self.opcode());
        dest.push(self.marker);
        dest.push(self.data_type);
        let mut s = self.size & 0x7FFF_FFFF;
        if self.chip_id != 0 {
            s |= 0x8000_0000;
        }
        dest.extend_from_slice(&s.to_le_bytes());
        dest.extend_from_slice(&self.data);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        // Expect offset to point at the marker byte. Preserve marker value,
        // do not validate it here; caller (parser) may choose to validate or not.
        let marker = read_u8_at(bytes, offset)?;
        let data_type = read_u8_at(bytes, offset + 1)?;
        let size = read_u32_le_at(bytes, offset + 2)?;
        let chip_id = if size & 0x8000_0000 != 0 { 1 } else { 0 }; // dual sound chip
        let size = 0x7FFF_FFFF & size;
        let data_slice = read_slice(bytes, offset + 6, size as usize)?;
        Ok((
            DataBlock {
                marker,
                chip_id,
                data_type,
                size,
                data: data_slice.to_vec(),
            },
            1 + 1 + 4 + size as usize,
        ))
    }
}

impl CommandSpec for PcmRamWrite {
    fn opcode(&self) -> u8 {
        0x68
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.marker);
        dest.push(self.chip_type);
        let o = self.read_offset & 0x00FF_FFFF;
        dest.push(((o >> 16) & 0xFF) as u8);
        dest.push(((o >> 8) & 0xFF) as u8);
        dest.push((o & 0xFF) as u8);
        let w = self.write_offset & 0x00FF_FFFF;
        dest.push(((w >> 16) & 0xFF) as u8);
        dest.push(((w >> 8) & 0xFF) as u8);
        dest.push((w & 0xFF) as u8);
        let s = self.size & 0x00FF_FFFF;
        dest.push(((s >> 16) & 0xFF) as u8);
        dest.push(((s >> 8) & 0xFF) as u8);
        dest.push((s & 0xFF) as u8);
        dest.extend_from_slice(&self.data);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        // Expect offset to point at the marker byte. Preserve marker value,
        // do not validate it here; caller (parser) may choose to validate or not.
        let marker = read_u8_at(bytes, offset)?;
        let chip_type = read_u8_at(bytes, offset + 1)?;
        let read_off = read_u24_be_at(bytes, offset + 2)?;
        let write_off = read_u24_be_at(bytes, offset + 5)?;
        let size = read_u24_be_at(bytes, offset + 8)?;
        let data_slice = read_slice(bytes, offset + 11, size as usize)?;
        Ok((
            PcmRamWrite {
                marker,
                chip_type,
                read_offset: read_off,
                write_offset: write_off,
                size,
                data: data_slice.to_vec(),
            },
            1 + 1 + 3 + 3 + 3 + size as usize,
        ))
    }
}

impl CommandSpec for WaitNSample {
    fn opcode(&self) -> u8 {
        0x70_u8.wrapping_add(self.0.saturating_sub(1))
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _off: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        Ok((WaitNSample(opcode - 0x70 + 1), 0))
    }
}

impl CommandSpec for Ym2612Port0Address2AWriteAndWaitN {
    fn opcode(&self) -> u8 {
        0x80_u8.wrapping_add(self.0)
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
    }
    fn parse(_bytes: &[u8], _off: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        Ok((
            Ym2612Port0Address2AWriteAndWaitN(opcode.wrapping_sub(0x80)),
            0,
        ))
    }
}

impl CommandSpec for SetupStreamControl {
    fn opcode(&self) -> u8 {
        0x90
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.stream_id);
        dest.push(self.chip_type);
        dest.push(self.write_port);
        dest.push(self.write_command);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let stream_id = read_u8_at(bytes, off)?;
        let chip_type = read_u8_at(bytes, off + 1)?;
        let write_port = read_u8_at(bytes, off + 2)?;
        let write_command = read_u8_at(bytes, off + 3)?;
        Ok((
            SetupStreamControl {
                stream_id,
                chip_type,
                write_port,
                write_command,
            },
            4,
        ))
    }
}

impl CommandSpec for SetStreamData {
    fn opcode(&self) -> u8 {
        0x91
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.stream_id);
        dest.push(self.data_bank_id);
        dest.push(self.step_size);
        dest.push(self.step_base);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let stream_id = read_u8_at(bytes, off)?;
        let data_bank_id = read_u8_at(bytes, off + 1)?;
        let step_size = read_u8_at(bytes, off + 2)?;
        let step_base = read_u8_at(bytes, off + 3)?;
        Ok((
            SetStreamData {
                stream_id,
                data_bank_id,
                step_size,
                step_base,
            },
            4,
        ))
    }
}

impl CommandSpec for SetStreamFrequency {
    fn opcode(&self) -> u8 {
        0x92
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.stream_id);
        dest.extend_from_slice(&self.frequency.to_le_bytes());
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let stream_id = read_u8_at(bytes, off)?;
        let freq = read_u32_le_at(bytes, off + 1)?;
        Ok((
            SetStreamFrequency {
                stream_id,
                frequency: freq,
            },
            1 + 4,
        ))
    }
}

impl CommandSpec for StartStream {
    fn opcode(&self) -> u8 {
        0x93
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.stream_id);
        dest.extend_from_slice(&self.data_start_offset.to_le_bytes());
        dest.push(self.length_mode);
        dest.extend_from_slice(&self.data_length.to_le_bytes());
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let stream_id = read_u8_at(bytes, off)?;
        let start_offset = read_i32_le_at(bytes, off + 1)?;
        let length_mode = read_u8_at(bytes, off + 5)?;
        let data_length = read_u32_le_at(bytes, off + 6)?;
        Ok((
            StartStream {
                stream_id,
                data_start_offset: start_offset,
                length_mode,
                data_length,
            },
            1 + 4 + 1 + 4,
        ))
    }
}

impl CommandSpec for StopStream {
    fn opcode(&self) -> u8 {
        0x94
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.stream_id);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let stream_id = read_u8_at(bytes, off)?;
        Ok((StopStream { stream_id }, 1))
    }
}

impl CommandSpec for StartStreamFastCall {
    fn opcode(&self) -> u8 {
        0x95
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.stream_id);
        dest.push((self.block_id >> 8) as u8);
        dest.push(self.block_id as u8);
        dest.push(self.flags);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let stream_id = read_u8_at(bytes, off)?;
        let b0 = read_u8_at(bytes, off + 1)?;
        let b1 = read_u8_at(bytes, off + 2)?;
        let block_id = ((b0 as u16) << 8) | (b1 as u16);
        let flags = read_u8_at(bytes, off + 3)?;
        Ok((
            StartStreamFastCall {
                stream_id,
                block_id,
                flags,
            },
            4,
        ))
    }
}

impl CommandSpec for SeekOffset {
    fn opcode(&self) -> u8 {
        0xE0
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.extend_from_slice(&self.0.to_le_bytes());
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let v = read_u32_le_at(bytes, off)?;
        Ok((SeekOffset(v), 4))
    }
}

impl CommandSpec for ReservedU8 {
    // base opcode for the ReservedU8 range (0x30..=0x3F)
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.dd);
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let dd = read_u8_at(bytes, offset)?;
        Ok((ReservedU8 { opcode, dd }, 1))
    }
}

impl CommandSpec for ReservedU16 {
    // base opcode for the ReservedU16 range (0x41..=0x4E)
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.dd1);
        dest.push(self.dd2);
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let dd1 = read_u8_at(bytes, offset)?;
        let dd2 = read_u8_at(bytes, offset + 1)?;
        Ok((ReservedU16 { opcode, dd1, dd2 }, 2))
    }
}

impl CommandSpec for ReservedU24 {
    // base opcode for one of the ReservedU24 ranges (0xC9..=0xCF / 0xD7..=0xDF)
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.dd1);
        dest.push(self.dd2);
        dest.push(self.dd3);
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let dd1 = read_u8_at(bytes, offset)?;
        let dd2 = read_u8_at(bytes, offset + 1)?;
        let dd3 = read_u8_at(bytes, offset + 2)?;
        Ok((
            ReservedU24 {
                opcode,
                dd1,
                dd2,
                dd3,
            },
            3,
        ))
    }
}

impl CommandSpec for ReservedU32 {
    // base opcode for the ReservedU32 range (0xE2..=0xFF)
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.dd1);
        dest.push(self.dd2);
        dest.push(self.dd3);
        dest.push(self.dd4);
    }
    fn parse(bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let dd1 = read_u8_at(bytes, offset)?;
        let dd2 = read_u8_at(bytes, offset + 1)?;
        let dd3 = read_u8_at(bytes, offset + 2)?;
        let dd4 = read_u8_at(bytes, offset + 3)?;
        Ok((
            ReservedU32 {
                opcode,
                dd1,
                dd2,
                dd3,
                dd4,
            },
            4,
        ))
    }
}

impl CommandSpec for UnknownSpec {
    fn opcode(&self) -> u8 {
        self.opcode
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        // Unknown spec is encoded as a single opcode byte only.
        dest.push(self.opcode);
    }
    fn parse(_bytes: &[u8], offset: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        // Treat unknown command as a single-byte command (only opcode).
        Ok((UnknownSpec { opcode, offset }, 1))
    }
}

impl From<Ay8910StereoMask> for VgmCommand {
    fn from(s: Ay8910StereoMask) -> Self {
        VgmCommand::AY8910StereoMask(s)
    }
}

impl From<WaitSamples> for VgmCommand {
    fn from(s: WaitSamples) -> Self {
        VgmCommand::WaitSamples(s)
    }
}

impl From<Wait735Samples> for VgmCommand {
    fn from(s: Wait735Samples) -> Self {
        VgmCommand::Wait735Samples(s)
    }
}

impl From<Wait882Samples> for VgmCommand {
    fn from(s: Wait882Samples) -> Self {
        VgmCommand::Wait882Samples(s)
    }
}

impl From<EndOfData> for VgmCommand {
    fn from(s: EndOfData) -> Self {
        VgmCommand::EndOfData(s)
    }
}

impl From<DataBlock> for VgmCommand {
    fn from(s: DataBlock) -> Self {
        VgmCommand::DataBlock(s)
    }
}

impl From<PcmRamWrite> for VgmCommand {
    fn from(s: PcmRamWrite) -> Self {
        VgmCommand::PcmRamWrite(s)
    }
}

impl From<WaitNSample> for VgmCommand {
    fn from(s: WaitNSample) -> Self {
        VgmCommand::WaitNSample(s)
    }
}

impl From<Ym2612Port0Address2AWriteAndWaitN> for VgmCommand {
    fn from(s: Ym2612Port0Address2AWriteAndWaitN) -> Self {
        VgmCommand::YM2612Port0Address2AWriteAndWaitN(s)
    }
}

impl From<SetupStreamControl> for VgmCommand {
    fn from(s: SetupStreamControl) -> Self {
        VgmCommand::SetupStreamControl(s)
    }
}

impl From<SetStreamData> for VgmCommand {
    fn from(s: SetStreamData) -> Self {
        VgmCommand::SetStreamData(s)
    }
}

impl From<SetStreamFrequency> for VgmCommand {
    fn from(s: SetStreamFrequency) -> Self {
        VgmCommand::SetStreamFrequency(s)
    }
}

impl From<StartStream> for VgmCommand {
    fn from(s: StartStream) -> Self {
        VgmCommand::StartStream(s)
    }
}

impl From<StopStream> for VgmCommand {
    fn from(s: StopStream) -> Self {
        VgmCommand::StopStream(s)
    }
}

impl From<StartStreamFastCall> for VgmCommand {
    fn from(s: StartStreamFastCall) -> Self {
        VgmCommand::StartStreamFastCall(s)
    }
}

impl From<SeekOffset> for VgmCommand {
    fn from(s: SeekOffset) -> Self {
        VgmCommand::SeekOffset(s)
    }
}

impl From<ReservedU8> for VgmCommand {
    fn from(s: ReservedU8) -> Self {
        VgmCommand::ReservedU8Write(s)
    }
}

impl From<ReservedU16> for VgmCommand {
    fn from(s: ReservedU16) -> Self {
        VgmCommand::ReservedU16Write(s)
    }
}

impl From<ReservedU24> for VgmCommand {
    fn from(s: ReservedU24) -> Self {
        VgmCommand::ReservedU24Write(s)
    }
}

impl From<ReservedU32> for VgmCommand {
    fn from(s: ReservedU32) -> Self {
        VgmCommand::ReservedU32Write(s)
    }
}

impl From<UnknownSpec> for VgmCommand {
    fn from(s: UnknownSpec) -> Self {
        VgmCommand::UnknownCommand(s)
    }
}

impl From<(Instance, chip::PsgSpec)> for VgmCommand {
    fn from(v: (Instance, chip::PsgSpec)) -> Self {
        VgmCommand::Sn76489Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym2413Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym2413Spec)) -> Self {
        VgmCommand::Ym2413Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym2612Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym2612Spec)) -> Self {
        VgmCommand::Ym2612Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym2151Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym2151Spec)) -> Self {
        VgmCommand::Ym2151Write(v.0, v.1)
    }
}

impl From<(Instance, chip::SegaPcmSpec)> for VgmCommand {
    fn from(v: (Instance, chip::SegaPcmSpec)) -> Self {
        VgmCommand::SegaPcmWrite(v.0, v.1)
    }
}
impl From<(Instance, chip::Rf5c68U8Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Rf5c68U8Spec)) -> Self {
        VgmCommand::Rf5c68U8Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Rf5c68U16Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Rf5c68U16Spec)) -> Self {
        VgmCommand::Rf5c68U16Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym2203Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym2203Spec)) -> Self {
        VgmCommand::Ym2203Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym2608Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym2608Spec)) -> Self {
        VgmCommand::Ym2608Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym2610Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym2610Spec)) -> Self {
        VgmCommand::Ym2610bWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym3812Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym3812Spec)) -> Self {
        VgmCommand::Ym3812Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ym3526Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ym3526Spec)) -> Self {
        VgmCommand::Ym3526Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Y8950Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Y8950Spec)) -> Self {
        VgmCommand::Y8950Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ymf262Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ymf262Spec)) -> Self {
        VgmCommand::Ymf262Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ymf278bSpec)> for VgmCommand {
    fn from(v: (Instance, chip::Ymf278bSpec)) -> Self {
        VgmCommand::Ymf278bWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Ymf271Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ymf271Spec)) -> Self {
        VgmCommand::Ymf271Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Scc1Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Scc1Spec)) -> Self {
        VgmCommand::Scc1Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ymz280bSpec)> for VgmCommand {
    fn from(v: (Instance, chip::Ymz280bSpec)) -> Self {
        VgmCommand::Ymz280bWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Rf5c164U8Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Rf5c164U8Spec)) -> Self {
        VgmCommand::Rf5c164U8Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Rf5c164U16Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Rf5c164U16Spec)) -> Self {
        VgmCommand::Rf5c164U16Write(v.0, v.1)
    }
}

impl From<(Instance, chip::PwmSpec)> for VgmCommand {
    fn from(v: (Instance, chip::PwmSpec)) -> Self {
        VgmCommand::PwmWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Ay8910Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ay8910Spec)) -> Self {
        VgmCommand::Ay8910Write(v.0, v.1)
    }
}

impl From<(Instance, chip::GbDmgSpec)> for VgmCommand {
    fn from(v: (Instance, chip::GbDmgSpec)) -> Self {
        VgmCommand::GbDmgWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::NesApuSpec)> for VgmCommand {
    fn from(v: (Instance, chip::NesApuSpec)) -> Self {
        VgmCommand::NesApuWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::MultiPcmSpec)> for VgmCommand {
    fn from(v: (Instance, chip::MultiPcmSpec)) -> Self {
        VgmCommand::MultiPcmWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::MultiPcmBankSpec)> for VgmCommand {
    fn from(v: (Instance, chip::MultiPcmBankSpec)) -> Self {
        VgmCommand::MultiPcmBankWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Upd7759Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Upd7759Spec)) -> Self {
        VgmCommand::Upd7759Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Okim6258Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Okim6258Spec)) -> Self {
        VgmCommand::Okim6258Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Okim6295Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Okim6295Spec)) -> Self {
        VgmCommand::Okim6295Write(v.0, v.1)
    }
}

impl From<(Instance, chip::K054539Spec)> for VgmCommand {
    fn from(v: (Instance, chip::K054539Spec)) -> Self {
        VgmCommand::K054539Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Huc6280Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Huc6280Spec)) -> Self {
        VgmCommand::Huc6280Write(v.0, v.1)
    }
}

impl From<(Instance, chip::C140Spec)> for VgmCommand {
    fn from(v: (Instance, chip::C140Spec)) -> Self {
        VgmCommand::C140Write(v.0, v.1)
    }
}

impl From<(Instance, chip::K053260Spec)> for VgmCommand {
    fn from(v: (Instance, chip::K053260Spec)) -> Self {
        VgmCommand::K053260Write(v.0, v.1)
    }
}

impl From<(Instance, chip::PokeySpec)> for VgmCommand {
    fn from(v: (Instance, chip::PokeySpec)) -> Self {
        VgmCommand::PokeyWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::QsoundSpec)> for VgmCommand {
    fn from(v: (Instance, chip::QsoundSpec)) -> Self {
        VgmCommand::QsoundWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::ScspSpec)> for VgmCommand {
    fn from(v: (Instance, chip::ScspSpec)) -> Self {
        VgmCommand::ScspWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::WonderSwanSpec)> for VgmCommand {
    fn from(v: (Instance, chip::WonderSwanSpec)) -> Self {
        VgmCommand::WonderSwanWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::VsuSpec)> for VgmCommand {
    fn from(v: (Instance, chip::VsuSpec)) -> Self {
        VgmCommand::VsuWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Saa1099Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Saa1099Spec)) -> Self {
        VgmCommand::Saa1099Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Es5503Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Es5503Spec)) -> Self {
        VgmCommand::Es5503Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Es5506U8Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Es5506U8Spec)) -> Self {
        VgmCommand::Es5506BEWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::Es5506U16Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Es5506U16Spec)) -> Self {
        VgmCommand::Es5506D6Write(v.0, v.1)
    }
}

impl From<(Instance, chip::X1010Spec)> for VgmCommand {
    fn from(v: (Instance, chip::X1010Spec)) -> Self {
        VgmCommand::X1010Write(v.0, v.1)
    }
}

impl From<(Instance, chip::C352Spec)> for VgmCommand {
    fn from(v: (Instance, chip::C352Spec)) -> Self {
        VgmCommand::C352Write(v.0, v.1)
    }
}

impl From<(Instance, chip::Ga20Spec)> for VgmCommand {
    fn from(v: (Instance, chip::Ga20Spec)) -> Self {
        VgmCommand::Ga20Write(v.0, v.1)
    }
}

impl From<(Instance, chip::MikeySpec)> for VgmCommand {
    fn from(v: (Instance, chip::MikeySpec)) -> Self {
        VgmCommand::MikeyWrite(v.0, v.1)
    }
}

impl From<(Instance, chip::GameGearPsgSpec)> for VgmCommand {
    fn from(v: (Instance, chip::GameGearPsgSpec)) -> Self {
        VgmCommand::GameGearPsgWrite(v.0, v.1)
    }
}

impl CommandSpec for chip::PsgSpec {
    // PSG (SN76489/SN76496) write value dd
    fn opcode(&self) -> u8 {
        0x50
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], offset: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let val = read_u8_at(bytes, offset)?;
        Ok((chip::PsgSpec { value: val }, 1))
    }
}

impl CommandSpec for chip::Ym2413Spec {
    // YM2413, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x51
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym2413Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ym2612Spec {
    // YM2612 port 0, write value dd to register aa
    // YM2612 port 1, write value dd to register aa
    fn opcode(&self) -> u8 {
        if self.port == 0 { 0x52 } else { 0x53 }
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let port = if opcode == 0x52 { 0 } else { 1 };
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym2612Spec {
                port,
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ym2151Spec {
    // YM2151, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x54u8
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym2151Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::SegaPcmSpec {
    // SegaPCM, write value dd to memory offset aabb
    fn opcode(&self) -> u8 {
        0xC0
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let off_hi = read_u8_at(bytes, off)?;
        let off_lo = read_u8_at(bytes, off + 1)?;
        let offv = ((off_hi as u16) << 8) | (off_lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::SegaPcmSpec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Rf5c68U8Spec {
    // RF5C68, write value dd to memory offset aa
    fn opcode(&self) -> u8 {
        0xB0
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.offset);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let offset = read_u8_at(bytes, off)?;
        let value = read_u8_at(bytes, off + 1)?;
        Ok((chip::Rf5c68U8Spec { offset, value }, 2))
    }
}

impl CommandSpec for chip::Rf5c68U16Spec {
    // RF5C68, write value dd to memory offset aabb
    fn opcode(&self) -> u8 {
        0xC1
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let off_hi = read_u8_at(bytes, off)?;
        let off_lo = read_u8_at(bytes, off + 1)?;
        let offv = ((off_hi as u16) << 8) | (off_lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::Rf5c68U16Spec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Ym2203Spec {
    // YM2203, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x55
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym2203Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ym2608Spec {
    // YM2608 port 0, write value dd to register aa
    // YM2608 port 1, write value dd to register aa
    fn opcode(&self) -> u8 {
        if self.port == 0 { 0x56 } else { 0x57 }
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        let port = if opcode == 0x56 { 0 } else { 1 };
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym2608Spec {
                port,
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ym2610Spec {
    // YM2610 port 0, write value dd to register aa
    // YM2610 port 1, write value dd to register aa
    fn opcode(&self) -> u8 {
        if self.port == 0 { 0x58 } else { 0x59 }
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        let port = if opcode == 0x58 { 0 } else { 1 };
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym2610Spec {
                port,
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ym3812Spec {
    // YM3812, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x5A
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym3812Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ym3526Spec {
    // YM3526, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x5B
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ym3526Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Y8950Spec {
    // Y8950, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x5C
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Y8950Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ymf262Spec {
    // YMF262 port 0, write value dd to register aa
    // YMF262 port 1, write value dd to register aa
    fn opcode(&self) -> u8 {
        if self.port == 0 { 0x5E } else { 0x5F }
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, opcode: u8) -> Result<(Self, usize), ParseError> {
        let port = if opcode == 0x5E { 0 } else { 1 };
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ymf262Spec {
                port,
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Ymf278bSpec {
    // YMF278B, port pp, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xD0
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.port);
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let port = read_u8_at(bytes, off)?;
        let reg = read_u8_at(bytes, off + 1)?;
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::Ymf278bSpec {
                port,
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Ymf271Spec {
    // YMF271, port pp, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xD1
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.port);
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let port = read_u8_at(bytes, off)?;
        let reg = read_u8_at(bytes, off + 1)?;
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::Ymf271Spec {
                port,
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Scc1Spec {
    // SCC1, port pp, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xD2
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.port);
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let port = read_u8_at(bytes, off)?;
        let reg = read_u8_at(bytes, off + 1)?;
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::Scc1Spec {
                port,
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Ymz280bSpec {
    // YMZ280B, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x5D
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ymz280bSpec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Rf5c164U8Spec {
    // RF5C164, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB1
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.offset);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Rf5c164U8Spec {
                offset: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Rf5c164U16Spec {
    // RF5C164, write value dd to memory offset aabb
    fn opcode(&self) -> u8 {
        0xC2
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let off_hi = read_u8_at(bytes, off)?;
        let off_lo = read_u8_at(bytes, off + 1)?;
        let offv = ((off_hi as u16) << 8) | (off_lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::Rf5c164U16Spec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::PwmSpec {
    // PWM, write value ddd to register a (d is MSB, dd is LSB)
    fn opcode(&self) -> u8 {
        0xB2
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        let hi: u8 = ((self.register & 0x0F) << 4) | (((self.value >> 8) & 0x0F) as u8);
        dest.push(hi);
        dest.push((self.value & 0xFF) as u8);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let offset = read_u8_at(bytes, off)?;
        let data = read_u8_at(bytes, off + 1)?;
        let register = (offset & 0xf0) >> 4_u8;
        let value: u32 = ((offset as u16 & 0x0f) << 8 | data as u16) as u32;
        Ok((chip::PwmSpec { register, value }, 2))
    }
}

impl CommandSpec for chip::Ay8910Spec {
    // AY8910, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xA0
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ay8910Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::GbDmgSpec {
    // GameBoy DMG, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB3
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::GbDmgSpec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::NesApuSpec {
    // NES APU, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB4
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::NesApuSpec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::MultiPcmSpec {
    // MultiPCM, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB5
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::MultiPcmSpec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::MultiPcmBankSpec {
    // MultiPCM, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB5
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.channel);
        dest.push((self.bank_offset >> 8) as u8);
        dest.push(self.bank_offset as u8);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let hi = read_u8_at(bytes, off + 1)?;
        let lo = read_u8_at(bytes, off + 2)?;
        let bank = ((hi as u16) << 8) | (lo as u16);
        Ok((
            chip::MultiPcmBankSpec {
                channel: reg,
                bank_offset: bank,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Upd7759Spec {
    // uPD7759, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB6
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Upd7759Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Okim6258Spec {
    // OKIM6258, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB7
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Okim6258Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Okim6295Spec {
    // OKIM6295, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB8
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError> {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Okim6295Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::K054539Spec {
    // K054539, write value dd to register ppaa
    fn opcode(&self) -> u8 {
        0xD3
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.register >> 8) as u8);
        dest.push(self.register as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let reg = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::K054539Spec {
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Huc6280Spec {
    // HuC6280, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xB9
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Huc6280Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::C140Spec {
    // C140, write value dd to register ppaa
    fn opcode(&self) -> u8 {
        0xD4
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.register >> 8) as u8);
        dest.push(self.register as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let reg = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::C140Spec {
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::K053260Spec {
    // K053260, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xBA
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::K053260Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::PokeySpec {
    // Pokey, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xBB
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::PokeySpec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::QsoundSpec {
    // QSound, write value mmll to register rr (mm - data MSB, ll - data LSB)
    fn opcode(&self) -> u8 {
        0xC4
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(((self.value >> 8) & 0xFF) as u8);
        dest.push((self.value & 0xFF) as u8);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let hi = read_u8_at(bytes, off + 1)?;
        let lo = read_u8_at(bytes, off + 2)?;
        let val = ((hi as u16) << 8) | (lo as u16);
        Ok((
            chip::QsoundSpec {
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::ScspSpec {
    // SCSP, write value dd to memory offset mmll (mm - offset MSB, ll - offset LSB)
    fn opcode(&self) -> u8 {
        0xC5
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let offv = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::ScspSpec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::WonderSwanSpec {
    // WonderSwan, write value dd to memory offset mmll (mm - offset MSB, ll - offset LSB)
    fn opcode(&self) -> u8 {
        0xC6
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let offv = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::WonderSwanSpec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::VsuSpec {
    // VSU, write value dd to memory offset mmll (mm - offset MSB, ll - offset LSB)
    fn opcode(&self) -> u8 {
        0xC7
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let offv = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::VsuSpec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Saa1099Spec {
    // SAA1099, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xBD
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Saa1099Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Es5503Spec {
    // ES5503, write value dd to register ppaa
    fn opcode(&self) -> u8 {
        0xD5
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.register >> 8) as u8);
        dest.push(self.register as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let reg = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::Es5503Spec {
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::Es5506U8Spec {
    // ES5506, write value dd to register aa
    //  Note: This command writes 8-bit data.
    fn opcode(&self) -> u8 {
        0xBE
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Es5506U8Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::Es5506U16Spec {
    // ES5506, write value aadd to register pp
    //  Note: This command writes 16-bit data.
    fn opcode(&self) -> u8 {
        0xD6
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        // TODO: Support 16-bit data write
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(((self.value >> 8) & 0xFF) as u8);
        dest.push((self.value & 0xFF) as u8);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let hi = read_u8_at(bytes, off + 1)?;
        let lo = read_u8_at(bytes, off + 2)?;
        let val = ((hi as u16) << 8) | (lo as u16);
        Ok((
            chip::Es5506U16Spec {
                register: reg,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::X1010Spec {
    // X1-010, write value dd to memory offset mmll (mm - offset MSB, ll - offset LSB)
    fn opcode(&self) -> u8 {
        0xC8
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.offset >> 8) as u8);
        dest.push(self.offset as u8);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let offv = ((hi as u16) << 8) | (lo as u16);
        let val = read_u8_at(bytes, off + 2)?;
        Ok((
            chip::X1010Spec {
                offset: offv,
                value: val,
            },
            3,
        ))
    }
}

impl CommandSpec for chip::C352Spec {
    // C352, write value aadd to register mmll
    fn opcode(&self) -> u8 {
        0xE1
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push((self.register >> 8) as u8);
        dest.push(self.register as u8);
        dest.push(((self.value >> 8) & 0xFF) as u8);
        dest.push((self.value & 0xFF) as u8);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let hi = read_u8_at(bytes, off)?;
        let lo = read_u8_at(bytes, off + 1)?;
        let reg = ((hi as u16) << 8) | (lo as u16);
        let vhi = read_u8_at(bytes, off + 2)?;
        let vlo = read_u8_at(bytes, off + 3)?;
        let val = ((vhi as u16) << 8) | (vlo as u16);
        Ok((
            chip::C352Spec {
                register: reg,
                value: val,
            },
            4,
        ))
    }
}

impl CommandSpec for chip::Ga20Spec {
    // GA20, write value dd to register aa
    fn opcode(&self) -> u8 {
        0xBF
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::Ga20Spec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::MikeySpec {
    // Mikey, write value dd to register aa
    fn opcode(&self) -> u8 {
        0x40
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.register);
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let reg = read_u8_at(bytes, off)?;
        let val = read_u8_at(bytes, off + 1)?;
        Ok((
            chip::MikeySpec {
                register: reg,
                value: val,
            },
            2,
        ))
    }
}

impl CommandSpec for chip::GameGearPsgSpec {
    // Game Gear PSG, write value dd
    fn opcode(&self) -> u8 {
        0x4F
    }
    fn to_vgm_bytes(&self, dest: &mut Vec<u8>) {
        dest.push(self.opcode());
        dest.push(self.value);
    }
    fn parse(bytes: &[u8], off: usize, _opcode: u8) -> Result<(Self, usize), ParseError>
    where
        Self: Sized,
    {
        let v = read_u8_at(bytes, off)?;
        Ok((chip::GameGearPsgSpec { value: v }, 1))
    }
}

/// Serialize a single `VgmCommand` into its VGM byte representation.
///
/// Returns a tuple `(Vec<u8>, usize)` where the first element is the
/// serialized bytes for the command and the second element is the length
/// (number of bytes) of that serialization.
///
/// This helper covers both plain command specs (which implement
/// `CommandSpec::to_vgm_bytes`) and chip-specific specs that include a
/// `ChipId`. For chip-specific specs the opcode byte is adjusted for
/// secondary instances (the opcode is incremented by `0x50`) via
/// `spec_to_vgm_bytes`.
///
/// The function is intended for internal use by other routines that need
/// per-command bytes or lengths (for example computing command offsets)
/// without serializing the entire document.
///
/// # Returns
/// - `(bytes, len)` — `bytes` contains the VGM bytes for the command,
///   `len` is equal to `bytes.len()`.
pub fn command_to_vgm_bytes(command: &VgmCommand) -> (Vec<u8>, usize) {
    let mut buf: Vec<u8> = Vec::new();
    use crate::vgm::command::VgmCommand::*;
    match command {
        AY8910StereoMask(s) => s.to_vgm_bytes(&mut buf),
        WaitSamples(s) => s.to_vgm_bytes(&mut buf),
        Wait735Samples(s) => s.to_vgm_bytes(&mut buf),
        Wait882Samples(s) => s.to_vgm_bytes(&mut buf),
        EndOfData(s) => s.to_vgm_bytes(&mut buf),
        DataBlock(s) => s.to_vgm_bytes(&mut buf),
        PcmRamWrite(s) => s.to_vgm_bytes(&mut buf),
        WaitNSample(s) => s.to_vgm_bytes(&mut buf),
        YM2612Port0Address2AWriteAndWaitN(s) => s.to_vgm_bytes(&mut buf),
        SetupStreamControl(s) => s.to_vgm_bytes(&mut buf),
        SetStreamData(s) => s.to_vgm_bytes(&mut buf),
        SetStreamFrequency(s) => s.to_vgm_bytes(&mut buf),
        StartStream(s) => s.to_vgm_bytes(&mut buf),
        StopStream(s) => s.to_vgm_bytes(&mut buf),
        StartStreamFastCall(s) => s.to_vgm_bytes(&mut buf),
        SeekOffset(s) => s.to_vgm_bytes(&mut buf),
        Sn76489Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym2413Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym2612Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym2151Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        SegaPcmWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Rf5c68U8Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Rf5c68U16Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym2203Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym2608Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym2610bWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym3812Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ym3526Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Y8950Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ymf262Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ymf278bWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ymf271Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Scc1Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ymz280bWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Rf5c164U8Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Rf5c164U16Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        PwmWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ay8910Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        GbDmgWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        NesApuWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        MultiPcmWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        MultiPcmBankWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Upd7759Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Okim6258Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Okim6295Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        K054539Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Huc6280Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        C140Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        K053260Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        PokeyWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        QsoundWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        ScspWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        WonderSwanWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        VsuWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Saa1099Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Es5503Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Es5506BEWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Es5506D6Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        X1010Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        C352Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        Ga20Write(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        MikeyWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        GameGearPsgWrite(id, s) => spec_to_vgm_bytes(*id, s, &mut buf),
        ReservedU8Write(s) => s.to_vgm_bytes(&mut buf),
        ReservedU16Write(s) => s.to_vgm_bytes(&mut buf),
        ReservedU24Write(s) => s.to_vgm_bytes(&mut buf),
        ReservedU32Write(s) => s.to_vgm_bytes(&mut buf),
        UnknownCommand(s) => s.to_vgm_bytes(&mut buf),
    }
    let len = buf.len();

    (buf, len)
}

/// `spec_to_vgm_bytes` is a module-visible associated helper used to
/// convert a chip-specific `CommandSpec` into bytes while adjusting the
/// opcode according to the chip instance (primary/secondary).
fn spec_to_vgm_bytes<C: CommandSpec + ?Sized>(chip_id: Instance, spec: &C, cmd_buf: &mut Vec<u8>) {
    let start = cmd_buf.len();
    spec.to_vgm_bytes(cmd_buf);
    cmd_buf[start] = match chip_id {
        Instance::Primary => cmd_buf[start],
        Instance::Secondary => {
            let primary = cmd_buf[start];
            match primary {
                0x50 => 0x30,
                0x51..=0x5F => primary.wrapping_add(0x50),
                0x00..=0x7F => primary.wrapping_add(0x80),
                _ => primary.wrapping_add(0x50),
            }
        }
    };
}

impl VgmDocument {
    /// Compute the total samples represented by the command stream.
    ///
    /// Returns the summed sample counts from wait and special wait-like
    /// commands; does not modify the document.
    pub(crate) fn total_samples(&self) -> u32 {
        self.commands
            .iter()
            .map(|cmd| match cmd {
                VgmCommand::WaitSamples(s) => s.0 as u32,
                VgmCommand::Wait735Samples(_) => 735,
                VgmCommand::Wait882Samples(_) => 882,
                VgmCommand::WaitNSample(s) => s.0 as u32,
                VgmCommand::YM2612Port0Address2AWriteAndWaitN(s) => s.0 as u32,
                _ => 0,
            })
            .sum::<u32>()
    }

    /// Serialize the `VgmDocument` into a complete VGM file byte stream.
    ///
    /// This constructs the VGM header (including GD3 and data offsets),
    /// serializes the document's command stream into VGM command bytes, and
    /// appends optional extra-header and GD3 metadata. Header fields that
    /// depend on the serialized size (for example file size, extra-header
    /// offset and GD3 offset) are updated in-place before the final byte
    /// vector is returned.
    ///
    /// Note: this function does NOT automatically append an End-of-Data
    /// opcode. If an EndOfData is required, it must be present in `self.commands`
    /// before calling `to_bytes()`.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let cmd_buf = self.commands_to_bytes_up_to(self.commands.len());

        // `wrote_end_in_cmds` removed: we no longer auto-append EndOfData.
        // If an EndOfData opcode is required, it must be present in `self.commands`
        // before calling `to_bytes()`.

        // data offset (0x34)
        let data_offset: u32 = match self.header.data_offset {
            0 => VGM_MAX_HEADER_SIZE.wrapping_sub(0x34),
            v => v,
        };

        // Build header bytes
        let mut header = self.header.to_bytes(0, data_offset);

        let header_size: usize = if self.header.data_offset == 0 {
            VgmHeader::fallback_header_size_for_version(self.header.version)
        } else {
            0x34_usize.wrapping_add(data_offset as usize)
        };

        // Build extra_header bytes
        if let Some(extra_header) = &self.extra_header {
            let extra_bytes = extra_header.to_bytes();
            if self.header.extra_header_offset != 0 {
                let stored_offset = self.header.extra_header_offset;
                let desired_start = stored_offset.wrapping_add(0xBC) as usize;

                // Ensure extra header placement doesn't overlap the data; clear stored offset if so.
                let allowed_header_size = 0x34_usize.wrapping_add(data_offset as usize);
                if desired_start >= allowed_header_size {
                    let needed = 0xBC_usize + 4;
                    if header_size >= needed {
                        if header.len() < needed {
                            header.resize(needed, 0);
                        }
                        header[0xBC..0xC0].copy_from_slice(&0_u32.to_le_bytes());
                    }
                } else {
                    // Place extra header at desired_start; write into reserved area or resize/append.
                    let place_end = desired_start + extra_bytes.len();
                    if place_end <= header.len() {
                        header[desired_start..place_end].copy_from_slice(&extra_bytes);
                    } else if desired_start <= header.len() {
                        let append_from = header.len().saturating_sub(desired_start);
                        header.extend_from_slice(&extra_bytes[append_from..]);
                    } else {
                        header.resize(desired_start, 0);
                        header.extend_from_slice(&extra_bytes);
                    }

                    let needed = 0xBC_usize + 4;
                    if header_size >= needed {
                        if header.len() < needed {
                            header.resize(needed, 0);
                        }
                        header[0xBC..0xC0].copy_from_slice(&stored_offset.to_le_bytes());
                    }
                }
            } else {
                let extra_start = header.len() as u32;
                let extra_offset_val = extra_start.wrapping_sub(0xBC_u32);
                header.extend_from_slice(&extra_bytes);
                let needed = 0xBC_usize + 4;
                if header_size >= needed {
                    if header.len() < needed {
                        header.resize(needed, 0);
                    }
                    header[0xBC..0xC0].copy_from_slice(&extra_offset_val.to_le_bytes());
                }
            }
        } else {
            // Only write a zero stored extra-header offset if the header layout
            // actually contains that field.
            let needed = 0xBCusize + 4usize;
            if header_size >= needed {
                if header.len() < needed {
                    header.resize(needed, 0);
                }
                header[0xBC..0xC0].copy_from_slice(&0_u32.to_le_bytes());
            }
        }

        // Append command stream. Note: do NOT append EndOfData automatically here.
        // Callers should include EndOfData in `self.commands` if desired.
        header.extend_from_slice(&cmd_buf);

        // If GD3 metadata is present, append the full GD3 chunk and update
        // the header's GD3 offset field to point to its location. Only write
        // the GD3 offset into the main header if that field exists for the
        // computed header_size.
        if let Some(gd3) = &self.gd3 {
            let gd3_start = header.len() as u32;
            let gd3_offset_val = gd3_start.wrapping_sub(0x14_u32);
            let gd3_bytes = gd3.to_bytes();
            header.extend_from_slice(&gd3_bytes);
            let gd3_off_bytes = gd3_offset_val.to_le_bytes();
            let needed = 0x14_usize + 4;
            if header_size >= needed {
                if header.len() < needed {
                    header.resize(needed, 0);
                }
                header[0x14..0x18].copy_from_slice(&gd3_off_bytes);
            }
        } else {
            let needed = 0x14_usize + 4;
            if header_size >= needed {
                if header.len() < needed {
                    header.resize(needed, 0);
                }
                header[0x14..0x18].copy_from_slice(&0_u32.to_le_bytes());
            }
        }

        // Update EOF offset (file size - 4) in header (0x04..0x08).
        // Respect an existing stored EOF value in the header: only overwrite
        // the EOF field if header.eof_offset == 0. This allows callers to
        // preset an explicit EOF offset that is honored during serialization.
        let needed = 0x04_usize + 4;
        if header_size >= needed {
            if header.len() < needed {
                header.resize(needed, 0);
            }
            // Only compute and write EOF if the header's stored eof_offset is zero.
            if self.header.eof_offset == 0 {
                let file_size = header.len() as u32;
                let eof_offset = file_size.wrapping_sub(4);
                let eof_bytes = eof_offset.to_le_bytes();
                header[0x04..0x08].copy_from_slice(&eof_bytes);
            }
            // otherwise leave the value written by VgmHeader::to_bytes (which
            // emitted self.header.eof_offset) untouched
        }

        header
    }

    /// Convert commands up to `end` (exclusive) into VGM command bytes.
    /// This mirrors the logic used by `VgmDocument::to_bytes()` so callers
    /// (e.g. `finalize`) can compute lengths/offsets without duplicating code.
    fn commands_to_bytes_up_to(&self, end: usize) -> Vec<u8> {
        let mut cmd_buf: Vec<u8> = Vec::new();
        let upto = std::cmp::min(end, self.commands.len());
        for cmd in &self.commands[..upto] {
            let (b, _len) = command_to_vgm_bytes(cmd);
            cmd_buf.extend_from_slice(&b);
        }

        cmd_buf
    }

    /// Compute per-command (offset, length) tuples for the document's command
    /// stream.
    ///
    /// Returns a `Vec<(usize, usize)>` where each tuple represents the byte
    /// offset (relative to the start of the command stream) and the serialized
    /// length of the corresponding command in `self.commands`. This uses the
    /// crate-local `command_to_vgm_bytes` helper to determine each command's
    /// serialized length without serializing the entire document.
    pub(crate) fn command_offsets_and_lengths(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::with_capacity(self.commands.len());
        let mut offset: usize = 0;

        for cmd in &self.commands {
            // `command_to_vgm_bytes` returns (bytes, len). We only need len here.
            let (_bytes, len) = command_to_vgm_bytes(cmd);
            out.push((offset, len));
            offset = offset.wrapping_add(len);
        }
        out
    }

    /// Return command offsets/lengths as absolute file offsets (including header size).
    ///
    /// This computes the header length using the same rules as `to_bytes()` and
    /// adds that header length to the per-command offsets returned by
    /// `command_offsets_and_lengths()` so callers receive absolute file offsets
    /// suitable for highlighting bytes in the original serialized VGM file.
    pub fn sourcemap(&self) -> Vec<(usize, usize)> {
        // compute data_offset the same way as to_bytes()
        let data_offset: u32 = if self.header.data_offset == 0 {
            (VgmHeader::fallback_header_size_for_version(self.header.version).wrapping_sub(0x34))
                as u32
        } else {
            self.header.data_offset
        };

        // compute header length using the header's to_bytes helper so layout rules match to_bytes()
        let header_len = self.header.to_bytes(0, data_offset).len();

        self.command_offsets_and_lengths()
            .into_iter()
            .map(|(off, len)| (header_len + off, len))
            .collect()
    }
}
