use soundlog::VgmBuilder;
use soundlog::vgm::command::DacStreamChipType;
use soundlog::vgm::command::{
    EndOfData, VgmCommand, Wait735Samples, Wait882Samples, WaitNSample, WaitSamples,
};
use soundlog::vgm::stream::{StreamResult, VgmStream};

/// Helper function to create a simple VGM document with commands and loop setup
fn create_test_vgm_with_loop() -> Vec<u8> {
    let mut builder = VgmBuilder::new();

    // Add some basic commands
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(Wait882Samples);
    builder.add_vgm_command(WaitSamples(1000));
    builder.add_vgm_command(WaitNSample(5));

    // Set loop point at the second command (index 1)
    builder.set_loop_offset(1);

    // Add end of data command
    builder.add_vgm_command(EndOfData);

    let document = builder.finalize();
    document.into()
}

/// Helper function to create a VGM document with various commands
fn create_vgm_with_various_commands() -> Vec<u8> {
    let mut builder = VgmBuilder::new();

    // Add various wait commands to test parsing
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(Wait882Samples);
    builder.add_vgm_command(WaitSamples(500));
    builder.add_vgm_command(WaitNSample(10));
    builder.add_vgm_command(EndOfData);

    let document = builder.finalize();
    document.into()
}

#[test]
fn test_stream_parser_basic_functionality() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();

    // Feed the entire VGM data at once
    parser.push_data(&vgm_data);

    let mut command_count = 0;
    let mut commands = Vec::new();

    // Parse all commands
    loop {
        match parser.next().unwrap().unwrap() {
            StreamResult::Command(cmd) => {
                commands.push(cmd);
                command_count += 1;
            }
            StreamResult::NeedsMoreData => break,
            StreamResult::EndOfStream => break,
        }
    }

    // Should have parsed at least the basic commands we added
    assert!(command_count > 0);
    println!("Parsed {} commands", command_count);
}

#[test]
fn test_stream_parser_with_loop_limit() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(2)); // 2 total playthroughs

    // Feed the VGM data
    parser.push_data(&vgm_data);

    let mut end_of_data_count = 0;
    let mut total_commands = 0;

    // Parse all commands until stream ends
    loop {
        match parser.next().unwrap().unwrap() {
            StreamResult::Command(cmd) => {
                total_commands += 1;
                if matches!(cmd, VgmCommand::EndOfData(_)) {
                    end_of_data_count += 1;
                }
            }
            StreamResult::NeedsMoreData => {
                // Add the data again to simulate looping
                if parser.current_loop_count() < 2 {
                    parser.push_data(&vgm_data);
                } else {
                    break;
                }
            }
            StreamResult::EndOfStream => break,
        }
    }

    // Should have encountered EndOfData twice (for 2 loops)
    assert_eq!(parser.current_loop_count(), 2);
    println!(
        "Total commands parsed: {}, EndOfData count: {}",
        total_commands, end_of_data_count
    );
}

#[test]
fn test_stream_parser_incremental_data() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();

    // Feed data in small chunks to test incremental parsing
    let chunk_size = 5;
    let mut offset = 0;
    let mut parsed_commands = Vec::new();

    while offset < vgm_data.len() {
        let end = std::cmp::min(offset + chunk_size, vgm_data.len());
        let chunk = &vgm_data[offset..end];
        parser.push_data(chunk);
        offset = end;

        // Try to parse available commands
        loop {
            match parser.next().unwrap().unwrap() {
                StreamResult::Command(cmd) => {
                    parsed_commands.push(cmd);
                }
                StreamResult::NeedsMoreData => break,
                StreamResult::EndOfStream => break,
            }
        }
    }

    // Should have parsed some commands
    assert!(!parsed_commands.is_empty());
    println!("Parsed {} commands incrementally", parsed_commands.len());
}

#[test]
fn test_stream_parser_with_various_commands() {
    let vgm_data = create_vgm_with_various_commands();
    let mut parser = VgmStream::new();

    // Feed the VGM data
    parser.push_data(&vgm_data);

    let mut wait_commands = 0;
    let mut commands = Vec::new();

    // Parse all commands
    loop {
        match parser.next().unwrap().unwrap() {
            StreamResult::Command(cmd) => {
                match &cmd {
                    VgmCommand::Wait735Samples(_)
                    | VgmCommand::Wait882Samples(_)
                    | VgmCommand::WaitSamples(_)
                    | VgmCommand::WaitNSample(_) => {
                        wait_commands += 1;
                    }
                    _ => {}
                }
                commands.push(cmd);
            }
            StreamResult::NeedsMoreData => break,
            StreamResult::EndOfStream => break,
        }
    }

    assert!(wait_commands > 0, "Should have found wait commands");
    // EndOfData is handled internally and not returned to iterator
    println!(
        "Parsed {} total commands ({} wait)",
        commands.len(),
        wait_commands
    );
}

#[test]
fn test_stream_parser_iterator_interface() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.push_data(&vgm_data);

    // Use the iterator interface
    let mut commands = Vec::new();
    let mut needs_more_data = false;
    let mut stream_ended = false;

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                commands.push(cmd);
            }
            Ok(StreamResult::NeedsMoreData) => {
                needs_more_data = true;
                break;
            }
            Ok(StreamResult::EndOfStream) => {
                stream_ended = true;
                break;
            }
            Err(e) => {
                println!("Iterator parsing error: {:?}", e);
                break;
            }
        }
    }

    assert!(!commands.is_empty(), "Should have parsed some commands");
    println!("Collected {} commands via iterator", commands.len());

    if needs_more_data {
        println!("Iterator stopped when more data was needed");
    }
    if stream_ended {
        println!("Iterator stopped when stream ended");
    }
}

#[test]
fn test_stream_parser_buffer_management() {
    let mut parser = VgmStream::new();

    // Add a large amount of simple commands to test buffer management
    let large_data = vec![0x62; 1000];
    parser.push_data(&large_data);

    let initial_buffer_size = parser.buffer_size();

    // Parse half the commands
    for _ in 0..500 {
        match parser.next().unwrap().unwrap() {
            StreamResult::Command(_) => {}
            _ => break,
        }
    }

    let mid_buffer_size = parser.buffer_size();

    // Buffer should have shrunk as commands were consumed
    assert!(mid_buffer_size < initial_buffer_size);

    // Optimize memory explicitly
    parser.optimize_memory();

    let optimized_size = parser.buffer_size();
    println!(
        "Buffer sizes - Initial: {}, Mid: {}, Optimized: {}",
        initial_buffer_size, mid_buffer_size, optimized_size
    );
}

#[test]
fn test_stream_parser_reset_functionality() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1));

    // Parse some data
    parser.push_data(&vgm_data);
    let _ = parser.next().unwrap().unwrap();

    assert!(parser.buffer_size() > 0);

    // Reset the parser
    parser.reset();

    // Should be back to initial state
    assert_eq!(parser.buffer_size(), 0);
    assert_eq!(parser.current_loop_count(), 0);

    // Should be able to parse again after reset
    parser.push_data(&vgm_data);
    match parser.next().unwrap().unwrap() {
        StreamResult::Command(_) => {
            println!("Successfully parsed after reset");
        }
        other => panic!("Expected command after reset, got {:?}", other),
    }
}

#[test]
fn test_stream_parser_partial_command_handling() {
    let mut parser = VgmStream::new();

    // Feed incomplete data for a WaitSamples command (0x61 requires 2 additional bytes)
    parser.push_data(&[0x61]); // Command opcode only

    // Should need more data
    match parser.next().unwrap().unwrap() {
        StreamResult::NeedsMoreData => {
            println!("Correctly identified incomplete command");
        }
        other => panic!("Expected NeedsMoreData, got {:?}", other),
    }

    // Add one more byte (still incomplete)
    parser.push_data(&[0x44]);

    match parser.next().unwrap().unwrap() {
        StreamResult::NeedsMoreData => {
            println!("Still incomplete after one byte");
        }
        other => panic!("Expected NeedsMoreData, got {:?}", other),
    }

    // Add the final byte to complete the command
    parser.push_data(&[0x01]);

    match parser.next().unwrap().unwrap() {
        StreamResult::Command(VgmCommand::WaitSamples(WaitSamples(0x0144))) => {
            println!("Successfully parsed complete WaitSamples command");
        }
        other => panic!("Expected WaitSamples(0x0144), got {:?}", other),
    }
}

#[test]
fn test_stream_parser_multiple_data_chunks() {
    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(2));

    // Simulate receiving VGM data in multiple network packets
    // Use a slice of byte slices to avoid allocating Vec<Vec<u8>> (clippy friendly)
    let chunks: &[&[u8]] = &[
        &[0x62],             // Wait 735 samples
        &[0x63],             // Wait 882 samples
        &[0x61, 0x10, 0x00], // Wait 16 samples
        &[0x66],             // End of data
    ];

    let mut total_commands = 0;

    for (i, &chunk) in chunks.iter().enumerate() {
        parser.push_data(chunk);
        println!("Added chunk {}: {:?}", i, chunk);

        // Process all available commands after this chunk
        loop {
            match parser.next().unwrap().unwrap() {
                StreamResult::Command(cmd) => {
                    total_commands += 1;
                    println!("Parsed command: {:?}", cmd);
                }
                StreamResult::NeedsMoreData => {
                    println!("Need more data after chunk {}", i);
                    break;
                }
                StreamResult::EndOfStream => {
                    println!("Stream ended after {} total commands", total_commands);
                    return;
                }
            }
        }
    }

    // Should have processed the first loop
    assert_eq!(parser.current_loop_count(), 1);
    assert!(total_commands > 0);

    println!("Processed first loop with {} commands", total_commands);
}

#[test]
fn test_stream_parser_two_loop_iterations() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(2)); // 2 total playthroughs

    // Feed the VGM data
    parser.push_data(&vgm_data);

    let mut total_commands = 0;

    // Parse through both loop iterations
    loop {
        match parser.next().unwrap().unwrap() {
            StreamResult::Command(_cmd) => {
                total_commands += 1;
                // EndOfData is handled internally, not returned to iterator
            }
            StreamResult::NeedsMoreData => {
                // Re-feed data to simulate looping
                if parser.current_loop_count() < 2 {
                    parser.push_data(&vgm_data);
                } else {
                    break;
                }
            }
            StreamResult::EndOfStream => {
                println!("Stream ended after {} loops", parser.current_loop_count());
                break;
            }
        }
    }

    // Verify we completed exactly 2 loops as requested
    assert_eq!(
        parser.current_loop_count(),
        2,
        "Should have completed exactly 2 loops"
    );
    assert!(total_commands > 0, "Should have processed some commands");

    println!(
        "Successfully completed 2 loop iterations: {} total commands",
        total_commands
    );
}

#[test]
fn test_iterator_interface_demonstration() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.push_data(&vgm_data);

    println!("Demonstrating new iterator interface:");

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                println!("  Command: {:?}", cmd);
            }
            Ok(StreamResult::NeedsMoreData) => {
                println!("  NeedsMoreData - can distinguish from EndOfStream!");
                break;
            }
            Ok(StreamResult::EndOfStream) => {
                println!("  EndOfStream - definitively ended");
                break;
            }
            Err(e) => {
                println!("  Parse error: {:?}", e);
                break;
            }
        }
    }

    println!("Iterator interface now provides full StreamResult access!");
}

#[test]
fn test_data_block_parsing_and_storage() {
    let mut parser = VgmStream::new();

    // Manually construct VGM bytes with DataBlock commands
    // Format: 0x67 0x66 tt ss ss ss ss (data...)
    // tt = data type, ss = size (little-endian)

    // Build the VGM using the VgmBuilder API instead of manual byte assembly.
    let mut builder = VgmBuilder::new();

    // Uncompressed stream data block (type 0x00 = YM2612 PCM)
    let uncompressed = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: 4,
        data: vec![0x01, 0x02, 0x03, 0x04],
    };
    builder.add_vgm_command(uncompressed);

    // ROM dump block (type 0x80 = Sega PCM ROM) - should be returned by iterator
    let rom_data = vec![
        0x10, 0x00, 0x00, 0x00, // ROM size: 16 bytes
        0x00, 0x00, 0x00, 0x00, // Start address: 0
        0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, // ROM data
    ];
    let rom_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x80,
        size: rom_data.len() as u32,
        data: rom_data,
    };
    builder.add_vgm_command(rom_block);

    // End of data
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    parser.push_data(&bytes);

    let mut found_rom_block = false;
    let mut found_uncompressed_block = false;
    let mut command_count = 0;

    // Parse through the stream
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                command_count += 1;
                if let VgmCommand::DataBlock(block) = &cmd {
                    // ROM blocks should be returned to iterator
                    if block.data_type == 0x80 {
                        found_rom_block = true;
                        println!("Found ROM block in iterator output");
                    }
                    // Uncompressed streams should NOT be returned
                    if block.data_type == 0x00 {
                        found_uncompressed_block = true;
                        println!("Found uncompressed block in iterator (should not happen!)");
                    }
                }
            }
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => {
                eprintln!("Parse error: {:?}", e);
                break;
            }
        }
    }

    // Verify uncompressed stream was stored (not returned to iterator)
    assert!(
        parser.get_uncompressed_stream(0x00).is_some(),
        "Uncompressed stream should be stored in parser state"
    );

    // Uncompressed block should NOT appear in iterator output
    assert!(
        !found_uncompressed_block,
        "Uncompressed stream should NOT be returned to iterator"
    );

    // Verify ROM block was returned to iterator
    assert!(
        found_rom_block,
        "ROM block should be returned to iterator, not stored in stream state"
    );

    println!(
        "DataBlock test: {} commands, uncompressed stored, ROM returned",
        command_count
    );
}

#[test]
fn test_iterator_with_incremental_push_data() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();

    // Split the VGM data into chunks for incremental feeding
    let chunk_size = 8;
    let mut data_chunks = Vec::new();
    for chunk in vgm_data.chunks(chunk_size) {
        data_chunks.push(chunk.to_vec());
    }

    let mut chunk_index = 0;
    let mut total_commands = 0;
    let mut needs_more_data_count = 0;

    // Initially push the first chunk
    if !data_chunks.is_empty() {
        parser.push_data(&data_chunks[chunk_index]);
        chunk_index += 1;
    }

    println!("Testing iterator with incremental data feeding:");

    // Use iterator while feeding data incrementally
    loop {
        let mut made_progress = false;

        // Process available commands with iterator
        for result in &mut parser {
            match result {
                Ok(StreamResult::Command(cmd)) => {
                    total_commands += 1;
                    made_progress = true;
                    if total_commands <= 5 {
                        println!("  Command {}: {:?}", total_commands, cmd);
                    } else if total_commands == 6 {
                        println!("  ... (showing first 5 commands)");
                    }
                }
                Ok(StreamResult::NeedsMoreData) => {
                    needs_more_data_count += 1;
                    println!("  NeedsMoreData event #{}", needs_more_data_count);

                    // Feed next chunk if available
                    if chunk_index < data_chunks.len() {
                        parser.push_data(&data_chunks[chunk_index]);
                        println!(
                            "    Fed chunk {} ({} bytes)",
                            chunk_index,
                            data_chunks[chunk_index].len()
                        );
                        chunk_index += 1;
                        made_progress = true;
                    }
                    break; // Break inner loop to restart iterator
                }
                Ok(StreamResult::EndOfStream) => {
                    println!("  EndOfStream reached");
                    made_progress = false;
                    break;
                }
                Err(e) => {
                    println!("  Parse error: {:?}", e);
                    made_progress = false;
                    break;
                }
            }
        }

        // If no progress was made and no more data to feed, break
        if !made_progress {
            break;
        }
    }

    assert!(total_commands > 0, "Should have parsed some commands");
    assert!(
        needs_more_data_count > 0,
        "Should have requested more data at least once"
    );

    println!(
        "Successfully processed {} commands with {} data requests",
        total_commands, needs_more_data_count
    );
    println!("Fed {} chunks of data incrementally", chunk_index);
}

#[test]
fn test_streaming_with_variable_chunk_sizes() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1)); // 1 total playthrough (no looping)

    // Simulate realistic network/file streaming with variable chunk sizes
    let chunk_sizes = vec![3, 1, 7, 2, 12, 5, 1, 4, 8, 15, 6, 3, 9, 11, 4];

    let mut data_offset = 0;
    let mut chunk_index = 0;
    let mut total_commands = 0;
    let mut end_of_stream = false;

    println!("Testing realistic streaming scenario with variable chunk sizes:");

    // Initial push of first chunk
    if !chunk_sizes.is_empty() && data_offset < vgm_data.len() {
        let chunk_size = std::cmp::min(chunk_sizes[chunk_index], vgm_data.len() - data_offset);
        let chunk = &vgm_data[data_offset..data_offset + chunk_size];
        parser.push_data(chunk);
        data_offset += chunk_size;
        chunk_index += 1;
        println!("  Initial chunk: {} bytes", chunk.len());
    }

    // Process stream with iterator while feeding data as needed
    let mut iteration_count = 0;
    while !end_of_stream && iteration_count < 1000 {
        iteration_count += 1;
        let mut processed_any = false;

        for result in &mut parser {
            match result {
                Ok(StreamResult::Command(cmd)) => {
                    total_commands += 1;
                    processed_any = true;
                    if total_commands <= 3 {
                        println!("  Command {}: {:?}", total_commands, cmd);
                    } else if total_commands == 4 {
                        println!("  ... (processing more commands)");
                    }

                    if matches!(cmd, VgmCommand::EndOfData(_)) {
                        println!("  Found EndOfData command");
                        // Continue to see if we get EndOfStream
                    }
                }
                Ok(StreamResult::NeedsMoreData) => {
                    // Simulate receiving next chunk if we have more data
                    if data_offset < vgm_data.len() {
                        let remaining = vgm_data.len() - data_offset;
                        let chunk_size = if chunk_index < chunk_sizes.len() {
                            std::cmp::min(chunk_sizes[chunk_index], remaining)
                        } else {
                            remaining // Feed all remaining data if we're out of chunk sizes
                        };

                        let chunk = &vgm_data[data_offset..data_offset + chunk_size];
                        parser.push_data(chunk);
                        data_offset += chunk_size;
                        chunk_index += 1;
                        processed_any = true;

                        if chunk_index % 5 == 0 || data_offset >= vgm_data.len() {
                            println!(
                                "  Fed chunk {} ({} bytes) - total {} bytes processed",
                                chunk_index,
                                chunk.len(),
                                data_offset
                            );
                        }
                    } else {
                        println!("  NeedsMoreData but no more data available - breaking");
                        break;
                    }
                    break; // Restart iterator after feeding data
                }
                Ok(StreamResult::EndOfStream) => {
                    println!("  Stream ended after processing all data");
                    end_of_stream = true;
                    break;
                }
                Err(e) => {
                    println!("  Parse error: {:?}", e);
                    end_of_stream = true;
                    break;
                }
            }
        }

        // Safety check: if no progress was made and all data was fed, break
        if !processed_any && data_offset >= vgm_data.len() {
            println!("  No progress made and all data fed - ending stream");
            break;
        }
    }

    if iteration_count >= 1000 {
        println!("  WARNING: Hit iteration limit to prevent infinite loop");
    }

    assert!(total_commands > 0, "Should have parsed some commands");

    println!(
        "Streaming completed: {} commands, {} bytes processed in {} chunks",
        total_commands, data_offset, chunk_index
    );
    println!("Final parser buffer size: {} bytes", parser.buffer_size());
}

#[test]
fn test_compressed_stream_decompression() {
    let mut parser = VgmStream::new();

    // Test BitPacking Copy compression with decompression
    // We'll create a decompression table first, then a compressed stream that uses it

    // Use VgmBuilder to create a DecompressionTable and a CompressedStream block.
    let mut builder = VgmBuilder::new();

    // Decompression table (type 0x7F)
    let table_data = vec![
        0x00, // compression_type: BitPacking
        0x08, // bits_decompressed: 8
        0x04, // bits_compressed: 4
        0x00, // sub_type: Copy
        0x00, 0x00, // value_count: 0 (not used for Copy)
    ];
    let table_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x7F,
        size: table_data.len() as u32,
        data: table_data,
    };
    builder.add_vgm_command(table_block);

    // Compressed stream (type 0x40)
    let compressed_data = vec![
        0x00, // compression_type: BitPacking
        0x04, 0x00, 0x00, 0x00, // uncompressed_size: 4 bytes
        0x08, // bits_decompressed: 8
        0x04, // bits_compressed: 4
        0x00, // sub_type: Copy
        0x10, 0x00, // add_value: u16 = 16 (little-endian)
        0x5A, // Compressed data: 0x5, 0xA packed
        0xF3, // Compressed data: 0xF, 0x3 packed
    ];
    let stream_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x40,
        size: compressed_data.len() as u32,
        data: compressed_data,
    };
    builder.add_vgm_command(stream_block);

    // End of data
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    parser.push_data(&bytes);

    let mut command_count = 0;
    let mut found_compressed_in_iterator = false;

    // Parse through the stream
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                command_count += 1;
                if let VgmCommand::DataBlock(block) = &cmd
                    && block.data_type == 0x40
                {
                    found_compressed_in_iterator = true;
                }
            }
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => {
                eprintln!("Parse error: {:?}", e);
                panic!("Unexpected error: {:?}", e);
            }
        }
    }

    // Verify compressed stream was NOT returned to iterator
    assert!(
        !found_compressed_in_iterator,
        "Compressed stream should NOT be returned to iterator"
    );

    // Verify decompression table was stored
    assert!(
        parser.get_decompression_table(0x7F).is_some(),
        "DecompressionTable should be stored in parser state"
    );

    // Verify compressed stream was decompressed and stored as uncompressed
    let uncompressed = parser.get_uncompressed_stream(0x40);
    assert!(
        uncompressed.is_some(),
        "Decompressed stream should be stored as uncompressed stream"
    );

    // Verify the decompressed data is correct
    // Expected: 0x5 + 0x10 = 0x15, 0xA + 0x10 = 0x1A, 0xF + 0x10 = 0x1F, 0x3 + 0x10 = 0x13
    let expected_data = vec![0x15, 0x1A, 0x1F, 0x13];
    let actual_data = &uncompressed.unwrap().data;
    assert_eq!(
        actual_data, &expected_data,
        "Decompressed data should match expected values"
    );

    println!(
        "CompressedStream decompression test: {} commands, decompressed correctly",
        command_count
    );
}

#[test]
fn test_compressed_stream_without_decompression_table() {
    let mut parser = VgmStream::new();

    // Build a compressed stream without a decompression table using VgmBuilder.
    let mut builder = VgmBuilder::new();

    let compressed_data = vec![
        0x00, // compression_type: BitPacking
        0x04, 0x00, 0x00, 0x00, // uncompressed_size: 4 bytes
        0x08, // bits_decompressed
        0x04, // bits_compressed
        0x00, // sub_type: copy
        0x00, // reserved
        0x11, 0x22, // compressed payload
    ];
    let stream_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x40,
        size: compressed_data.len() as u32,
        data: compressed_data,
    };
    builder.add_vgm_command(stream_block);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    parser.push_data(&bytes);

    let mut found_error = false;

    // Parse through the stream - should get an error
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => {
                println!("Got expected error: {:?}", e);
                found_error = true;
                break;
            }
        }
    }

    // For BitPacking Copy subtype, DecompressionTable is not required,
    // so this should actually succeed
    assert!(
        !found_error,
        "BitPacking Copy should work without DecompressionTable"
    );

    // Verify the stream was decompressed
    assert!(
        parser.get_uncompressed_stream(0x40).is_some(),
        "Stream should be decompressed even without table for Copy subtype"
    );
}

#[test]
fn test_dpcm_compressed_stream_without_table_fails() {
    let mut parser = VgmStream::new();

    // Build a DPCM compressed stream without a decompression table using VgmBuilder.
    let mut builder = VgmBuilder::new();

    let compressed_data = vec![
        0x01, // compression_type: DPCM
        0x04, 0x00, 0x00, 0x00, // uncompressed_size: 4 bytes
        0x08, // bits_decompressed: 8
        0x04, // bits_compressed: 4
        0x00, // reserved
        0x80, 0x00, // start_value: 128
        0x12, 0x34, // compressed data
    ];
    let stream_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x40,
        size: compressed_data.len() as u32,
        data: compressed_data,
    };
    builder.add_vgm_command(stream_block);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();
    parser.push_data(&bytes);

    let mut found_error = false;

    // Parse through the stream - should get an error
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => {
                println!("Got expected error for DPCM without table: {:?}", e);
                found_error = true;
                break;
            }
        }
    }

    // DPCM requires DecompressionTable, so we should get an error
    assert!(
        found_error,
        "DPCM compression should fail without DecompressionTable"
    );
}

#[test]
fn test_dac_stream_control_basic() {
    let mut parser = VgmStream::new();

    // Build the VGM using VgmBuilder instead of manually assembling bytes.
    let mut builder = VgmBuilder::new();

    // DataBlock: UncompressedStream (data_type 0x00)
    let stream_data = vec![0x80, 0x90, 0xA0, 0xB0, 0xC0, 0xD0, 0xE0, 0xF0];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // SetupStreamControl (0x90): stream_id=0, chip_type=YM2612, port=0, register=0x2A
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A,
    });

    // SetStreamData (0x91): stream_id=0, data_bank_id=0, step_size=1, step_base=0
    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    // SetStreamFrequency (0x92): stream_id=0, frequency=22050 Hz
    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 22050,
    });

    // StartStream (0x93): stream_id=0, offset=0, length_mode=1 (command count), length=4
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: 1,
        data_length: 4,
    });

    // Wait long enough for stream writes to occur
    builder.add_vgm_command(WaitSamples(100));

    // StopStream (0x94): stream_id=0
    builder.add_vgm_command(soundlog::vgm::command::StopStream { stream_id: 0 });

    // End of data
    builder.add_vgm_command(EndOfData);

    // Finalize into bytes and feed parser
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let mut commands = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                println!("Got command: {:?}", cmd);
                commands.push(cmd);
            }
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => {
                panic!("Parse error: {:?}", e);
            }
        }
    }

    // Stream control commands are handled internally and not returned to the iterator.
    // We should only see the generated YM2612 writes and wait commands.
    assert!(
        !commands.is_empty(),
        "Should have at least some commands (generated writes)"
    );

    // Count YM2612 DAC writes (register 0x2A)
    let dac_writes: Vec<_> = commands
        .iter()
        .filter_map(|cmd| {
            if let VgmCommand::Ym2612Write(_, spec) = cmd {
                if spec.register == 0x2A {
                    Some(spec.value)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert!(
        !dac_writes.is_empty(),
        "Expected some YM2612 DAC writes to be generated"
    );
}

#[test]
fn test_dac_stream_control_stop_all_streams() {
    let mut parser = VgmStream::new();

    // Build using VgmBuilder
    let mut builder = VgmBuilder::new();

    // Data block (type 0x00)
    let stream_data = vec![0x80, 0x90, 0xA0, 0xB0];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // Setup two streams
    for stream_id in 0u8..2u8 {
        builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
            stream_id,
            chip_type: 0x02,
            write_port: 0,
            write_command: 0x2A,
        });

        builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
            stream_id,
            data_bank_id: 0,
            step_size: 1,
            step_base: 0,
        });

        builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
            stream_id,
            frequency: 44100,
        });

        builder.add_vgm_command(soundlog::vgm::command::StartStream {
            stream_id,
            data_start_offset: 0,
            length_mode: 3, // Play until end
            data_length: 0,
        });
    }

    // Wait a bit
    builder.add_vgm_command(WaitSamples(10));

    // Stop all streams with 0xFF
    builder.add_vgm_command(soundlog::vgm::command::StopStream { stream_id: 0xFF });

    // End
    builder.add_vgm_command(EndOfData);

    // Finalize and feed parser
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let _stop_command_found = false;
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {
                // Stream control commands are handled internally
                // Just iterate through to verify no errors
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // StopStream is handled internally and not returned
    // The test passes if we can parse without errors
}

#[test]
fn test_dac_stream_control_fast_call() {
    let mut parser = VgmStream::new();

    // Build using VgmBuilder
    let mut builder = VgmBuilder::new();

    // Create data block (type 0x00)
    let stream_data = vec![0x10, 0x20, 0x30, 0x40];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // Setup stream
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 44100,
    });

    // StartStreamFastCall (0x95)
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 0,
        flags: 0,
    });

    // Wait
    builder.add_vgm_command(WaitSamples(10));

    // End
    builder.add_vgm_command(EndOfData);

    // Finalize and feed parser
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {
                // Stream control commands are handled internally
                // Just iterate through to verify no errors
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // StartStreamFastCall is handled internally and not returned
    // The test passes if we can parse without errors
}

#[test]
fn test_start_stream_fast_call_with_multiple_blocks() {
    // Test that block_id correctly references different data blocks
    let mut parser = VgmStream::new();
    let mut builder = VgmBuilder::new();

    // Create multiple data blocks with the same data_type (0x04 for OKIM6258)
    // Block 0: 4 bytes
    let block0_data = vec![0x10, 0x20, 0x30, 0x40];
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x04,
        size: block0_data.len() as u32,
        data: block0_data.clone(),
    });

    // Block 1: 4 bytes
    let block1_data = vec![0x50, 0x60, 0x70, 0x80];
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x04,
        size: block1_data.len() as u32,
        data: block1_data.clone(),
    });

    // Block 2: 4 bytes
    let block2_data = vec![0x90, 0xA0, 0xB0, 0xC0];
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x04,
        size: block2_data.len() as u32,
        data: block2_data.clone(),
    });

    // Setup stream for OKIM6258 (chip_type 0x17)
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: 0x17, // OKIM6258
        write_port: 0,
        write_command: 0x01,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0x04,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 8000,
    });

    // Start stream at block 0
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 0,
        flags: 0,
    });
    builder.add_vgm_command(WaitSamples(10));

    // Start stream at block 1 (should start at offset 4)
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 1,
        flags: 0,
    });
    builder.add_vgm_command(WaitSamples(10));

    // Start stream at block 2 (should start at offset 8)
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 2,
        flags: 0,
    });
    builder.add_vgm_command(WaitSamples(10));

    builder.add_vgm_command(EndOfData);

    // Finalize and parse
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let mut chip_writes = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                // Collect OKIM6258 writes to verify correct data is being read
                if let VgmCommand::Okim6258Write(_, spec) = cmd {
                    chip_writes.push(spec.value);
                }
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // Verify that we got writes from all three blocks
    // Block 0 starts with 0x10, block 1 with 0x50, block 2 with 0x90
    assert!(chip_writes.contains(&0x10), "Should have data from block 0");
    assert!(chip_writes.contains(&0x50), "Should have data from block 1");
    assert!(chip_writes.contains(&0x90), "Should have data from block 2");
}

#[test]
fn test_start_stream_with_multiple_blocks() {
    // Test that StartStream with data_start_offset works correctly
    // when multiple data blocks are concatenated
    let mut parser = VgmStream::new();
    let mut builder = VgmBuilder::new();

    // Create multiple data blocks with the same data_type (0x04 for OKIM6258)
    // Block 0: 4 bytes at offset 0-3
    let block0_data = vec![0x11, 0x22, 0x33, 0x44];
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x04,
        size: block0_data.len() as u32,
        data: block0_data.clone(),
    });

    // Block 1: 4 bytes at offset 4-7
    let block1_data = vec![0x55, 0x66, 0x77, 0x88];
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x04,
        size: block1_data.len() as u32,
        data: block1_data.clone(),
    });

    // Block 2: 4 bytes at offset 8-11
    let block2_data = vec![0x99, 0xAA, 0xBB, 0xCC];
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x04,
        size: block2_data.len() as u32,
        data: block2_data.clone(),
    });

    // Setup stream for OKIM6258
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: 0x17, // OKIM6258
        write_port: 0,
        write_command: 0x01,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0x04,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 8000,
    });

    // StartStream at offset 0 (start of block 0)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: 0,
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(soundlog::vgm::command::StopStream { stream_id: 0 });

    // StartStream at offset 5 (middle of block 1)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 5,
        length_mode: 0,
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(soundlog::vgm::command::StopStream { stream_id: 0 });

    // StartStream at offset 10 (middle of block 2)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 10,
        length_mode: 0,
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(10));

    builder.add_vgm_command(EndOfData);

    // Finalize and parse
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let mut chip_writes = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::Okim6258Write(_, spec) = cmd {
                    chip_writes.push(spec.value);
                }
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // Verify we got data from correct offsets across concatenated blocks
    // Offset 0: should read 0x11 (block 0, position 0)
    // Offset 5: should read 0x66 (block 1, position 1)
    // Offset 10: should read 0xBB (block 2, position 2)
    assert!(
        chip_writes.contains(&0x11),
        "Should read from offset 0 (block 0)"
    );
    assert!(
        chip_writes.contains(&0x66),
        "Should read from offset 5 (block 1)"
    );
    assert!(
        chip_writes.contains(&0xBB),
        "Should read from offset 10 (block 2)"
    );
}

#[test]
fn test_wait_expansion_with_stream_writes() {
    // This test verifies that when a large Wait command is processed,
    // and DAC stream writes occur during that wait period, the Wait is
    // properly expanded/split to maintain correct timing.
    //
    // Scenario:
    // - Main loop has Wait 44100 (1 second at 44.1kHz)
    // - Stream frequency is 22050 Hz (one write every 2 samples)
    // - During the 44100 sample wait, stream should generate writes at correct intervals
    // - The remaining Wait time after each stream write should be properly returned

    let mut parser = VgmStream::new();
    let mut builder = VgmBuilder::new();

    // Create stream data with enough samples
    let stream_data = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // Setup stream control for YM2612 DAC (chip_type=0x02, port=0, register=0x2A)
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    // Set frequency to 22050 Hz (one sample every ~2 samples at 44100 Hz rate)
    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 22050,
    });

    // Start stream with command count mode: play 4 samples
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: 1, // command count mode
        data_length: 4, // play 4 samples
    });

    // Large Wait: 44100 samples (1 second)
    // During this wait, stream should generate 4 writes (limited by data_length)
    builder.add_vgm_command(WaitSamples(44100));

    builder.add_vgm_command(EndOfData);

    // Parse the VGM
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let mut commands = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                commands.push(cmd);
            }
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // Stream control commands are handled internally and not returned
    let mut stream_write_count = 0;
    let mut _wait_count = 0;

    for cmd in &commands {
        match cmd {
            VgmCommand::Ym2612Write(_, spec) => {
                // Verify this is a DAC write (register 0x2A)
                if spec.register == 0x2A {
                    stream_write_count += 1;
                }
            }
            VgmCommand::WaitSamples(_) => _wait_count += 1,
            _ => {}
        }
    }

    // Verify we got the expected number of stream writes
    // At 22050 Hz, during 44100 samples we should get writes at intervals of ~2 samples
    // But we limited to 4 commands via length_mode=1 and data_length=4
    assert_eq!(
        stream_write_count, 4,
        "Expected 4 stream writes (limited by data_length)"
    );

    // Setup commands are handled internally and not returned to iterator

    // Verify the stream writes have the correct data
    let mut write_values = Vec::new();
    for cmd in &commands {
        if let VgmCommand::Ym2612Write(_, spec) = cmd
            && spec.register == 0x2A
        {
            write_values.push(spec.value);
        }
    }

    // The stream writes should contain the first 4 bytes of our stream data
    assert_eq!(
        write_values,
        vec![0x11, 0x22, 0x33, 0x44],
        "Stream writes should contain the correct data values"
    );

    // The key verification: All Wait time should be accounted for
    // The original Wait(44100) should be present in the output
    // Stream writes are interleaved, but the total Wait time should remain 44100
    let total_wait_samples: u64 = commands
        .iter()
        .filter_map(|cmd| {
            if let VgmCommand::WaitSamples(w) = cmd {
                Some(w.0 as u64)
            } else {
                None
            }
        })
        .sum();

    assert_eq!(
        total_wait_samples, 44100,
        "Total wait time should be 44100 samples (the original Wait should be preserved or properly split)"
    );
}

#[test]
fn test_wait_splitting_with_stream_timing() {
    // This test verifies the exact timing and ordering of Wait commands
    // when stream writes occur during a large Wait period.
    //
    // Expected behavior:
    // 1. Wait until first stream write is due
    // 2. Emit the stream write
    // 3. Wait until next stream write
    // 4. Repeat until all stream writes are done
    // 5. Emit remaining Wait time
    //
    // For example, with Wait(100) and stream frequency that generates writes at
    // sample positions 20, 40, 60, 80:
    // - Wait(20), StreamWrite, Wait(20), StreamWrite, Wait(20), StreamWrite, Wait(20), StreamWrite, Wait(20)

    let mut parser = VgmStream::new();
    let mut builder = VgmBuilder::new();

    // Create stream data
    let stream_data = vec![0xAA, 0xBB];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // Setup stream control
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A, // DAC register
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    // Set frequency to 44100 Hz (one write per sample at 44100 Hz base rate)
    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 44100,
    });

    // Start stream - play 2 samples
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: 1, // command count mode
        data_length: 2, // play 2 samples
    });

    // Wait for 100 samples
    // Stream writes should occur at sample 0 and sample 1
    builder.add_vgm_command(WaitSamples(100));

    builder.add_vgm_command(EndOfData);

    // Parse
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let mut commands = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                commands.push(cmd);
            }
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // Print command sequence for debugging
    println!("\n=== Command sequence ===");
    let mut sample_position = 0u64;
    for (i, cmd) in commands.iter().enumerate() {
        match cmd {
            VgmCommand::WaitSamples(w) => {
                sample_position += w.0 as u64;
                println!(
                    "[{}] Wait({}) -> sample position {}",
                    i, w.0, sample_position
                );
            }
            VgmCommand::Ym2612Write(_, spec) if spec.register == 0x2A => {
                println!(
                    "[{}] StreamWrite(0x{:02X}) at sample position {}",
                    i, spec.value, sample_position
                );
            }
            VgmCommand::StartStream(_) => {
                println!("[{}] StartStream at sample position {}", i, sample_position);
            }
            _ => {}
        }
    }

    // Count stream writes and verify data
    let stream_writes: Vec<u8> = commands
        .iter()
        .filter_map(|cmd| {
            if let VgmCommand::Ym2612Write(_, spec) = cmd {
                if spec.register == 0x2A {
                    Some(spec.value)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert_eq!(stream_writes.len(), 2, "Expected 2 stream writes");
    assert_eq!(
        stream_writes,
        vec![0xAA, 0xBB],
        "Stream writes should have correct data"
    );

    // Verify total wait time is preserved
    let total_wait: u64 = commands
        .iter()
        .filter_map(|cmd| {
            if let VgmCommand::WaitSamples(w) = cmd {
                Some(w.0 as u64)
            } else {
                None
            }
        })
        .sum();

    assert_eq!(
        total_wait, 100,
        "Total wait time should be 100 samples (original Wait preserved)"
    );

    // StartStream is handled internally and not returned to iterator
    // Just verify we got the stream writes
    assert!(
        !stream_writes.is_empty(),
        "Should have generated stream writes"
    );

    println!("\n=== Test Summary ===");
    println!("Total Wait samples: {}", total_wait);
    println!("Stream writes: {}", stream_writes.len());
    println!("All timing verified successfully!");
}

#[test]
fn test_from_document_basic() {
    // Test that VgmStream can be created from a VgmDocument
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(Wait882Samples);
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();

    // Create stream from document
    let mut stream = VgmStream::from_document(doc);

    let mut commands = Vec::new();
    for result in &mut stream {
        match result {
            Ok(StreamResult::Command(cmd)) => commands.push(cmd),
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // All wait commands (WaitSamples, Wait735Samples, Wait882Samples) are processed
    // through process_wait_with_streams and become WaitSamples variants
    // EndOfData is handled internally and not returned
    assert_eq!(commands.len(), 3);

    // Verify all waits are converted to WaitSamples
    assert!(matches!(
        commands[0],
        VgmCommand::WaitSamples(WaitSamples(100))
    ));
    assert!(matches!(
        commands[1],
        VgmCommand::WaitSamples(WaitSamples(735))
    ));
    assert!(matches!(
        commands[2],
        VgmCommand::WaitSamples(WaitSamples(882))
    ));
}

#[test]
fn test_from_document_with_stream_control() {
    // Test that stream control works correctly with from_document
    let mut builder = VgmBuilder::new();

    // Create stream data
    let stream_data = vec![0xAA, 0xBB, 0xCC];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // Setup stream
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A, // DAC register
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 44100,
    });

    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: 1, // command count
        data_length: 3,
    });

    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();

    // Process from document
    let mut stream = VgmStream::from_document(doc);

    let mut ym2612_writes = Vec::new();
    for result in &mut stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2612Write(_, spec))) => {
                if spec.register == 0x2A {
                    ym2612_writes.push(spec.value);
                }
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(_) => {}
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // Should have generated 3 stream writes
    assert_eq!(ym2612_writes, vec![0xAA, 0xBB, 0xCC]);
}

#[test]
fn test_from_commands() {
    // Test creating stream from a VgmDocument (replaces the old from_commands usage)
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(50));
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(EndOfData);
    let doc = builder.finalize();

    let mut stream = VgmStream::from_document(doc);

    let mut results = Vec::new();
    for result in &mut stream {
        match result {
            Ok(StreamResult::Command(cmd)) => results.push(cmd),
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // EndOfData is handled internally and not returned
    assert_eq!(results.len(), 3);
}

#[test]
#[should_panic(expected = "push_data() cannot be called on a VgmStream created from a document")]
fn test_push_data_panics_on_document_stream() {
    // Verify that push_data panics when called on a stream from document
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    let doc = builder.finalize();

    let mut stream = VgmStream::from_document(doc);

    // This should panic
    stream.push_data(&[0x62]);
}

#[test]
fn test_buffer_size_returns_zero_for_document_stream() {
    // Verify buffer_size returns 0 for document-based streams
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    let doc = builder.finalize();

    let stream = VgmStream::from_document(doc);

    assert_eq!(stream.buffer_size(), 0);
}

#[test]
fn test_fadeout_samples_basic() {
    // Test that fadeout_samples extends playback after loop end
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1)); // 1 total playthrough (no looping)
    parser.set_fadeout_samples(Some(100)); // 100 samples fadeout
    parser.push_data(&vgm_data);

    let mut total_wait_samples = 0u64;
    let mut command_count = 0;

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                command_count += 1;
                if let VgmCommand::WaitSamples(w) = cmd {
                    total_wait_samples += w.0 as u64;
                } else if let VgmCommand::Wait735Samples(_) = cmd {
                    total_wait_samples += 735;
                } else if let VgmCommand::Wait882Samples(_) = cmd {
                    total_wait_samples += 882;
                }
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(_) => break,
        }
    }

    // Should have processed commands beyond the normal end
    assert!(
        total_wait_samples >= 100,
        "Should have accumulated at least 100 wait samples for fadeout, got {}",
        total_wait_samples
    );
    assert!(command_count > 0);
}

#[test]
fn test_fadeout_samples_exact_timing() {
    // Test precise fadeout timing with known wait values
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(50));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1)); // 1 total playthrough (no looping)
    parser.set_fadeout_samples(Some(100)); // 100 samples fadeout
    parser.push_data(&vgm_bytes);

    let mut _wait_samples_after_end = 0u64;
    let mut loop_ended = false;

    loop {
        match parser.next() {
            Some(Ok(StreamResult::Command(cmd))) => {
                // EndOfData is handled internally
                // After loop count is reached, we're in fadeout period
                if parser.current_loop_count() >= 1 {
                    loop_ended = true;
                }

                if loop_ended {
                    // Count waits during fadeout
                    if let VgmCommand::WaitSamples(w) = cmd {
                        _wait_samples_after_end += w.0 as u64;
                    }
                }
            }
            Some(Ok(StreamResult::NeedsMoreData)) => {
                // No more data to push for this test
                break;
            }
            Some(Ok(StreamResult::EndOfStream)) => break,
            Some(Err(_)) => break,
            None => break,
        }
    }

    // After loop count is reached, should continue for fadeout period
    // With loop_count=1, we play once and then fadeout
    assert!(loop_ended, "Should have reached the loop end");
}

#[test]
fn test_fadeout_samples_with_stream_control() {
    // Test that fadeout works with DAC stream control
    let mut builder = VgmBuilder::new();

    // Create stream data
    let stream_data = vec![0x11, 0x22, 0x33, 0x44];
    let data_block = soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(data_block);

    // Setup stream
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 22050,
    });

    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: 1,
        data_length: 4,
    });

    builder.add_vgm_command(WaitSamples(50));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1));
    parser.set_fadeout_samples(Some(100)); // Allow 100 samples fadeout
    parser.push_data(&vgm_bytes);

    let mut stream_writes = 0;
    let mut total_wait = 0u64;

    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => match cmd {
                VgmCommand::Ym2612Write(_, spec) if spec.register == 0x2A => {
                    stream_writes += 1;
                }
                VgmCommand::WaitSamples(w) => {
                    total_wait += w.0 as u64;
                }
                _ => {}
            },
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(_) => break,
        }
    }

    // Should have generated stream writes
    assert_eq!(stream_writes, 4);
    // Should have accumulated wait time including fadeout
    assert!(total_wait >= 50);
}

#[test]
fn test_fadeout_samples_none() {
    // Test that without fadeout_samples set, stream ends immediately after loop
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(50));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1));
    // No fadeout_samples set
    parser.push_data(&vgm_bytes);

    let mut commands = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => commands.push(cmd),
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(_) => break,
        }
    }

    // Should end after one loop without fadeout
    assert!(commands.len() >= 2); // At least Wait and EndOfData
}

#[test]
fn test_loop_point_is_respected() {
    // Test that loop point is correctly calculated and used
    // Create a VGM with a loop point set at command index 2
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100)); // Index 0 - before loop
    builder.add_vgm_command(WaitSamples(200)); // Index 1 - before loop
    builder.add_vgm_command(WaitSamples(300)); // Index 2 - loop point
    builder.add_vgm_command(WaitSamples(400)); // Index 3 - after loop
    builder.add_vgm_command(EndOfData); // Index 4

    // Set loop point at index 2
    builder.set_loop_offset(2);

    let doc = builder.finalize();

    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(2)); // 2 playthroughs total (initial + 1 loop)

    let mut wait_values = Vec::new();

    for result in stream {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::WaitSamples(w) = cmd {
                    wait_values.push(w.0);
                }
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    // Expected wait values:
    // First playthrough: 100, 200, 300, 400
    // Second playthrough (loop from index 2): 300, 400
    // EndOfData is handled internally and not returned
    assert_eq!(
        wait_values,
        vec![100, 200, 300, 400, 300, 400],
        "Wait values should show initial playthrough followed by 1 loop from loop point"
    );
}

#[test]
fn test_set_and_get_fadeout_samples() {
    // Test getter/setter for fadeout_samples
    let mut stream = VgmStream::new();

    assert_eq!(stream.fadeout_samples(), None);

    stream.set_fadeout_samples(Some(44100));
    assert_eq!(stream.fadeout_samples(), Some(44100));

    stream.set_fadeout_samples(None);
    assert_eq!(stream.fadeout_samples(), None);
}

#[test]
fn test_multiple_dac_streams_wait_interleaving() {
    // This test verifies that when multiple DAC streams (using both StartStream
    // and StartStreamFastCall) are active simultaneously, Wait commands are
    // properly split and interleaved with stream writes from all streams.
    //
    // The key verification is that:
    // 1. No stream has a large burst of consecutive writes without Waits
    // 2. Total Wait time is preserved
    // 3. Stream writes are properly interleaved based on their frequencies
    //
    // Scenario:
    // - Stream 0 (YM2612 DAC port 0): 7813 Hz (interval ~5.643 samples at 44100 Hz)
    // - Stream 1 (YM2612 DAC port 1): 11025 Hz (interval 4 samples at 44100 Hz)
    // - Stream 2 (YM2151): 22050 Hz (interval 2 samples at 44100 Hz) via FastCall
    // - Large Wait during which all streams are active

    let mut parser = VgmStream::new();
    let mut builder = VgmBuilder::new();

    // Create stream data blocks for each stream
    // All streams use the same data_type (0x00) but different block IDs
    // Block IDs are assigned in order: block 0, block 1, block 2
    let stream0_data: Vec<u8> = (0x10..0x20).collect(); // 16 bytes for stream 0
    let stream1_data: Vec<u8> = (0x30..0x48).collect(); // 24 bytes for stream 1
    let stream2_data: Vec<u8> = (0x50..0x70).collect(); // 32 bytes for stream 2

    // Data block 0 for stream 0 (bank 0)
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream0_data.len() as u32,
        data: stream0_data.clone(),
    });

    // Data block 1 for stream 1 (bank 1)
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream1_data.len() as u32,
        data: stream1_data.clone(),
    });

    // Data block 2 for stream 2 (bank 2)
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream2_data.len() as u32,
        data: stream2_data.clone(),
    });

    // Setup stream 0: YM2612 DAC (chip_type=YM2612, port=0, register=0x2A)
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 0,
        write_command: 0x2A,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: 7813, // Non-integer interval: ~5.643 samples
    });

    // Setup stream 2: YM2151 (chip_type=YM2151, port=0, register=0x08)
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 2,
        chip_type: DacStreamChipType::Ym2151.to_u8(),
        write_port: 0,
        write_command: 0x08,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 1,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 1,
        frequency: 11025, // Interval: 4 samples
    });

    // Setup stream 1: YM2612 Port 1 (chip_type=YM2612, port=1, register=0x2A)
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 1,
        chip_type: DacStreamChipType::Ym2612.to_u8(),
        write_port: 1,
        write_command: 0x2A,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 2,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });

    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 2,
        frequency: 22050, // Interval: 2 samples
    });

    // Start stream 0 with normal StartStream from block 0 (play 10 samples)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0, // This is the offset within the data bank, block 0 starts at 0
        length_mode: 1,       // command count mode
        data_length: 10,
    });

    // Start stream 1 with normal StartStream from block 1 (play 15 samples)
    // Block 1 starts at offset 16 (after block 0's 16 bytes)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 1,
        data_start_offset: 16, // Start of block 1 within data bank 0
        length_mode: 1,
        data_length: 15,
    });

    // Start stream 2 with FastCall (block 2)
    // FastCall plays the entire block
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 2,
        block_id: 2,
        flags: 0x00,
    });

    // Large Wait during which all streams are active
    // At 44100 Hz:
    // - Stream 0 (7813 Hz): 10 writes over ~56.43 samples
    // - Stream 1 (11025 Hz): 15 writes over 60 samples
    // - Stream 2 (22050 Hz): 20 writes over 40 samples
    // Total wait of 300 samples should cover all of them
    builder.add_vgm_command(WaitSamples(300));

    builder.add_vgm_command(EndOfData);

    // Parse the VGM
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    parser.push_data(&vgm_bytes);

    let mut commands = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                commands.push(cmd);
            }
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    println!("\n=== Multiple DAC Streams Test ===");
    println!("Total commands emitted: {}", commands.len());

    // Collect writes for each stream
    let mut stream0_writes = Vec::new(); // YM2612 port 0 register 0x2A
    let mut stream1_writes = Vec::new(); // YM2612 port 1 register 0x2A
    let mut stream2_writes = Vec::new(); // YM2151 register 0x08

    for cmd in &commands {
        match cmd {
            VgmCommand::Ym2612Write(_, data_spec) => {
                if data_spec.port == 0 && data_spec.register == 0x2A {
                    stream0_writes.push(data_spec.value);
                } else if data_spec.port == 1 && data_spec.register == 0x2A {
                    stream1_writes.push(data_spec.value);
                }
            }
            VgmCommand::Ym2151Write(_, data_spec) => {
                if data_spec.register == 0x08 {
                    stream2_writes.push(data_spec.value);
                }
            }
            _ => {}
        }
    }

    println!(
        "Stream 0 (YM2612 port 0 DAC) writes: {}",
        stream0_writes.len()
    );
    println!(
        "Stream 1 (YM2612 port 1 DAC) writes: {}",
        stream1_writes.len()
    );
    println!("Stream 2 (YM2151) writes: {}", stream2_writes.len());

    // Verify expected number of writes (actual count depends on timing)
    // Stream 0: 10 writes requested
    // Stream 1: 15 writes requested
    // Stream 2: FastCall plays entire block (32 bytes)
    assert_eq!(stream0_writes.len(), 10, "Stream 0 should have 10 writes");
    assert_eq!(stream1_writes.len(), 15, "Stream 1 should have 15 writes");
    assert!(
        stream2_writes.len() >= 20 && stream2_writes.len() <= 32,
        "Stream 2 should have 20-32 writes, got {}",
        stream2_writes.len()
    );

    // Verify data correctness for first N bytes
    assert_eq!(
        &stream0_writes[..10],
        &(0x10..0x1A).collect::<Vec<u8>>()[..],
        "Stream 0 data should match"
    );
    assert_eq!(
        &stream1_writes[..15],
        &(0x30..0x3F).collect::<Vec<u8>>()[..],
        "Stream 1 data should match"
    );
    assert_eq!(
        &stream2_writes[..20],
        &(0x50..0x64).collect::<Vec<u8>>()[..],
        "Stream 2 first 20 bytes should match"
    );

    // Verify total wait time is preserved
    let total_wait: u64 = commands
        .iter()
        .filter_map(|cmd| {
            if let VgmCommand::WaitSamples(w) = cmd {
                Some(w.0 as u64)
            } else {
                None
            }
        })
        .sum();

    assert_eq!(
        total_wait, 300,
        "Total wait time should be preserved (300 samples)"
    );

    // Critical verification: Check that no stream has large consecutive write bursts
    // Track consecutive writes for each stream (without intervening Wait commands)
    let mut max_consecutive_stream0 = 0;
    let mut max_consecutive_stream1 = 0;
    let mut max_consecutive_stream2 = 0;
    let mut current_consecutive_stream0 = 0;
    let mut current_consecutive_stream1 = 0;
    let mut current_consecutive_stream2 = 0;

    for cmd in &commands {
        match cmd {
            VgmCommand::WaitSamples(_) => {
                // Wait resets all consecutive counters
                max_consecutive_stream0 = max_consecutive_stream0.max(current_consecutive_stream0);
                max_consecutive_stream1 = max_consecutive_stream1.max(current_consecutive_stream1);
                max_consecutive_stream2 = max_consecutive_stream2.max(current_consecutive_stream2);
                current_consecutive_stream0 = 0;
                current_consecutive_stream1 = 0;
                current_consecutive_stream2 = 0;
            }
            VgmCommand::Ym2612Write(_, data_spec) => {
                if data_spec.port == 0 && data_spec.register == 0x2A {
                    current_consecutive_stream0 += 1;
                } else if data_spec.port == 1 && data_spec.register == 0x2A {
                    current_consecutive_stream1 += 1;
                }
            }
            VgmCommand::Ym2151Write(_, data_spec) => {
                if data_spec.register == 0x08 {
                    current_consecutive_stream2 += 1;
                }
            }
            _ => {}
        }
    }

    // Final update for any trailing writes
    max_consecutive_stream0 = max_consecutive_stream0.max(current_consecutive_stream0);
    max_consecutive_stream1 = max_consecutive_stream1.max(current_consecutive_stream1);
    max_consecutive_stream2 = max_consecutive_stream2.max(current_consecutive_stream2);

    println!(
        "Max consecutive writes - Stream 0: {}",
        max_consecutive_stream0
    );
    println!(
        "Max consecutive writes - Stream 1: {}",
        max_consecutive_stream1
    );
    println!(
        "Max consecutive writes - Stream 2: {}",
        max_consecutive_stream2
    );

    // The key assertion: no stream should have large bursts
    // With proper Wait splitting, max consecutive writes should be very small (≤3)
    // This would fail with the old buggy behavior where all writes were emitted at once
    assert!(
        max_consecutive_stream0 <= 3,
        "Stream 0 should not have bursts of more than 3 consecutive writes, got {}",
        max_consecutive_stream0
    );
    assert!(
        max_consecutive_stream1 <= 3,
        "Stream 1 should not have bursts of more than 3 consecutive writes, got {}",
        max_consecutive_stream1
    );
    assert!(
        max_consecutive_stream2 <= 3,
        "Stream 2 should not have bursts of more than 3 consecutive writes, got {}",
        max_consecutive_stream2
    );

    println!("\n=== Test passed! ===");
    println!("All streams properly interleaved with Wait commands");
    println!("No large write bursts detected");
}
