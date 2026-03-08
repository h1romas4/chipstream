use soundlog::VgmBuilder;
use soundlog::VgmDocument;
use soundlog::vgm::command::{DataBlock, EndOfData, VgmCommand};
use soundlog::vgm::detail::{
    BitPackingCompression, BitPackingSubType, CompressedStream, CompressedStreamData,
    CompressionType, DecompressionTable, RamWrite16, RamWrite32, RomRamDump, StreamChipType,
    UncompressedStream,
};
use soundlog::vgm::stream::{StreamResult, VgmStream};

/// Helper: push a document into a VgmStream and drain until NeedsMoreData/EndOfStream,
/// collecting any DataBlock commands returned by the iterator.
fn collect_returned_datablocks(doc: VgmDocument) -> Vec<DataBlock> {
    let bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    let mut found = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::DataBlock(block) = cmd {
                    found.push(*block);
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }
    found
}

#[test]
fn test_handle_data_block_uncompressed_stream_is_stored() {
    let mut builder = VgmBuilder::new();
    // Uncompressed stream type 0x00 (YM2612 PCM)
    let stream_data = vec![0x01u8, 0x02u8, 0x03u8];
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    });
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    // Drain iterator and ensure no DataBlock command for data_type 0x00 is returned
    let mut returned_types = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::DataBlock(block) = cmd {
                    returned_types.push(block.data_type);
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    // Uncompressed stream should be stored internally and not returned to iterator
    assert!(
        !returned_types.contains(&0x00),
        "Uncompressed stream should not be returned to iterator"
    );
    assert!(
        parser.get_uncompressed_stream(0x00).is_some(),
        "Uncompressed stream should be stored"
    );
}

#[test]
fn test_handle_data_block_uncompressed_stream_is_stored_attach() {
    let mut builder = VgmBuilder::new();

    let stream = UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0x01u8, 0x02u8, 0x03u8],
    };

    // Use attach_data_block helper which uses detail -> DataBlock construction.
    builder.attach_data_block(stream);
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    // Drain iterator and ensure stored
    let mut returned_types = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::DataBlock(block) = cmd {
                    returned_types.push(block.data_type);
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    assert!(
        !returned_types.contains(&0x00),
        "Uncompressed stream (attach) should not be returned to iterator"
    );
    assert!(
        parser.get_uncompressed_stream(0x00).is_some(),
        "Uncompressed stream (attach) should be stored"
    );
}

#[test]
fn test_handle_data_block_decompression_table_is_stored() {
    let mut builder = VgmBuilder::new();
    // Decompression table type 0x7F (minimal valid header for tests)
    // Format: compression_type, sub_type, bits_decompressed, bits_compressed, value_count (le), table bytes...
    let table_data = vec![
        0x00u8, 0x02u8, 0x08u8, 0x04u8, 0x02u8, 0x00u8, 0x10u8, 0x11u8,
    ];
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x7F,
        size: table_data.len() as u32,
        data: table_data.clone(),
    });
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    // Process until NeedsMoreData / EndOfStream
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    assert!(
        parser.get_decompression_table(0x7F).is_some(),
        "Decompression table should be stored"
    );
}

#[test]
fn test_handle_data_block_decompression_table_is_stored_attach() {
    let mut builder = VgmBuilder::new();

    let table = DecompressionTable {
        compression_type: CompressionType::BitPacking,
        sub_type: 0x02,
        bits_decompressed: 0x08,
        bits_compressed: 0x04,
        value_count: 2,
        table_data: vec![0x10u8, 0x11u8],
    };

    builder.attach_data_block(table);
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    assert!(
        parser.get_decompression_table(0x7F).is_some(),
        "Decompression table (attach) should be stored"
    );
}

#[test]
fn test_handle_data_block_compressed_stream_is_decompressed_and_stored() {
    let mut builder = VgmBuilder::new();
    // Compressed stream type 0x40 (example). We'll construct a minimal BitPacking
    // block that includes a small payload so the parser takes the compressed branch.
    //
    // Layout expected by parser (see detail.rs tests):
    // [compression_type=0x00(BitPacking),
    //  uncompressed_size (4le),
    //  bits_decompressed,
    //  bits_compressed,
    //  sub_type,
    //  add_value (2le),
    //  payload...]
    let mut payload = Vec::new();
    payload.push(0x00u8); // BitPacking
    payload.extend_from_slice(&0u32.to_le_bytes()); // uncompressed_size = 0
    payload.push(8u8); // bits_decompressed
    payload.push(4u8); // bits_compressed
    payload.push(0x00u8); // sub_type = Copy
    payload.extend_from_slice(&0u16.to_le_bytes()); // add_value = 0
    // add one byte of compressed payload so parser sees >= 11 bytes for BitPacking case
    payload.push(0xFFu8);

    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x40,
        size: payload.len() as u32,
        data: payload.clone(),
    });
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();

    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    // Drain iterator; compressed stream should be processed and stored as uncompressed.
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    // After processing, the uncompressed stream entry for 0x40 should exist
    assert!(
        parser.get_uncompressed_stream(0x40).is_some(),
        "Compressed stream should be decompressed and stored as uncompressed stream"
    );
}

#[test]
fn test_handle_data_block_compressed_stream_is_decompressed_and_stored_attach() {
    let mut builder = VgmBuilder::new();

    let bp = BitPackingCompression {
        bits_decompressed: 8,
        bits_compressed: 4,
        sub_type: BitPackingSubType::Copy,
        add_value: 0,
        data: vec![0xFFu8], // ensure there's payload
    };

    let stream = CompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        compression_type: CompressionType::BitPacking,
        uncompressed_size: 0,
        compression: CompressedStreamData::BitPacking(bp),
    };

    // Use attach_data_block to construct the DataBlock bytes from detail type.
    builder.attach_data_block(stream);
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();

    let mut parser = VgmStream::new();
    parser.push_chunk(&bytes).expect("push chunk");

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    assert!(
        parser.get_uncompressed_stream(0x40).is_some(),
        "Compressed stream (attach) should be decompressed and stored as uncompressed stream"
    );
}

#[test]
fn test_handle_data_block_rom_ram_dump_is_returned() {
    let mut builder = VgmBuilder::new();
    // ROM/RAM dump type 0x80 (Sega PCM ROM)
    // Format: rom_size (4 le), start_address (4 le), data...
    let mut full = Vec::new();
    full.extend_from_slice(&4096u32.to_le_bytes());
    full.extend_from_slice(&0u32.to_le_bytes());
    full.extend_from_slice(&[0x48u8, 0x65u8, 0x6Cu8]); // "Hel"
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x80,
        size: full.len() as u32,
        data: full.clone(),
    });
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let returned = collect_returned_datablocks(doc);
    // Should return exactly one DataBlock of type 0x80 with identical payload
    assert!(
        returned
            .iter()
            .any(|b| b.data_type == 0x80 && b.data == full)
    );
}

#[test]
fn test_handle_data_block_rom_ram_dump_is_returned_attach() {
    let mut builder = VgmBuilder::new();

    let dump = RomRamDump {
        chip_type: soundlog::vgm::detail::RomRamChipType::SegaPcmRom,
        rom_size: 4096,
        start_address: 0,
        data: vec![0x48u8, 0x65u8, 0x6Cu8],
    };

    builder.attach_data_block(dump);
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let returned = collect_returned_datablocks(doc);

    // The attach path serializes rom_size + start_address + data into the DataBlock::data
    let mut expected = Vec::new();
    expected.extend_from_slice(&4096u32.to_le_bytes());
    expected.extend_from_slice(&0u32.to_le_bytes());
    expected.extend_from_slice(&[0x48u8, 0x65u8, 0x6Cu8]);

    assert!(
        returned
            .iter()
            .any(|b| b.data_type == 0x80 && b.data == expected),
        "attach() path should produce identical serialized RomRamDump payload"
    );
}

#[test]
fn test_handle_data_block_ram_write16_and_32_are_returned() {
    let mut builder = VgmBuilder::new();

    // RamWrite16 type 0xC0: start_address (2 le) + data
    let mut d16 = Vec::new();
    d16.extend_from_slice(&0x1000u16.to_le_bytes());
    d16.extend_from_slice(&[0xAAu8, 0xBBu8]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xC0,
        size: d16.len() as u32,
        data: d16.clone(),
    });

    // RamWrite32 type 0xE0: start_address (4 le) + data
    let mut d32 = Vec::new();
    d32.extend_from_slice(&0x0001_0000u32.to_le_bytes());
    d32.extend_from_slice(&[0x01u8, 0x02u8, 0x03u8]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xE0,
        size: d32.len() as u32,
        data: d32.clone(),
    });

    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let returned = collect_returned_datablocks(doc);

    let mut found_c0 = false;
    let mut found_e0 = false;
    for b in returned {
        if b.data_type == 0xC0 {
            assert_eq!(b.data, d16);
            found_c0 = true;
        } else if b.data_type == 0xE0 {
            assert_eq!(b.data, d32);
            found_e0 = true;
        }
    }

    assert!(
        found_c0,
        "RamWrite16 should be returned to iterator as DataBlock"
    );
    assert!(
        found_e0,
        "RamWrite32 should be returned to iterator as DataBlock"
    );
}

#[test]
fn test_handle_data_block_ram_write16_and_32_are_returned_attach() {
    let mut builder = VgmBuilder::new();

    let write16 = RamWrite16 {
        chip_type: soundlog::vgm::detail::RamWrite16ChipType::Rf5c68,
        start_address: 0x1000,
        data: vec![0xAAu8, 0xBBu8],
    };

    let write32 = RamWrite32 {
        chip_type: soundlog::vgm::detail::RamWrite32ChipType::Scsp,
        start_address: 0x0001_0000,
        data: vec![0x01u8, 0x02u8, 0x03u8],
    };

    builder.attach_data_block(write16);
    builder.attach_data_block(write32);
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let returned = collect_returned_datablocks(doc);

    let mut found_c0 = false;
    let mut found_e0 = false;
    for b in returned {
        if b.data_type == 0xC0 {
            // start address + data
            found_c0 = true;
        } else if b.data_type == 0xE0 {
            found_e0 = true;
        }
    }

    assert!(
        found_c0,
        "RamWrite16 (attach) should be returned to iterator as DataBlock"
    );
    assert!(
        found_e0,
        "RamWrite32 (attach) should be returned to iterator as DataBlock"
    );
}

#[test]
fn test_handle_data_block_parse_failure_returns_raw_block() {
    let mut builder = VgmBuilder::new();
    // Intentionally construct a Rom/Ram dump block with insufficient length to trigger parse failure.
    // Rom/Ram dump expects at least 8 bytes for rom_size + start_address.
    let raw = vec![0x00u8, 0x01u8, 0x02u8]; // too short
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x80,
        size: raw.len() as u32,
        data: raw.clone(),
    });
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let returned = collect_returned_datablocks(doc);

    // The original (raw) block should be returned unchanged when parsing fails.
    assert!(
        returned
            .iter()
            .any(|b| b.data_type == 0x80 && b.data == raw),
        "Failed parse should return the original raw DataBlock"
    );
}

/// Test: ensure `create_stream_write_command_static` mapping for multiple `ChipId` values
/// is exercised end-to-end by configuring a DAC stream that reads one byte from an
/// attached PCM bank and emits a single write command targeting the configured chip.
///
/// The test iterates a selection of `ChipId` variants, configures a stream (SetupStreamControl,
/// SetStreamData, SetStreamFrequency, StartStream) which causes the stream processing code
/// to invoke the internal `create_stream_write_command_static`. The test then inspects the
/// produced `VgmCommand` and maps it back to a `ChipId` to assert the emitted command type
/// corresponds to the requested `ChipId`.
#[test]
fn test_create_stream_write_command_static_chipid_mapping() {
    use soundlog::vgm::command::DacStreamChipType;
    use soundlog::vgm::command::VgmCommand;
    use soundlog::vgm::command::{
        EndOfData, LengthMode, SetStreamData, SetStreamFrequency, SetupStreamControl, StartStream,
    };
    use soundlog::vgm::header::ChipId;
    use soundlog::vgm::stream::VgmStream;

    // List of ChipIds to exercise. This covers the branch mapping implemented in
    // `create_stream_write_command_static`. We choose a representative set that
    // appear in the mapping.
    let chip_ids = vec![
        ChipId::Sn76489,
        ChipId::Ym2413,
        ChipId::Ym2612,
        ChipId::Ym2151,
        ChipId::SegaPcm,
        ChipId::Rf5c68,
        ChipId::Ym2203,
        ChipId::Ym2608,
        ChipId::Ym2610,
        ChipId::Ym3812,
        ChipId::Ym3526,
        ChipId::Y8950,
        ChipId::Ymf262,
        ChipId::Ymf278b,
        ChipId::Ymf271,
        ChipId::Ymz280b,
        ChipId::Rf5c164,
        ChipId::Pwm,
        ChipId::Ay8910,
        ChipId::GbDmg,
        ChipId::NesApu,
        ChipId::MultiPcm,
        ChipId::Upd7759,
        ChipId::Okim6258,
        ChipId::Okim6295,
        ChipId::K051649,
        ChipId::K054539,
        ChipId::Huc6280,
        ChipId::C140,
        ChipId::K053260,
        ChipId::Pokey,
        ChipId::Qsound,
        ChipId::Scsp,
        ChipId::WonderSwan,
        ChipId::Vsu,
        ChipId::Saa1099,
        ChipId::Es5503,
        ChipId::Es5506,
        ChipId::X1010,
        ChipId::C352,
        ChipId::Ga20,
        ChipId::Mikey,
    ];

    // Helper to map an observed write command back to the ChipId it corresponds to.
    fn chipid_from_command(cmd: &VgmCommand) -> Option<ChipId> {
        match cmd {
            VgmCommand::Sn76489Write(_, _) => Some(ChipId::Sn76489),
            VgmCommand::Ym2413Write(_, _) => Some(ChipId::Ym2413),
            VgmCommand::Ym2612Write(_, _) => Some(ChipId::Ym2612),
            VgmCommand::Ym2151Write(_, _) => Some(ChipId::Ym2151),
            VgmCommand::SegaPcmWrite(_, _) => Some(ChipId::SegaPcm),
            VgmCommand::Rf5c68U8Write(_, _) => Some(ChipId::Rf5c68),
            VgmCommand::Ym2203Write(_, _) => Some(ChipId::Ym2203),
            VgmCommand::Ym2608Write(_, _) => Some(ChipId::Ym2608),
            VgmCommand::Ym2610bWrite(_, _) => Some(ChipId::Ym2610),
            VgmCommand::Ym3812Write(_, _) => Some(ChipId::Ym3812),
            VgmCommand::Ym3526Write(_, _) => Some(ChipId::Ym3526),
            VgmCommand::Y8950Write(_, _) => Some(ChipId::Y8950),
            VgmCommand::Ymf262Write(_, _) => Some(ChipId::Ymf262),
            VgmCommand::Ymf278bWrite(_, _) => Some(ChipId::Ymf278b),
            VgmCommand::Ymf271Write(_, _) => Some(ChipId::Ymf271),
            VgmCommand::Ymz280bWrite(_, _) => Some(ChipId::Ymz280b),
            VgmCommand::Rf5c164U8Write(_, _) => Some(ChipId::Rf5c164),
            VgmCommand::PwmWrite(_, _) => Some(ChipId::Pwm),
            VgmCommand::Ay8910Write(_, _) => Some(ChipId::Ay8910),
            VgmCommand::GbDmgWrite(_, _) => Some(ChipId::GbDmg),
            VgmCommand::NesApuWrite(_, _) => Some(ChipId::NesApu),
            VgmCommand::MultiPcmWrite(_, _) => Some(ChipId::MultiPcm),
            VgmCommand::Upd7759Write(_, _) => Some(ChipId::Upd7759),
            VgmCommand::Okim6258Write(_, _) => Some(ChipId::Okim6258),
            VgmCommand::Okim6295Write(_, _) => Some(ChipId::Okim6295),
            VgmCommand::Scc1Write(_, _) => Some(ChipId::K051649),
            VgmCommand::K054539Write(_, _) => Some(ChipId::K054539),
            VgmCommand::Huc6280Write(_, _) => Some(ChipId::Huc6280),
            VgmCommand::C140Write(_, _) => Some(ChipId::C140),
            VgmCommand::K053260Write(_, _) => Some(ChipId::K053260),
            VgmCommand::PokeyWrite(_, _) => Some(ChipId::Pokey),
            VgmCommand::QsoundWrite(_, _) => Some(ChipId::Qsound),
            VgmCommand::ScspWrite(_, _) => Some(ChipId::Scsp),
            VgmCommand::WonderSwanWrite(_, _) => Some(ChipId::WonderSwan),
            VgmCommand::VsuWrite(_, _) => Some(ChipId::Vsu),
            VgmCommand::Saa1099Write(_, _) => Some(ChipId::Saa1099),
            VgmCommand::Es5503Write(_, _) => Some(ChipId::Es5503),
            VgmCommand::Es5506BEWrite(_, _) => Some(ChipId::Es5506),
            VgmCommand::X1010Write(_, _) => Some(ChipId::X1010),
            VgmCommand::C352Write(_, _) => Some(ChipId::C352),
            VgmCommand::Ga20Write(_, _) => Some(ChipId::Ga20),
            VgmCommand::MikeyWrite(_, _) => Some(ChipId::Mikey),
            _ => None,
        }
    }

    // Iterate chip ids and exercise the stream write generation
    for chip_id in chip_ids.into_iter() {
        // Build a VGM with:
        // - an attached PCM bank (uncompressed stream type 0x00) with a single byte payload
        // - SetupStreamControl selecting the requested chip_id
        // - SetStreamData referring to bank 0
        // - SetStreamFrequency non-zero (so the stream is considered active)
        // - StartStream that requests one command's worth (CommandCount = 1)
        let mut b = soundlog::VgmBuilder::new();

        // Attach a simple PCM bank.
        // chip_type (StreamChipType) is the label for the data bank slot (data block types
        // 0x00..0x3F) and determines the on-disk data_type byte of the DataBlock command.
        // In the Stream Control path (0x90-0x95), VgmStream only reads the raw byte payload
        // and never inspects chip_type, so the choice here does not affect the mapping under
        // test. Unknown(0x3F) makes this independence explicit by avoiding any named chip.
        // data_bank_id in SetStreamData must match the data_type derived from chip_type,
        // i.e. u8::from(StreamChipType::Unknown(0x3F)) == 0x3F.
        use soundlog::vgm::detail::{StreamChipType, UncompressedStream};
        b.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Unknown(0x3F),
            data: vec![0x7Fu8],
        });

        // Setup stream control for stream_id 0 and the target chip_id
        let dac_type = DacStreamChipType::new(chip_id, soundlog::vgm::command::Instance::Primary);
        b.add_vgm_command(SetupStreamControl {
            stream_id: 0,
            chip_type: dac_type,
            write_port: 0,
            write_command: 0,
        });

        // Point stream 0 at data bank 0x3F (matches chip_type: Unknown(0x3F) above) with step_size=1
        b.add_vgm_command(SetStreamData {
            stream_id: 0,
            data_bank_id: 0x3F,
            step_size: 1,
            step_base: 0,
        });

        // Set a valid frequency so stream will be processed
        b.add_vgm_command(SetStreamFrequency {
            stream_id: 0,
            frequency: 44100u32,
        });

        // Start the stream with CommandCount length mode requesting a single command
        b.add_vgm_command(StartStream {
            stream_id: 0,
            data_start_offset: 0,
            length_mode: LengthMode::CommandCount {
                reverse: false,
                looped: false,
            },
            data_length: 1,
        });

        // Insert a small wait so stream processing has a chance to generate writes
        // (stream writes are produced during wait processing; without a Wait the
        // StartStream may not immediately yield writes).
        b.add_vgm_command(soundlog::vgm::command::WaitSamples(1));

        // End of data
        b.add_vgm_command(EndOfData);

        let doc = b.finalize();
        let mut parser = VgmStream::from_document(doc);

        // Drain until we find a write-like command (not DataBlock / EndOfData / WaitSamples).
        let mut observed: Option<VgmCommand> = None;
        for result in &mut parser {
            match result {
                Ok(StreamResult::Command(cmd)) => match &cmd {
                    VgmCommand::DataBlock(_) => continue,
                    VgmCommand::WaitSamples(_) => continue,
                    VgmCommand::EndOfData(_) => continue,
                    _ => {
                        observed = Some(cmd);
                        break;
                    }
                },
                Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
                Err(e) => panic!("stream error: {e:?}"),
            }
        }

        let cmd = observed.expect("expected a write command to be produced for configured chip");
        let produced = chipid_from_command(&cmd).expect("produced command must map to a ChipId");
        // Debug output to help diagnose mapping failures in CI logs:
        // - show requested ChipId, the produced VgmCommand, and the mapped ChipId
        println!("DEBUG: requested ChipId = {:?}", chip_id);
        println!("DEBUG: produced VgmCommand = {:?}", cmd);
        println!("DEBUG: produced mapped ChipId = {:?}", produced);
        assert_eq!(
            produced, chip_id,
            "Emitted command type should correspond to requested ChipId ({:?}); produced command: {:?}",
            chip_id, cmd
        );
    }
}
