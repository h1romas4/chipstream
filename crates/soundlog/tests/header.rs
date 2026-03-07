use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;
use soundlog::vgm::header::{
    Ay8910ChipType, Ay8910Flags, C140ChipType, ChipClock, ChipId, ChipVolume, K054539Flags,
    Okim6258Flags, Sn76489Feedback, Sn76489Flags, Sn76489ShiftRegisterWidth, Ym2203AyFlags,
    Ym2608AyFlags,
};

#[test]
fn test_chipid_masks_paired_bit() {
    // Known chip id 0x02 => Ym2612. If bit 7 is set in the on-disk byte,
    // ChipId::from_u8 should still return the canonical variant.
    let raw_with_paired = 0x80 | 0x02;
    let id = ChipId::from_u8(raw_with_paired);
    assert_eq!(id, ChipId::Ym2612);
    // to_u8 for known variants should return the canonical low-7-bit value.
    assert_eq!(id.to_u8(), 0x02);
}

#[test]
fn test_extra_header_build_and_decode_roundtrip() {
    // Build a minimal VGM document with an extra header containing one clock
    // entry and one volume entry and verify round-trip parse/serialize and
    // semantic decoding of fields (including paired bit preservation).
    let mut builder = soundlog::VgmBuilder::new();
    // add a minimal command so builder produces a document
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(1));

    let extra = soundlog::vgm::VgmExtraHeader {
        header_size: 0,
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: vec![ChipClock::new(ChipId::Ym2203, Instance::Primary, 12345u32)],
        chip_volumes: vec![ChipVolume::new_paired(
            ChipId::Ay8910,
            Instance::Secondary,
            777u16,
        )],
    };

    builder.set_extra_header(extra);
    let doc = builder.finalize();
    let serialized: Vec<u8> = (&doc).into();

    // Parse back into a VgmDocument
    let parsed: soundlog::VgmDocument = serialized.as_slice().try_into().expect("failed to parse");
    let parsed_extra = parsed.extra_header.expect("expected extra header");

    // Validate chip_clock entry
    assert_eq!(parsed_extra.chip_clocks.len(), 1);
    let cc = &parsed_extra.chip_clocks[0];
    assert_eq!(cc.chip_id, ChipId::Ym2203);
    assert_eq!(cc.clock, 12345u32);
    // Instance for chip_clock was Primary
    assert_eq!(cc.instance, Instance::Primary);

    // Validate chip_volume entry
    assert_eq!(parsed_extra.chip_volumes.len(), 1);
    let pv = &parsed_extra.chip_volumes[0];
    // paired bit should survive round-trip
    assert!(pv.paired_chip);
    // Decoded canonical chip id should be Ay8910
    assert_eq!(pv.chip_id, ChipId::Ay8910);
    // Instance should be Secondary as encoded
    assert_eq!(pv.instance, Instance::Secondary);
    assert_eq!(pv.volume, 777u16);
}

#[test]
fn test_vgm_header_roundtrip_all_fields() {
    // Build a document via builder (so required EndOfData is present),
    // then overwrite the header with a header populated with distinct values,
    // serialize to bytes and parse back, verifying all header fields round-trip.
    let mut builder = soundlog::VgmBuilder::new();
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(1));
    let mut doc = builder.finalize();

    // Populate a header with non-default, distinguishable values using a
    // struct literal to avoid clippy warnings about repeated field assignments.
    let h = soundlog::VgmHeader {
        ident: *b"Vgm ",
        eof_offset: 0x1111_1111,
        version: 0x00000172,
        sn76489_clock: 0x0000_1234,
        ym2413_clock: 0x0000_2345,
        gd3_offset: 0x0000_3456,
        total_samples: 0x0000_4567,
        loop_offset: 0x0000_5678,
        loop_samples: 0x0000_6789,
        sample_rate: 48000,
        sn76489_feedback: Sn76489Feedback::Sn76489a,
        sn76489_shift_register_width: Sn76489ShiftRegisterWidth::Unknown(0x7F),
        sn76489_flags: Sn76489Flags::from(0x01),
        ym2612_clock: 0x0100_0000,
        ym2151_clock: 0x0200_0000,
        data_offset: 0,
        sega_pcm_clock: 0x0300_0000,
        spcm_interface: 0x0400_0000,
        rf5c68_clock: 0x0500_0000,
        ym2203_clock: 0x0600_0000,
        ym2608_clock: 0x0700_0000,
        ym2610b_clock: 0x0800_0000,
        ym3812_clock: 0x0900_0000,
        ym3526_clock: 0x0A00_0000,
        y8950_clock: 0x0B00_0000,
        ymf262_clock: 0x0C00_0000,
        ymf278b_clock: 0x0D00_0000,
        ymf271_clock: 0x0E00_0000,
        ymz280b_clock: 0x0F00_0000,
        rf5c164_clock: 0x1000_0000,
        pwm_clock: 0x1100_0000,
        ay8910_clock: 0x1200_0000,
        ay_chip_type: Ay8910ChipType::from(0x9A),
        ay8910_flags: Ay8910Flags::from(0x01),
        ym2203_ay8910_flags: Ym2203AyFlags::from(0x02),
        ym2608_ay8910_flags: Ym2608AyFlags::from(0x03),
        volume_modifier: 0x04,
        reserved_7d: 0x05,
        loop_base: 0x06_i8,
        loop_modifier: 0x07,
        gb_dmg_clock: 0x1300_0000,
        nes_apu_clock: 0x1400_0000,
        multipcm_clock: 0x1500_0000,
        upd7759_clock: 0x1600_0000,
        okim6258_clock: 0x1700_0000,
        okim6258_flags: Okim6258Flags::from(0x0A),
        okim6295_clock: 0x1800_0000,
        k051649_clock: 0x1900_0000,
        k054539_clock: 0x1A00_0000,
        k054539_flags: K054539Flags::from(0x0C),
        huc6280_clock: 0x1B00_0000,
        c140_clock: 0x1C00_0000,
        c140_chip_type: C140ChipType::from(0x0D),
        reserved_97: 0x0B,
        k053260_clock: 0x1D00_0000,
        pokey_clock: 0x1E00_0000,
        qsound_clock: 0x1F00_0000,
        scsp_clock: 0x2000_0000,
        extra_header_offset: 0x30,
        wonderswan_clock: 0x2100_0000,
        vsu_clock: 0x2200_0000,
        saa1099_clock: 0x2300_0000,
        es5503_clock: 0x2400_0000,
        es5506_clock: 0x2500_0000,
        es5503_output_channels: 0x01,
        es5506_output_channels: 0x02,
        c352_clock_divider: 0x03,
        x1_010_clock: 0x2600_0000,
        c352_clock: 0x2700_0000,
        ga20_clock: 0x2800_0000,
        mikey_clock: 0x2900_0000,
        reserved_e8_ef: [0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF],
        reserved_f0_ff: [
            0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD,
            0xFE, 0xFF,
        ],
    };

    // Overwrite the document header with our populated header.
    doc.header = h.clone();

    // Serialize and parse back
    let serialized: Vec<u8> = (&doc).into();
    let parsed: soundlog::VgmDocument = serialized.as_slice().try_into().expect("failed to parse");

    let ph = parsed.header;

    // Assert equality for all header fields
    assert_eq!(ph.ident, h.ident);
    assert_eq!(ph.eof_offset, h.eof_offset);
    assert_eq!(ph.version, h.version);
    assert_eq!(ph.sn76489_clock, h.sn76489_clock);
    assert_eq!(ph.ym2413_clock, h.ym2413_clock);
    assert_eq!(ph.total_samples, h.total_samples);
    assert_eq!(ph.loop_offset, h.loop_offset);
    assert_eq!(ph.loop_samples, h.loop_samples);
    assert_eq!(ph.sample_rate, h.sample_rate);
    assert_eq!(ph.sn76489_feedback, h.sn76489_feedback);
    assert_eq!(
        ph.sn76489_shift_register_width,
        h.sn76489_shift_register_width
    );
    assert_eq!(ph.sn76489_flags, h.sn76489_flags);
    assert_eq!(ph.ym2612_clock, h.ym2612_clock);
    assert_eq!(ph.ym2151_clock, h.ym2151_clock);
    assert_eq!(ph.sega_pcm_clock, h.sega_pcm_clock);
    assert_eq!(ph.spcm_interface, h.spcm_interface);
    assert_eq!(ph.rf5c68_clock, h.rf5c68_clock);
    assert_eq!(ph.ym2203_clock, h.ym2203_clock);
    assert_eq!(ph.ym2608_clock, h.ym2608_clock);
    assert_eq!(ph.ym2610b_clock, h.ym2610b_clock);
    assert_eq!(ph.ym3812_clock, h.ym3812_clock);
    assert_eq!(ph.ym3526_clock, h.ym3526_clock);
    assert_eq!(ph.y8950_clock, h.y8950_clock);
    assert_eq!(ph.ymf262_clock, h.ymf262_clock);
    assert_eq!(ph.ymf278b_clock, h.ymf278b_clock);
    assert_eq!(ph.ymf271_clock, h.ymf271_clock);
    assert_eq!(ph.ymz280b_clock, h.ymz280b_clock);
    assert_eq!(ph.rf5c164_clock, h.rf5c164_clock);
    assert_eq!(ph.pwm_clock, h.pwm_clock);
    assert_eq!(ph.ay8910_clock, h.ay8910_clock);
    assert_eq!(ph.ay_chip_type, h.ay_chip_type);
    assert_eq!(ph.ay8910_flags, h.ay8910_flags);
    assert_eq!(ph.ym2203_ay8910_flags, h.ym2203_ay8910_flags);
    assert_eq!(ph.ym2608_ay8910_flags, h.ym2608_ay8910_flags);
    assert_eq!(ph.volume_modifier, h.volume_modifier);
    assert_eq!(ph.reserved_7d, h.reserved_7d);
    assert_eq!(ph.loop_base, h.loop_base);
    assert_eq!(ph.loop_modifier, h.loop_modifier);
    assert_eq!(ph.gb_dmg_clock, h.gb_dmg_clock);
    assert_eq!(ph.nes_apu_clock, h.nes_apu_clock);
    assert_eq!(ph.multipcm_clock, h.multipcm_clock);
    assert_eq!(ph.upd7759_clock, h.upd7759_clock);
    assert_eq!(ph.okim6258_clock, h.okim6258_clock);
    assert_eq!(ph.okim6258_flags, h.okim6258_flags);
    assert_eq!(ph.okim6295_clock, h.okim6295_clock);
    assert_eq!(ph.k051649_clock, h.k051649_clock);
    assert_eq!(ph.k054539_clock, h.k054539_clock);
    assert_eq!(ph.k054539_flags, h.k054539_flags);
    assert_eq!(ph.huc6280_clock, h.huc6280_clock);
    assert_eq!(ph.c140_clock, h.c140_clock);
    assert_eq!(ph.c140_chip_type, h.c140_chip_type);
    assert_eq!(ph.reserved_97, h.reserved_97);
    assert_eq!(ph.k053260_clock, h.k053260_clock);
    assert_eq!(ph.pokey_clock, h.pokey_clock);
    assert_eq!(ph.qsound_clock, h.qsound_clock);
    assert_eq!(ph.scsp_clock, h.scsp_clock);
    assert_eq!(ph.wonderswan_clock, h.wonderswan_clock);
    assert_eq!(ph.vsu_clock, h.vsu_clock);
    assert_eq!(ph.saa1099_clock, h.saa1099_clock);
    assert_eq!(ph.es5503_clock, h.es5503_clock);
    assert_eq!(ph.es5506_clock, h.es5506_clock);
    assert_eq!(ph.es5503_output_channels, h.es5503_output_channels);
    assert_eq!(ph.es5506_output_channels, h.es5506_output_channels);
    assert_eq!(ph.c352_clock_divider, h.c352_clock_divider);
    assert_eq!(ph.x1_010_clock, h.x1_010_clock);
    assert_eq!(ph.c352_clock, h.c352_clock);
    assert_eq!(ph.ga20_clock, h.ga20_clock);
    assert_eq!(ph.mikey_clock, h.mikey_clock);

    // Skip auto calc
    // assert_eq!(ph.extra_header_offset, h.extra_header_offset);
    // assert_eq!(ph.data_offset, expected_data_offset);
    // assert_eq!(ph.gd3_offset, h.gd3_offset);
    // Trancate
    // assert_eq!(ph.reserved_e8_ef, h.reserved_e8_ef);
    // assert_eq!(ph.reserved_f0_ff, h.reserved_f0_ff);
}

#[test]
fn test_chip_instances_substitute_ym2413_for_ym2612() {
    // Legacy behavior: when version <= 1.01 and ym2413_clock > 5_000_000 and ym2612_clock == 0,
    // misc() suggests substituting the YM2413 clock for YM2612. Verify chip_instances()
    // reflects that substitution.
    let h = soundlog::VgmHeader {
        version: 0x00000101,
        ym2413_clock: 6_000_000u32,
        ym2612_clock: 0u32,
        ..Default::default()
    };
    let instances = h.chip_instances();
    let mut found = false;
    for (inst, ch, clock_hz) in instances.iter() {
        if *ch == Chip::Ym2612 {
            // Expect primary instance with substituted clock ~ 6_000_000
            assert_eq!(*inst, Instance::Primary);
            assert!(
                ((*clock_hz) - 6_000_000.0).abs() < 0.1,
                "unexpected clock for Ym2612: {}",
                clock_hz
            );
            found = true;
        }
    }
    assert!(
        found,
        "Expected Ym2612 instance derived from YM2413 substitution"
    );
}

#[test]
fn test_chip_instances_substitute_ym2413_for_ym2151() {
    // Legacy behavior: when version <= 1.01 and ym2413_clock < 5_000_000 and ym2151_clock == 0,
    // misc() suggests substituting the YM2413 clock for YM2151. Verify chip_instances()
    // reflects that substitution.
    let h = soundlog::VgmHeader {
        version: 0x00000101,
        ym2413_clock: 4_000_000u32,
        ym2151_clock: 0u32,
        ..Default::default()
    };
    let instances = h.chip_instances();
    let mut found = false;
    for (inst, ch, clock_hz) in instances.iter() {
        if *ch == Chip::Ym2151 {
            // Expect primary instance with substituted clock ~ 4_000_000
            assert_eq!(*inst, Instance::Primary);
            assert!(
                ((*clock_hz) - 4_000_000.0).abs() < 0.1,
                "unexpected clock for Ym2151: {}",
                clock_hz
            );
            found = true;
        }
    }
    assert!(
        found,
        "Expected Ym2151 instance derived from YM2413 substitution"
    );
}

#[test]
fn test_gd3_tryfrom_short_header() {
    // Fewer than 12 bytes should yield a HeaderTooShort("gd3") error.
    let bytes: &[u8] = &[0u8; 8];
    let res = soundlog::meta::Gd3::try_from(bytes);
    match res {
        Err(soundlog::ParseError::HeaderTooShort(ref s)) => {
            assert_eq!(s, "gd3");
        }
        other => panic!("expected HeaderTooShort(\"gd3\"), got {:?}", other),
    }
}

#[test]
fn test_gd3_tryfrom_invalid_ident() {
    // A 12-byte header with wrong ident should produce InvalidIdent.
    let mut bytes = vec![0u8; 12];
    bytes[0..4].copy_from_slice(b"BAD!");
    let res = soundlog::meta::Gd3::try_from(bytes.as_slice());
    match res {
        Err(soundlog::ParseError::InvalidIdent(id)) => {
            assert_eq!(&id, b"BAD!");
        }
        other => panic!("expected InvalidIdent, got {:?}", other),
    }
}

#[test]
fn test_gd3_truncated_utf16_fields_yields_none() {
    // Construct a Gd3 header that claims 1 byte of data (which is truncated for UTF-16).
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"Gd3 ");
    bytes.extend_from_slice(&0x00000100u32.to_le_bytes()); // version
    bytes.extend_from_slice(&1u32.to_le_bytes()); // length = 1 byte (truncated)
    bytes.push(0xAA); // single data byte -> truncated stream

    let gd3 = soundlog::meta::Gd3::try_from(bytes.as_slice())
        .expect("expected parse to succeed despite truncation");
    // All string fields should be None due to truncation semantics.
    assert_eq!(gd3.track_name_en, None);
    assert_eq!(gd3.notes, None);
    assert_eq!(gd3.version, 0x00000100);
}

#[test]
fn test_gd3_invalid_utf16_yields_other_error() {
    // Provide a field with an unpaired surrogate (0xD800) followed by terminator.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"Gd3 ");
    bytes.extend_from_slice(&0x00000100u32.to_le_bytes()); // version
    bytes.extend_from_slice(&4u32.to_le_bytes()); // length = 4 bytes (one u16 + terminator)

    // Append invalid UTF-16: single unpaired high surrogate 0xD800, then 0x0000 terminator.
    bytes.extend_from_slice(&0xD800u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());

    let res = soundlog::meta::Gd3::try_from(bytes.as_slice());
    match res {
        Err(soundlog::ParseError::Other(ref s)) => {
            assert!(
                s.contains("invalid utf16 in gd3"),
                "unexpected message: {}",
                s
            );
        }
        other => panic!("expected Other(invalid utf16), got {:?}", other),
    }
}

//
// Additional UnexpectedEof tests for data block parsing branches.
// These ensure parse_data_block returns ParseError::UnexpectedEof
// when the provided data buffer is too short for the declared type.
//

#[test]
fn test_parse_data_block_unexpected_eof_compressed_short_header() {
    // Compressed stream requires at least 5 bytes for the header.
    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0x40, // compressed stream
        size: 2,
        data: vec![0x00, 0x01], // insufficient for header (needs at least 5)
    };
    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}

#[test]
fn test_parse_data_block_unexpected_eof_bitpacking_inner_header() {
    // Compressed stream with BitPacking: outer header present (>=5 bytes), but
    // BitPacking requires >=11 bytes total. Use length 8 to trigger inner EOF.
    let mut data: Vec<u8> = Vec::new();
    data.push(0x00); // compression type = BitPacking
    data.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size (4 bytes)
    // leave the rest short so total length < 11
    data.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // partial fields

    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0x40, // compressed stream
        size: data.len() as u32,
        data,
    };

    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}

#[test]
fn test_parse_data_block_unexpected_eof_dpcm_inner_header() {
    // Compressed stream with DPCM: outer header present (>=5 bytes), but
    // DPCM requires >=10 bytes total. Use length 9 to trigger inner EOF.
    let mut data: Vec<u8> = Vec::new();
    data.push(0x01); // compression type = Dpcm
    data.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size (4 bytes)
    // leave the rest short so total length < 10
    data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // partial fields (still short)

    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0x40, // compressed stream
        size: data.len() as u32,
        data,
    };

    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}

#[test]
fn test_parse_data_block_unexpected_eof_decompression_table() {
    // Decompression table (0x7F) requires at least 6 bytes.
    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0x7F,
        size: 4,
        data: vec![0x00, 0x01, 0x02, 0x03], // too short (needs at least 6)
    };

    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}

#[test]
fn test_parse_data_block_unexpected_eof_romram_dump() {
    // ROM/RAM dump (0x80..=0xBF) requires at least 8 bytes for rom_size/start_address.
    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0x80,
        size: 4,
        data: vec![0x00, 0x01, 0x02, 0x03], // too short (needs at least 8)
    };

    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}

#[test]
fn test_parse_data_block_unexpected_eof_ramwrite16() {
    // RAM write 16-bit (0xC0..=0xDF) requires at least 2 bytes for start_address.
    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0xC0,
        size: 1,
        data: vec![0xFF], // too short (needs at least 2)
    };

    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}

#[test]
fn test_parse_data_block_unexpected_eof_ramwrite32() {
    // RAM write 32-bit (0xE0..=0xFF) requires at least 4 bytes for start_address.
    let block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: soundlog::vgm::command::Instance::Primary as u8,
        data_type: 0xE0,
        size: 2,
        data: vec![0xAA, 0xBB], // too short (needs at least 4)
    };

    let res = soundlog::vgm::detail::parse_data_block(block);
    assert!(res.is_err());
    if let Err((_blk, e)) = res {
        match e {
            soundlog::ParseError::UnexpectedEof => { /* expected */ }
            other => panic!("expected UnexpectedEof, got {:?}", other),
        }
    } else {
        panic!("expected error");
    }
}
