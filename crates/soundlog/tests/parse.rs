use soundlog::VgmDocument;
use std::convert::TryInto;

/// Build a minimal VGM that includes an `extra_header`, serialize it,
/// and assert that the serialization produced some bytes. This keeps the
/// test focused on builder->serialize behavior for extra headers for now.
#[test]
fn test_build_serialize_with_extra_header() {
    use soundlog::VgmBuilder;
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::command::WaitSamples;

    // Build a simple document with one wait command.
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(1));

    // Construct an extra header with one clock entry and one volume entry.
    let extra = VgmExtraHeader {
        header_size: 0, // to_bytes computes and writes size; this field is not used directly.
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: vec![(1u8, 12345u32)],
        chip_volumes: vec![(2u8, 0x01u8, 1000u16)],
    };
    builder.set_extra_header(extra);

    let doc = builder.finalize();

    // Serialize to bytes.
    let serialized: Vec<u8> = (&doc).into();

    // Basic sanity: serialization produced bytes.
    assert!(
        !serialized.is_empty(),
        "serialization produced empty output"
    );

    // Read header fields from the serialized bytes and assert they look sane.
    // EOF offset (0x04), data_offset (0x34), extra_header_offset (0xBC) are
    // stored as 32-bit little-endian values in the header.
    assert!(
        serialized.len() >= 0x100,
        "serialized output too small to contain full header"
    );

    let eof_offset = u32::from_le_bytes(serialized[0x04..0x08].try_into().unwrap());
    let data_offset = u32::from_le_bytes(serialized[0x34..0x38].try_into().unwrap());
    let extra_header_offset = u32::from_le_bytes(serialized[0xBC..0xC0].try_into().unwrap());

    // EOF offset is file size minus 4; ensure it points within the file.
    let file_size = serialized.len() as u32;
    assert!(
        eof_offset <= file_size.wrapping_sub(4),
        "eof_offset points beyond file"
    );

    // Compute header length as 0x34 + data_offset. Ensure header length is within file.
    let header_len = 0x34u32.wrapping_add(data_offset) as usize;
    assert!(
        header_len <= serialized.len(),
        "computed header length exceeds serialized size"
    );

    // The builder attached an extra header; ensure the header contains a
    // non-zero stored extra_header_offset and validate the extra header's size.
    assert!(extra_header_offset != 0, "extra_header_offset is zero");
    let extra_start = extra_header_offset.wrapping_add(0xBC) as usize;
    // The extra header must fit at least 12 bytes for the size+offsets fields.
    assert!(
        extra_start + 12 <= serialized.len(),
        "extra header start out of range"
    );
    let stored_extra_size =
        u32::from_le_bytes(serialized[extra_start..extra_start + 4].try_into().unwrap());
    // header_size should be non-zero and not exceed remaining file length.
    assert!(stored_extra_size != 0, "extra header size is zero");
    assert!(
        (extra_start + stored_extra_size as usize) <= serialized.len(),
        "extra header extends beyond file bounds"
    );

    // Ensure ordering: main header < extra header < data region start.
    let extra_end = extra_start + stored_extra_size as usize;
    assert!(
        extra_start >= 0x34,
        "extra header starts before main header end"
    );
    assert!(
        extra_start < header_len,
        "extra header does not lie within header region"
    );
    assert!(
        extra_end <= header_len,
        "extra header extends into the data region; expected it to be in the header"
    );
}

/// Full round-trip test: build a document with an extra header, serialize it,
/// parse the serialized bytes back into a `VgmDocument`, re-serialize and
/// assert the two serialized representations are identical. Also verify that
/// the parsed extra header contains the expected entries.
#[test]
fn test_build_parse_build_with_extra_header_roundtrip() {
    use soundlog::VgmBuilder;
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::command::WaitSamples;

    // Build a simple document with one wait command.
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(1));

    // Construct an extra header with one clock entry and one volume entry.
    let extra = VgmExtraHeader {
        header_size: 0, // to_bytes computes and writes size; this field is not used directly.
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: vec![(1u8, 12345u32)],
        chip_volumes: vec![(2u8, 0x01u8, 1000u16)],
    };

    // Attach the extra header to the builder and finalize the document.
    builder.set_extra_header(extra);
    let doc = builder.finalize();

    // Serialize to bytes.
    let serialized: Vec<u8> = (&doc).into();

    // Parse back into a VgmDocument.
    let parsed: VgmDocument = serialized
        .as_slice()
        .try_into()
        .expect("failed to parse serialized VGM with extra header");

    // Re-serialize the parsed document.
    let reserialized: Vec<u8> = (&parsed).into();

    // The two serialized forms should match exactly.
    assert_eq!(
        serialized, reserialized,
        "round-trip serialize/parse/serialize with extra_header did not produce identical bytes"
    );

    // Also verify that the parsed document contains an extra header and that
    // it has the expected entries.
    assert!(
        parsed.extra_header.is_some(),
        "parsed document missing extra_header"
    );
    let parsed_extra = parsed.extra_header.unwrap();
    assert_eq!(parsed_extra.chip_clocks.len(), 1);
    assert_eq!(parsed_extra.chip_clocks[0], (1u8, 12345u32));
    assert_eq!(parsed_extra.chip_volumes.len(), 1);
    assert_eq!(parsed_extra.chip_volumes[0], (2u8, 0x01u8, 1000u16));
}

#[test]
fn test_parse_error_extra_header_offset_out_of_range() {
    use soundlog::ParseError;
    use soundlog::VgmBuilder;
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::command::WaitSamples;

    // Build a simple document with an extra header and serialize it.
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(1));
    let extra = VgmExtraHeader {
        header_size: 0,
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: vec![(1u8, 12345u32)],
        chip_volumes: vec![(2u8, 0x01u8, 1000u16)],
    };
    builder.set_extra_header(extra);
    let doc = builder.finalize();
    let mut serialized: Vec<u8> = (&doc).into();

    // Corrupt the stored extra_header_offset so it points outside the file.
    let bad_offset: u32 = 0xFFFF_FF00;
    serialized[0xBC..0xC0].copy_from_slice(&bad_offset.to_le_bytes());
    let expected_start = bad_offset.wrapping_add(0xBC) as usize;

    // Parsing should fail with OffsetOutOfRange for the computed start.
    let res: Result<VgmDocument, ParseError> = serialized.as_slice().try_into();
    assert!(
        res.is_err(),
        "parser unexpectedly succeeded on corrupted offset"
    );
    match res.unwrap_err() {
        ParseError::OffsetOutOfRange {
            offset,
            needed,
            available,
            context,
        } => {
            assert_eq!(offset, expected_start);
            // Sanity: available should equal buffer length
            assert_eq!(available, serialized.len());
            // needed should be at least 1
            assert!(needed >= 1);
            // Context should be present (we expect meta start context)
            assert!(context.is_some());
        }
        e => panic!("expected OffsetOutOfRange, got {:?}", e),
    }
}

#[test]
fn test_parse_error_extra_header_chip_clock_offset_out_of_range() {
    use soundlog::ParseError;
    use soundlog::VgmBuilder;
    use soundlog::VgmDocument;
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::command::WaitSamples;

    // Build and serialize a document with an extra header.
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(1));
    let extra = VgmExtraHeader {
        header_size: 0,
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: vec![(1u8, 12345u32)],
        chip_volumes: vec![],
    };
    builder.set_extra_header(extra);
    let doc = builder.finalize();
    let mut serialized: Vec<u8> = (&doc).into();

    // Compute extra_start from stored offset and then set chip_clock_offset to point past EOF.
    let stored_offset = u32::from_le_bytes(serialized[0xBC..0xC0].try_into().unwrap());
    let extra_start = stored_offset.wrapping_add(0xBC_u32) as usize;
    let bad_clock_offset: u32 = (serialized.len() as u32).wrapping_add(1000);
    serialized[extra_start + 4..extra_start + 8].copy_from_slice(&bad_clock_offset.to_le_bytes());

    // Parsing should fail when attempting to read the chip clock block.
    let res: Result<VgmDocument, ParseError> = serialized.as_slice().try_into();
    assert!(
        res.is_err(),
        "parser unexpectedly succeeded on bad chip_clock_offset"
    );
    match res.unwrap_err() {
        ParseError::OffsetOutOfRange { offset, .. } => {
            let expected = extra_start.wrapping_add(bad_clock_offset as usize);
            assert_eq!(offset, expected);
        }
        e => panic!("expected OffsetOutOfRange, got {:?}", e),
    }
}

#[test]
fn test_parse_error_extra_header_chip_vol_offset_out_of_range() {
    use soundlog::ParseError;
    use soundlog::VgmBuilder;
    use soundlog::VgmDocument;
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::command::WaitSamples;

    // Build and serialize a document with an extra header that contains volumes.
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(1));
    let extra = VgmExtraHeader {
        header_size: 0,
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: vec![],
        chip_volumes: vec![(2u8, 0x01u8, 1000u16)],
    };
    builder.set_extra_header(extra);
    let doc = builder.finalize();
    let mut serialized: Vec<u8> = (&doc).into();

    // Compute extra_start from stored offset and then set chip_vol_offset to point past EOF.
    let stored_offset = u32::from_le_bytes(serialized[0xBC..0xC0].try_into().unwrap());
    let extra_start = stored_offset.wrapping_add(0xBC_u32) as usize;
    let bad_vol_offset: u32 = (serialized.len() as u32).wrapping_add(5000);
    serialized[extra_start + 8..extra_start + 12].copy_from_slice(&bad_vol_offset.to_le_bytes());

    // Parsing should fail when attempting to read the chip volume block.
    let res: Result<VgmDocument, ParseError> = serialized.as_slice().try_into();
    assert!(
        res.is_err(),
        "parser unexpectedly succeeded on bad chip_vol_offset"
    );
    match res.unwrap_err() {
        ParseError::OffsetOutOfRange { offset, .. } => {
            let expected = extra_start.wrapping_add(bad_vol_offset as usize);
            assert_eq!(offset, expected);
        }
        e => panic!("expected OffsetOutOfRange, got {:?}", e),
    }
}

#[test]
fn test_parse_error_gd3_offset_out_of_range() {
    use soundlog::ParseError;
    use soundlog::VgmBuilder;
    use soundlog::VgmDocument;
    use soundlog::vgm::command::WaitSamples;

    // Build and serialize a simple document (no GD3 initially).
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(1));
    let doc = builder.finalize();
    let mut serialized: Vec<u8> = (&doc).into();

    // Corrupt GD3 offset at 0x14..0x18 to point beyond EOF.
    let bad_gd3_offset: u32 = (serialized.len() as u32).wrapping_add(0x1000);
    serialized[0x14..0x18].copy_from_slice(&bad_gd3_offset.to_le_bytes());

    // Parsing should fail with OffsetOutOfRange(gd3_start)
    let res: Result<VgmDocument, ParseError> = serialized.as_slice().try_into();
    assert!(
        res.is_err(),
        "parser unexpectedly succeeded on bad gd3_offset"
    );
    match res.unwrap_err() {
        ParseError::OffsetOutOfRange { offset, .. } => {
            let expected = bad_gd3_offset.wrapping_add(0x14) as usize;
            assert_eq!(offset, expected);
        }
        e => panic!("expected OffsetOutOfRange, got {:?}", e),
    }
}
