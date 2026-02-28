use soundlog::vgm::command::Instance;
use soundlog::vgm::header::{ChipClock, ChipId, ChipVolume};

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
        sn76489_feedback: 0x55AA,
        sn76489_shift_register_width: 0x7F,
        sn76489_flags: 0x01,
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
        ay_chip_type: 0x9A,
        ay8910_flags: 0x01,
        ym2203_ay8910_flags: 0x02,
        ym2608_ay8910_flags: 0x03,
        volume_modifier: 0x04,
        reserved_7d: 0x05,
        loop_base: 0x06,
        loop_modifier: 0x07,
        gb_dmg_clock: 0x1300_0000,
        nes_apu_clock: 0x1400_0000,
        multipcm_clock: 0x1500_0000,
        upd7759_clock: 0x1600_0000,
        okim6258_clock: 0x1700_0000,
        okim6258_flags: 0x0A,
        okim6295_clock: 0x1800_0000,
        k051649_clock: 0x1900_0000,
        k054539_clock: 0x1A00_0000,
        k054539_flags: 0x0C,
        huc6280_clock: 0x1B00_0000,
        c140_clock: 0x1C00_0000,
        c140_chip_type: 0x0D,
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
