use soundlog::vgm::command::DataBlock;
use soundlog::vgm::detail::*;

#[test]
fn test_parse_uncompressed_stream_ym2612() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00, // YM2612 PCM
        size: 4,
        data: vec![0x01, 0x02, 0x03, 0x04],
    };

    let result = parse_data_block(block).expect("Failed to parse uncompressed stream");

    match result {
        DataBlockType::UncompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::Ym2612Pcm);
            assert_eq!(stream.data, vec![0x01, 0x02, 0x03, 0x04]);
        }
        _ => panic!("Expected UncompressedStream"),
    }
}

#[test]
fn test_parse_uncompressed_stream_rf5c68() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x01, // RF5C68 PCM
        size: 3,
        data: vec![0xAA, 0xBB, 0xCC],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::UncompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::Rf5c68Pcm);
            assert_eq!(stream.data, vec![0xAA, 0xBB, 0xCC]);
        }
        _ => panic!("Expected UncompressedStream"),
    }
}

#[test]
fn test_parse_uncompressed_stream_unknown() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x20, // Unknown stream type
        size: 2,
        data: vec![0xFF, 0xEE],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::UncompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::Unknown(0x20));
            assert_eq!(stream.data, vec![0xFF, 0xEE]);
        }
        _ => panic!("Expected UncompressedStream"),
    }
}

#[test]
fn test_parse_compressed_stream_bit_packing() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x40, // Compressed YM2612 PCM
        size: 14,
        data: vec![
            0x00, // compression_type: BitPacking
            0x00, 0x10, 0x00, 0x00, // uncompressed_size: 4096
            0x08, // bits_decompressed: 8
            0x04, // bits_compressed: 4
            0x00, // sub_type: Copy
            0x10, 0x00, // add_value: 16
            0x12, 0x34, 0x56, 0x78, // compressed data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::CompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::Ym2612Pcm);
            assert_eq!(stream.compression_type, CompressionType::BitPacking);
            assert_eq!(stream.uncompressed_size, 4096);

            match stream.compression {
                CompressedStreamData::BitPacking(bp) => {
                    assert_eq!(bp.bits_decompressed, 8);
                    assert_eq!(bp.bits_compressed, 4);
                    assert_eq!(bp.sub_type, BitPackingSubType::Copy);
                    assert_eq!(bp.add_value, 16);
                    assert_eq!(bp.data, vec![0x12, 0x34, 0x56, 0x78]);
                }
                _ => panic!("Expected BitPacking compression"),
            }
        }
        _ => panic!("Expected CompressedStream"),
    }
}

#[test]
fn test_parse_compressed_stream_dpcm() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x46, // Compressed SCSP PCM
        size: 13,
        data: vec![
            0x01, // compression_type: DPCM
            0x00, 0x20, 0x00, 0x00, // uncompressed_size: 8192
            0x10, // bits_decompressed: 16
            0x08, // bits_compressed: 8
            0x00, // reserved
            0x80, 0x00, // start_value: 128
            0xAB, 0xCD, 0xEF, // compressed data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::CompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::ScspPcm);
            assert_eq!(stream.compression_type, CompressionType::Dpcm);
            assert_eq!(stream.uncompressed_size, 8192);

            match stream.compression {
                CompressedStreamData::Dpcm(dpcm) => {
                    assert_eq!(dpcm.bits_decompressed, 16);
                    assert_eq!(dpcm.bits_compressed, 8);
                    assert_eq!(dpcm.reserved, 0);
                    assert_eq!(dpcm.start_value, 128);
                    assert_eq!(dpcm.data, vec![0xAB, 0xCD, 0xEF]);
                }
                _ => panic!("Expected DPCM compression"),
            }
        }
        _ => panic!("Expected CompressedStream"),
    }
}

#[test]
fn test_parse_compressed_stream_unknown_compression() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x45, // Compressed HuC6280 PCM
        size: 10,
        data: vec![
            0xFF, // compression_type: Unknown
            0x00, 0x10, 0x00, 0x00, // uncompressed_size: 4096
            0x11, 0x22, 0x33, 0x44, 0x55, // raw data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::CompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::Huc6280Pcm);
            assert_eq!(stream.compression_type, CompressionType::Unknown(0xFF));

            match stream.compression {
                CompressedStreamData::Unknown {
                    compression_type,
                    data,
                } => {
                    assert_eq!(compression_type, 0xFF);
                    assert_eq!(data, vec![0x11, 0x22, 0x33, 0x44, 0x55]);
                }
                _ => panic!("Expected Unknown compression"),
            }
        }
        _ => panic!("Expected CompressedStream"),
    }
}

#[test]
fn test_parse_decompression_table() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x7F, // Decompression table
        size: 12,
        data: vec![
            0x00, // compression_type: BitPacking
            0x02, // sub_type: UseTable
            0x08, // bits_decompressed: 8
            0x04, // bits_compressed: 4
            0x10, 0x00, // value_count: 16
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // table data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::DecompressionTable(table) => {
            assert_eq!(table.compression_type, CompressionType::BitPacking);
            assert_eq!(table.sub_type, 0x02);
            assert_eq!(table.bits_decompressed, 8);
            assert_eq!(table.bits_compressed, 4);
            assert_eq!(table.value_count, 16);
            assert_eq!(table.table_data, vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        }
        _ => panic!("Expected DecompressionTable"),
    }
}

#[test]
fn test_parse_rom_ram_dump_sega_pcm() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x80, // Sega PCM ROM
        size: 16,
        data: vec![
            0x00, 0x10, 0x00, 0x00, // rom_size: 4096
            0x00, 0x00, 0x00, 0x00, // start_address: 0
            0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x21, 0x21, 0x21, // ROM data: "Hello!!!"
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RomRamDump(dump) => {
            assert_eq!(dump.chip_type, RomRamChipType::SegaPcmRom);
            assert_eq!(dump.rom_size, 4096);
            assert_eq!(dump.start_address, 0);
            assert_eq!(
                dump.data,
                vec![0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x21, 0x21, 0x21]
            );
        }
        _ => panic!("Expected RomRamDump"),
    }
}

#[test]
fn test_parse_rom_ram_dump_ym2608() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x81, // YM2608 DELTA-T ROM
        size: 12,
        data: vec![
            0x00, 0x80, 0x00, 0x00, // rom_size: 32768
            0x00, 0x40, 0x00, 0x00, // start_address: 16384
            0xDE, 0xAD, 0xBE, 0xEF, // ROM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RomRamDump(dump) => {
            assert_eq!(dump.chip_type, RomRamChipType::Ym2608DeltaTRom);
            assert_eq!(dump.rom_size, 32768);
            assert_eq!(dump.start_address, 16384);
            assert_eq!(dump.data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        }
        _ => panic!("Expected RomRamDump"),
    }
}

#[test]
fn test_parse_rom_ram_dump_unknown() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xA0, // Unknown ROM type
        size: 10,
        data: vec![
            0xFF, 0xFF, 0x00, 0x00, // rom_size: 65535
            0x00, 0x00, 0x00, 0x00, // start_address: 0
            0x11, 0x22, // ROM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RomRamDump(dump) => {
            assert_eq!(dump.chip_type, RomRamChipType::Unknown(0xA0));
            assert_eq!(dump.rom_size, 65535);
            assert_eq!(dump.start_address, 0);
            assert_eq!(dump.data, vec![0x11, 0x22]);
        }
        _ => panic!("Expected RomRamDump"),
    }
}

#[test]
fn test_parse_ram_write_16bit_rf5c68() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xC0, // RF5C68 RAM write
        size: 6,
        data: vec![
            0x00, 0x10, // start_address: 4096
            0xAA, 0xBB, 0xCC, 0xDD, // RAM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RamWrite16(ram) => {
            assert_eq!(ram.chip_type, RamWrite16ChipType::Rf5c68);
            assert_eq!(ram.start_address, 4096);
            assert_eq!(ram.data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        }
        _ => panic!("Expected RamWrite16"),
    }
}

#[test]
fn test_parse_ram_write_16bit_nes_apu() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xC2, // NES APU RAM write
        size: 4,
        data: vec![
            0x00, 0x40, // start_address: 16384
            0x12, 0x34, // RAM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RamWrite16(ram) => {
            assert_eq!(ram.chip_type, RamWrite16ChipType::NesApu);
            assert_eq!(ram.start_address, 16384);
            assert_eq!(ram.data, vec![0x12, 0x34]);
        }
        _ => panic!("Expected RamWrite16"),
    }
}

#[test]
fn test_parse_ram_write_16bit_unknown() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xD0, // Unknown RAM write
        size: 5,
        data: vec![
            0xFF, 0xFF, // start_address: 65535
            0x11, 0x22, 0x33, // RAM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RamWrite16(ram) => {
            assert_eq!(ram.chip_type, RamWrite16ChipType::Unknown(0xD0));
            assert_eq!(ram.start_address, 65535);
            assert_eq!(ram.data, vec![0x11, 0x22, 0x33]);
        }
        _ => panic!("Expected RamWrite16"),
    }
}

#[test]
fn test_parse_ram_write_32bit_scsp() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xE0, // SCSP RAM write
        size: 8,
        data: vec![
            0x00, 0x00, 0x01, 0x00, // start_address: 65536
            0x01, 0x02, 0x03, 0x04, // RAM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RamWrite32(ram) => {
            assert_eq!(ram.chip_type, RamWrite32ChipType::Scsp);
            assert_eq!(ram.start_address, 65536);
            assert_eq!(ram.data, vec![0x01, 0x02, 0x03, 0x04]);
        }
        _ => panic!("Expected RamWrite32"),
    }
}

#[test]
fn test_parse_ram_write_32bit_es5503() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xE1, // ES5503 RAM write
        size: 6,
        data: vec![
            0x00, 0x10, 0x00, 0x00, // start_address: 4096
            0xFE, 0xED, // RAM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RamWrite32(ram) => {
            assert_eq!(ram.chip_type, RamWrite32ChipType::Es5503);
            assert_eq!(ram.start_address, 4096);
            assert_eq!(ram.data, vec![0xFE, 0xED]);
        }
        _ => panic!("Expected RamWrite32"),
    }
}

#[test]
fn test_parse_ram_write_32bit_unknown() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xF0, // Unknown RAM write
        size: 7,
        data: vec![
            0xFF, 0xFF, 0xFF, 0xFF, // start_address: 4294967295
            0xCA, 0xFE, 0xBA, // RAM data
        ],
    };

    let result = parse_data_block(block).expect("Failed to parse");

    match result {
        DataBlockType::RamWrite32(ram) => {
            assert_eq!(ram.chip_type, RamWrite32ChipType::Unknown(0xF0));
            assert_eq!(ram.start_address, 4294967295);
            assert_eq!(ram.data, vec![0xCA, 0xFE, 0xBA]);
        }
        _ => panic!("Expected RamWrite32"),
    }
}

#[test]
fn test_parse_error_insufficient_data_compressed() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x40, // Compressed stream
        size: 3,
        data: vec![0x00, 0x01, 0x02], // Too short for compressed stream header
    };

    let result = parse_data_block(block);
    assert!(result.is_err(), "Expected error for insufficient data");
}

#[test]
fn test_parse_error_insufficient_data_rom_dump() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x80, // ROM dump
        size: 4,
        data: vec![0x00, 0x01, 0x02, 0x03], // Too short for ROM dump header
    };

    let result = parse_data_block(block);
    assert!(result.is_err(), "Expected error for insufficient data");
}

#[test]
fn test_parse_empty_data_uncompressed() {
    let block = DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00, // YM2612 PCM
        size: 0,
        data: vec![],
    };

    let result = parse_data_block(block).expect("Failed to parse empty data");

    match result {
        DataBlockType::UncompressedStream(stream) => {
            assert_eq!(stream.chip_type, StreamChipType::Ym2612Pcm);
            assert!(stream.data.is_empty());
        }
        _ => panic!("Expected UncompressedStream"),
    }
}

#[test]
fn test_bit_packing_sub_types() {
    // Test Copy sub-type
    assert_eq!(BitPackingSubType::from(0x00), BitPackingSubType::Copy);
    // Test ShiftLeft sub-type
    assert_eq!(BitPackingSubType::from(0x01), BitPackingSubType::ShiftLeft);
    // Test UseTable sub-type
    assert_eq!(BitPackingSubType::from(0x02), BitPackingSubType::UseTable);
    // Test Unknown sub-type
    assert_eq!(
        BitPackingSubType::from(0xFF),
        BitPackingSubType::Unknown(0xFF)
    );
}

#[test]
fn test_compression_types() {
    assert_eq!(CompressionType::from(0x00), CompressionType::BitPacking);
    assert_eq!(CompressionType::from(0x01), CompressionType::Dpcm);
    assert_eq!(CompressionType::from(0x99), CompressionType::Unknown(0x99));
}

#[test]
fn test_stream_chip_type_masking() {
    // Test that the upper bits are masked correctly for compressed streams
    assert_eq!(StreamChipType::from(0x40), StreamChipType::Ym2612Pcm); // 0x40 & 0x3F = 0x00
    assert_eq!(StreamChipType::from(0x41), StreamChipType::Rf5c68Pcm); // 0x41 & 0x3F = 0x01
    assert_eq!(StreamChipType::from(0x7E), StreamChipType::Unknown(0x3E)); // 0x7E & 0x3F = 0x3E
}

#[test]
fn test_bit_packing_decompress_copy() {
    let mut compression = BitPackingCompression {
        bits_decompressed: 8,
        bits_compressed: 4,
        sub_type: BitPackingSubType::Copy,
        add_value: 10,
        data: vec![0x12, 0x34], // 0001 0010 0011 0100 -> 1, 2, 3, 4 (4-bit values)
    };

    compression.decompress(None).expect("Decompression failed");

    // Each 4-bit value should be copied and add_value (10) added
    assert_eq!(compression.data, vec![11, 12, 13, 14]); // 1+10, 2+10, 3+10, 4+10
}

#[test]
fn test_bit_packing_decompress_shift_left() {
    let mut compression = BitPackingCompression {
        bits_decompressed: 8,
        bits_compressed: 4,
        sub_type: BitPackingSubType::ShiftLeft,
        add_value: 5,
        data: vec![0x12], // 0001 0010 -> 1, 2 (4-bit values)
    };

    compression.decompress(None).expect("Decompression failed");

    // Each 4-bit value should be shifted left by 4 bits and add_value (5) added
    assert_eq!(compression.data, vec![21, 37]); // (1<<4)+5, (2<<4)+5 = 16+5, 32+5
}

#[test]
fn test_bit_packing_decompress_use_table() {
    let table = DecompressionTable {
        compression_type: CompressionType::BitPacking,
        sub_type: 0x02,
        bits_decompressed: 8,
        bits_compressed: 4,
        value_count: 16,
        table_data: vec![
            100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115,
        ],
    };

    let mut compression = BitPackingCompression {
        bits_decompressed: 8,
        bits_compressed: 4,
        sub_type: BitPackingSubType::UseTable,
        add_value: 0,
        data: vec![0x01, 0x23], // indices: 0, 1, 2, 3
    };

    compression
        .decompress(Some(&table))
        .expect("Decompression failed");

    // Look up each index in the table
    assert_eq!(compression.data, vec![100, 101, 102, 103]);
}

#[test]
fn test_bit_packing_decompress_use_table_missing_table() {
    let mut compression = BitPackingCompression {
        bits_decompressed: 8,
        bits_compressed: 4,
        sub_type: BitPackingSubType::UseTable,
        add_value: 0,
        data: vec![0x01, 0x23],
    };

    let result = compression.decompress(None);
    assert!(
        result.is_err(),
        "Should fail when table is required but not provided"
    );
}

#[test]
fn test_dpcm_decompress() {
    let table = DecompressionTable {
        compression_type: CompressionType::Dpcm,
        sub_type: 0x00,
        bits_decompressed: 8,
        bits_compressed: 4,
        value_count: 16,
        table_data: vec![
            0, 1, 2, 3, 4, 5, 6, 7, 248, 249, 250, 251, 252, 253, 254,
            255, // -8, -7, -6, -5, -4, -3, -2, -1 as signed
        ],
    };

    let mut compression = DpcmCompression {
        bits_decompressed: 8,
        bits_compressed: 4,
        reserved: 0,
        start_value: 128,
        data: vec![0x01, 0x23], // delta indices: 0, 1, 2, 3
    };

    compression
        .decompress(&table)
        .expect("Decompression failed");

    // Start at 128, add deltas: 0, 1, 2, 3
    assert_eq!(compression.data, vec![128, 129, 131, 134]);
}

#[test]
fn test_bit_packing_decompress_16bit_values() {
    let mut compression = BitPackingCompression {
        bits_decompressed: 16,
        bits_compressed: 8,
        sub_type: BitPackingSubType::Copy,
        add_value: 1000,
        data: vec![0x10, 0x20], // 16, 32 (8-bit values)
    };

    compression.decompress(None).expect("Decompression failed");

    // Each 8-bit value + 1000, stored as 16-bit little-endian
    // 16 + 1000 = 1016 = 0x03F8 = [0xF8, 0x03]
    // 32 + 1000 = 1032 = 0x0408 = [0x08, 0x04]
    assert_eq!(compression.data, vec![0xF8, 0x03, 0x08, 0x04]);
}

#[test]
fn test_ay8910_stereo_mask_from_mask_all_enabled() {
    let mask = Ay8910StereoMaskDetail::from_mask(0b00111111);
    assert_eq!(mask.chip_instance, 0);
    assert_eq!(mask.is_ym2203, false);
    assert_eq!(mask.left_ch1, true);
    assert_eq!(mask.right_ch1, true);
    assert_eq!(mask.left_ch2, true);
    assert_eq!(mask.right_ch2, true);
    assert_eq!(mask.left_ch3, true);
    assert_eq!(mask.right_ch3, true);
}

#[test]
fn test_ay8910_stereo_mask_from_mask_instance1_ym2203() {
    let mask = Ay8910StereoMaskDetail::from_mask(0b11110011);
    assert_eq!(mask.chip_instance, 1);
    assert_eq!(mask.is_ym2203, true);
    assert_eq!(mask.left_ch1, true);
    assert_eq!(mask.right_ch1, true);
    assert_eq!(mask.left_ch2, false);
    assert_eq!(mask.right_ch2, false);
    assert_eq!(mask.left_ch3, true);
    assert_eq!(mask.right_ch3, true);
}

#[test]
fn test_ay8910_stereo_mask_from_mask_left_only() {
    let mask = Ay8910StereoMaskDetail::from_mask(0b00010101);
    assert_eq!(mask.chip_instance, 0);
    assert_eq!(mask.is_ym2203, false);
    assert_eq!(mask.left_ch1, true);
    assert_eq!(mask.right_ch1, false);
    assert_eq!(mask.left_ch2, true);
    assert_eq!(mask.right_ch2, false);
    assert_eq!(mask.left_ch3, true);
    assert_eq!(mask.right_ch3, false);
}

#[test]
fn test_ay8910_stereo_mask_from_mask_right_only() {
    let mask = Ay8910StereoMaskDetail::from_mask(0b00101010);
    assert_eq!(mask.chip_instance, 0);
    assert_eq!(mask.is_ym2203, false);
    assert_eq!(mask.left_ch1, false);
    assert_eq!(mask.right_ch1, true);
    assert_eq!(mask.left_ch2, false);
    assert_eq!(mask.right_ch2, true);
    assert_eq!(mask.left_ch3, false);
    assert_eq!(mask.right_ch3, true);
}

#[test]
fn test_ay8910_stereo_mask_to_mask_roundtrip() {
    let original = 0b11110011;
    let mask = Ay8910StereoMaskDetail::from_mask(original);
    let result = mask.to_mask();
    assert_eq!(result, original);
}

#[test]
fn test_ay8910_stereo_mask_to_mask_construction() {
    let mask = Ay8910StereoMaskDetail {
        chip_instance: 1,
        is_ym2203: true,
        left_ch1: true,
        right_ch1: true,
        left_ch2: false,
        right_ch2: false,
        left_ch3: true,
        right_ch3: true,
    };
    assert_eq!(mask.to_mask(), 0b11110011);
}

#[test]
fn test_ay8910_stereo_mask_from_u8_trait() {
    let mask: Ay8910StereoMaskDetail = 0b00111111u8.into();
    assert_eq!(mask.chip_instance, 0);
    assert!(mask.left_ch1 && mask.right_ch1);
}

#[test]
fn test_ay8910_stereo_mask_into_u8_trait() {
    let mask = Ay8910StereoMaskDetail {
        chip_instance: 0,
        is_ym2203: false,
        left_ch1: true,
        right_ch1: true,
        left_ch2: true,
        right_ch2: true,
        left_ch3: true,
        right_ch3: true,
    };
    let byte: u8 = mask.into();
    assert_eq!(byte, 0b00111111);
}

#[test]
fn test_ay8910_stereo_mask_all_bits() {
    // Test each bit individually
    for bit in 0..8 {
        let value = 1u8 << bit;
        let mask = Ay8910StereoMaskDetail::from_mask(value);
        let result = mask.to_mask();
        assert_eq!(result, value, "Failed for bit {}", bit);
    }
}

#[test]
fn test_ay8910_stereo_mask_parse() {
    use soundlog::vgm::command::Ay8910StereoMask;

    let mask = Ay8910StereoMask(0b00111111);
    let detail = parse_ay8910_stereo_mask(mask);

    assert_eq!(detail.chip_instance, 0);
    assert_eq!(detail.is_ym2203, false);
    assert_eq!(detail.left_ch1, true);
    assert_eq!(detail.right_ch1, true);
    assert_eq!(detail.left_ch2, true);
    assert_eq!(detail.right_ch2, true);
    assert_eq!(detail.left_ch3, true);
    assert_eq!(detail.right_ch3, true);
}

#[test]
fn test_ay8910_stereo_mask_parse_ym2203() {
    use soundlog::vgm::command::Ay8910StereoMask;

    let mask = Ay8910StereoMask(0b11110011);
    let detail = parse_ay8910_stereo_mask(mask);

    assert_eq!(detail.chip_instance, 1);
    assert_eq!(detail.is_ym2203, true);
    assert_eq!(detail.left_ch1, true);
    assert_eq!(detail.right_ch1, true);
    assert_eq!(detail.left_ch2, false);
    assert_eq!(detail.right_ch2, false);
    assert_eq!(detail.left_ch3, true);
    assert_eq!(detail.right_ch3, true);
}
