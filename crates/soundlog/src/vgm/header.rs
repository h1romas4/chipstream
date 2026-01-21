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
//!
use crate::binutil::{write_slice, write_u8, write_u16, write_u32};
use crate::chip;
use crate::vgm::command::Instance;
use std::convert::TryFrom;

pub(crate) const VGM_MAX_HEADER_SIZE: u32 = 0x100;

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
    SnFb,
    Snw,
    Sf,
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
    AyMisc,
    GbDmgClock,
    NesApuClock,
    MultipcmClock,
    Upd7759Clock,
    Okim6258Clock,
    Okim6258Flags,
    Okim6295Clock,
    K051649Clock,
    K054539Clock,
    Huc6280Clock,
    C140Clock,
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
    Es5506Channels,
    Es5506Cd,
    Es5506Reserved,
    X1_010,
    C352,
    Ga20,
    Mikey,
    ReservedE8EF,
    ReservedF0FF,
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
            VgmHeaderField::SnFb => 0x28,
            VgmHeaderField::Snw => 0x2A,
            VgmHeaderField::Sf => 0x2B,
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
            VgmHeaderField::AyMisc => 0x78,
            VgmHeaderField::GbDmgClock => 0x80,
            VgmHeaderField::NesApuClock => 0x84,
            VgmHeaderField::MultipcmClock => 0x88,
            VgmHeaderField::Upd7759Clock => 0x8C,
            VgmHeaderField::Okim6258Clock => 0x90,
            VgmHeaderField::Okim6258Flags => 0x94,
            VgmHeaderField::Okim6295Clock => 0x98,
            VgmHeaderField::K051649Clock => 0x9C,
            VgmHeaderField::K054539Clock => 0xA0,
            VgmHeaderField::Huc6280Clock => 0xA4,
            VgmHeaderField::C140Clock => 0xA8,
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
            VgmHeaderField::Es5506Channels => 0xD4,
            VgmHeaderField::Es5506Cd => 0xD6,
            VgmHeaderField::Es5506Reserved => 0xD7,
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
            VgmHeaderField::SnFb => 2,
            VgmHeaderField::Snw => 1,
            VgmHeaderField::Sf => 1,
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
            VgmHeaderField::AyMisc => 8,
            VgmHeaderField::GbDmgClock => 4,
            VgmHeaderField::NesApuClock => 4,
            VgmHeaderField::MultipcmClock => 4,
            VgmHeaderField::Upd7759Clock => 4,
            VgmHeaderField::Okim6258Clock => 4,
            VgmHeaderField::Okim6258Flags => 4,
            VgmHeaderField::Okim6295Clock => 4,
            VgmHeaderField::K051649Clock => 4,
            VgmHeaderField::K054539Clock => 4,
            VgmHeaderField::Huc6280Clock => 4,
            VgmHeaderField::C140Clock => 4,
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
            VgmHeaderField::Es5506Channels => 2,
            VgmHeaderField::Es5506Cd => 1,
            VgmHeaderField::Es5506Reserved => 1,
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
    pub sn_fb: u16,
    pub snw: u8,
    pub sf: u8,
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
    pub ay_misc: [u8; 8],
    pub gb_dmg_clock: u32,
    pub nes_apu_clock: u32,
    pub multipcm_clock: u32,
    pub upd7759_clock: u32,
    pub okim6258_clock: u32,
    pub okim6258_flags: [u8; 4],
    pub okim6295_clock: u32,
    pub k051649_clock: u32,
    pub k054539_clock: u32,
    pub huc6280_clock: u32,
    pub c140_clock: u32,
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
    pub es5506_channels: u16,
    pub es5506_cd: u8,
    pub es5506_reserved: u8,
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
            sn_fb: 0,
            snw: 0,
            sf: 0,
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
            ay_misc: [0u8; 8],
            gb_dmg_clock: 0,
            nes_apu_clock: 0,
            multipcm_clock: 0,
            upd7759_clock: 0,
            okim6258_clock: 0,
            okim6258_flags: [0u8; 4],
            okim6295_clock: 0,
            k051649_clock: 0,
            k054539_clock: 0,
            huc6280_clock: 0,
            c140_clock: 0,
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
            es5506_channels: 0,
            es5506_cd: 0,
            es5506_reserved: 0,
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
        // If an EOF offset was already set in the header, emit it here so callers
        // (e.g. higher-level serialization) can choose to respect the stored value.
        // Otherwise emit 0 and allow the document-level serializer to compute/update it.
        if self.eof_offset != 0 {
            write_u32(
                &mut buf,
                VgmHeaderField::EofOffset.offset(),
                self.eof_offset,
            );
        } else {
            write_u32(&mut buf, VgmHeaderField::EofOffset.offset(), 0);
        }
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
        // SN FB (0x28) u16
        write_u16(&mut buf, VgmHeaderField::SnFb.offset(), self.sn_fb);
        // SNW (0x2A) u8
        write_u8(&mut buf, VgmHeaderField::Snw.offset(), self.snw);
        // SF (0x2B) u8
        write_u8(&mut buf, VgmHeaderField::Sf.offset(), self.sf);
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
        // data offset (0x34)
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
        // AY misc (0x78..0x7F)
        write_slice(&mut buf, VgmHeaderField::AyMisc.offset(), &self.ay_misc);
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
        // OKIM6258 flags (0x94..0x97)
        write_slice(
            &mut buf,
            VgmHeaderField::Okim6258Flags.offset(),
            &self.okim6258_flags,
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
        write_u16(
            &mut buf,
            VgmHeaderField::Es5506Channels.offset(),
            self.es5506_channels,
        );
        write_u8(&mut buf, VgmHeaderField::Es5506Cd.offset(), self.es5506_cd);
        write_u8(
            &mut buf,
            VgmHeaderField::Es5506Reserved.offset(),
            self.es5506_reserved,
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
    /// Scans the header clock fields and returns a `Vec` of tuples
    /// `(Instance, chip::Chip)` for each clock that is non-zero. The
    /// high bit (0x8000_0000) on stored clock values indicates a
    /// secondary instance per VGM convention.
    pub fn chip_instances(&self) -> Vec<(Instance, chip::Chip)> {
        let mut out: Vec<(Instance, chip::Chip)> = Vec::new();

        let mut push = |raw_clock: u32, ch: chip::Chip| {
            if raw_clock != 0 {
                let inst = if (raw_clock & 0x8000_0000_u32) != 0 {
                    Instance::Secondary
                } else {
                    Instance::Primary
                };
                out.push((inst, ch));
            }
        };

        push(self.sn76489_clock, chip::Chip::Sn76489);
        push(self.ym2413_clock, chip::Chip::Ym2413);
        push(self.ym2612_clock, chip::Chip::Ym2612);
        push(self.ym2151_clock, chip::Chip::Ym2151);
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

        out
    }

    /// Return the legacy header size to use when a stored `data_offset` is 0
    /// (older VGM versions omitted the data_offset field or used smaller
    /// headers). The returned size is the total header size in bytes and
    /// includes the 32-bit `data_offset` field region when applicable.
    pub(crate) fn fallback_header_size_for_version(version: u32) -> usize {
        match version {
            0x00000100 => 0x20 + 4,
            0x00000101 => 0x24 + 4,
            0x00000110 => 0x30 + 4,
            0x00000150 => 0x34 + 4,
            0x00000151 | 0x00000160 => 0x7f + 4,
            0x00000170 => 0xbc + 4,
            0x00000171 => 0xe0 + 4,
            0x00000172 => 0xe4 + 4,
            _ => VGM_MAX_HEADER_SIZE as usize,
        }
    }
}

/// Attempt to convert a raw VGM byte slice into a `VgmHeader`.
impl TryFrom<&[u8]> for VgmHeader {
    type Error = crate::binutil::ParseError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        crate::vgm::parser::parse_vgm_header(bytes).map(|(h, _)| h)
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

    /// Parsed chip clock entries: (chip_id, clock)
    /// chip_id is the 1-byte ID following the main header's chip order.
    pub chip_clocks: Vec<(u8, u32)>,

    /// Parsed chip volume entries: (chip_id_with_flags, flags, volume)
    /// Each entry in the on-disk format is: 1 byte chip id, 1 byte flags, 2 bytes volume (LE).
    pub chip_volumes: Vec<(u8, u8, u16)>,
}

impl VgmExtraHeader {
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
                    for (chip_id, clock) in &self.chip_clocks {
                        buf[pos] = *chip_id;
                        buf[pos + 1..pos + 5].copy_from_slice(&clock.to_le_bytes());
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
                    for (chip_id, flags, volume) in &self.chip_volumes {
                        buf[pos] = *chip_id;
                        buf[pos + 1] = *flags;
                        buf[pos + 2..pos + 4].copy_from_slice(&volume.to_le_bytes());
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
            for (chip_id, clock) in &self.chip_clocks {
                buf.push(*chip_id);
                buf.extend_from_slice(&clock.to_le_bytes());
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
            for (chip_id, flags, volume) in &self.chip_volumes {
                buf.push(*chip_id);
                buf.push(*flags);
                buf.extend_from_slice(&volume.to_le_bytes());
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
