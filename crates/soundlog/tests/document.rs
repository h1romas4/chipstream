use soundlog::vgm::command::{DataBlock, VgmCommand};
use soundlog::vgm::detail::{
    CompressionType, DecompressionTable, StreamChipType, UncompressedStream,
};
use soundlog::{VgmBuilder, VgmDocument};

/// Helper: extract the first DataBlock found in a document's command stream.
fn find_first_datablock(doc: &VgmDocument) -> Option<&DataBlock> {
    for cmd in doc.iter() {
        if let VgmCommand::DataBlock(db) = cmd {
            return Some(db);
        }
    }
    None
}

#[test]
fn attach_data_block_owned_moves_and_stores_payload() {
    let mut builder = VgmBuilder::new();

    // Use ownership-style call (preferred: no clone)
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0x01, 0x02, 0x03],
    });

    let doc = builder.finalize();

    // The document should contain a DataBlock as its first command
    let db = find_first_datablock(&doc).expect("expected a DataBlock command");

    // marker written by build_data_block is 0x66
    assert_eq!(db.marker, 0x66);
    // size should match payload length
    assert_eq!(db.size as usize, db.data.len());
    // payload round-trips
    assert_eq!(db.data, vec![0x01, 0x02, 0x03]);
}

#[test]
fn attach_data_block_borrowed_clones_and_preserves_original() {
    let mut builder = VgmBuilder::new();

    // Prepare a detail value and pass it by reference.
    // This exercises the `From<&UncompressedStream>` -> DataBlockType path
    // which will clone the detail into the DataBlockType.
    let detail = UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0x0A, 0x0B],
    };

    // Passing a reference should not move `detail` and `detail` remains usable.
    builder.attach_data_block(&detail);

    // original `detail` still accessible and unchanged
    assert_eq!(detail.data, vec![0x0A, 0x0B]);

    let doc = builder.finalize();

    // Document should contain the cloned DataBlock with identical payload.
    let db = find_first_datablock(&doc).expect("expected a DataBlock command");
    assert_eq!(db.data, vec![0x0A, 0x0B]);
    assert_eq!(db.marker, 0x66);
    assert_eq!(db.size as usize, db.data.len());
}

#[test]
fn attach_multiple_data_blocks_preserves_order() {
    let mut builder = VgmBuilder::new();

    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![1],
    });

    let second = UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![2, 3],
    };

    // Demonstrate both owned and borrowed usage in sequence
    builder.attach_data_block(&second);
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(10));
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![4, 5, 6],
    });

    let doc = builder.finalize();

    // Collect DataBlock payloads in document order
    let datablock_payloads: Vec<Vec<u8>> = doc
        .iter()
        .filter_map(|cmd| {
            if let VgmCommand::DataBlock(db) = cmd {
                Some(db.data.clone())
            } else {
                None
            }
        })
        .collect();

    // Expect three DataBlocks in the order we appended them
    assert_eq!(datablock_payloads.len(), 3);
    assert_eq!(datablock_payloads[0], vec![1]);
    assert_eq!(datablock_payloads[1], vec![2, 3]);
    assert_eq!(datablock_payloads[2], vec![4, 5, 6]);
}

#[test]
fn relocate_data_block_moves_datablocks_to_front_and_updates_loop_index() {
    let mut builder = VgmBuilder::new();

    // Build a sequence with two DataBlocks, one before and one after the intended loop target.
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(1));
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![10],
    }); // DataBlock A (index 1)
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(2)); // loop target (index 2)
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![20],
    }); // DataBlock B (index 3)
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(3));

    // Set loop to the command index pointing at the WaitSamples(2) which is index 2.
    builder.set_loop_index(2);

    let doc = builder.finalize();

    // After finalize, DataBlocks should be prepended to the front preserving their original relative order.
    // The collected DataBlocks from the start of the command stream should be [10], [20].
    let mut iter = doc.iter();
    if let Some(VgmCommand::DataBlock(db)) = iter.next() {
        assert_eq!(db.data, vec![10]);
    } else {
        panic!("expected DataBlock at position 0");
    }

    if let Some(VgmCommand::DataBlock(db)) = iter.next() {
        assert_eq!(db.data, vec![20]);
    } else {
        panic!("expected DataBlock at position 1");
    }

    // The builder should have adjusted the stored loop index so that the logical
    // loop command (the original WaitSamples(2)) is still referenced after reordering.
    // In this setup one DataBlock was at-or-after the original loop index, so the
    // final command index should be original_index + 1 => 3.
    let loop_idx = doc
        .loop_command_index()
        .expect("expected loop command index to be present");
    assert_eq!(loop_idx, 3);

    // Verify the command at that index is the WaitSamples(2) we targeted originally.
    match &doc.commands[loop_idx] {
        VgmCommand::WaitSamples(s) => assert_eq!(s.0, 2),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
    match &doc.commands[loop_idx + 1] {
        VgmCommand::WaitSamples(s) => assert_eq!(s.0, 3),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
}

#[test]
fn relocate_data_block_many_datablocks_between_waits() {
    let mut builder = VgmBuilder::new();

    // Build sequence:
    // 0..4: DataBlock pre (5 entries)
    // 5: WaitSamples(1)
    // 6: DataBlock A
    // 7..16: DataBlock middle (10 entries)
    // 17: WaitSamples(2) <- loop target
    // 18: DataBlock after
    // 19: WaitSamples(3)
    // Insert five DataBlocks before the first WaitSamples
    for i in 0..5 {
        builder.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Ym2612Pcm,
            data: vec![(200 + i) as u8],
        });
    }

    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(1));

    // DataBlock A
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0],
    });

    // Ten middle DataBlocks
    for i in 0..10 {
        builder.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Ym2612Pcm,
            data: vec![i as u8 + 1],
        });
    }

    // Loop target
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(2));

    // A DataBlock after the loop target
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![99],
    });

    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(3));

    // Set loop to the command index pointing at the WaitSamples(2)
    // With the five pre-pended DataBlocks the original index is shifted to 17.
    builder.set_loop_index(17);

    let doc = builder.finalize();

    // Count DataBlocks and ensure they are at the front preserving relative order.
    let dbs: Vec<Vec<u8>> = doc
        .iter()
        .take_while(|cmd| matches!(cmd, VgmCommand::DataBlock(_)))
        .filter_map(|cmd| {
            if let VgmCommand::DataBlock(db) = cmd {
                Some(db.data.clone())
            } else {
                None
            }
        })
        .collect();

    // Expect 17 DataBlocks: 5 (pre) + 1 (A) + 10 (middle) + 1 (after)
    assert_eq!(dbs.len(), 17);

    // Pre blocks
    let expected_pre: Vec<Vec<u8>> = (0..5).map(|i| vec![(200 + i) as u8]).collect();
    assert_eq!(&dbs[0..5], &expected_pre[..]);

    // DataBlock A now at position 5
    assert_eq!(dbs[5], vec![0]);

    // Middle blocks follow
    let expected_middle: Vec<Vec<u8>> = (0..10).map(|i| vec![(i as u8) + 1]).collect();
    assert_eq!(&dbs[6..16], &expected_middle[..]);

    // Final post-loop DataBlock
    assert_eq!(dbs[16], vec![99]);

    // The final loop index should have been adjusted so the original WaitSamples(2)
    // (the logical loop target) remains the loop command index after reordering.
    // One DataBlock was at-or-after the original loop index (the [99] block), so the
    // stored loop index should be original + 1 => 18.
    let loop_idx = doc
        .loop_command_index()
        .expect("expected loop command index to be present");
    assert_eq!(loop_idx, 18);

    // Validate the command at loop_idx is WaitSamples(2)
    match &doc.commands[loop_idx] {
        VgmCommand::WaitSamples(ws) => assert_eq!(ws.0, 2),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
}

#[test]
fn decompression_table_is_moved_to_front() {
    let mut builder = VgmBuilder::new();

    // Add an UncompressedStream before the table
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![1],
    });

    // Attach a DecompressionTable (data_type == 0x7F)
    builder.attach_data_block(DecompressionTable {
        compression_type: CompressionType::BitPacking,
        sub_type: 0x01,
        bits_decompressed: 8,
        bits_compressed: 4,
        value_count: 2,
        table_data: vec![0xAA, 0xBB],
    });

    // Add another UncompressedStream after the table
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![2],
    });

    let doc = builder.finalize();

    // The first DataBlock should be the DecompressionTable (data_type == 0x7F)
    let mut iter = doc.iter();
    if let Some(VgmCommand::DataBlock(db)) = iter.next() {
        assert_eq!(db.data_type, 0x7F);
    } else {
        panic!("expected DataBlock at position 0");
    }

    // The next DataBlock should be one of the UncompressedStream blocks (0x00..=0x3F)
    if let Some(VgmCommand::DataBlock(db)) = iter.next() {
        assert!(db.data_type <= 0x3F);
    } else {
        panic!("expected second DataBlock to be an uncompressed stream");
    }
}

#[test]
fn relocate_data_block_moves_datablocks_to_front_and_updates_loop_index_with_offset() {
    let mut builder = VgmBuilder::new();

    // Build a sequence with two DataBlocks, one before and one after the intended loop target.
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(1));
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![10],
    }); // DataBlock A (index 1)
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(2)); // loop target (index 2)
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![20],
    }); // DataBlock B (index 3)
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(3));

    // Set loop to the command index pointing at the WaitSamples(2) using the non-DataBlock offset API.
    // `set_loop_offset(n)` selects the n-th non-DataBlock command (0-based). Here the non-DataBlock
    // sequence is [WaitSamples(1), WaitSamples(2), WaitSamples(3)] so passing 1 selects WaitSamples(2).
    builder.set_loop_offset(1);

    let doc = builder.finalize();

    // After finalize, DataBlocks should be prepended to the front preserving their original relative order.
    // The collected DataBlocks from the start of the command stream should be [10], [20].
    let mut iter = doc.iter();
    if let Some(VgmCommand::DataBlock(db)) = iter.next() {
        assert_eq!(db.data, vec![10]);
    } else {
        panic!("expected DataBlock at position 0");
    }

    if let Some(VgmCommand::DataBlock(db)) = iter.next() {
        assert_eq!(db.data, vec![20]);
    } else {
        panic!("expected DataBlock at position 1");
    }

    // The builder should have adjusted the stored loop index so that the logical
    // loop command (the original WaitSamples(2)) is still referenced after reordering.
    let loop_idx = doc
        .loop_command_index()
        .expect("expected loop command index to be present");
    assert_eq!(loop_idx, 3);

    // Verify the command at that index is the WaitSamples(2) we targeted originally.
    match &doc.commands[loop_idx] {
        VgmCommand::WaitSamples(s) => assert_eq!(s.0, 2),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
    match &doc.commands[loop_idx + 1] {
        VgmCommand::WaitSamples(s) => assert_eq!(s.0, 3),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
}

#[test]
fn relocate_data_block_many_datablocks_between_waits_with_offset() {
    let mut builder = VgmBuilder::new();

    // Build sequence:
    // 0..4: DataBlock pre (5 entries)
    // 5: WaitSamples(1)
    // 6: DataBlock A
    // 7..16: DataBlock middle (10 entries)
    // 17: WaitSamples(2) <- loop target
    // 18: DataBlock after
    // 19: WaitSamples(3)
    // Insert five DataBlocks before the first WaitSamples
    for i in 0..5 {
        builder.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Ym2612Pcm,
            data: vec![(200 + i) as u8],
        });
    }

    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(1));

    // DataBlock A
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0],
    });

    // Ten middle DataBlocks
    for i in 0..10 {
        builder.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Ym2612Pcm,
            data: vec![i as u8 + 1],
        });
    }

    // Loop target
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(2));

    // A DataBlock after the loop target
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![99],
    });

    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(3));

    // Use set_loop_offset to select the non-DataBlock offset. The non-DataBlock sequence here is:
    // [WaitSamples(1), WaitSamples(2), WaitSamples(3)] so WaitSamples(2) is index 1 in that sequence.
    builder.set_loop_offset(1);

    let doc = builder.finalize();

    // Count DataBlocks and ensure they are at the front preserving relative order.
    let dbs: Vec<Vec<u8>> = doc
        .iter()
        .take_while(|cmd| matches!(cmd, VgmCommand::DataBlock(_)))
        .filter_map(|cmd| {
            if let VgmCommand::DataBlock(db) = cmd {
                Some(db.data.clone())
            } else {
                None
            }
        })
        .collect();

    // Expect 17 DataBlocks: 5 (pre) + 1 (A) + 10 (middle) + 1 (after)
    assert_eq!(dbs.len(), 17);

    // Pre blocks
    let expected_pre: Vec<Vec<u8>> = (0..5).map(|i| vec![(200 + i) as u8]).collect();
    assert_eq!(&dbs[0..5], &expected_pre[..]);

    // DataBlock A now at position 5
    assert_eq!(dbs[5], vec![0]);

    // Middle blocks follow
    let expected_middle: Vec<Vec<u8>> = (0..10).map(|i| vec![(i as u8) + 1]).collect();
    assert_eq!(&dbs[6..16], &expected_middle[..]);

    // Final post-loop DataBlock
    assert_eq!(dbs[16], vec![99]);

    // The final loop index should have been adjusted so the original WaitSamples(2)
    // (the logical loop target) remains the loop command index after reordering.
    // One DataBlock was at-or-after the original loop index (the [99] block), so the
    // stored loop index should be original + 1 => 18.
    let loop_idx = doc
        .loop_command_index()
        .expect("expected loop command index to be present");
    assert_eq!(loop_idx, 18);

    // Validate the command at loop_idx is WaitSamples(2)
    match &doc.commands[loop_idx] {
        VgmCommand::WaitSamples(ws) => assert_eq!(ws.0, 2),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
}

#[test]
fn common_case_ten_datablocks_ten_commands_loop_offset_four() {
    let mut builder = VgmBuilder::new();

    // Add 10 DataBlocks (these will be relocated to the front on finalize)
    for i in 0..10 {
        builder.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Ym2612Pcm,
            data: vec![i as u8],
        });
    }

    // Add 10 WaitSamples commands (non-DataBlock commands)
    for i in 1..=10 {
        builder.add_vgm_command(soundlog::vgm::command::WaitSamples(i));
    }

    // Select the 5th non-DataBlock command (0-based index 4) -> WaitSamples(5)
    builder.set_loop_offset(4);

    let doc = builder.finalize();

    // First 10 commands should be DataBlocks with payloads 0..9
    let dbs: Vec<Vec<u8>> = doc
        .iter()
        .take_while(|cmd| matches!(cmd, VgmCommand::DataBlock(_)))
        .filter_map(|cmd| {
            if let VgmCommand::DataBlock(db) = cmd {
                Some(db.data.clone())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(dbs.len(), 10);
    for (i, db) in dbs.iter().enumerate().take(10) {
        assert_eq!(db, &vec![i as u8]);
    }

    // Loop should point to the 5th WaitSamples command which is at overall index 10 + 4 = 14
    let loop_idx = doc
        .loop_command_index()
        .expect("expected loop command index");
    assert_eq!(loop_idx, 14);

    match &doc.commands[loop_idx] {
        VgmCommand::WaitSamples(ws) => assert_eq!(ws.0, 5),
        other => panic!("unexpected command at loop index: {:?}", other),
    }
}
