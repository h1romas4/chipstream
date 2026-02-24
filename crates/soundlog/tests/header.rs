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
    // Raw chip id should not have paired bit set for this clock entry
    assert_eq!(cc.raw_chip_id & 0x80, 0x00);

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
    // Raw fields preserved
    assert_eq!(pv.raw_flags & 0x01, 0x01);
    assert_eq!(pv.raw_chip_id & 0x80, 0x80);
}
