//! VGM header and extra-header utilities
//!
//! This module defines `VgmHeader` — the in-memory representation of the
//! VGM main header — along with serialization/deserialization helpers
//! and utilities for managing chip clock fields and extra-header layout.
//!
//! Contents and responsibilities:
//! - `VgmHeader` struct with all VGM header fields and a `to_bytes` method
//!   to serialize the header according to the computed `data_offset`.
//! - `VgmExtraHeader` struct for the v1.70+ extra header and `to_bytes`.
//! - Helpers to set per-chip clocks (`set_chip_clock`) and enumerate
//!   chip instances stored in the header (`chip_instances`).
//!
//! Notes:
//! - The module exposes `VGM_MAX_HEADER_SIZE` constant and preserves the
//!   writer/reader convention where `data_offset == 0` falls back to the
//!   legacy header size.
use crate::binutil::{ParseError, write_slice, write_u8, write_u16, write_u32};
use crate::chip;
use crate::vgm::command::Instance;
use crate::vgm::parser::parse_vgm_header;
use std::convert::TryFrom;

// For unknown/future versions, use the maximum header size
pub(crate) const VGM_MAX_HEADER_SIZE: u32 = 0x100;

/// A list of chip instances found in a VGM header.
///
/// Each entry is a tuple of `(Instance, Chip, clock_hz)` indicating whether the chip
/// is a primary or secondary instance, which chip type it is, and its clock frequency.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipInstances(pub Vec<(Instance, chip::Chip, f32)>);

impl ChipInstances {
    /// Returns an iterator over the chip instances.
    pub fn iter(&self) -> impl Iterator<Item = &(Instance, chip::Chip, f32)> {
        self.0.iter()
    }

    /// Returns the number of chip instances.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true if there are no chip instances.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl IntoIterator for ChipInstances {
    type Item = (Instance, chip::Chip, f32);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ChipInstances {
    type Item = &'a (Instance, chip::Chip, f32);
    type IntoIter = std::slice::Iter<'a, (Instance, chip::Chip, f32)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Enum identifying header fields and their on-disk offsets.
#[derive(Copy, Clone, Debug)]
pub enum VgmHeaderField {
    Ident,
    EofOffset,
    Version,
    Sn76489Clock,
    Ym2413Clock,
    Gd3Offset,
    TotalSamples,
    LoopOffset,
    LoopSamples,
    SampleRate,
    Sn76489Feedback,
    Sn76489ShiftRegisterWidth,
    Sn76489Flags,
    Ym2612Clock,
    Ym2151Clock,
    DataOffset,
    SegaPcmClock,
    SpcmInterface,
    Rf5c68Clock,
    Ym2203Clock,
    Ym2608Clock,
    Ym2610bClock,
    Ym3812Clock,
    Ym3526Clock,
    Y8950Clock,
    Ymf262Clock,
    Ymf278bClock,
    Ymf271Clock,
    Ymz280bClock,
    Rf5c164Clock,
    PwmClock,
    Ay8910Clock,
    Ay8910ChipType,
    Ay8910Flags,
    Ym2203Ay8910Flags,
    Ym2608Ay8910Flags,
    VolumeModifier,
    Reserved7D,
    LoopBase,
    LoopModifier,
    GbDmgClock,
    NesApuClock,
    MultipcmClock,
    Upd7759Clock,
    Okim6258Clock,
    Okim6258Flags,
    K054539Flags,
    C140ChipType,
    Okim6295Clock,
    K051649Clock,
    K054539Clock,
    Huc6280Clock,
    C140Clock,
    Reserved97,
    K053260Clock,
    PokeyClock,
    QsoundClock,
    ScspClock,
    ExtraHeaderOffset,
    WonderSwan,
    Vsu,
    Saa1099,
    Es5503,
    Es5506,
    Es5503OutputChannels,
    Es5506OutputChannels,
    C352ClockDivider,
    X1_010,
    C352,
    Ga20,
    Mikey,
    ReservedE8EF,
    ReservedF0FF,
}

/// SN76489 feedback variants used in the header (u16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sn76489Feedback {
    /// 0x0003: SN76489 (SN94624)
    Sn94624,
    /// 0x0006: Sometimes (incorrectly) used to refer to SN76489A / SN76494 / SN76496 / Y204
    /// in combination with LFSR width 16. The correct combination for those parts is
    /// feedback 0x000C with LFSR width 17 (reflects the extra latency bit).
    IncorrectA,
    /// 0x0009: Sega Master System 2 / Game Gear / Mega Drive (SN76489/SN76496 integrated into VDP)
    SegaVdp,
    /// 0x000C: SN76489A, SN76494, SN76496, Y204
    Sn76489a,
    /// 0x0022: NCR8496, PSSJ3
    Ncr8496,
    /// Unknown(x) preserves unknown raw value.
    Unknown(u16),
}

impl From<u16> for Sn76489Feedback {
    fn from(value: u16) -> Self {
        match value {
            0x0003 => Sn76489Feedback::Sn94624,
            0x0006 => Sn76489Feedback::IncorrectA,
            0x0009 => Sn76489Feedback::SegaVdp,
            0x000C => Sn76489Feedback::Sn76489a,
            0x0022 => Sn76489Feedback::Ncr8496,
            other => Sn76489Feedback::Unknown(other),
        }
    }
}

impl From<Sn76489Feedback> for u16 {
    fn from(feedback: Sn76489Feedback) -> Self {
        match feedback {
            Sn76489Feedback::Sn94624 => 0x0003,
            Sn76489Feedback::IncorrectA => 0x0006,
            Sn76489Feedback::SegaVdp => 0x0009,
            Sn76489Feedback::Sn76489a => 0x000C,
            Sn76489Feedback::Ncr8496 => 0x0022,
            Sn76489Feedback::Unknown(value) => value,
        }
    }
}

/// SN76489 shift register width codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sn76489ShiftRegisterWidth {
    /// 15: SN76489, SN94624
    Sn94624,
    /// 16: Sega Master System 2 / Game Gear / Mega Drive
    /// (SN76489 / SN76496 integrated into Sega VDP chip),
    /// NCR8496, PSSJ3
    SegaVdp,
    /// 17: SN76489A, SN76494, SN76496, Y204
    Sn76489a,
    /// Unknown(x) preserves unknown raw value.
    Unknown(u8),
}

impl From<u8> for Sn76489ShiftRegisterWidth {
    fn from(value: u8) -> Self {
        match value {
            15 => Sn76489ShiftRegisterWidth::Sn94624,
            16 => Sn76489ShiftRegisterWidth::SegaVdp,
            17 => Sn76489ShiftRegisterWidth::Sn76489a,
            other => Sn76489ShiftRegisterWidth::Unknown(other),
        }
    }
}

impl From<Sn76489ShiftRegisterWidth> for u8 {
    fn from(width: Sn76489ShiftRegisterWidth) -> Self {
        match width {
            Sn76489ShiftRegisterWidth::Sn94624 => 15,
            Sn76489ShiftRegisterWidth::SegaVdp => 16,
            Sn76489ShiftRegisterWidth::Sn76489a => 17,
            Sn76489ShiftRegisterWidth::Unknown(value) => value,
        }
    }
}

/// AY/8910 chip type enumerations used in the header (1 byte)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ay8910ChipType {
    /// 0x00: AY8910
    Ay8910,
    /// 0x01: AY8912
    Ay8912,
    /// 0x02: AY8913
    Ay8913,
    /// 0x03: AY8930
    Ay8930,
    /// 0x04: AY8914
    Ay8914,
    /// 0x10: YM2149
    Ym2149,
    /// 0x11: YM3439
    Ym3439,
    /// 0x12: YMZ284
    Ymz284,
    /// 0x13: YMZ294
    Ymz294,
    /// Unknown(x) preserves unknown raw value.
    Unknown(u8),
}

impl From<u8> for Ay8910ChipType {
    fn from(value: u8) -> Self {
        match value {
            0x00 => Ay8910ChipType::Ay8910,
            0x01 => Ay8910ChipType::Ay8912,
            0x02 => Ay8910ChipType::Ay8913,
            0x03 => Ay8910ChipType::Ay8930,
            0x04 => Ay8910ChipType::Ay8914,
            0x10 => Ay8910ChipType::Ym2149,
            0x11 => Ay8910ChipType::Ym3439,
            0x12 => Ay8910ChipType::Ymz284,
            0x13 => Ay8910ChipType::Ymz294,
            other => Ay8910ChipType::Unknown(other),
        }
    }
}

impl From<Ay8910ChipType> for u8 {
    fn from(t: Ay8910ChipType) -> Self {
        match t {
            Ay8910ChipType::Ay8910 => 0x00,
            Ay8910ChipType::Ay8912 => 0x01,
            Ay8910ChipType::Ay8913 => 0x02,
            Ay8910ChipType::Ay8930 => 0x03,
            Ay8910ChipType::Ay8914 => 0x04,
            Ay8910ChipType::Ym2149 => 0x10,
            Ay8910ChipType::Ym3439 => 0x11,
            Ay8910ChipType::Ymz284 => 0x12,
            Ay8910ChipType::Ymz294 => 0x13,
            Ay8910ChipType::Unknown(v) => v,
        }
    }
}

/// C140 chip type enumerations used in the header (1 byte)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C140ChipType {
    /// 0x00: C140, Namco System 2
    C140System2,
    /// 0x01: C140, Namco System 21
    C140System21,
    /// 0x02: 219 ASIC, Namco NA-1/2
    Asic219Na12,
    /// Unknown(x) preserves unknown raw value.
    Unknown(u8),
}

impl From<u8> for C140ChipType {
    fn from(value: u8) -> Self {
        match value {
            0x00 => C140ChipType::C140System2,
            0x01 => C140ChipType::C140System21,
            0x02 => C140ChipType::Asic219Na12,
            other => C140ChipType::Unknown(other),
        }
    }
}

impl From<C140ChipType> for u8 {
    fn from(t: C140ChipType) -> Self {
        match t {
            C140ChipType::C140System2 => 0x00,
            C140ChipType::C140System21 => 0x01,
            C140ChipType::Asic219Na12 => 0x02,
            C140ChipType::Unknown(v) => v,
        }
    }
}

/// OKIM6258 flags stored in the VGM header (1 byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Okim6258Flags {
    /// bits 0-1: Clock Divider (values select divider; common dividers: 1024, 768, 512, 512)
    pub clock_divider: u8,
    /// bit  2: 3/4-bit ADPCM select (default is 4-bit; may not be fully supported)
    pub adpcm_3bit_select: bool,
    /// bit  3: 10/12-bit output select (default is 10-bit)
    pub output_12bit: bool,
    /// bits 4-7: reserved (must be zero)
    pub reserved: u8,
}

impl From<u8> for Okim6258Flags {
    fn from(value: u8) -> Self {
        let clock_divider = value & 0x03;
        let adpcm_3bit_select = (value & (1 << 2)) != 0;
        let output_12bit = (value & (1 << 3)) != 0;
        let reserved = value >> 4;
        Okim6258Flags {
            clock_divider,
            adpcm_3bit_select,
            output_12bit,
            reserved,
        }
    }
}

impl From<Okim6258Flags> for u8 {
    fn from(flags: Okim6258Flags) -> Self {
        let mut value = flags.clock_divider & 0x03;
        if flags.adpcm_3bit_select {
            value |= 1 << 2;
        }
        if flags.output_12bit {
            value |= 1 << 3;
        }
        value |= flags.reserved << 4;
        value
    }
}

/// SN76489 flags stored in the VGM header (1 byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sn76489Flags {
    /// bit 0: Frequency 0 is 0x400 (should be set for all chips except SEGA PSG)
    pub frequency_0_is_400: bool,
    /// bit 1: Output negate flag
    pub output_negate: bool,
    /// bit 2: GameGear stereo on/off (on when bit clear)
    pub gamegear_stereo: bool,
    /// bit 3: /8 Clock Divider on/off (on when bit clear)
    pub clock_divider_8: bool,
    /// bit 4: XNOR noise mode (for NCR8496/PSSJ-3)
    pub xnor_noise_mode: bool,
    /// bit 5-7: reserved (must be zero)
    pub reserved: u8,
}

impl From<u8> for Sn76489Flags {
    fn from(value: u8) -> Self {
        let frequency_0_is_400 = (value & (1 << 0)) != 0;
        let output_negate = (value & (1 << 1)) != 0;
        let gamegear_stereo = (value & (1 << 2)) == 0; // on when bit clear
        let clock_divider_8 = (value & (1 << 3)) == 0; // on when bit clear
        let xnor_noise_mode = (value & (1 << 4)) != 0;
        let reserved = value >> 5;
        Sn76489Flags {
            frequency_0_is_400,
            output_negate,
            gamegear_stereo,
            clock_divider_8,
            xnor_noise_mode,
            reserved,
        }
    }
}

impl From<Sn76489Flags> for u8 {
    fn from(flags: Sn76489Flags) -> Self {
        let mut value = 0u8;
        if flags.frequency_0_is_400 {
            value |= 1 << 0;
        }
        if flags.output_negate {
            value |= 1 << 1;
        }
        if !flags.gamegear_stereo {
            value |= 1 << 2;
        }
        if !flags.clock_divider_8 {
            value |= 1 << 3;
        }
        if flags.xnor_noise_mode {
            value |= 1 << 4;
        }
        value |= flags.reserved << 5;
        value
    }
}

/// AY/8910 flags stored in the VGM header (1 byte)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ay8910Flags {
    /// bit 0: Legacy Output (Spec default: true)
    pub legacy_output: bool,
    /// bit 1: Single Output
    pub single_output: bool,
    /// bit 2: Discrete Output
    pub discrete_output: bool,
    /// bit 3: RAW Output
    pub raw_output: bool,
    /// bit 4: YMxxxx pin 26 (clock divider) low
    pub ym_pin26_low: bool,
    /// bit 5-7: reserved
    pub reserved: u8,
}

impl From<u8> for Ay8910Flags {
    fn from(value: u8) -> Self {
        let legacy_output = (value & (1 << 0)) != 0;
        let single_output = (value & (1 << 1)) != 0;
        let discrete_output = (value & (1 << 2)) != 0;
        let raw_output = (value & (1 << 3)) != 0;
        let ym_pin26_low = (value & (1 << 4)) != 0;
        let reserved = value >> 5;
        Ay8910Flags {
            legacy_output,
            single_output,
            discrete_output,
            raw_output,
            ym_pin26_low,
            reserved,
        }
    }
}

impl From<Ay8910Flags> for u8 {
    fn from(flags: Ay8910Flags) -> Self {
        let mut value = 0u8;
        if flags.legacy_output {
            value |= 1 << 0;
        }
        if flags.single_output {
            value |= 1 << 1;
        }
        if flags.discrete_output {
            value |= 1 << 2;
        }
        if flags.raw_output {
            value |= 1 << 3;
        }
        if flags.ym_pin26_low {
            value |= 1 << 4;
        }
        value |= flags.reserved << 5;
        value
    }
}

/// AY/8910 flags stored in the VGM header (1 byte)
pub type Ym2203AyFlags = Ay8910Flags;

/// AY/8910 flags stored in the VGM header (1 byte)
pub type Ym2608AyFlags = Ay8910Flags;

/// K054539 flags stored in the VGM header (1 byte)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct K054539Flags {
    /// bit 0: Reverse Stereo (Spec default: true)
    pub reverse_stereo: bool,
    /// bit 1: Disable Reverb
    pub disable_reverb: bool,
    /// bit 2: Update at KeyOn
    pub update_at_keyon: bool,
    /// bit 3-7: reserved
    pub reserved: u8,
}

impl From<u8> for K054539Flags {
    fn from(value: u8) -> Self {
        let reverse_stereo = (value & (1 << 0)) != 0;
        let disable_reverb = (value & (1 << 1)) != 0;
        let update_at_keyon = (value & (1 << 2)) != 0;
        let reserved = value >> 3;
        K054539Flags {
            reverse_stereo,
            disable_reverb,
            update_at_keyon,
            reserved,
        }
    }
}

impl From<K054539Flags> for u8 {
    fn from(flags: K054539Flags) -> Self {
        let mut value = 0u8;
        if flags.reverse_stereo {
            value |= 1 << 0;
        }
        if flags.disable_reverb {
            value |= 1 << 1;
        }
        if flags.update_at_keyon {
            value |= 1 << 2;
        }
        value |= flags.reserved << 3;
        value
    }
}

impl VgmHeaderField {
    pub fn offset(self) -> usize {
        match self {
            VgmHeaderField::Ident => 0x00,
            VgmHeaderField::EofOffset => 0x04,
            VgmHeaderField::Version => 0x08,
            VgmHeaderField::Sn76489Clock => 0x0C,
            VgmHeaderField::Ym2413Clock => 0x10,
            VgmHeaderField::Gd3Offset => 0x14,
            VgmHeaderField::TotalSamples => 0x18,
            VgmHeaderField::LoopOffset => 0x1C,
            VgmHeaderField::LoopSamples => 0x20,
            VgmHeaderField::SampleRate => 0x24,
            VgmHeaderField::Sn76489Feedback => 0x28,
            VgmHeaderField::Sn76489ShiftRegisterWidth => 0x2A,
            VgmHeaderField::Sn76489Flags => 0x2B,
            VgmHeaderField::Ym2612Clock => 0x2C,
            VgmHeaderField::Ym2151Clock => 0x30,
            VgmHeaderField::DataOffset => 0x34,
            VgmHeaderField::SegaPcmClock => 0x38,
            VgmHeaderField::SpcmInterface => 0x3C,
            VgmHeaderField::Rf5c68Clock => 0x40,
            VgmHeaderField::Ym2203Clock => 0x44,
            VgmHeaderField::Ym2608Clock => 0x48,
            VgmHeaderField::Ym2610bClock => 0x4C,
            VgmHeaderField::Ym3812Clock => 0x50,
            VgmHeaderField::Ym3526Clock => 0x54,
            VgmHeaderField::Y8950Clock => 0x58,
            VgmHeaderField::Ymf262Clock => 0x5C,
            VgmHeaderField::Ymf278bClock => 0x60,
            VgmHeaderField::Ymf271Clock => 0x64,
            VgmHeaderField::Ymz280bClock => 0x68,
            VgmHeaderField::Rf5c164Clock => 0x6C,
            VgmHeaderField::PwmClock => 0x70,
            VgmHeaderField::Ay8910Clock => 0x74,
            VgmHeaderField::Ay8910ChipType => 0x78,
            VgmHeaderField::Ay8910Flags => 0x79,
            VgmHeaderField::Ym2203Ay8910Flags => 0x7A,
            VgmHeaderField::Ym2608Ay8910Flags => 0x7B,
            VgmHeaderField::VolumeModifier => 0x7C,
            VgmHeaderField::Reserved7D => 0x7D,
            VgmHeaderField::LoopBase => 0x7E,
            VgmHeaderField::LoopModifier => 0x7F,
            VgmHeaderField::GbDmgClock => 0x80,
            VgmHeaderField::NesApuClock => 0x84,
            VgmHeaderField::MultipcmClock => 0x88,
            VgmHeaderField::Upd7759Clock => 0x8C,
            VgmHeaderField::Okim6258Clock => 0x90,
            VgmHeaderField::Okim6258Flags => 0x94,
            VgmHeaderField::K054539Flags => 0x95,
            VgmHeaderField::C140ChipType => 0x96,
            VgmHeaderField::Okim6295Clock => 0x98,
            VgmHeaderField::K051649Clock => 0x9C,
            VgmHeaderField::K054539Clock => 0xA0,
            VgmHeaderField::Huc6280Clock => 0xA4,
            VgmHeaderField::C140Clock => 0xA8,
            VgmHeaderField::Reserved97 => 0x97,
            VgmHeaderField::K053260Clock => 0xAC,
            VgmHeaderField::PokeyClock => 0xB0,
            VgmHeaderField::QsoundClock => 0xB4,
            VgmHeaderField::ScspClock => 0xB8,
            VgmHeaderField::ExtraHeaderOffset => 0xBC,
            VgmHeaderField::WonderSwan => 0xC0,
            VgmHeaderField::Vsu => 0xC4,
            VgmHeaderField::Saa1099 => 0xC8,
            VgmHeaderField::Es5503 => 0xCC,
            VgmHeaderField::Es5506 => 0xD0,
            VgmHeaderField::Es5503OutputChannels => 0xD4,
            VgmHeaderField::Es5506OutputChannels => 0xD5,
            VgmHeaderField::C352ClockDivider => 0xD6,
            VgmHeaderField::X1_010 => 0xD8,
            VgmHeaderField::C352 => 0xDC,
            VgmHeaderField::Ga20 => 0xE0,
            VgmHeaderField::Mikey => 0xE4,
            VgmHeaderField::ReservedE8EF => 0xE8,
            VgmHeaderField::ReservedF0FF => 0xF0,
        }
    }

    /// Return the length in bytes for this field as stored in the header.
    pub fn len(self) -> usize {
        match self {
            VgmHeaderField::Ident => 4,
            VgmHeaderField::EofOffset => 4,
            VgmHeaderField::Version => 4,
            VgmHeaderField::Sn76489Clock => 4,
            VgmHeaderField::Ym2413Clock => 4,
            VgmHeaderField::Gd3Offset => 4,
            VgmHeaderField::TotalSamples => 4,
            VgmHeaderField::LoopOffset => 4,
            VgmHeaderField::LoopSamples => 4,
            VgmHeaderField::SampleRate => 4,
            VgmHeaderField::Sn76489Feedback => 2,
            VgmHeaderField::Sn76489ShiftRegisterWidth => 1,
            VgmHeaderField::Sn76489Flags => 1,
            VgmHeaderField::Ym2612Clock => 4,
            VgmHeaderField::Ym2151Clock => 4,
            VgmHeaderField::DataOffset => 4,
            VgmHeaderField::SegaPcmClock => 4,
            VgmHeaderField::SpcmInterface => 4,
            VgmHeaderField::Rf5c68Clock => 4,
            VgmHeaderField::Ym2203Clock => 4,
            VgmHeaderField::Ym2608Clock => 4,
            VgmHeaderField::Ym2610bClock => 4,
            VgmHeaderField::Ym3812Clock => 4,
            VgmHeaderField::Ym3526Clock => 4,
            VgmHeaderField::Y8950Clock => 4,
            VgmHeaderField::Ymf262Clock => 4,
            VgmHeaderField::Ymf278bClock => 4,
            VgmHeaderField::Ymf271Clock => 4,
            VgmHeaderField::Ymz280bClock => 4,
            VgmHeaderField::Rf5c164Clock => 4,
            VgmHeaderField::PwmClock => 4,
            VgmHeaderField::Ay8910Clock => 4,
            VgmHeaderField::Ay8910ChipType => 1,
            VgmHeaderField::Ay8910Flags => 1,
            VgmHeaderField::Ym2203Ay8910Flags => 1,
            VgmHeaderField::Ym2608Ay8910Flags => 1,
            VgmHeaderField::VolumeModifier => 1,
            VgmHeaderField::Reserved7D => 1,
            VgmHeaderField::LoopBase => 1,
            VgmHeaderField::LoopModifier => 1,
            VgmHeaderField::GbDmgClock => 4,
            VgmHeaderField::NesApuClock => 4,
            VgmHeaderField::MultipcmClock => 4,
            VgmHeaderField::Upd7759Clock => 4,
            VgmHeaderField::Okim6258Clock => 4,
            VgmHeaderField::Okim6258Flags => 1,
            VgmHeaderField::K054539Flags => 1,
            VgmHeaderField::C140ChipType => 1,
            VgmHeaderField::Okim6295Clock => 4,
            VgmHeaderField::K051649Clock => 4,
            VgmHeaderField::K054539Clock => 4,
            VgmHeaderField::Huc6280Clock => 4,
            VgmHeaderField::C140Clock => 4,
            VgmHeaderField::Reserved97 => 1,
            VgmHeaderField::K053260Clock => 4,
            VgmHeaderField::PokeyClock => 4,
            VgmHeaderField::QsoundClock => 4,
            VgmHeaderField::ScspClock => 4,
            VgmHeaderField::ExtraHeaderOffset => 4,
            VgmHeaderField::WonderSwan => 4,
            VgmHeaderField::Vsu => 4,
            VgmHeaderField::Saa1099 => 4,
            VgmHeaderField::Es5503 => 4,
            VgmHeaderField::Es5506 => 4,
            VgmHeaderField::Es5503OutputChannels => 1,
            VgmHeaderField::Es5506OutputChannels => 1,
            VgmHeaderField::C352ClockDivider => 1,
            VgmHeaderField::X1_010 => 4,
            VgmHeaderField::C352 => 4,
            VgmHeaderField::Ga20 => 4,
            VgmHeaderField::Mikey => 4,
            VgmHeaderField::ReservedE8EF => 8,
            VgmHeaderField::ReservedF0FF => 16,
        }
    }

    /// Return true if this field occupies zero bytes (none in current spec).
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Returns the minimum VGM version that introduced this field.
    /// Fields not present in a version should be treated as zero.
    ///
    /// Reference: <https://vgmrips.net/wiki/VGM_Specification>
    pub fn min_version(self) -> u32 {
        match self {
            // VGM 1.00 fields
            VgmHeaderField::Ident => 0x00000100,
            VgmHeaderField::EofOffset => 0x00000100,
            VgmHeaderField::Version => 0x00000100,
            VgmHeaderField::Sn76489Clock => 0x00000100,
            VgmHeaderField::Ym2413Clock => 0x00000100,
            VgmHeaderField::Gd3Offset => 0x00000100,
            VgmHeaderField::TotalSamples => 0x00000100,
            VgmHeaderField::LoopOffset => 0x00000100,
            VgmHeaderField::LoopSamples => 0x00000100,
            // VGM 1.01 additions
            VgmHeaderField::SampleRate => 0x00000101,
            // VGM 1.10 additions
            VgmHeaderField::Sn76489Feedback => 0x00000110,
            VgmHeaderField::Sn76489ShiftRegisterWidth => 0x00000110,
            VgmHeaderField::Ym2612Clock => 0x00000110,
            VgmHeaderField::Ym2151Clock => 0x00000110,
            // VGM 1.50 additions
            VgmHeaderField::DataOffset => 0x00000150,
            // VGM 1.51 additions
            VgmHeaderField::Sn76489Flags => 0x00000151,
            VgmHeaderField::SegaPcmClock => 0x00000151,
            VgmHeaderField::SpcmInterface => 0x00000151,
            VgmHeaderField::Rf5c68Clock => 0x00000151,
            VgmHeaderField::Ym2203Clock => 0x00000151,
            VgmHeaderField::Ym2608Clock => 0x00000151,
            VgmHeaderField::Ym2610bClock => 0x00000151,
            VgmHeaderField::Ym3812Clock => 0x00000151,
            VgmHeaderField::Ym3526Clock => 0x00000151,
            VgmHeaderField::Y8950Clock => 0x00000151,
            VgmHeaderField::Ymf262Clock => 0x00000151,
            VgmHeaderField::Ymf278bClock => 0x00000151,
            VgmHeaderField::Ymf271Clock => 0x00000151,
            VgmHeaderField::Ymz280bClock => 0x00000151,
            VgmHeaderField::Rf5c164Clock => 0x00000151,
            VgmHeaderField::PwmClock => 0x00000151,
            VgmHeaderField::Ay8910Clock => 0x00000151,
            VgmHeaderField::Ay8910ChipType => 0x00000151,
            VgmHeaderField::Ay8910Flags => 0x00000151,
            VgmHeaderField::Ym2203Ay8910Flags => 0x00000151,
            VgmHeaderField::Ym2608Ay8910Flags => 0x00000151,
            // VGM 1.60 additions
            VgmHeaderField::VolumeModifier => 0x00000160,
            VgmHeaderField::Reserved7D => 0x00000160,
            VgmHeaderField::LoopBase => 0x00000160,
            VgmHeaderField::LoopModifier => 0x00000151,
            // VGM 1.61 additions
            VgmHeaderField::GbDmgClock => 0x00000161,
            VgmHeaderField::NesApuClock => 0x00000161,
            VgmHeaderField::MultipcmClock => 0x00000161,
            VgmHeaderField::Upd7759Clock => 0x00000161,
            VgmHeaderField::Okim6258Clock => 0x00000161,
            VgmHeaderField::Okim6258Flags => 0x00000161,
            VgmHeaderField::K054539Flags => 0x00000161,
            VgmHeaderField::C140ChipType => 0x00000161,
            VgmHeaderField::Okim6295Clock => 0x00000161,
            VgmHeaderField::K051649Clock => 0x00000161,
            VgmHeaderField::K054539Clock => 0x00000161,
            VgmHeaderField::Huc6280Clock => 0x00000161,
            VgmHeaderField::C140Clock => 0x00000161,
            VgmHeaderField::Reserved97 => 0x00000161,
            VgmHeaderField::K053260Clock => 0x00000161,
            VgmHeaderField::PokeyClock => 0x00000161,
            VgmHeaderField::QsoundClock => 0x00000161,
            // VGM 1.71 additions
            VgmHeaderField::ScspClock => 0x00000171,
            // VGM 1.70 additions
            VgmHeaderField::ExtraHeaderOffset => 0x00000170,
            // VGM 1.71 additions
            VgmHeaderField::WonderSwan => 0x00000171,
            VgmHeaderField::Vsu => 0x00000171,
            VgmHeaderField::Saa1099 => 0x00000171,
            VgmHeaderField::Es5503 => 0x00000171,
            VgmHeaderField::Es5506 => 0x00000171,
            VgmHeaderField::Es5503OutputChannels => 0x00000171,
            VgmHeaderField::Es5506OutputChannels => 0x00000171,
            VgmHeaderField::C352ClockDivider => 0x00000171,
            VgmHeaderField::X1_010 => 0x00000171,
            VgmHeaderField::C352 => 0x00000171,
            VgmHeaderField::Ga20 => 0x00000171,
            // VGM 1.72 additions
            VgmHeaderField::Mikey => 0x00000172,
            // Reserved fields (use max version for future compatibility)
            VgmHeaderField::ReservedE8EF => 0x00000172,
            VgmHeaderField::ReservedF0FF => 0x00000172,
        }
    }

    /// Compute the byte range (start, length) of this field within the
    /// serialized header buffer considering the `data_offset` rules used by
    /// `VgmHeader::to_bytes()`.
    ///
    /// `header_version` should be the header's `version` value and is used
    /// when `data_offset == 0` to compute the legacy header size.
    pub fn byte_range(self, header_version: u32, data_offset: u32) -> Option<(usize, usize)> {
        // Compute header size using same rules as serialization
        let header_size = if data_offset == 0 {
            VgmHeader::fallback_header_size_for_version(header_version)
        } else {
            0x34_usize.wrapping_add(data_offset as usize)
        };

        let off = self.offset();
        let len = self.len();
        if off + len <= header_size {
            Some((off, len))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
/// VGM file header fields and utilities for serialization.
pub struct VgmHeader {
    pub ident: [u8; 4],
    pub eof_offset: u32,
    pub version: u32,
    pub sn76489_clock: u32,
    pub ym2413_clock: u32,
    pub gd3_offset: u32,
    pub total_samples: u32,
    pub loop_offset: u32,
    pub loop_samples: u32,
    pub sample_rate: u32,
    pub sn76489_feedback: Sn76489Feedback,
    pub sn76489_shift_register_width: Sn76489ShiftRegisterWidth,
    pub sn76489_flags: Sn76489Flags,
    pub ym2612_clock: u32,
    pub ym2151_clock: u32,
    pub data_offset: u32,
    pub sega_pcm_clock: u32,
    pub spcm_interface: u32,
    pub rf5c68_clock: u32,
    pub ym2203_clock: u32,
    pub ym2608_clock: u32,
    pub ym2610b_clock: u32,
    pub ym3812_clock: u32,
    pub ym3526_clock: u32,
    pub y8950_clock: u32,
    pub ymf262_clock: u32,
    pub ymf278b_clock: u32,
    pub ymf271_clock: u32,
    pub ymz280b_clock: u32,
    pub rf5c164_clock: u32,
    pub pwm_clock: u32,
    pub ay8910_clock: u32,
    pub ay_chip_type: Ay8910ChipType,
    pub ay8910_flags: Ay8910Flags,
    pub ym2203_ay8910_flags: Ym2203AyFlags,
    pub ym2608_ay8910_flags: Ym2608AyFlags,
    pub volume_modifier: u8,
    pub reserved_7d: u8,
    pub loop_base: i8,
    pub loop_modifier: u8,
    pub gb_dmg_clock: u32,
    pub nes_apu_clock: u32,
    pub multipcm_clock: u32,
    pub upd7759_clock: u32,
    pub okim6258_clock: u32,
    pub okim6258_flags: Okim6258Flags,
    pub k054539_flags: K054539Flags,
    pub c140_chip_type: C140ChipType,
    pub okim6295_clock: u32,
    pub k051649_clock: u32,
    pub k054539_clock: u32,
    pub huc6280_clock: u32,
    pub c140_clock: u32,
    pub reserved_97: u8,
    pub k053260_clock: u32,
    pub pokey_clock: u32,
    pub qsound_clock: u32,
    pub scsp_clock: u32,
    pub extra_header_offset: u32,
    pub wonderswan_clock: u32,
    pub vsu_clock: u32,
    pub saa1099_clock: u32,
    pub es5503_clock: u32,
    pub es5506_clock: u32,
    pub es5503_output_channels: u8,
    pub es5506_output_channels: u8,
    pub c352_clock_divider: u8,
    pub x1_010_clock: u32,
    pub c352_clock: u32,
    pub ga20_clock: u32,
    pub mikey_clock: u32,
    pub reserved_e8_ef: [u8; 8],
    pub reserved_f0_ff: [u8; 16],
}

impl Default for VgmHeader {
    fn default() -> Self {
        VgmHeader {
            ident: *b"Vgm ",
            eof_offset: 0,
            version: 0x00000172, // 1.72
            sn76489_clock: 0,
            ym2413_clock: 0,
            gd3_offset: 0,
            total_samples: 0,
            loop_offset: 0,
            loop_samples: 0,
            sample_rate: 44100,
            sn76489_feedback: Sn76489Feedback::Unknown(0),
            sn76489_shift_register_width: Sn76489ShiftRegisterWidth::Unknown(0),
            sn76489_flags: Sn76489Flags {
                frequency_0_is_400: false,
                output_negate: false,
                gamegear_stereo: true,
                clock_divider_8: true,
                xnor_noise_mode: false,
                reserved: 0,
            },
            ym2612_clock: 0,
            ym2151_clock: 0,
            data_offset: 0,
            sega_pcm_clock: 0,
            spcm_interface: 0,
            rf5c68_clock: 0,
            ym2203_clock: 0,
            ym2608_clock: 0,
            ym2610b_clock: 0,
            ym3812_clock: 0,
            ym3526_clock: 0,
            y8950_clock: 0,
            ymf262_clock: 0,
            ymf278b_clock: 0,
            ymf271_clock: 0,
            ymz280b_clock: 0,
            rf5c164_clock: 0,
            pwm_clock: 0,
            ay8910_clock: 0,
            ay_chip_type: Ay8910ChipType::Unknown(0),
            ay8910_flags: Ay8910Flags {
                legacy_output: false,
                single_output: false,
                discrete_output: false,
                raw_output: false,
                ym_pin26_low: false,
                reserved: 0,
            },
            ym2203_ay8910_flags: Ym2203AyFlags {
                legacy_output: false,
                single_output: false,
                discrete_output: false,
                raw_output: false,
                ym_pin26_low: false,
                reserved: 0,
            },
            ym2608_ay8910_flags: Ym2608AyFlags {
                legacy_output: false,
                single_output: false,
                discrete_output: false,
                raw_output: false,
                ym_pin26_low: false,
                reserved: 0,
            },
            volume_modifier: 0,
            reserved_7d: 0,
            loop_base: 0,
            loop_modifier: 0,
            gb_dmg_clock: 0,
            nes_apu_clock: 0,
            multipcm_clock: 0,
            upd7759_clock: 0,
            okim6258_clock: 0,
            okim6258_flags: Okim6258Flags {
                clock_divider: 0,
                adpcm_3bit_select: false,
                output_12bit: false,
                reserved: 0,
            },
            k054539_flags: K054539Flags {
                reverse_stereo: false,
                disable_reverb: false,
                update_at_keyon: false,
                reserved: 0,
            },
            c140_chip_type: C140ChipType::Unknown(0),
            okim6295_clock: 0,
            k051649_clock: 0,
            k054539_clock: 0,
            huc6280_clock: 0,
            c140_clock: 0,
            reserved_97: 0,
            k053260_clock: 0,
            pokey_clock: 0,
            qsound_clock: 0,
            scsp_clock: 0,
            extra_header_offset: 0,
            wonderswan_clock: 0,
            vsu_clock: 0,
            saa1099_clock: 0,
            es5503_clock: 0,
            es5506_clock: 0,
            es5503_output_channels: 0,
            es5506_output_channels: 0,
            c352_clock_divider: 0,
            x1_010_clock: 0,
            c352_clock: 0,
            ga20_clock: 0,
            mikey_clock: 0,
            reserved_e8_ef: [0u8; 8],
            reserved_f0_ff: [0u8; 16],
        }
    }
}

impl VgmHeader {
    pub(crate) fn to_bytes(&self, gd3_offset: u32, data_offset: u32) -> Vec<u8> {
        let mut buf: Vec<u8> = vec![0; VGM_MAX_HEADER_SIZE as usize];
        // ident (0x00)
        write_slice(&mut buf, VgmHeaderField::Ident.offset(), &self.ident);
        // eof_offset placeholder (0x04)
        write_u32(
            &mut buf,
            VgmHeaderField::EofOffset.offset(),
            self.eof_offset,
        );
        // version (0x08)
        write_u32(&mut buf, VgmHeaderField::Version.offset(), self.version);
        // SN76489 clock (0x0C)
        write_u32(
            &mut buf,
            VgmHeaderField::Sn76489Clock.offset(),
            self.sn76489_clock,
        );
        // YM2413 clock (0x10)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym2413Clock.offset(),
            self.ym2413_clock,
        );
        // GD3 offset (0x14)
        write_u32(&mut buf, VgmHeaderField::Gd3Offset.offset(), gd3_offset);
        // total samples (0x18)
        write_u32(
            &mut buf,
            VgmHeaderField::TotalSamples.offset(),
            self.total_samples,
        );
        // loop offset (0x1C)
        write_u32(
            &mut buf,
            VgmHeaderField::LoopOffset.offset(),
            self.loop_offset,
        );
        // loop samples (0x20)
        write_u32(
            &mut buf,
            VgmHeaderField::LoopSamples.offset(),
            self.loop_samples,
        );
        // sample rate (0x24)
        write_u32(
            &mut buf,
            VgmHeaderField::SampleRate.offset(),
            self.sample_rate,
        );
        // SN76489 feedback (0x28)
        write_u16(
            &mut buf,
            VgmHeaderField::Sn76489Feedback.offset(),
            u16::from(self.sn76489_feedback),
        );
        // SN76489 shift register width (0x2A)
        write_u8(
            &mut buf,
            VgmHeaderField::Sn76489ShiftRegisterWidth.offset(),
            u8::from(self.sn76489_shift_register_width),
        );
        // SN76489 flags (0x2B)
        write_u8(
            &mut buf,
            VgmHeaderField::Sn76489Flags.offset(),
            u8::from(self.sn76489_flags),
        );
        // YM2612 clock (0x2C)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym2612Clock.offset(),
            self.ym2612_clock,
        );
        // YM2151 clock (0x30)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym2151Clock.offset(),
            self.ym2151_clock,
        );
        // Data offset (0x34)
        write_u32(&mut buf, VgmHeaderField::DataOffset.offset(), data_offset);
        // SegaPCM clock (0x38)
        write_u32(
            &mut buf,
            VgmHeaderField::SegaPcmClock.offset(),
            self.sega_pcm_clock,
        );
        // SPCM interface (0x3C)
        write_u32(
            &mut buf,
            VgmHeaderField::SpcmInterface.offset(),
            self.spcm_interface,
        );
        // RF5C68 (0x40)
        write_u32(
            &mut buf,
            VgmHeaderField::Rf5c68Clock.offset(),
            self.rf5c68_clock,
        );
        // YM2203 (0x44)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym2203Clock.offset(),
            self.ym2203_clock,
        );
        // YM2608 (0x48)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym2608Clock.offset(),
            self.ym2608_clock,
        );
        // YM2610/B (0x4C)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym2610bClock.offset(),
            self.ym2610b_clock,
        );
        // YM3812 (0x50)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym3812Clock.offset(),
            self.ym3812_clock,
        );
        // YM3526 (0x54)
        write_u32(
            &mut buf,
            VgmHeaderField::Ym3526Clock.offset(),
            self.ym3526_clock,
        );
        // Y8950 (0x58)
        write_u32(
            &mut buf,
            VgmHeaderField::Y8950Clock.offset(),
            self.y8950_clock,
        );
        // YMF262 (0x5C)
        write_u32(
            &mut buf,
            VgmHeaderField::Ymf262Clock.offset(),
            self.ymf262_clock,
        );
        // YMF278B (0x60)
        write_u32(
            &mut buf,
            VgmHeaderField::Ymf278bClock.offset(),
            self.ymf278b_clock,
        );
        // YMF271 (0x64)
        write_u32(
            &mut buf,
            VgmHeaderField::Ymf271Clock.offset(),
            self.ymf271_clock,
        );
        // YMZ280B (0x68)
        write_u32(
            &mut buf,
            VgmHeaderField::Ymz280bClock.offset(),
            self.ymz280b_clock,
        );
        // RF5C164 (0x6C)
        write_u32(
            &mut buf,
            VgmHeaderField::Rf5c164Clock.offset(),
            self.rf5c164_clock,
        );
        // PWM (0x70)
        write_u32(&mut buf, VgmHeaderField::PwmClock.offset(), self.pwm_clock);
        // AY8910 (0x74)
        write_u32(
            &mut buf,
            VgmHeaderField::Ay8910Clock.offset(),
            self.ay8910_clock,
        );
        // AY8910 Chip Type (0x78)
        write_u8(
            &mut buf,
            VgmHeaderField::Ay8910ChipType.offset(),
            u8::from(self.ay_chip_type),
        );
        // AY8910 Flags (0x79)
        write_u8(
            &mut buf,
            VgmHeaderField::Ay8910Flags.offset(),
            u8::from(self.ay8910_flags),
        );
        // YM2203/AY8910 Flags (0x7A)
        write_u8(
            &mut buf,
            VgmHeaderField::Ym2203Ay8910Flags.offset(),
            u8::from(self.ym2203_ay8910_flags),
        );
        // YM2608/AY8910 Flags (0x7B)
        write_u8(
            &mut buf,
            VgmHeaderField::Ym2608Ay8910Flags.offset(),
            u8::from(self.ym2608_ay8910_flags),
        );
        // Volume Modifier (0x7C)
        write_u8(
            &mut buf,
            VgmHeaderField::VolumeModifier.offset(),
            self.volume_modifier,
        );
        // Reserved (0x7D)
        write_u8(
            &mut buf,
            VgmHeaderField::Reserved7D.offset(),
            self.reserved_7d,
        );
        // Loop Base (0x7E)
        write_u8(
            &mut buf,
            VgmHeaderField::LoopBase.offset(),
            self.loop_base as u8,
        );
        // Loop Modifier (0x7F)
        write_u8(
            &mut buf,
            VgmHeaderField::LoopModifier.offset(),
            self.loop_modifier,
        );
        // GB DMG (0x80)
        write_u32(
            &mut buf,
            VgmHeaderField::GbDmgClock.offset(),
            self.gb_dmg_clock,
        );
        // NES APU (0x84)
        write_u32(
            &mut buf,
            VgmHeaderField::NesApuClock.offset(),
            self.nes_apu_clock,
        );
        // MultiPCM (0x88)
        write_u32(
            &mut buf,
            VgmHeaderField::MultipcmClock.offset(),
            self.multipcm_clock,
        );
        // uPD7759 (0x8C)
        write_u32(
            &mut buf,
            VgmHeaderField::Upd7759Clock.offset(),
            self.upd7759_clock,
        );
        // OKIM6258 (0x90)
        write_u32(
            &mut buf,
            VgmHeaderField::Okim6258Clock.offset(),
            self.okim6258_clock,
        );
        // OKIM6258 flags (0x94)
        write_u8(
            &mut buf,
            VgmHeaderField::Okim6258Flags.offset(),
            u8::from(self.okim6258_flags),
        );
        // K054539 Flags (0x95)
        write_u8(
            &mut buf,
            VgmHeaderField::K054539Flags.offset(),
            u8::from(self.k054539_flags),
        );
        // C140 Chip Type (0x96)
        write_u8(
            &mut buf,
            VgmHeaderField::C140ChipType.offset(),
            u8::from(self.c140_chip_type),
        );
        // Reserved (0x97)
        write_u8(
            &mut buf,
            VgmHeaderField::Reserved97.offset(),
            self.reserved_97,
        );
        // OKIM6295 (0x98)
        write_u32(
            &mut buf,
            VgmHeaderField::Okim6295Clock.offset(),
            self.okim6295_clock,
        );
        // K051649 (0x9C)
        write_u32(
            &mut buf,
            VgmHeaderField::K051649Clock.offset(),
            self.k051649_clock,
        );
        // K054539 (0xA0)
        write_u32(
            &mut buf,
            VgmHeaderField::K054539Clock.offset(),
            self.k054539_clock,
        );
        // HuC6280 (0xA4)
        write_u32(
            &mut buf,
            VgmHeaderField::Huc6280Clock.offset(),
            self.huc6280_clock,
        );
        // C140 (0xA8)
        write_u32(
            &mut buf,
            VgmHeaderField::C140Clock.offset(),
            self.c140_clock,
        );
        // K053260 (0xAC)
        write_u32(
            &mut buf,
            VgmHeaderField::K053260Clock.offset(),
            self.k053260_clock,
        );
        // Pokey (0xB0)
        write_u32(
            &mut buf,
            VgmHeaderField::PokeyClock.offset(),
            self.pokey_clock,
        );
        // QSound (0xB4)
        write_u32(
            &mut buf,
            VgmHeaderField::QsoundClock.offset(),
            self.qsound_clock,
        );
        // SCSP (0xB8)
        write_u32(
            &mut buf,
            VgmHeaderField::ScspClock.offset(),
            self.scsp_clock,
        );
        // Extra header offset (0xBC)
        write_u32(
            &mut buf,
            VgmHeaderField::ExtraHeaderOffset.offset(),
            self.extra_header_offset,
        );
        // WonderSwan (0xC0)
        write_u32(
            &mut buf,
            VgmHeaderField::WonderSwan.offset(),
            self.wonderswan_clock,
        );
        // VSU (0xC4)
        write_u32(&mut buf, VgmHeaderField::Vsu.offset(), self.vsu_clock);
        // SAA1099 (0xC8)
        write_u32(
            &mut buf,
            VgmHeaderField::Saa1099.offset(),
            self.saa1099_clock,
        );
        // ES5503 (0xCC)
        write_u32(&mut buf, VgmHeaderField::Es5503.offset(), self.es5503_clock);
        // ES5506 (0xD0)
        write_u32(&mut buf, VgmHeaderField::Es5506.offset(), self.es5506_clock);
        // Es5503 output channels (0xD4)
        write_u8(
            &mut buf,
            VgmHeaderField::Es5503OutputChannels.offset(),
            self.es5503_output_channels,
        );
        // ES5506 output channels (0xD5)
        write_u8(
            &mut buf,
            VgmHeaderField::Es5506OutputChannels.offset(),
            self.es5506_output_channels,
        );
        // C352 clock divider (0xD6)
        write_u8(
            &mut buf,
            VgmHeaderField::C352ClockDivider.offset(),
            self.c352_clock_divider,
        );
        // X1-010 (0xD8)
        write_u32(&mut buf, VgmHeaderField::X1_010.offset(), self.x1_010_clock);
        // C352 (0xDC)
        write_u32(&mut buf, VgmHeaderField::C352.offset(), self.c352_clock);
        // GA20 (0xE0)
        write_u32(&mut buf, VgmHeaderField::Ga20.offset(), self.ga20_clock);
        // Mikey (0xE4)
        write_u32(&mut buf, VgmHeaderField::Mikey.offset(), self.mikey_clock);
        // reserved (0xE8..0xEF)
        write_slice(
            &mut buf,
            VgmHeaderField::ReservedE8EF.offset(),
            &self.reserved_e8_ef,
        );
        // reserved (0xF0..0xFF)
        write_slice(
            &mut buf,
            VgmHeaderField::ReservedF0FF.offset(),
            &self.reserved_f0_ff,
        );

        let header_size = if data_offset == 0 {
            VgmHeader::fallback_header_size_for_version(self.version)
        } else {
            0x34_usize.wrapping_add(data_offset as usize)
        };
        if header_size < buf.len() {
            buf.truncate(header_size);
        }
        buf
    }

    /// Get the raw stored clock field for a chip `ch`.
    ///
    /// Returns the raw clock value from the header, including the high bit
    /// (0x8000_0000) for secondary instances. Returns 0 if the chip is not present.
    pub fn get_chip_clock(&self, ch: &chip::Chip) -> u32 {
        match ch {
            chip::Chip::Sn76489 => self.sn76489_clock,
            chip::Chip::Ym2413 => self.ym2413_clock,
            chip::Chip::Ym2612 => self.ym2612_clock,
            chip::Chip::Ym2151 => self.ym2151_clock,
            chip::Chip::SegaPcm => self.sega_pcm_clock,
            chip::Chip::Rf5c68 => self.rf5c68_clock,
            chip::Chip::Ym2203 => self.ym2203_clock,
            chip::Chip::Ym2608 => self.ym2608_clock,
            chip::Chip::Ym2610b => self.ym2610b_clock,
            chip::Chip::Ym3812 => self.ym3812_clock,
            chip::Chip::Ym3526 => self.ym3526_clock,
            chip::Chip::Y8950 => self.y8950_clock,
            chip::Chip::Ymf262 => self.ymf262_clock,
            chip::Chip::Ymf278b => self.ymf278b_clock,
            chip::Chip::Ymf271 => self.ymf271_clock,
            chip::Chip::Ymz280b => self.ymz280b_clock,
            chip::Chip::Rf5c164 => self.rf5c164_clock,
            chip::Chip::Pwm => self.pwm_clock,
            chip::Chip::Ay8910 => self.ay8910_clock,
            chip::Chip::GbDmg => self.gb_dmg_clock,
            chip::Chip::NesApu => self.nes_apu_clock,
            chip::Chip::MultiPcm => self.multipcm_clock,
            chip::Chip::Upd7759 => self.upd7759_clock,
            chip::Chip::Okim6258 => self.okim6258_clock,
            chip::Chip::Okim6295 => self.okim6295_clock,
            chip::Chip::K051649 => self.k051649_clock,
            chip::Chip::K054539 => self.k054539_clock,
            chip::Chip::Huc6280 => self.huc6280_clock,
            chip::Chip::C140 => self.c140_clock,
            chip::Chip::K053260 => self.k053260_clock,
            chip::Chip::Pokey => self.pokey_clock,
            chip::Chip::Qsound => self.qsound_clock,
            chip::Chip::Scsp => self.scsp_clock,
            chip::Chip::WonderSwan => self.wonderswan_clock,
            chip::Chip::Vsu => self.vsu_clock,
            chip::Chip::Saa1099 => self.saa1099_clock,
            chip::Chip::Es5503 => self.es5503_clock,
            chip::Chip::Es5506U8 | chip::Chip::Es5506U16 => self.es5506_clock,
            chip::Chip::X1010 => self.x1_010_clock,
            chip::Chip::C352 => self.c352_clock,
            chip::Chip::Ga20 => self.ga20_clock,
            chip::Chip::Mikey => self.mikey_clock,
            _ => 0,
        }
    }

    /// Set the stored clock field for a chip `ch` at the given `instance`.
    /// For secondary instances the high bit is set on the stored value
    /// following VGM header convention.
    pub fn set_chip_clock(&mut self, ch: chip::Chip, instance: Instance, master_clock: u32) {
        let clock = match instance {
            Instance::Primary => master_clock,
            Instance::Secondary => master_clock | 0x8000_0000_u32,
        };

        match &ch {
            chip::Chip::Sn76489 => self.sn76489_clock = clock,
            chip::Chip::Ym2413 => self.ym2413_clock = clock,
            chip::Chip::Ym2612 => self.ym2612_clock = clock,
            chip::Chip::Ym2151 => self.ym2151_clock = clock,
            chip::Chip::SegaPcm => self.sega_pcm_clock = clock,
            chip::Chip::Rf5c68 => self.rf5c68_clock = clock,
            chip::Chip::Ym2203 => self.ym2203_clock = clock,
            chip::Chip::Ym2608 => self.ym2608_clock = clock,
            chip::Chip::Ym2610b => self.ym2610b_clock = clock,
            chip::Chip::Ym3812 => self.ym3812_clock = clock,
            chip::Chip::Ym3526 => self.ym3526_clock = clock,
            chip::Chip::Y8950 => self.y8950_clock = clock,
            chip::Chip::Ymf262 => self.ymf262_clock = clock,
            chip::Chip::Ymf278b => self.ymf278b_clock = clock,
            chip::Chip::Ymf271 => self.ymf271_clock = clock,
            chip::Chip::Ymz280b => self.ymz280b_clock = clock,
            chip::Chip::Rf5c164 => self.rf5c164_clock = clock,
            chip::Chip::Pwm => self.pwm_clock = clock,
            chip::Chip::Ay8910 => self.ay8910_clock = clock,
            chip::Chip::GbDmg => self.gb_dmg_clock = clock,
            chip::Chip::NesApu => self.nes_apu_clock = clock,
            chip::Chip::MultiPcm => self.multipcm_clock = clock,
            chip::Chip::Upd7759 => self.upd7759_clock = clock,
            chip::Chip::Okim6258 => self.okim6258_clock = clock,
            chip::Chip::Okim6295 => self.okim6295_clock = clock,
            chip::Chip::K051649 => self.k051649_clock = clock,
            chip::Chip::K054539 => self.k054539_clock = clock,
            chip::Chip::Huc6280 => self.huc6280_clock = clock,
            chip::Chip::C140 => self.c140_clock = clock,
            chip::Chip::K053260 => self.k053260_clock = clock,
            chip::Chip::Pokey => self.pokey_clock = clock,
            chip::Chip::Qsound => self.qsound_clock = clock,
            chip::Chip::Scsp => self.scsp_clock = clock,
            chip::Chip::WonderSwan => self.wonderswan_clock = clock,
            chip::Chip::Vsu => self.vsu_clock = clock,
            chip::Chip::Saa1099 => self.saa1099_clock = clock,
            chip::Chip::Es5503 => self.es5503_clock = clock,
            chip::Chip::Es5506U8 => self.es5506_clock = clock,
            chip::Chip::Es5506U16 => self.es5506_clock = clock,
            chip::Chip::X1010 => self.x1_010_clock = clock,
            chip::Chip::C352 => self.c352_clock = clock,
            chip::Chip::Ga20 => self.ga20_clock = clock,
            chip::Chip::Mikey => self.mikey_clock = clock,
            _ => {}
        }
    }

    /// Return a list of present chip instances found in the header.
    ///
    /// Scans the header clock fields and returns a `ChipInstances` containing tuples
    /// `(Instance, chip::Chip, clock_hz)` for each clock that is non-zero. The
    /// high bit (0x8000_0000) on stored clock values indicates a
    /// secondary instance per VGM convention.
    pub fn chip_instances(&self) -> ChipInstances {
        let mut out: Vec<(Instance, chip::Chip, f32)> = Vec::new();

        let mut push = |raw_clock: u32, ch: chip::Chip| {
            if raw_clock != 0 {
                // If the stored clock has the high bit set it indicates a
                // secondary instance per VGM convention.
                let is_secondary = (raw_clock & 0x8000_0000_u32) != 0;
                let clock_hz = (raw_clock & 0x7FFF_FFFF) as f32;
                if is_secondary {
                    out.push((Instance::Primary, ch.clone(), clock_hz));
                    out.push((Instance::Secondary, ch.clone(), clock_hz));
                } else {
                    out.push((Instance::Primary, ch.clone(), clock_hz));
                }
            }
        };

        // Heuristics for older VGM versions where YM2413 clock may carry YM2612/YM2151 info.
        let misc = self.misc();
        // Allow misc-derived substitution of YM2413 clock for YM2612/YM2151.
        // The secondary chip shall be treated as non-existent.
        let mut ym2612_clock = self.ym2612_clock;
        if let Some(clock) = misc.use_ym2413_clock_for_ym2612 {
            ym2612_clock = clock;
        }
        let mut ym2151_clock = self.ym2151_clock;
        if let Some(clock) = misc.use_ym2413_clock_for_ym2151 {
            ym2151_clock = clock;
        }

        push(self.sn76489_clock, chip::Chip::Sn76489);
        push(self.ym2413_clock, chip::Chip::Ym2413);
        push(ym2612_clock, chip::Chip::Ym2612);
        push(ym2151_clock, chip::Chip::Ym2151);
        push(self.sega_pcm_clock, chip::Chip::SegaPcm);
        push(self.rf5c68_clock, chip::Chip::Rf5c68);
        push(self.ym2203_clock, chip::Chip::Ym2203);
        push(self.ym2608_clock, chip::Chip::Ym2608);
        push(self.ym2610b_clock, chip::Chip::Ym2610b);
        push(self.ym3812_clock, chip::Chip::Ym3812);
        push(self.ym3526_clock, chip::Chip::Ym3526);
        push(self.y8950_clock, chip::Chip::Y8950);
        push(self.ymf262_clock, chip::Chip::Ymf262);
        push(self.ymf278b_clock, chip::Chip::Ymf278b);
        push(self.ymf271_clock, chip::Chip::Ymf271);
        push(self.ymz280b_clock, chip::Chip::Ymz280b);
        push(self.rf5c164_clock, chip::Chip::Rf5c164);
        push(self.pwm_clock, chip::Chip::Pwm);
        push(self.ay8910_clock, chip::Chip::Ay8910);
        push(self.gb_dmg_clock, chip::Chip::GbDmg);
        push(self.nes_apu_clock, chip::Chip::NesApu);
        push(self.multipcm_clock, chip::Chip::MultiPcm);
        push(self.upd7759_clock, chip::Chip::Upd7759);
        push(self.okim6258_clock, chip::Chip::Okim6258);
        push(self.okim6295_clock, chip::Chip::Okim6295);
        push(self.k051649_clock, chip::Chip::K051649);
        push(self.k054539_clock, chip::Chip::K054539);
        push(self.huc6280_clock, chip::Chip::Huc6280);
        push(self.c140_clock, chip::Chip::C140);
        push(self.k053260_clock, chip::Chip::K053260);
        push(self.pokey_clock, chip::Chip::Pokey);
        push(self.qsound_clock, chip::Chip::Qsound);
        push(self.scsp_clock, chip::Chip::Scsp);
        push(self.wonderswan_clock, chip::Chip::WonderSwan);
        push(self.vsu_clock, chip::Chip::Vsu);
        push(self.saa1099_clock, chip::Chip::Saa1099);
        push(self.es5503_clock, chip::Chip::Es5503);
        push(self.es5506_clock, chip::Chip::Es5506U8);
        push(self.x1_010_clock, chip::Chip::X1010);
        push(self.c352_clock, chip::Chip::C352);
        push(self.ga20_clock, chip::Chip::Ga20);
        push(self.mikey_clock, chip::Chip::Mikey);

        ChipInstances(out)
    }

    /// Return the legacy header size to use when a stored `data_offset` is 0
    /// (older VGM versions omitted the data_offset field or used smaller
    /// headers). The returned size is the total header size in bytes and
    /// includes the 32-bit `data_offset` field region when applicable.
    /// Returns the header size (in bytes) based on the VGM version number.
    ///
    /// This function is used when `data_offset` is zero, to determine how much
    /// of the file should be treated as header based on what fields were defined
    /// in each VGM specification version.
    pub fn fallback_header_size_for_version(version: u32) -> usize {
        match version {
            // VGM 1.00: Fields 0x00-0x23 (36 bytes total)
            0x00000100 => 0x24,
            // VGM 1.01: Added Rate at 0x24-0x27 (40 bytes total)
            0x00000101 => 0x28,
            // VGM 1.10: Added SN76489 feedback/width at 0x28-0x2B,
            //           and YM2612/YM2151 clocks at 0x2C-0x33 (52 bytes total)
            0x00000110 => 0x34,
            // VGM 1.50: Added VGM data offset at 0x34-0x37 (56 bytes total)
            0x00000150 => 0x38,
            // VGM 1.51: Added many chip clocks from 0x38-0x7F (128 bytes total)
            // VGM 1.60: Added Volume Modifier, Loop Base at 0x7C-0x7E (already in 1.51 range)
            0x00000151 | 0x00000160 => 0x80,
            // VGM 1.61: Added more chip clocks from 0x80-0xB7 (184 bytes total)
            0x00000161 => 0xB8,
            // VGM 1.70: Added Extra Header Offset at 0xBC-0xBF (192 bytes total)
            0x00000170 => 0xC0,
            // VGM 1.71: Added more chip clocks from 0xC0-0xE3 (228 bytes total)
            0x00000171 => 0xE4,
            // VGM 1.72: Added Mikey clock at 0xE4-0xE7 (232 bytes total)
            0x00000172 => 0xE8,
            // For unknown/future versions, use the maximum header size
            _ => VGM_MAX_HEADER_SIZE as usize,
        }
    }

    /// Compute an effective `data_offset` value to use when serializing or
    /// interpreting a VGM file.
    ///
    /// This applies the crate's fallback rules for the on-disk `data_offset`
    /// field:
    /// - If the stored `data_offset` is non-zero, that value is returned.
    /// - If the stored `data_offset` is zero, compute an effective offset by
    ///   subtracting the `DataOffset` field base (0x34) from the version's
    ///   legacy header size as returned by `fallback_header_size_for_version`.
    /// - For very old versions where the fallback header size is smaller than
    ///   the serialized `DataOffset` position, a safe minimum is chosen so
    ///   the command stream begins no earlier than 0x40 (64 bytes).
    ///
    /// # Arguments
    ///
    /// * `version` - VGM header `version` value (u32).
    /// * `data_offset` - stored `data_offset` field read from the header (may be 0).
    ///
    /// # Returns
    ///
    /// Effective `data_offset` value to use for computing header layout and
    /// command-start offsets (u32).
    pub fn data_offset(version: u32, data_offset: u32) -> u32 {
        // Mirrors the logic used elsewhere: when stored data_offset is 0,
        // compute an effective data_offset based on the legacy fallback header size.
        if data_offset == 0 {
            let version_header_size = VgmHeader::fallback_header_size_for_version(version);
            // For very old versions where the fallback header is smaller than the
            // serialized DataOffset position, choose a safe minimum so that the
            // resulting data start is at least 0x40 (64 bytes).
            if version_header_size < VgmHeaderField::DataOffset.offset() {
                // DataOffset + 0x0C = 0x40 (64 bytes)
                (0x40usize - VgmHeaderField::DataOffset.offset()) as u32
            } else {
                (version_header_size as u32)
                    .wrapping_sub(VgmHeaderField::DataOffset.offset() as u32)
            }
        } else {
            data_offset
        }
    }

    /// Compute the total serialized header size (in bytes) for the given
    /// version and stored `data_offset` value. This returns the number of
    /// bytes that should be treated as header in a serialized file.
    pub fn total_header_size(version: u32, data_offset: u32) -> usize {
        if data_offset == 0 {
            VgmHeader::fallback_header_size_for_version(version)
        } else {
            VgmHeaderField::DataOffset
                .offset()
                .wrapping_add(data_offset as usize)
        }
    }

    /// Compute the absolute byte offset (within a serialized file) where the
    /// command stream begins, given a header `version` and stored `data_offset`.
    /// This is equivalent to `DataOffset + data_offset`.
    pub fn command_start(version: u32, data_offset: u32) -> usize {
        Self::total_header_size(version, data_offset)
    }

    /// Parse a VGM header from a byte slice.
    ///
    /// This helper function parses a `VgmHeader` from the provided byte slice.
    /// If the slice is too short to contain a valid header, it returns a
    /// `ParseError` with detailed information about the missing bytes.
    ///
    /// # Arguments
    ///
    /// * `bytes` - The byte slice containing VGM header data
    ///
    /// # Returns
    ///
    /// * `Ok(VgmHeader)` - Successfully parsed header
    /// * `Err(ParseError)` - Parse error with details about what went wrong:
    ///   - `HeaderTooShort` - Buffer is smaller than minimum header size (0x34 bytes)
    ///   - `InvalidIdent` - Header doesn't start with "Vgm " identifier
    ///   - `OffsetOutOfRange` - Buffer is too small for the header size required by the version
    ///   - Other parse errors
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::VgmHeader;
    ///
    /// // Create a minimal VGM 1.00 header
    /// let mut data = vec![0u8; 0x34];
    /// data[0..4].copy_from_slice(b"Vgm ");  // Identifier
    /// data[0x08..0x0C].copy_from_slice(&0x00000100u32.to_le_bytes());  // Version 1.00
    ///
    /// match VgmHeader::from_bytes(&data) {
    ///     Ok(header) => assert_eq!(header.version, 0x00000100),
    ///     Err(e) => panic!("Parse error: {}", e),
    /// }
    ///
    /// // Too short buffer returns error with details
    /// let short_data = vec![0u8; 16];
    /// assert!(VgmHeader::from_bytes(&short_data).is_err());
    /// ```
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
        parse_vgm_header(bytes).map(|(h, _)| h)
    }
}

/// Attempt to convert a raw VGM byte slice into a `VgmHeader`.
impl TryFrom<&[u8]> for VgmHeader {
    type Error = ParseError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        VgmHeader::from_bytes(bytes)
    }
}

/// `ChipId` is used to represent the 1-byte chip id values stored in the
/// extra-header. It mirrors the values used by DAC stream chip identifiers but
/// also preserves unknown/extension values via `Unknown(u8)`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ChipId {
    Sn76489,
    Ym2413,
    Ym2612,
    Ym2151,
    SegaPcm,
    Rf5c68,
    Ym2203,
    Ym2608,
    Ym2610,
    Ym3812,
    Ym3526,
    Y8950,
    Ymf262,
    Ymf278b,
    Ymf271,
    Ymz280b,
    Rf5c164,
    Pwm,
    Ay8910,
    GbDmg,
    NesApu,
    MultiPcm,
    Upd7759,
    Okim6258,
    Okim6295,
    K051649,
    K054539,
    Huc6280,
    C140,
    K053260,
    Pokey,
    Qsound,
    Scsp,
    WonderSwan,
    Vsu,
    Saa1099,
    Es5503,
    Es5506,
    X1010,
    C352,
    Ga20,
    Mikey,
    /// Unknown or vendor-specific raw value
    Unknown(u8),
}

impl ChipId {
    /// Create a `ChipId` from a raw u8 as stored in VGM extra-header and DAC Stream.
    /// This number is clock-order in VGM header.
    pub fn from_u8(raw: u8) -> Self {
        let chip = raw & 0x7F;
        match chip {
            0x00 => ChipId::Sn76489,
            0x01 => ChipId::Ym2413,
            0x02 => ChipId::Ym2612,
            0x03 => ChipId::Ym2151,
            0x04 => ChipId::SegaPcm,
            0x05 => ChipId::Rf5c68,
            0x06 => ChipId::Ym2203,
            0x07 => ChipId::Ym2608,
            0x08 => ChipId::Ym2610,
            0x09 => ChipId::Ym3812,
            0x0A => ChipId::Ym3526,
            0x0B => ChipId::Y8950,
            0x0C => ChipId::Ymf262,
            0x0D => ChipId::Ymf278b,
            0x0E => ChipId::Ymf271,
            0x0F => ChipId::Ymz280b,
            0x10 => ChipId::Rf5c164,
            0x11 => ChipId::Pwm,
            0x12 => ChipId::Ay8910,
            0x13 => ChipId::GbDmg,
            0x14 => ChipId::NesApu,
            0x15 => ChipId::MultiPcm,
            0x16 => ChipId::Upd7759,
            0x17 => ChipId::Okim6258,
            0x18 => ChipId::Okim6295,
            0x19 => ChipId::K051649,
            0x1A => ChipId::K054539,
            0x1B => ChipId::Huc6280,
            0x1C => ChipId::C140,
            0x1D => ChipId::K053260,
            0x1E => ChipId::Pokey,
            0x1F => ChipId::Qsound,
            0x20 => ChipId::Scsp,
            0x21 => ChipId::WonderSwan,
            0x22 => ChipId::Vsu,
            0x23 => ChipId::Saa1099,
            0x24 => ChipId::Es5503,
            0x25 => ChipId::Es5506,
            0x26 => ChipId::X1010,
            0x27 => ChipId::C352,
            0x28 => ChipId::Ga20,
            0x29 => ChipId::Mikey,
            _other => ChipId::Unknown(raw),
        }
    }

    /// Convert the `ChipId` back to the raw `u8` value used on-disk.
    /// For `Unknown(u8)` the original raw value is returned.
    pub fn to_u8(&self) -> u8 {
        match self {
            ChipId::Sn76489 => 0x00,
            ChipId::Ym2413 => 0x01,
            ChipId::Ym2612 => 0x02,
            ChipId::Ym2151 => 0x03,
            ChipId::SegaPcm => 0x04,
            ChipId::Rf5c68 => 0x05,
            ChipId::Ym2203 => 0x06,
            ChipId::Ym2608 => 0x07,
            ChipId::Ym2610 => 0x08,
            ChipId::Ym3812 => 0x09,
            ChipId::Ym3526 => 0x0A,
            ChipId::Y8950 => 0x0B,
            ChipId::Ymf262 => 0x0C,
            ChipId::Ymf278b => 0x0D,
            ChipId::Ymf271 => 0x0E,
            ChipId::Ymz280b => 0x0F,
            ChipId::Rf5c164 => 0x10,
            ChipId::Pwm => 0x11,
            ChipId::Ay8910 => 0x12,
            ChipId::GbDmg => 0x13,
            ChipId::NesApu => 0x14,
            ChipId::MultiPcm => 0x15,
            ChipId::Upd7759 => 0x16,
            ChipId::Okim6258 => 0x17,
            ChipId::Okim6295 => 0x18,
            ChipId::K051649 => 0x19,
            ChipId::K054539 => 0x1A,
            ChipId::Huc6280 => 0x1B,
            ChipId::C140 => 0x1C,
            ChipId::K053260 => 0x1D,
            ChipId::Pokey => 0x1E,
            ChipId::Qsound => 0x1F,
            ChipId::Scsp => 0x20,
            ChipId::WonderSwan => 0x21,
            ChipId::Vsu => 0x22,
            ChipId::Saa1099 => 0x23,
            ChipId::Es5503 => 0x24,
            ChipId::Es5506 => 0x25,
            ChipId::X1010 => 0x26,
            ChipId::C352 => 0x27,
            ChipId::Ga20 => 0x28,
            ChipId::Mikey => 0x29,
            ChipId::Unknown(v) => *v,
        }
    }
}

impl From<u8> for ChipId {
    fn from(v: u8) -> Self {
        ChipId::from_u8(v)
    }
}

impl From<ChipId> for u8 {
    fn from(id: ChipId) -> Self {
        id.to_u8()
    }
}

/// Extra header introduced in VGM v1.70.
///
/// Format summary (see VGM specification):
/// - 32-bit LE header size (including this 4-byte size field)
/// - 32-bit LE offset to chip-clock block (relative to start of extra header, 0 = none)
/// - 32-bit LE offset to chip-volume block (relative to start of extra header, 0 = none)
/// - additional data follows at offsets above
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VgmExtraHeader {
    /// Full extra header size (as stored on-disk)
    pub header_size: u32,
    /// Offset (relative to start of extra header) to chip clock list (0 if absent)
    pub chip_clock_offset: u32,
    /// Offset (relative to start of extra header) to chip volume list (0 if absent)
    pub chip_vol_offset: u32,
    /// Parsed chip clock entries.
    pub chip_clocks: Vec<ChipClock>,
    /// Parsed chip volume entries.
    pub chip_volumes: Vec<ChipVolume>,
}

/// Representation of a chip clock entry in the extra header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipClock {
    /// Decoded chip id (known or Unknown(raw)).
    pub chip_id: ChipId,
    /// Decoded instance derived from the raw_chip_id (bit 7: secondary).
    pub instance: Instance,
    /// Clock value (Hz) as stored on-disk.
    pub clock: u32,
    /// Stored raw chip-id byte as read/written on-disk (preserves instance bit).
    raw_chip_id: u8,
}

impl ChipClock {
    pub fn new(chip_id: ChipId, instance: Instance, clock: u32) -> Self {
        let mut raw = chip_id.to_u8();
        if let Instance::Secondary = instance {
            raw |= 0x80;
        }
        ChipClock {
            chip_id,
            raw_chip_id: raw,
            instance,
            clock,
        }
    }

    pub fn from_raw(raw_chip_id: u8, clock: u32) -> Self {
        let instance = if (raw_chip_id & 0x80) != 0 {
            Instance::Secondary
        } else {
            Instance::Primary
        };
        ChipClock {
            chip_id: ChipId::from_u8(raw_chip_id),
            raw_chip_id,
            instance,
            clock,
        }
    }
}

/// Representation of a chip volume entry in the extra header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipVolume {
    /// Decoded chip id.
    pub chip_id: ChipId,
    /// True if the raw chip-id byte had bit 7 set indicating a paired chip's volume.
    pub paired_chip: bool,
    /// Decoded instance derived from raw_flags (bit 0: secondary).
    pub instance: Instance,
    /// Volume value
    pub volume: u16,
    /// Raw chip-id byte as read/written on-disk.
    raw_chip_id: u8,
    /// Raw flags byte as read/written on-disk (preserves vendor-specific bits).
    raw_flags: u8,
}

impl ChipVolume {
    pub fn new(chip_id: ChipId, instance: Instance, volume: u16) -> Self {
        let raw_chip = chip_id.to_u8();
        let raw_flags = if let Instance::Secondary = instance {
            0x01
        } else {
            0x00
        };
        ChipVolume {
            chip_id,
            raw_chip_id: raw_chip,
            paired_chip: false,
            raw_flags,
            instance,
            volume,
        }
    }

    /// Construct a `ChipVolume` that will be serialized with the paired bit
    /// (0x80) set in the chip-id byte.
    pub fn new_paired(chip_id: ChipId, instance: Instance, volume: u16) -> Self {
        let mut raw_chip = chip_id.to_u8();
        raw_chip |= 0x80;
        let raw_flags = if let Instance::Secondary = instance {
            0x01
        } else {
            0x00
        };
        ChipVolume {
            chip_id,
            raw_chip_id: raw_chip,
            paired_chip: true,
            raw_flags,
            instance,
            volume,
        }
    }

    pub fn from_raw(raw_chip_id: u8, raw_flags: u8, volume: u16) -> Self {
        let instance = if (raw_flags & 0x01) != 0 {
            Instance::Secondary
        } else {
            Instance::Primary
        };
        // If bit 7 is set, it's the volume for a paired chip.
        let paired = (raw_chip_id & 0x80) != 0;
        ChipVolume {
            chip_id: ChipId::from_u8(raw_chip_id & 0x7F),
            raw_chip_id,
            paired_chip: paired,
            raw_flags,
            instance,
            volume,
        }
    }
}

impl VgmExtraHeader {
    /// Interpret bit 0 of the on-disk flags byte as an `Instance`.
    /// (bit 0 = 0 => Primary, bit 0 = 1 => Secondary)
    pub fn instance_from_flags(flags: u8) -> Instance {
        if (flags & 0x01) != 0 {
            Instance::Secondary
        } else {
            Instance::Primary
        }
    }

    /// Encode an `Instance` into the on-disk flags byte (uses bit 0).
    pub fn flags_from_instance(instance: Instance) -> u8 {
        match instance {
            Instance::Primary => 0x00,
            Instance::Secondary => 0x01,
        }
    }

    /// Serialize the extra header into bytes using the VGM extra-header format.
    ///
    /// The serializer constructs a canonical extra-header buffer from parsed
    /// fields. Raw-preservation of the on-disk extra-header has been removed.
    pub fn to_bytes(&self) -> Vec<u8> {
        // If header_size is non-zero, respect it and emit only header_size
        // bytes consisting of the header_size (u32), chip_clock_offset (u32),
        // and chip_vol_offset (u32). Also attempt to place chip clock and chip
        // volume blocks at the stored offsets if they fit within the stored size.
        if self.header_size != 0 {
            let hsz = self.header_size as usize;
            // Ensure we return at least the 12 bytes needed for the three fields.
            let out_len = if hsz < 12 { 12 } else { hsz };
            let mut buf: Vec<u8> = vec![0u8; out_len];

            // header_size (4 bytes LE)
            let hsz_bytes = self.header_size.to_le_bytes();
            buf[0..4].copy_from_slice(&hsz_bytes);

            // chip_clock_offset (4 bytes LE)
            let cco_bytes = self.chip_clock_offset.to_le_bytes();
            buf[4..8].copy_from_slice(&cco_bytes);

            // chip_vol_offset (4 bytes LE)
            let cvo_bytes = self.chip_vol_offset.to_le_bytes();
            buf[8..12].copy_from_slice(&cvo_bytes);

            // Attempt to write chip_clock block at stored offset if present and fits.
            if self.chip_clock_offset != 0 && !self.chip_clocks.is_empty() {
                let start = self.chip_clock_offset as usize;
                // needed size: 1 + count*(1+4)
                let needed = 1usize + self.chip_clocks.len() * 5;
                if start + needed <= out_len {
                    buf[start] = self.chip_clocks.len() as u8;
                    let mut pos = start + 1;
                    for chip_clock in &self.chip_clocks {
                        // Use the preserved raw_chip_id for on-disk representation.
                        buf[pos] = chip_clock.raw_chip_id;
                        buf[pos + 1..pos + 5].copy_from_slice(&chip_clock.clock.to_le_bytes());
                        pos += 5;
                    }
                }
            }

            // Attempt to write chip_volume block at stored offset if present and fits.
            if self.chip_vol_offset != 0 && !self.chip_volumes.is_empty() {
                let start = self.chip_vol_offset as usize;
                // needed size: 1 + count*(1+1+2)
                let needed = 1usize + self.chip_volumes.len() * 4;
                if start + needed <= out_len {
                    buf[start] = self.chip_volumes.len() as u8;
                    let mut pos = start + 1;
                    for chip_vol in &self.chip_volumes {
                        // Use preserved raw bytes for on-disk representation.
                        buf[pos] = chip_vol.raw_chip_id;
                        buf[pos + 1] = chip_vol.raw_flags;
                        buf[pos + 2..pos + 4].copy_from_slice(&chip_vol.volume.to_le_bytes());
                        pos += 4;
                    }
                }
            }

            return buf;
        }

        // Start with a 12-byte placeholder for header_size, chip_clock_offset, chip_vol_offset
        let mut buf: Vec<u8> = vec![0u8; 12];

        // chip_clock block
        let chip_clock_offset: u32 = if !self.chip_clocks.is_empty() {
            let off = buf.len() as u32;
            // count (1 byte)
            buf.push(self.chip_clocks.len() as u8);
            // entries: chip_id (1 byte) + clock (4 bytes LE)
            for chip_clock in &self.chip_clocks {
                buf.push(chip_clock.raw_chip_id);
                buf.extend_from_slice(&chip_clock.clock.to_le_bytes());
            }
            off
        } else {
            0u32
        };

        // chip_volume block
        let chip_vol_offset: u32 = if !self.chip_volumes.is_empty() {
            let off = buf.len() as u32;
            // count (1 byte)
            buf.push(self.chip_volumes.len() as u8);
            // entries: chip_id (1 byte) + flags (1 byte) + volume (2 bytes LE)
            for chip_vol in &self.chip_volumes {
                buf.push(chip_vol.raw_chip_id);
                buf.push(chip_vol.raw_flags);
                buf.extend_from_slice(&chip_vol.volume.to_le_bytes());
            }
            off
        } else {
            0u32
        };

        // Now fill in the 3 header fields
        let header_size = buf.len() as u32;
        buf[0..4].copy_from_slice(&header_size.to_le_bytes());
        buf[4..8].copy_from_slice(&chip_clock_offset.to_le_bytes());
        buf[8..12].copy_from_slice(&chip_vol_offset.to_le_bytes());

        buf
    }
}
/// Miscellaneous derived/interpretation results for a parsed `VgmHeader`.
///
/// Fields are `Option<u32>` for clock-derivation hints and `Option<bool>` for
/// presence/variant hints. For the `use_ym2413_clock_for_*` fields:
/// - `None` = interpretation not applicable or explicitly determined to not apply
///   (do not use YM2413).
/// - `Some(x)` where `x != 0` = `x` is the YM2413 clock value that should be
///   used as the derived clock for the corresponding chip.
///
/// These entries capture cases where on-disk header fields are overloaded (a single
/// stored clock/flag bit can change the effective meaning of another header field).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VgmHeaderMisc {
    /// True when the header indicates the T6W28 PSG variant (Neo Geo Pocket).
    pub t6w28_detected: Option<bool>,
    /// For old VGM versions (<= 1.01): when applicable, returns the YM2413
    /// clock value that should be used for YM2612. `None` = not applicable or
    /// undetermined, `Some(0)` = explicitly determined to not apply,
    /// `Some(x)` where `x != 0` = use `x` as the derived clock.
    pub use_ym2413_clock_for_ym2612: Option<u32>,
    /// For old VGM versions (<= 1.01): when applicable, returns the YM2413
    /// clock value that should be used for YM2151. Same `Option<u32>`
    /// semantics as `use_ym2413_clock_for_ym2612`.
    pub use_ym2413_clock_for_ym2151: Option<u32>,
    /// Whether the YM2610 is the 'B' variant (bit 31 convention).
    pub ym2610b_detected: Option<bool>,
    /// Whether the FDS sound addon is present/enabled (bit 31 convention).
    pub fds_detected: Option<bool>,
    /// Whether the chip is ES5506 (bit 31 set) or ES5505 (bit 31 clear).
    /// `Some(true)` means ES5506, `Some(false)` means ES5505.
    pub is_es5506: Option<bool>,
}

impl VgmHeader {
    /// Produce a `VgmHeaderMisc` by deriving interpretation hints from the raw header.
    ///
    /// This is intentionally conservative: fields are `None` when the header
    /// doesn't provide information for the given interpretation. Where the
    /// header encodes a variant via the high bit (0x8000_0000) of a stored
    /// clock field, this method exposes that as `Some(true/false)`.
    pub fn misc(&self) -> VgmHeaderMisc {
        let mut misc = VgmHeaderMisc::default();

        // Heuristics for older VGM versions where YM2413 clock may carry YM2612/YM2151 info.
        if self.version <= 0x00000101 && self.ym2413_clock != 0 {
            // If YM2612 is not explicitly present and the YM2413 clock is
            // large (strictly greater than 5_000_000 Hz = 5 MHz), some
            // real-world files used the YM2413's clock value for YM2612.
            if self.ym2612_clock == 0 && self.ym2413_clock > 5_000_000 {
                // Use the YM2413 clock value for YM2612. Mask off the high bit
                // here: substitutions should not carry the secondary-instance
                // high-bit. misc() returns a raw clock value with the high bit
                // cleared.
                misc.use_ym2413_clock_for_ym2612 = Some(self.ym2413_clock & 0x7FFF_FFFF);
            } else if self.ym2612_clock != 0 {
                // Explicitly present: do not substitute YM2413 (represented as `None`).
                misc.use_ym2413_clock_for_ym2612 = None;
            }
            // Similarly for YM2151: some older files used the YM2413 clock
            // for YM2151 when the YM2413 clock is small (strictly less than
            // 5_000_000 Hz = 5 MHz).
            if self.ym2151_clock == 0 && self.ym2413_clock < 5_000_000 {
                // Use the YM2413 clock value for YM2151. Mask off the high bit
                // here so the substitution is a plain clock value without the
                // secondary-instance indicator.
                misc.use_ym2413_clock_for_ym2151 = Some(self.ym2413_clock & 0x7FFF_FFFF);
            } else if self.ym2151_clock != 0 {
                // Explicitly present: do not substitute YM2413 (represented as `None`).
                misc.use_ym2413_clock_for_ym2151 = None;
            }
        }

        // YM2610B: bit 31 on the stored ym2610b_clock indicates B variant when present.
        if self.ym2610b_clock != 0 {
            misc.ym2610b_detected = Some((self.ym2610b_clock & 0x8000_0000) != 0);
        }

        // FDS addon: encoded in the NES APU clock high bit in some files.
        if self.nes_apu_clock != 0 {
            misc.fds_detected = Some((self.nes_apu_clock & 0x8000_0000) != 0);
        }

        // ES5506 vs ES5505: check ES5503 clock's high bit when present in header.
        if self.es5503_clock != 0 {
            misc.is_es5506 = Some((self.es5503_clock & 0x8000_0000) != 0);
        }

        // T6W28 detection: the SN76489 clock's high bit (0x8000_0000) is used as
        // a dual-chip indicator for the T6W28 PSG variant. When the SN76489 clock
        // field is present, expose the high-bit as the detected flag.
        if self.sn76489_clock != 0 {
            misc.t6w28_detected = Some((self.sn76489_clock & 0x8000_0000) != 0);
        }

        misc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chipvolume_from_raw_decodes_paired_and_instance() {
        // Use Ym2203 (0x06) with paired bit set and flags indicating secondary.
        let raw_chip = 0x80 | 0x06;
        let raw_flags = 0x01; // bit0 = secondary
        let volume: u16 = 1234;

        let cv = ChipVolume::from_raw(raw_chip, raw_flags, volume);

        // Paired bit should be detected.
        assert!(cv.paired_chip);
        // Instance should be decoded from flags (bit 0).
        assert_eq!(cv.instance, Instance::Secondary);
        // Decoded chip id should ignore the paired bit.
        assert_eq!(cv.chip_id, ChipId::Ym2203);
        // Raw fields should be preserved for round-trip.
        assert_eq!(cv.raw_chip_id, raw_chip);
        assert_eq!(cv.raw_flags, raw_flags);
        assert_eq!(cv.volume, volume);
    }

    #[test]
    fn test_chipvolume_new_paired_sets_raw_bit() {
        // Construct a paired ChipVolume via helper and verify raw byte has bit 7.
        let cv = ChipVolume::new_paired(ChipId::Ay8910, Instance::Primary, 500u16);

        assert!(cv.paired_chip);
        assert_eq!(cv.chip_id, ChipId::Ay8910);
        assert_eq!(cv.instance, Instance::Primary);
        // Raw chip id should include the paired bit.
        assert_eq!(cv.raw_chip_id & 0x80, 0x80);
    }

    #[test]
    fn test_chipclock_new_and_from_raw_instance_roundtrip() {
        // Secondary instance should set bit 7 on raw_chip_id in ChipClock::new
        let cc = ChipClock::new(ChipId::Ym2612, Instance::Secondary, 44100u32);

        assert_eq!(cc.instance, Instance::Secondary);
        assert_eq!(cc.chip_id, ChipId::Ym2612);
        assert_eq!(cc.raw_chip_id & 0x80, 0x80);

        // Now decode from raw and ensure we get the same semantic fields.
        let decoded = ChipClock::from_raw(cc.raw_chip_id, cc.clock);
        assert_eq!(decoded.instance, Instance::Secondary);
        assert_eq!(decoded.chip_id, ChipId::Ym2612);
        assert_eq!(decoded.clock, 44100u32);
    }

    #[test]
    fn test_misc_ym2413_to_ym2612_and_ym2151() {
        // YM2413 -> YM2612 (legacy behavior, threshold > 5_000_000)
        let mut h = VgmHeader {
            version: 0x00000101,
            ym2413_clock: 6_000_000,
            ym2612_clock: 0,
            ..Default::default()
        };
        let misc1 = h.misc();
        assert_eq!(misc1.use_ym2413_clock_for_ym2612, Some(6_000_000u32));

        // If YM2612 clock is explicitly present, do not substitute YM2413 (None).
        h.ym2612_clock = 7_000_000;
        let misc2 = h.misc();
        assert_eq!(misc2.use_ym2413_clock_for_ym2612, None);

        // YM2413 -> YM2151 (legacy behavior, threshold < 5_000_000)
        h.ym2413_clock = 4_000_000;
        h.ym2151_clock = 0;
        h.ym2612_clock = 0; // reset to not interfere
        let misc3 = h.misc();
        assert_eq!(misc3.use_ym2413_clock_for_ym2151, Some(4_000_000u32));

        // If YM2151 is explicitly present, do not substitute YM2413 (None).
        h.ym2151_clock = 3_000_000;
        let misc4 = h.misc();
        assert_eq!(misc4.use_ym2413_clock_for_ym2151, None);
    }

    #[test]
    fn test_misc_variant_bits_detection() {
        let mut h = VgmHeader {
            ..Default::default()
        };

        // T6W28 detection: SN76489 clock high bit indicates variant when present.
        h.sn76489_clock = 0x8000_0000;
        let misc = h.misc();
        assert_eq!(misc.t6w28_detected, Some(true));

        // YM2610B detection via ym2610b_clock high bit.
        h.ym2610b_clock = 0x8000_0001;
        let misc = h.misc();
        assert_eq!(misc.ym2610b_detected, Some(true));

        // FDS addon detection via nes_apu_clock high bit.
        h.nes_apu_clock = 0x8000_0002;
        let misc = h.misc();
        assert_eq!(misc.fds_detected, Some(true));

        // ES5506 detection via es5503_clock high bit.
        h.es5503_clock = 0x8000_0003;
        let misc = h.misc();
        assert_eq!(misc.is_es5506, Some(true));
    }
}
