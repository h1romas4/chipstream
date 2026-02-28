//! VGM parser utilities
//!
//! This module provides functions to parse VGM files (versioned VGM
//! headers, command streams, and optional GD3 metadata) into the
//! crate-internal data structures used by the `soundlog` crate.
//!
//! Reference: <https://vgmrips.net/wiki/VGM_Specification>
//!
//! Public (crate-visible) entry points:
//! - `parse_vgm(bytes)` — parse an entire VGM file into a `VgmDocument`.
//! - `parse_vgm_header(bytes)` — parse only the VGM header and return
//!   the header plus the header size in bytes.
//! - `parse_vgm_extra_header(bytes, offset)` — parse the v1.70+ extra
//!   header located at `offset`.
//! - `parse_vgm_command(bytes, off)`, `parse_chip_write(...)`,
//!   `parse_reserved_write(...)` — command-level parsers used while
//!   iterating the command stream.
//!
//! The parser performs strict validation and returns `ParseError` for
//! invalid input (short buffers, invalid identifiers, out-of-range
//! offsets, unknown opcodes, etc.). There are also debug-only helper
//! functions (marked `#[doc(hidden)]`) that produce partial command
//! traces for diagnostics.
//!
//! Notes:
//! - Offsets stored in VGM headers are interpreted with the same
//!   fallbacks used by the crate's writer (e.g. a `data_offset` of `0`
//!   uses the legacy header size fallback).
//! - GD3 metadata, when present, is parsed via `crate::meta::parse_gd3`.
//!   GD3 parsing errors are propagated to the caller when parsing the
//!   full document.
use crate::binutil::{ParseError, read_slice, read_u8_at, read_u16_le_at, read_u32_le_at};
use crate::chip;
use crate::meta::parse_gd3;
use crate::vgm::command::{
    Ay8910StereoMask, CommandSpec, DataBlock, EndOfData, Instance, PcmRamWrite, ReservedU8,
    ReservedU16, ReservedU24, ReservedU32, SeekOffset, SetStreamData, SetStreamFrequency,
    SetupStreamControl, StartStream, StartStreamFastCall, StopStream, UnknownSpec, VgmCommand,
    Wait735Samples, Wait882Samples, WaitNSample, WaitSamples, Ym2612Port0Address2AWriteAndWaitN,
};
use crate::vgm::document::VgmDocument;
use crate::vgm::header::{VgmExtraHeader, VgmHeader, VgmHeaderField};

/// Parse a complete VGM file from a byte slice into a `VgmDocument`.
///
/// High-level parsing steps:
/// 1. Parse the VGM header with `parse_vgm_header`, which returns the
///    parsed `VgmHeader` and the header size in bytes.
/// 2. Iterate commands starting immediately after the header and decode
///    each command using `parse_vgm_command`. Each command parse returns
///    a `(VgmCommand, consumed_bytes)` pair; consumed bytes include the
///    opcode and payload.
/// 3. If the header declares a non-zero `gd3_offset`, attempt to parse
///    the GD3 metadata using `crate::meta::parse_gd3` and attach it to
///    the resulting `VgmDocument::gd3` field. GD3 parsing errors are
///    ignored here (the document will contain `None` on failure).
///
/// Returns `Ok(VgmDocument)` on success or a `ParseError` if header or
/// any command parsing fails.
pub(crate) fn parse_vgm(bytes: &[u8]) -> Result<VgmDocument, ParseError> {
    let (header, mut off) = parse_vgm_header(bytes)?;

    let mut commands: Vec<VgmCommand> = Vec::new();

    let gd3_start_opt =
        (header.gd3_offset != 0).then(|| header.gd3_offset.wrapping_add(0x14) as usize);

    while off < bytes.len() {
        if let Some(gd3_start) = gd3_start_opt
            && off >= gd3_start
        {
            break;
        }

        let (cmd, cons) = parse_vgm_command(bytes, off)?;
        commands.push(cmd.clone());
        off = off.wrapping_add(cons);

        if let VgmCommand::EndOfData(_) = commands.last().unwrap() {
            break;
        }
    }

    // Attach GD3 metadata if present (gd3_offset is stored as gd3_start - 0x14).
    let gd3 = if header.gd3_offset != 0 {
        let gd3_start = header.gd3_offset.wrapping_add(0x14) as usize;
        // If the computed start is outside the buffer, treat it as an out-of-range offset.
        if gd3_start >= bytes.len() {
            return Err(ParseError::OffsetOutOfRange {
                offset: gd3_start,
                needed: 1,
                available: bytes.len(),
                context: Some("gd3_start".into()),
            });
        }
        // Attempt to parse GD3 and propagate any parse error to the caller.
        match parse_gd3(&bytes[gd3_start..]) {
            Ok(g) => Some(g),
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    // Attach extra header if present (extra_header_offset stored at 0xBC in main header).
    let extra_header = if header.extra_header_offset != 0 {
        let start = header.extra_header_offset.wrapping_add(0xBC) as usize;
        // If the computed start is outside the buffer, treat it as an out-of-range offset.
        if start >= bytes.len() {
            return Err(ParseError::OffsetOutOfRange {
                offset: start,
                needed: 1,
                available: bytes.len(),
                context: Some("extra_header_start".into()),
            });
        }
        // Parse the extra header and propagate any parse error to the caller.
        match parse_vgm_extra_header(bytes, start) {
            Ok((eh, _hsz)) => {
                // Parse extra-header normally; do not preserve raw bytes.
                // No need to compute the clamped end here.
                Some(eh)
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };

    Ok(VgmDocument {
        header,
        commands,
        gd3,
        extra_header,
    })
}

/// Parse a VGM header located at the start of `bytes`.
///
/// This performs strict validation of the header: verifies the 4-byte
/// ident (`"Vgm "`), reads the version and the `data_offset` field,
/// and uses the legacy fallback when `data_offset` is zero
/// (interpreted as `VGM_V171_HEADER_SIZE - 0x34`). The full header
/// size is computed as `0x34 + data_offset`. The function ensures that
/// the provided slice contains the complete header before reading
/// extended fields.
///
/// On success returns `(VgmHeader, header_size)`, where `header_size`
/// is the number of bytes consumed by the header. On failure returns a
/// `ParseError` (for example `HeaderTooShort`, `InvalidIdent`, or
/// `UnexpectedEof`).
pub(crate) fn parse_vgm_header(bytes: &[u8]) -> Result<(VgmHeader, usize), ParseError> {
    if bytes.len() < 0x34 {
        return Err(ParseError::HeaderTooShort("vgm: base header (0x34)".into()));
    }

    let ident_slice = read_slice(bytes, 0x00, 4)?;
    if ident_slice != b"Vgm " {
        let mut id: [u8; 4] = [0; 4];
        id.copy_from_slice(ident_slice);
        return Err(ParseError::InvalidIdent(id));
    }

    let version = read_u32_le_at(bytes, 0x08)?;

    // For VGM < 1.50, the data_offset field was not defined (it was added in 1.50).
    // Only read it if version >= 1.50, otherwise it may not exist in the file.
    let data_offset = if version >= 0x00000150 {
        read_u32_le_at(bytes, 0x34)?
    } else {
        0
    };

    // Compute actual data start position based on version and data_offset
    let actual_data_start: usize = if version < 0x00000150 {
        // VGM < 1.50: data_offset field doesn't exist, use fallback
        VgmHeader::fallback_header_size_for_version(version)
    } else if data_offset == 0 {
        // VGM 1.50+: data_offset is 0, use fallback
        VgmHeader::fallback_header_size_for_version(version)
    } else {
        // VGM 1.50+: data_offset is non-zero, use it
        0x34usize.wrapping_add(data_offset as usize)
    };

    // Determine the maximum header size allowed for this version.
    // This prevents reading fields that were not defined in this version.
    let version_max_header_size = VgmHeader::fallback_header_size_for_version(version);

    // VGM 1.50+ specification: "All header sizes are valid for all versions
    // from 1.50 on, as long as header has at least 64 bytes. If the VGM data
    // starts at an offset that is lower than 0x100, all overlapping header
    // bytes have to be handled as they were zero."
    //
    // This means: for version 1.50+, if data starts before 0x100, we must
    // limit the header size to the actual data start position to avoid reading
    // data bytes as header fields. Fields beyond this point are treated as zero.
    //
    // However, we must never read main header fields that were not defined in this version.
    // For example, VGM 1.70 files should NOT read VGM 1.71 main header fields.
    //
    // Note: We need two separate sizes:
    // 1. header_size_for_fields: Used to determine which main header fields to read
    //    (limited to version-defined maximum)
    // 2. total_header_size: Used to locate extra header and data start
    //    (based on data_offset, may include extra header)

    // Limit header_size_for_fields to actual_data_start to prevent reading
    // VGM command data as header fields when data_offset is small
    let header_size_for_fields: usize = actual_data_start.min(version_max_header_size);

    let total_header_size: usize = if version >= 0x00000150 {
        // Use actual_data_start directly - this is where the command stream begins
        actual_data_start
    } else {
        // For version < 1.50, use version-defined fallback
        version_max_header_size
    };

    // Derive a usable data_offset value for subsequent calculations. When the
    // stored data_offset is zero we compute an effective offset from the
    // chosen fallback header size.
    if data_offset == 0 {
        version_max_header_size.wrapping_sub(0x34)
    } else {
        data_offset as usize
    };

    if bytes.len() < total_header_size {
        return Err(ParseError::OffsetOutOfRange {
            offset: total_header_size,
            needed: total_header_size,
            available: bytes.len(),
            context: Some("header_size".into()),
        });
    }

    let mut h = VgmHeader::default();

    // Core fields always present
    h.ident.copy_from_slice(&bytes[0x00..0x04]);
    h.eof_offset = read_u32_le_at(bytes, 0x04)?;
    h.version = version;
    h.sn76489_clock = read_u32_le_at(bytes, 0x0C)?;
    h.ym2413_clock = read_u32_le_at(bytes, 0x10)?;
    h.gd3_offset = read_u32_le_at(bytes, 0x14)?;
    h.total_samples = read_u32_le_at(bytes, 0x18)?;
    h.loop_offset = read_u32_le_at(bytes, 0x1C)?;
    h.loop_samples = read_u32_le_at(bytes, 0x20)?;
    h.sample_rate = read_u32_le_at(bytes, 0x24)?;
    h.sn76489_feedback = read_u16_le_at(bytes, 0x28)?;
    h.sn76489_shift_register_width = read_u8_at(bytes, 0x2A)?;
    h.sn76489_flags = read_u8_at(bytes, 0x2B)?;
    h.ym2612_clock = read_u32_le_at(bytes, 0x2C)?;
    h.ym2151_clock = read_u32_le_at(bytes, 0x30)?;
    h.data_offset = data_offset;
    // Following fields are part of the extended header region.
    // For VGM 1.50+: All header sizes are valid as long as header has at least
    // 64 bytes. Fields are available if they fit within header_size.
    // For VGM < 1.50: Fields are only available if both:
    //   1. The field was defined in that version (version check)
    //   2. The field fits within header_size (space check)
    //
    // The `should_read` check implements the VGM spec requirement for reading
    // fields based on version and available space. Fields that don't meet the
    // criteria are treated as zero.
    let should_read = |field: VgmHeaderField| -> bool {
        let off = field.offset();
        let sz = field.len();
        let min_ver = field.min_version();
        let has_space = header_size_for_fields >= off + sz;
        if version >= 0x00000150 {
            // VGM 1.50+: Read if space is available (limited to version-defined fields)
            has_space
        } else {
            // VGM < 1.50: Read only if version supports it AND space is available
            has_space && version >= min_ver
        }
    };

    h.sega_pcm_clock = if should_read(VgmHeaderField::SegaPcmClock) {
        read_u32_le_at(bytes, VgmHeaderField::SegaPcmClock.offset())?
    } else {
        0
    };
    h.spcm_interface = if should_read(VgmHeaderField::SpcmInterface) {
        read_u32_le_at(bytes, VgmHeaderField::SpcmInterface.offset())?
    } else {
        0
    };
    h.rf5c68_clock = if should_read(VgmHeaderField::Rf5c68Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Rf5c68Clock.offset())?
    } else {
        0
    };
    h.ym2203_clock = if should_read(VgmHeaderField::Ym2203Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ym2203Clock.offset())?
    } else {
        0
    };
    h.ym2608_clock = if should_read(VgmHeaderField::Ym2608Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ym2608Clock.offset())?
    } else {
        0
    };
    h.ym2610b_clock = if should_read(VgmHeaderField::Ym2610bClock) {
        read_u32_le_at(bytes, VgmHeaderField::Ym2610bClock.offset())?
    } else {
        0
    };
    h.ym3812_clock = if should_read(VgmHeaderField::Ym3812Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ym3812Clock.offset())?
    } else {
        0
    };
    h.ym3526_clock = if should_read(VgmHeaderField::Ym3526Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ym3526Clock.offset())?
    } else {
        0
    };
    h.y8950_clock = if should_read(VgmHeaderField::Y8950Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Y8950Clock.offset())?
    } else {
        0
    };
    h.ymf262_clock = if should_read(VgmHeaderField::Ymf262Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ymf262Clock.offset())?
    } else {
        0
    };
    h.ymf278b_clock = if should_read(VgmHeaderField::Ymf278bClock) {
        read_u32_le_at(bytes, VgmHeaderField::Ymf278bClock.offset())?
    } else {
        0
    };
    h.ymf271_clock = if should_read(VgmHeaderField::Ymf271Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ymf271Clock.offset())?
    } else {
        0
    };
    h.ymz280b_clock = if should_read(VgmHeaderField::Ymz280bClock) {
        read_u32_le_at(bytes, VgmHeaderField::Ymz280bClock.offset())?
    } else {
        0
    };
    h.rf5c164_clock = if should_read(VgmHeaderField::Rf5c164Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Rf5c164Clock.offset())?
    } else {
        0
    };
    h.pwm_clock = if should_read(VgmHeaderField::PwmClock) {
        read_u32_le_at(bytes, VgmHeaderField::PwmClock.offset())?
    } else {
        0
    };
    h.ay8910_clock = if should_read(VgmHeaderField::Ay8910Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Ay8910Clock.offset())?
    } else {
        0
    };
    h.ay_chip_type = if should_read(VgmHeaderField::AyChipType) {
        read_u8_at(bytes, VgmHeaderField::AyChipType.offset())?
    } else {
        0
    };
    h.ay8910_flags = if should_read(VgmHeaderField::Ay8910Flags) {
        read_u8_at(bytes, VgmHeaderField::Ay8910Flags.offset())?
    } else {
        0
    };
    h.ym2203_ay8910_flags = if should_read(VgmHeaderField::Ym2203Ay8910Flags) {
        read_u8_at(bytes, VgmHeaderField::Ym2203Ay8910Flags.offset())?
    } else {
        0
    };
    h.ym2608_ay8910_flags = if should_read(VgmHeaderField::Ym2608Ay8910Flags) {
        read_u8_at(bytes, VgmHeaderField::Ym2608Ay8910Flags.offset())?
    } else {
        0
    };
    h.volume_modifier = if should_read(VgmHeaderField::VolumeModifier) {
        read_u8_at(bytes, VgmHeaderField::VolumeModifier.offset())?
    } else {
        0
    };
    h.reserved_7d = if should_read(VgmHeaderField::Reserved7D) {
        read_u8_at(bytes, VgmHeaderField::Reserved7D.offset())?
    } else {
        0
    };
    h.loop_base = if should_read(VgmHeaderField::LoopBase) {
        read_u8_at(bytes, VgmHeaderField::LoopBase.offset())?
    } else {
        0
    };
    h.loop_modifier = if should_read(VgmHeaderField::LoopModifier) {
        read_u8_at(bytes, VgmHeaderField::LoopModifier.offset())?
    } else {
        0
    };
    h.gb_dmg_clock = if should_read(VgmHeaderField::GbDmgClock) {
        read_u32_le_at(bytes, VgmHeaderField::GbDmgClock.offset())?
    } else {
        0
    };
    h.nes_apu_clock = if should_read(VgmHeaderField::NesApuClock) {
        read_u32_le_at(bytes, VgmHeaderField::NesApuClock.offset())?
    } else {
        0
    };
    h.multipcm_clock = if should_read(VgmHeaderField::MultipcmClock) {
        read_u32_le_at(bytes, VgmHeaderField::MultipcmClock.offset())?
    } else {
        0
    };
    h.upd7759_clock = if should_read(VgmHeaderField::Upd7759Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Upd7759Clock.offset())?
    } else {
        0
    };
    h.okim6258_clock = if should_read(VgmHeaderField::Okim6258Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Okim6258Clock.offset())?
    } else {
        0
    };
    h.okim6258_flags = if should_read(VgmHeaderField::Okim6258Flags) {
        read_u8_at(bytes, VgmHeaderField::Okim6258Flags.offset())?
    } else {
        0
    };
    h.k054539_flags = if should_read(VgmHeaderField::K054539Flags) {
        read_u8_at(bytes, VgmHeaderField::K054539Flags.offset())?
    } else {
        0
    };
    h.c140_chip_type = if should_read(VgmHeaderField::C140ChipType) {
        read_u8_at(bytes, VgmHeaderField::C140ChipType.offset())?
    } else {
        0
    };
    h.reserved_97 = if should_read(VgmHeaderField::Reserved97) {
        read_u8_at(bytes, VgmHeaderField::Reserved97.offset())?
    } else {
        0
    };
    h.okim6295_clock = if should_read(VgmHeaderField::Okim6295Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Okim6295Clock.offset())?
    } else {
        0
    };
    h.k051649_clock = if should_read(VgmHeaderField::K051649Clock) {
        read_u32_le_at(bytes, VgmHeaderField::K051649Clock.offset())?
    } else {
        0
    };
    h.k054539_clock = if should_read(VgmHeaderField::K054539Clock) {
        read_u32_le_at(bytes, VgmHeaderField::K054539Clock.offset())?
    } else {
        0
    };
    h.huc6280_clock = if should_read(VgmHeaderField::Huc6280Clock) {
        read_u32_le_at(bytes, VgmHeaderField::Huc6280Clock.offset())?
    } else {
        0
    };
    h.c140_clock = if should_read(VgmHeaderField::C140Clock) {
        read_u32_le_at(bytes, VgmHeaderField::C140Clock.offset())?
    } else {
        0
    };
    h.reserved_97 = if should_read(VgmHeaderField::Reserved97) {
        read_u8_at(bytes, VgmHeaderField::Reserved97.offset())?
    } else {
        0
    };
    h.k053260_clock = if should_read(VgmHeaderField::K053260Clock) {
        read_u32_le_at(bytes, VgmHeaderField::K053260Clock.offset())?
    } else {
        0
    };
    h.pokey_clock = if should_read(VgmHeaderField::PokeyClock) {
        read_u32_le_at(bytes, VgmHeaderField::PokeyClock.offset())?
    } else {
        0
    };
    h.qsound_clock = if should_read(VgmHeaderField::QsoundClock) {
        read_u32_le_at(bytes, VgmHeaderField::QsoundClock.offset())?
    } else {
        0
    };
    h.scsp_clock = if should_read(VgmHeaderField::ScspClock) {
        read_u32_le_at(bytes, VgmHeaderField::ScspClock.offset())?
    } else {
        0
    };
    h.extra_header_offset = if should_read(VgmHeaderField::ExtraHeaderOffset) {
        read_u32_le_at(bytes, VgmHeaderField::ExtraHeaderOffset.offset())?
    } else {
        0
    };
    h.wonderswan_clock = if should_read(VgmHeaderField::WonderSwan) {
        read_u32_le_at(bytes, VgmHeaderField::WonderSwan.offset())?
    } else {
        0
    };
    h.vsu_clock = if should_read(VgmHeaderField::Vsu) {
        read_u32_le_at(bytes, VgmHeaderField::Vsu.offset())?
    } else {
        0
    };
    h.saa1099_clock = if should_read(VgmHeaderField::Saa1099) {
        read_u32_le_at(bytes, VgmHeaderField::Saa1099.offset())?
    } else {
        0
    };
    h.es5503_clock = if should_read(VgmHeaderField::Es5503) {
        read_u32_le_at(bytes, VgmHeaderField::Es5503.offset())?
    } else {
        0
    };
    h.es5506_clock = if should_read(VgmHeaderField::Es5506) {
        read_u32_le_at(bytes, VgmHeaderField::Es5506.offset())?
    } else {
        0
    };
    h.es5503_output_channels = if should_read(VgmHeaderField::Es5503OutputChannels) {
        read_u8_at(bytes, VgmHeaderField::Es5503OutputChannels.offset())?
    } else {
        0
    };
    h.es5506_output_channels = if should_read(VgmHeaderField::Es5506OutputChannels) {
        read_u8_at(bytes, VgmHeaderField::Es5506OutputChannels.offset())?
    } else {
        0
    };
    h.c352_clock_divider = if should_read(VgmHeaderField::C352ClockDivider) {
        read_u8_at(bytes, VgmHeaderField::C352ClockDivider.offset())?
    } else {
        0
    };
    h.x1_010_clock = if should_read(VgmHeaderField::X1_010) {
        read_u32_le_at(bytes, VgmHeaderField::X1_010.offset())?
    } else {
        0
    };
    h.c352_clock = if should_read(VgmHeaderField::C352) {
        read_u32_le_at(bytes, VgmHeaderField::C352.offset())?
    } else {
        0
    };
    h.ga20_clock = if should_read(VgmHeaderField::Ga20) {
        read_u32_le_at(bytes, VgmHeaderField::Ga20.offset())?
    } else {
        0
    };
    h.mikey_clock = if should_read(VgmHeaderField::Mikey) {
        read_u32_le_at(bytes, VgmHeaderField::Mikey.offset())?
    } else {
        0
    };
    h.reserved_e8_ef = if should_read(VgmHeaderField::ReservedE8EF) {
        let s = read_slice(
            bytes,
            VgmHeaderField::ReservedE8EF.offset(),
            VgmHeaderField::ReservedE8EF.len(),
        )?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        a
    } else {
        [0u8; 8]
    };
    h.reserved_f0_ff = if should_read(VgmHeaderField::ReservedF0FF) {
        let s = read_slice(
            bytes,
            VgmHeaderField::ReservedF0FF.offset(),
            VgmHeaderField::ReservedF0FF.len(),
        )?;
        let mut a = [0u8; 16];
        a.copy_from_slice(s);
        a
    } else {
        [0u8; 16]
    };

    Ok((h, total_header_size))
}

/// Parse a VGM extra-header (v1.70+) located at `offset` within `bytes`.
///
/// The extra header format:
/// - u32 LE header_size (including this field)
/// - u32 LE offset to chip-clock block (relative to start of extra header, 0 = none)
/// - u32 LE offset to chip-volume block (relative to start of extra header, 0 = none)
/// - optional chip-clock block: 1 byte count, then count * (1 byte chip_id + 4 byte LE clock)
/// - optional chip-volume block: 1 byte count, then count * (1 byte chip_id + 1 byte flags + 2 byte LE volume)
pub(crate) fn parse_vgm_extra_header(
    bytes: &[u8],
    offset: usize,
) -> Result<(VgmExtraHeader, usize), ParseError> {
    // Read the three header fields (12 bytes)
    let header_size = read_u32_le_at(bytes, offset)?;
    let chip_clock_offset = read_u32_le_at(bytes, offset + 4)?;
    let chip_vol_offset = read_u32_le_at(bytes, offset + 8)?;

    // The 12-byte header boundary (after header_size, chip_clock_offset, chip_vol_offset fields)
    let data_base = offset.wrapping_add(12);

    // Track the actual offsets used for reading (normalized values)
    let mut actual_chip_clock_offset = chip_clock_offset;
    let mut actual_chip_vol_offset = chip_vol_offset;

    let mut extra = VgmExtraHeader {
        header_size,
        chip_clock_offset,
        chip_vol_offset,
        chip_clocks: Vec::new(),
        chip_volumes: Vec::new(),
    };

    // Parse chip clocks block if present
    // Offsets are relative to the extra_header start (offset)
    // However, some VGM files have invalid offsets (< 12, pointing into the header itself)
    // In such cases, fall back to reading sequentially from data_base
    if chip_clock_offset != 0 {
        let cc_base = offset.wrapping_add(chip_clock_offset as usize);

        // Check if offset is completely out of bounds
        if cc_base >= bytes.len() {
            return Err(ParseError::OffsetOutOfRange {
                offset: cc_base,
                needed: 1,
                available: bytes.len(),
                context: Some("extra_header chip_clock_offset".into()),
            });
        }

        // Use data_base if offset is invalid (< 12, pointing into the header itself)
        let actual_cc_base = if chip_clock_offset < 12 {
            // Normalize the offset to the correct value (12 = start of data area)
            actual_chip_clock_offset = 12;
            data_base
        } else {
            cc_base
        };

        // Verify the fallback offset is within bounds
        if actual_cc_base >= bytes.len() {
            return Err(ParseError::OffsetOutOfRange {
                offset: actual_cc_base,
                needed: 1,
                available: bytes.len(),
                context: Some("extra_header chip_clock_offset fallback".into()),
            });
        }
        // first byte is entry count
        let count = read_u8_at(bytes, actual_cc_base)?;
        let mut cur = actual_cc_base + 1;
        for _ in 0..count {
            let chip_id = read_u8_at(bytes, cur)?;
            let clock = read_u32_le_at(bytes, cur + 1)?;
            // Create a ChipClock preserving the raw chip-id byte and decoded instance.
            extra
                .chip_clocks
                .push(crate::vgm::header::ChipClock::from_raw(chip_id, clock));
            cur = cur.wrapping_add(5);
        }
    }

    // Parse chip volumes block if present
    // Offsets are relative to the extra_header start (offset)
    // However, some VGM files have invalid offsets (< 12, pointing into the header itself)
    // In such cases, fall back to reading sequentially from data_base
    if chip_vol_offset != 0 {
        let cv_base = offset.wrapping_add(chip_vol_offset as usize);

        // Check if offset is completely out of bounds
        if cv_base >= bytes.len() {
            return Err(ParseError::OffsetOutOfRange {
                offset: cv_base,
                needed: 1,
                available: bytes.len(),
                context: Some("extra_header chip_vol_offset".into()),
            });
        }

        // Use data_base if offset is invalid (< 12, pointing into the header itself)
        let actual_cv_base = if chip_vol_offset < 12 {
            // Normalize the offset to the correct value
            // If chip_clocks is empty, use 12; otherwise calculate after chip_clocks
            let normalized_offset = if extra.chip_clocks.is_empty() {
                12
            } else {
                // chip_clocks block: 1 byte count + count * 5 bytes per entry
                12 + 1 + (extra.chip_clocks.len() * 5)
            };
            actual_chip_vol_offset = normalized_offset as u32;
            data_base
        } else {
            cv_base
        };

        // Verify the fallback offset is within bounds
        if actual_cv_base >= bytes.len() {
            return Err(ParseError::OffsetOutOfRange {
                offset: actual_cv_base,
                needed: 1,
                available: bytes.len(),
                context: Some("extra_header chip_vol_offset fallback".into()),
            });
        }
        // first byte is entry count
        let count = read_u8_at(bytes, actual_cv_base)?;
        let mut cur = actual_cv_base + 1;
        for _ in 0..count {
            let chip_id = read_u8_at(bytes, cur)?;
            let flags = read_u8_at(bytes, cur + 1)?;
            let volume = read_u16_le_at(bytes, cur + 2)?;
            // Create a ChipVolume preserving raw bytes and decoded instance.
            extra
                .chip_volumes
                .push(crate::vgm::header::ChipVolume::from_raw(
                    chip_id, flags, volume,
                ));
            cur = cur.wrapping_add(4);
        }
    }

    // Update the extra header with normalized offset values if they were corrected
    if actual_chip_clock_offset != chip_clock_offset {
        extra.chip_clock_offset = actual_chip_clock_offset;
    }
    if actual_chip_vol_offset != chip_vol_offset {
        extra.chip_vol_offset = actual_chip_vol_offset;
    }

    // Recalculate header_size to include all data blocks
    let mut calculated_size = 12u32; // Base 12 bytes for the three u32 fields

    if !extra.chip_clocks.is_empty() {
        // chip_clock block size: 1 byte count + entries
        calculated_size += 1 + (extra.chip_clocks.len() as u32 * 5);
    }

    if !extra.chip_volumes.is_empty() {
        // chip_volume block size: 1 byte count + entries
        calculated_size += 1 + (extra.chip_volumes.len() as u32 * 4);
    }

    // Update header_size to the calculated value for correct round-trip serialization
    extra.header_size = calculated_size;

    Ok((extra, header_size as usize))
}

/// Parse a single VGM command beginning at `off` within `bytes`.
///
/// The function reads the opcode byte at `off`, dispatches to the
/// appropriate per-command parser for commands with payload, and
/// returns the decoded `VgmCommand` together with the total number of
/// bytes consumed (including the opcode byte). If the opcode is not a
/// recognized non-chip command, the parser will try to interpret it as
/// a chip write for the primary instance and then for the secondary
/// instance (secondary opcodes are the primary opcode + 0x50) by
/// delegating to `parse_chip_write`.
///
/// Returns `Ok((VgmCommand, consumed_bytes))` on success or a
/// `ParseError` on failure.
pub(crate) fn parse_vgm_command(
    bytes: &[u8],
    off: usize,
) -> Result<(VgmCommand, usize), ParseError> {
    let opcode = read_u8_at(bytes, off)?;
    let mut cur = off + 1;
    match opcode {
        0x31 => {
            let (v, n) = Ay8910StereoMask::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::AY8910StereoMask(v), 1 + n))
        }
        0x61 => {
            let (v, n) = WaitSamples::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::WaitSamples(v), 1 + n))
        }
        0x62 => {
            let (v, n) = Wait735Samples::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::Wait735Samples(v), 1 + n))
        }
        0x63 => {
            let (v, n) = Wait882Samples::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::Wait882Samples(v), 1 + n))
        }
        0x66 => {
            let (v, n) = EndOfData::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::EndOfData(v), 1 + n))
        }
        0x67 => {
            let (db, n) = DataBlock::parse(bytes, cur, opcode)?;
            cur += n;
            Ok((VgmCommand::DataBlock(db), cur - off))
        }
        0x68 => {
            let (pr, n) = PcmRamWrite::parse(bytes, cur, opcode)?;
            cur += n;
            Ok((VgmCommand::PcmRamWrite(pr), cur - off))
        }
        0x70..=0x7F => {
            let (v, n) = WaitNSample::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::WaitNSample(v), 1 + n))
        }
        0x80..=0x8F => {
            let (v, n) = Ym2612Port0Address2AWriteAndWaitN::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::YM2612Port0Address2AWriteAndWaitN(v), 1 + n))
        }
        0x90 => {
            let (v, n) = SetupStreamControl::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::SetupStreamControl(v), 1 + n))
        }
        0x91 => {
            let (v, n) = SetStreamData::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::SetStreamData(v), 1 + n))
        }
        0x92 => {
            let (v, n) = SetStreamFrequency::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::SetStreamFrequency(v), 1 + n))
        }
        0x93 => {
            let (v, n) = StartStream::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::StartStream(v), 1 + n))
        }
        0x94 => {
            let (v, n) = StopStream::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::StopStream(v), 1 + n))
        }
        0x95 => {
            let (v, n) = StartStreamFastCall::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::StartStreamFastCall(v), 1 + n))
        }
        0xE0 => {
            let (v, n) = SeekOffset::parse(bytes, cur, opcode)?;
            Ok((VgmCommand::SeekOffset(v), 1 + n))
        }
        other => {
            // Try to parse as a chip write (primary or secondary instance).
            for &instance in &[Instance::Primary, Instance::Secondary] {
                let opcode = match instance {
                    Instance::Primary => other,
                    Instance::Secondary => match other {
                        // The second SN76489 PSG uses 0x30 (0x3F for GG Stereo).
                        0x30 | 0x3F => 0x50,
                        // All chips of the YM-family that use command 0x5n use 0xAn for the second chip.
                        0xA0..=0xAF => other.wrapping_sub(0x50),
                        // All other chips use bit 7 (0x80) of the first parameter byte
                        // to distinguish between the 1st and 2nd chip.
                        0x80..=0xFF => other.wrapping_sub(0x80),
                        // Fallback to the older heuristic (subtract 0x50).
                        _ => other.wrapping_sub(0x50),
                    },
                };
                match parse_chip_write(opcode, instance, bytes, cur) {
                    Ok((cmd, cons)) => return Ok((cmd, 1 + cons)),
                    Err(ParseError::Other(_)) => continue,
                    Err(e) => return Err(e),
                }
            }

            // If no chip write matched, try reserved opcode ranges as a fallback.
            match parse_reserved_write(other, bytes, cur) {
                Ok((cmd, cons)) => return Ok((cmd, 1 + cons)),
                Err(ParseError::Other(_)) => {}
                Err(e) => return Err(e),
            }

            // If no known command matched, return an UnknownCommand that
            // preserves the opcode byte but treats the command as a single-
            // byte (opcode-only) command rather than an error.
            Ok((
                VgmCommand::UnknownCommand(UnknownSpec {
                    opcode: other,
                    offset: cur,
                }),
                1,
            ))
        }
    }
}

/// Parse a chip write payload and return the corresponding
/// `VgmCommand` plus the number of bytes consumed by the chip-specific
/// payload parser.
///
/// The `opcode` parameter is the base opcode value for the primary
/// instance (the caller is responsible for passing the correctly
/// adjusted base for secondary instances if required). `instance`
/// indicates whether the command targets the primary or secondary
/// chip instance and is encoded into the returned `VgmCommand`.
///
/// `bytes` and `offset` indicate the source buffer and the start of
/// the chip-specific payload (the per-chip `CommandSpec::parse`
/// implementations expect `offset` to point at the payload bytes,
/// not the opcode). This function dispatches to the appropriate
/// `<chip::XxxSpec as CommandSpec>::parse` implementation and wraps the
/// resulting spec into the matching `VgmCommand` variant.
pub(crate) fn parse_chip_write(
    opcode: u8,
    instance: Instance,
    bytes: &[u8],
    offset: usize,
) -> Result<(VgmCommand, usize), ParseError> {
    match opcode {
        0x40 => {
            let (spec, n) = <chip::MikeySpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::MikeyWrite(instance, spec), n))
        }
        0x4F => {
            let (spec, n) = <chip::GameGearPsgSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::GameGearPsgWrite(instance, spec), n))
        }
        0x50 => {
            let (spec, n) = <chip::PsgSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Sn76489Write(instance, spec), n))
        }
        0x51 => {
            let (spec, n) = <chip::Ym2413Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym2413Write(instance, spec), n))
        }
        0x52 | 0x53 => {
            let (spec, n) = <chip::Ym2612Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym2612Write(instance, spec), n))
        }
        0x54 => {
            let (spec, n) = <chip::Ym2151Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym2151Write(instance, spec), n))
        }
        0x55 => {
            let (spec, n) = <chip::Ym2203Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym2203Write(instance, spec), n))
        }
        0x56 | 0x57 => {
            let (spec, n) = <chip::Ym2608Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym2608Write(instance, spec), n))
        }
        0x58 | 0x59 => {
            let (spec, n) = <chip::Ym2610Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym2610bWrite(instance, spec), n))
        }
        0x5A => {
            let (spec, n) = <chip::Ym3812Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym3812Write(instance, spec), n))
        }
        0x5B => {
            let (spec, n) = <chip::Ym3526Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ym3526Write(instance, spec), n))
        }
        0x5C => {
            let (spec, n) = <chip::Y8950Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Y8950Write(instance, spec), n))
        }
        0x5D => {
            let (spec, n) = <chip::Ymz280bSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ymz280bWrite(instance, spec), n))
        }
        0x5E | 0x5F => {
            let (spec, n) = <chip::Ymf262Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ymf262Write(instance, spec), n))
        }
        0xA0 => {
            let (spec, n) = <chip::Ay8910Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ay8910Write(instance, spec), n))
        }
        0xB0 => {
            let (spec, n) = <chip::Rf5c68U8Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Rf5c68U8Write(instance, spec), n))
        }
        0xB1 => {
            let (spec, n) = <chip::Rf5c164U8Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Rf5c164U8Write(instance, spec), n))
        }
        0xB2 => {
            let (spec, n) = <chip::PwmSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::PwmWrite(instance, spec), n))
        }
        0xB3 => {
            let (spec, n) = <chip::GbDmgSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::GbDmgWrite(instance, spec), n))
        }
        0xB4 => {
            let (spec, n) = <chip::NesApuSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::NesApuWrite(instance, spec), n))
        }
        0xB5 => {
            let (spec, n) = <chip::MultiPcmSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::MultiPcmWrite(instance, spec), n))
        }
        0xB6 => {
            let (spec, n) = <chip::Upd7759Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Upd7759Write(instance, spec), n))
        }
        0xB7 => {
            let (spec, n) = <chip::Okim6258Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Okim6258Write(instance, spec), n))
        }
        0xB8 => {
            let (spec, n) = <chip::Okim6295Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Okim6295Write(instance, spec), n))
        }
        0xB9 => {
            let (spec, n) = <chip::Huc6280Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Huc6280Write(instance, spec), n))
        }
        0xBA => {
            let (spec, n) = <chip::K053260Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::K053260Write(instance, spec), n))
        }
        0xBB => {
            let (spec, n) = <chip::PokeySpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::PokeyWrite(instance, spec), n))
        }
        0xBC => {
            // 0xBC: WonderSwan register write (8-bit register form)
            let (spec, n) = <chip::WonderSwanRegSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::WonderSwanRegWrite(instance, spec), n))
        }
        0xBD => {
            let (spec, n) = <chip::Saa1099Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Saa1099Write(instance, spec), n))
        }
        0xBE => {
            let (spec, n) = <chip::Es5506U8Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Es5506BEWrite(instance, spec), n))
        }
        0xBF => {
            let (spec, n) = <chip::Ga20Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ga20Write(instance, spec), n))
        }
        0xC0 => {
            let (spec, n) = <chip::SegaPcmSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::SegaPcmWrite(instance, spec), n))
        }
        0xC1 => {
            let (spec, n) = <chip::Rf5c68U16Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Rf5c68U16Write(instance, spec), n))
        }
        0xC2 => {
            let (spec, n) = <chip::Rf5c164U16Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Rf5c164U16Write(instance, spec), n))
        }
        0xC3 => {
            let (spec, n) = <chip::MultiPcmBankSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::MultiPcmBankWrite(instance, spec), n))
        }
        0xC4 => {
            let (spec, n) = <chip::QsoundSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::QsoundWrite(instance, spec), n))
        }
        0xC5 => {
            let (spec, n) = <chip::ScspSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::ScspWrite(instance, spec), n))
        }
        // WonderSwan, write value dd to memory offset mmll (mm - offset MSB, ll - offset LSB)
        0xC6 => {
            let (spec, n) = <chip::WonderSwanSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::WonderSwanWrite(instance, spec), n))
        }
        0xC7 => {
            let (spec, n) = <chip::VsuSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::VsuWrite(instance, spec), n))
        }
        0xC8 => {
            let (spec, n) = <chip::X1010Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::X1010Write(instance, spec), n))
        }
        0xD0 => {
            let (spec, n) = <chip::Ymf278bSpec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ymf278bWrite(instance, spec), n))
        }
        0xD1 => {
            let (spec, n) = <chip::Ymf271Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Ymf271Write(instance, spec), n))
        }
        0xD2 => {
            let (spec, n) = <chip::Scc1Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Scc1Write(instance, spec), n))
        }
        0xD3 => {
            let (spec, n) = <chip::K054539Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::K054539Write(instance, spec), n))
        }
        0xD4 => {
            let (spec, n) = <chip::C140Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::C140Write(instance, spec), n))
        }
        0xD5 => {
            let (spec, n) = <chip::Es5503Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Es5503Write(instance, spec), n))
        }
        0xD6 => {
            let (spec, n) = <chip::Es5506U16Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::Es5506D6Write(instance, spec), n))
        }
        0xE1 => {
            let (spec, n) = <chip::C352Spec as CommandSpec>::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::C352Write(instance, spec), n))
        }
        _ => Err(ParseError::Other(format!(
            "unknown chip base opcode {:#X}",
            opcode
        ))),
    }
}

/// Parse reserved (non-chip) VGM write opcodes.
///
/// This mirrors the structure of `parse_chip_write` but handles the
/// reserved opcode ranges that map to `ReservedU8`, `ReservedU16`,
/// `ReservedU24`, and `ReservedU32` command specs. The `opcode`
/// parameter is the opcode byte as seen in the VGM stream (the parser
/// expects the caller to have consumed the opcode byte already and
/// `offset` points at the first payload byte).
pub(crate) fn parse_reserved_write(
    opcode: u8,
    bytes: &[u8],
    offset: usize,
) -> Result<(VgmCommand, usize), ParseError> {
    match opcode {
        // ReservedU8: 0x30..=0x3F
        0x30..=0x3F => {
            let (spec, n) = ReservedU8::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::ReservedU8Write(spec), n))
        }

        // ReservedU16: 0x41..=0x4E
        0x41..=0x4E => {
            let (spec, n) = ReservedU16::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::ReservedU16Write(spec), n))
        }

        // ReservedU24: 0xC9..=0xCF and 0xD7..=0xDF
        0xC9..=0xCF | 0xD7..=0xDF => {
            let (spec, n) = ReservedU24::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::ReservedU24Write(spec), n))
        }

        // ReservedU32: 0xE2..=0xFF
        0xE2..=0xFF => {
            let (spec, n) = ReservedU32::parse(bytes, offset, opcode)?;
            Ok((VgmCommand::ReservedU32Write(spec), n))
        }

        _ => Err(ParseError::Other(format!(
            "unknown reserved opcode {:#X}",
            opcode
        ))),
    }
}

/// Trace commands but return partial results on error.
///
/// Returns a tuple of `(commands, error)` where `error` is `Some(ParseError)`
/// if parsing failed at some point; otherwise `error` is `None` on success.
///
/// Addresses in the returned tuples are absolute byte offsets into the VGM
/// data (i.e. absolute binary addresses).
///
/// Debug-only: this is an internal diagnostic helper and not part of the
/// public stable API; it may change or be removed.
#[doc(hidden)]
pub fn trace_vgm_commands_until_error(
    bytes: &[u8],
) -> (Vec<(usize, u8, usize)>, Option<ParseError>) {
    let (_header, off0) = match parse_vgm_header(bytes) {
        Ok(v) => v,
        Err(e) => return (Vec::new(), Some(e)),
    };

    let mut out: Vec<(usize, u8, usize)> = Vec::new();
    let mut off = off0;

    while off < bytes.len() {
        // Try to read opcode; if out of range, return current out and error
        let opcode = match read_u8_at(bytes, off) {
            Ok(b) => b,
            Err(e) => return (out, Some(e)),
        };

        // Parse command at current offset; on error, return partial results.
        let parsed = match parse_vgm_command(bytes, off) {
            Ok((_cmd, cons)) => cons,
            Err(e) => return (out, Some(e)),
        };

        // Use absolute offset (binary address) instead of relative offset.
        let abs = off;
        out.push((abs, opcode, parsed));
        off = off.wrapping_add(parsed);

        if opcode == 0x66 {
            return (out, None);
        }
    }

    (out, None)
}
