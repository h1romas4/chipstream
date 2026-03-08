use soundlog::VgmBuilder;
use soundlog::VgmDocument;
use soundlog::vgm::command::DacStreamChipType;
use soundlog::vgm::command::{
    EndOfData, Instance, VgmCommand, Wait735Samples, Wait882Samples, WaitNSample, WaitSamples,
};
use soundlog::vgm::header::ChipId;
use soundlog::vgm::stream::{StreamResult, VgmStream};
use soundlog::{VgmCallbackStream, chip};
use std::cell::RefCell;
use std::rc::Rc;

/// Push only the command region of a serialized VGM file into a [`VgmStream`].
///
/// `VgmStream::push_chunk` operates on raw command bytes with no header.
/// This helper strips the VGM header by computing `command_start` from the
/// embedded version and data_offset fields before calling `push_chunk`.
fn push_vgm_bytes(stream: &mut VgmStream, vgm_bytes: &[u8]) {
    let header = soundlog::VgmHeader::from_bytes(vgm_bytes).expect("parse VGM header");
    let cmd_start = soundlog::VgmHeader::command_start(header.version, header.data_offset);
    stream
        .push_chunk(&vgm_bytes[cmd_start..])
        .expect("push chunk");
}

/// Helper function to create a simple VGM document with commands and loop setup
fn create_test_vgm_with_loop() -> Vec<u8> {
    let mut builder = VgmBuilder::new();

    // Add some basic commands
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(Wait882Samples);
    builder.add_vgm_command(WaitSamples(1000));
    builder.add_vgm_command(WaitNSample(5));

    // Set loop point at the second command (index 1)
    builder.set_loop_index(1);

    // Add end of data command
    builder.add_vgm_command(EndOfData);

    let document = builder.finalize();
    document.into()
}

#[test]
fn test_ym2612_0x8n_and_seekoffset_attach_datablock() {
    // This test verifies the 0x8n YM2612 DAC command expansion path when:
    // - an uncompressed PCM data block for YM2612 is attached via `attach_data_block`
    // - a `SeekOffset` is present to set the PCM read cursor
    // - successive `Ym2612Port0Address2AWriteAndWaitN` commands read bytes from the PCM bank
    //
    // We build a tiny PCM bank [0x11, 0x22], explicitly seek to offset 0 and issue two
    // 0x8n commands. We expect two Ym2612 write commands with values 0x11 and 0x22 in order.
    use soundlog::vgm::command::{SeekOffset, Ym2612Port0Address2AWriteAndWaitN};
    use soundlog::vgm::detail::{StreamChipType, UncompressedStream};

    let mut b = VgmBuilder::new();

    // Attach PCM bank for YM2612 (data block type 0x00)
    b.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0x11u8, 0x22u8],
    });

    // Explicitly set PCM cursor to 0 via SeekOffset
    b.add_vgm_command(SeekOffset(0u32));

    // Two 0x8n commands: each should read one byte and emit a Ym2612 write
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(0u8));
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(0u8));

    // Terminate
    b.add_vgm_command(EndOfData);

    let raw: Vec<u8> = b.finalize().into();

    let mut parser = VgmStream::new();
    parser.push_chunk(&raw).expect("push chunk");

    // Collect encountered Ym2612 write values
    let mut values = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::Ym2612Write(_inst, spec) = cmd {
                    values.push(spec.value);
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }

    assert_eq!(
        values,
        vec![0x11u8, 0x22u8],
        "Ym2612 writes should match PCM bank bytes after SeekOffset(0)"
    );
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

/// Test A: data exists + wait > 0
///
/// Exercises the branch in `handle_ym2612_port0_address_2a_write_and_wait_n` where
/// a PCM byte is available and wait_samples > 0. The function should emit a
/// Ym2612 write (value read from PCM bank) as part of stream processing.
#[test]
fn test_ym2612_0x8n_with_data_and_wait_gt_zero() {
    use soundlog::vgm::command::{SeekOffset, Ym2612Port0Address2AWriteAndWaitN};
    use soundlog::vgm::detail::{StreamChipType, UncompressedStream};

    let mut b = VgmBuilder::new();

    // Attach PCM bank for YM2612 (data block type 0x00)
    b.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0xAAu8],
    });

    // Seek to offset 0
    b.add_vgm_command(SeekOffset(0u32));

    // Issue 0x8n with wait > 0
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(3u8));

    // Terminate
    b.add_vgm_command(EndOfData);

    let raw: Vec<u8> = b.finalize().into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&raw).expect("push chunk");

    // Ensure Ym2612Write with value 0xAA is produced
    let mut found = false;
    for res in &mut parser {
        match res {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::Ym2612Write(_inst, spec) = cmd {
                    assert_eq!(spec.value, 0xAAu8);
                    found = true;
                    break;
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }
    assert!(
        found,
        "Expected Ym2612Write produced from PCM byte when wait>0"
    );
}

/// Test B: no data + wait > 0
///
/// Exercises the branch where no PCM data is available and wait_samples > 0.
/// In this case the handler should delegate to `process_wait_with_streams` and
/// a `WaitSamples` command should be returned (no Ym2612 write).
#[test]
fn test_ym2612_0x8n_no_data_and_wait_gt_zero() {
    use soundlog::vgm::command::Ym2612Port0Address2AWriteAndWaitN;

    let mut b = VgmBuilder::new();

    // No PCM bank attached.
    // Emit 0x8n with wait > 0
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(5u8));
    // Terminate
    b.add_vgm_command(EndOfData);

    let raw: Vec<u8> = b.finalize().into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&raw).expect("push chunk");

    // The first emitted command should be a WaitSamples (because there's no PCM byte).
    let mut saw_wait = false;
    for res in &mut parser {
        match res {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::WaitSamples(ws) = cmd {
                    // wait value should be the requested samples (clipped to u16 max)
                    assert_eq!(ws.0 as usize, 5usize);
                    saw_wait = true;
                    break;
                } else if let VgmCommand::Ym2612Write(_, _) = cmd {
                    panic!("Unexpected Ym2612Write when no PCM data should be available");
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }
    assert!(
        saw_wait,
        "Expected a WaitSamples command when no PCM data and wait>0"
    );
}

/// Test C: no data + wait == 0
///
/// Exercises the branch where no PCM data is available and wait_samples == 0.
/// The handler should call `next_command()` to continue parsing the following
/// command; if the following command is a `WaitSamples`, it will be emitted.
#[test]
fn test_ym2612_0x8n_no_data_and_wait_zero() {
    use soundlog::vgm::command::{WaitSamples, Ym2612Port0Address2AWriteAndWaitN};

    let mut b = VgmBuilder::new();

    // No PCM bank attached.
    // Emit 0x8n with wait == 0, followed by an explicit wait so that next_command()
    // advances to a known command that we can assert on.
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(0u8));
    b.add_vgm_command(WaitSamples(2u16));
    b.add_vgm_command(EndOfData);

    let raw: Vec<u8> = b.finalize().into();
    let mut parser = VgmStream::new();
    parser.push_chunk(&raw).expect("push chunk");

    // We expect to observe the explicit WaitSamples(2) as the handler should have
    // advanced to the next command when there was no PCM data and wait == 0.
    let mut saw_wait2 = false;
    for res in &mut parser {
        match res {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::WaitSamples(ws) = cmd {
                    assert_eq!(ws.0, 2u16);
                    saw_wait2 = true;
                    break;
                } else if let VgmCommand::Ym2612Write(_, _) = cmd {
                    panic!("Unexpected Ym2612Write when no PCM data and wait==0");
                }
            }
            Ok(StreamResult::NeedsMoreData) | Ok(StreamResult::EndOfStream) => break,
            Err(e) => panic!("parse error: {e:?}"),
        }
    }
    assert!(
        saw_wait2,
        "Expected the following WaitSamples(2) to be emitted after next_command()"
    );
}

#[test]
fn test_stream_parser_basic_functionality() {
    let vgm_data = create_test_vgm_with_loop();
    let mut parser = VgmStream::new();

    // Feed the entire VGM data at once
    parser.push_chunk(&vgm_data).expect("push chunk");
    // Verify VGM header parses from the provided bytes
    let header = soundlog::VgmHeader::from_bytes(&vgm_data).expect("parse header");
    assert_eq!(&header.ident, b"Vgm ");

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
    parser.push_chunk(&vgm_data).expect("push chunk");
    // Verify VGM header parses from the provided bytes
    let header = soundlog::VgmHeader::from_bytes(&vgm_data).expect("parse header");
    assert_eq!(&header.ident, b"Vgm ");

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
                    parser.push_chunk(&vgm_data).expect("push chunk");
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
fn test_stream_parser_none_infinite_loop() {
    // This test verifies that setting loop_count to None enables infinite looping
    // behavior for a stream created from a parsed VgmDocument (document-backed source).
    //
    // To keep the test bounded we break after observing ~10 command events and assert
    // that we were able to observe that many commands (i.e., the stream continued
    // looping rather than terminating immediately).
    let vgm_data = create_test_vgm_with_loop();

    // Parse into a VgmDocument and create a document-backed stream so the stream
    // can internally jump back to the loop index on EndOfData.
    let doc: VgmDocument = (&vgm_data[..]).try_into().expect("parse document");
    let mut parser = VgmStream::from_document(doc);

    // Enable infinite loop behavior
    parser.set_loop_count(None);

    // Feed is not needed for document-backed streams, but keep counter to break the test.
    let mut command_count = 0usize;
    let target = 10usize;

    // Iterate and count commands, breaking when we have observed enough.
    // Use a safety loop limit to avoid an actual infinite loop in pathological failures.
    let mut iterations = 0usize;
    let max_iterations = 1000usize;

    loop {
        iterations += 1;
        assert!(
            iterations < max_iterations,
            "Exceeded max iterations while testing infinite loop behavior"
        );

        match parser.next().unwrap().unwrap() {
            StreamResult::Command(_cmd) => {
                command_count += 1;
                if command_count >= target {
                    break;
                }
            }
            StreamResult::NeedsMoreData => {
                // Document-backed streams should not request more data, but break if they do.
                break;
            }
            StreamResult::EndOfStream => {
                // If the stream ended, infinite loop behavior is not present.
                break;
            }
        }
    }

    assert!(
        command_count >= target,
        "Expected to observe at least {} commands when loop_count is None (infinite loop); observed {}",
        target,
        command_count
    );
}

// Run this single test and show stdout:
// cargo test test_callback_stream_iteration_borrowing -p soundlog -- --nocapture
#[test]
fn test_callback_stream_iteration_borrowing() {
    use soundlog::VgmBuilder;
    use soundlog::VgmCallbackStream;
    use soundlog::chip;
    use soundlog::vgm::command::Instance;
    use std::cell::RefCell;

    // Build a small VGM with a chip write so callbacks will be invoked
    let mut b = VgmBuilder::new();
    b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
    b.add_chip_write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x22,
            value: 0x91,
        },
    );
    // Add an EndOfData to terminate
    b.add_vgm_command(soundlog::vgm::command::EndOfData);

    let raw: Vec<u8> = b.finalize().into();

    // Counter to be incremented by callback
    let invoked = RefCell::new(0usize);

    let mut cb_stream = VgmCallbackStream::from_vgm(raw).expect("valid VGM");

    // Print sizes to help reason about overhead
    println!(
        "size_of::<VgmCommand>() = {}",
        std::mem::size_of::<VgmCommand>()
    );
    println!(
        "size_of::<StreamResult>() = {}",
        std::mem::size_of::<StreamResult>()
    );

    // Register a write callback that increments the counter. This callback
    // captures nothing heavy and should be invoked even if the iterator's
    // consumer does not take ownership of the yielded VgmCommand.
    cb_stream.on_write(
        |_inst: Instance, _spec: chip::Ym2612Spec, _sample: usize, _event| {
            *invoked.borrow_mut() += 1;
        },
    );

    // Iterate using as_ref() / borrow so we don't move the yielded StreamResult's inner command.
    for result in &mut cb_stream {
        match result.as_ref().unwrap() {
            StreamResult::Command(_) => {
                // Do not move or examine the command; callbacks already handled work.
            }
            StreamResult::EndOfStream => break,
            StreamResult::NeedsMoreData => break,
        }
    }

    assert!(
        *invoked.borrow() > 0,
        "Expected callback to be invoked at least once"
    );
}

#[test]
fn test_stream_parser_incremental_data() {
    let vgm_data = create_test_vgm_with_loop();
    // Verify VGM header parses from the full document bytes before incremental feeding
    let header = soundlog::VgmHeader::from_bytes(&vgm_data).expect("parse header");
    assert_eq!(&header.ident, b"Vgm ");
    let mut parser = VgmStream::new();

    // Feed data in small chunks to test incremental parsing
    let chunk_size = 5;
    let mut offset = 0;
    let mut parsed_commands = Vec::new();

    while offset < vgm_data.len() {
        let end = std::cmp::min(offset + chunk_size, vgm_data.len());
        let chunk = &vgm_data[offset..end];
        parser.push_chunk(chunk).expect("push chunk");
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
    // Verify VGM header parses from the provided bytes
    let header = soundlog::VgmHeader::from_bytes(&vgm_data).expect("parse header");
    assert_eq!(&header.ident, b"Vgm ");
    let mut parser = VgmStream::new();

    // Feed the VGM data
    parser.push_chunk(&vgm_data).expect("push chunk");

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
    // Verify VGM header parses from the provided bytes
    let header = soundlog::VgmHeader::from_bytes(&vgm_data).expect("parse header");
    assert_eq!(&header.ident, b"Vgm ");
    let mut parser = VgmStream::new();
    parser.push_chunk(&vgm_data).expect("push chunk");

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
    parser.push_chunk(&large_data).expect("push chunk");

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
    parser.push_chunk(&vgm_data).expect("push chunk");
    let _ = parser.next().unwrap().unwrap();

    assert!(parser.buffer_size() > 0);

    // Reset the parser
    parser.reset();

    // Should be back to initial state
    assert_eq!(parser.buffer_size(), 0);
    assert_eq!(parser.current_loop_count(), 0);

    // Should be able to parse again after reset
    parser.push_chunk(&vgm_data).expect("push chunk");
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
    parser.push_chunk(&[0x61]).expect("push chunk"); // Command opcode only

    // Should need more data
    match parser.next().unwrap().unwrap() {
        StreamResult::NeedsMoreData => {
            println!("Correctly identified incomplete command");
        }
        other => panic!("Expected NeedsMoreData, got {:?}", other),
    }

    // Add one more byte (still incomplete)
    parser.push_chunk(&[0x44]).expect("push chunk");

    match parser.next().unwrap().unwrap() {
        StreamResult::NeedsMoreData => {
            println!("Still incomplete after one byte");
        }
        other => panic!("Expected NeedsMoreData, got {:?}", other),
    }

    // Add the final byte to complete the command
    parser.push_chunk(&[0x01]).expect("push chunk");

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
        parser.push_chunk(chunk).expect("push chunk");
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
    parser.push_chunk(&vgm_data).expect("push chunk");

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
                    parser.push_chunk(&vgm_data).expect("push chunk");
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
    parser.push_chunk(&vgm_data).expect("push chunk");

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
    parser.push_chunk(&bytes).expect("push chunk");

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
    // Verify VGM header parses from the full document bytes
    let header = soundlog::VgmHeader::from_bytes(&vgm_data).expect("parse header");
    assert_eq!(&header.ident, b"Vgm ");
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
        parser
            .push_chunk(&data_chunks[chunk_index])
            .expect("push chunk");
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
                        parser
                            .push_chunk(&data_chunks[chunk_index])
                            .expect("push chunk");
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
        parser.push_chunk(chunk).expect("push chunk");
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
                        parser.push_chunk(chunk).expect("push chunk");
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
    parser.push_chunk(&bytes).expect("push chunk");

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
    parser.push_chunk(&bytes).expect("push chunk");

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
    parser.push_chunk(&bytes).expect("push chunk");

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        },
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
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
            chip_type: DacStreamChipType {
                chip_id: ChipId::Ym2612,
                instance: Instance::Primary,
            },
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
            length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
                reverse: false,
                looped: false,
            }, // Play until end
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
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: false,
            looped: false,
        },
    });

    // Wait
    builder.add_vgm_command(WaitSamples(10));

    // End
    builder.add_vgm_command(EndOfData);

    // Finalize and feed parser
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Okim6258,
            instance: Instance::Primary,
        },
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
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: false,
            looped: false,
        },
    });
    builder.add_vgm_command(WaitSamples(10));

    // Start stream at block 1 (should start at offset 4)
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 1,
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: false,
            looped: false,
        },
    });
    builder.add_vgm_command(WaitSamples(10));

    // Start stream at block 2 (should start at offset 8)
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 2,
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: false,
            looped: false,
        },
    });
    builder.add_vgm_command(WaitSamples(10));

    builder.add_vgm_command(EndOfData);

    // Finalize and parse
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Okim6258,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::Ignore {
            reverse: false,
            looped: false,
        },
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(soundlog::vgm::command::StopStream { stream_id: 0 });

    // StartStream at offset 5 (middle of block 1)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 5,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: false,
            looped: false,
        },
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(soundlog::vgm::command::StopStream { stream_id: 0 });

    // StartStream at offset 10 (middle of block 2)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 10,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: false,
            looped: false,
        },
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(10));

    builder.add_vgm_command(EndOfData);

    // Finalize and parse
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        }, // command count mode
        data_length: 4, // play 4 samples
    });

    // Large Wait: 44100 samples (1 second)
    // During this wait, stream should generate 4 writes (limited by data_length)
    builder.add_vgm_command(WaitSamples(44100));

    builder.add_vgm_command(EndOfData);

    // Parse the VGM
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        }, // command count mode
        data_length: 2, // play 2 samples
    });

    // Wait for 100 samples
    // Stream writes should occur at sample 0 and sample 1
    builder.add_vgm_command(WaitSamples(100));

    builder.add_vgm_command(EndOfData);

    // Parse
    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        }, // command count
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
fn test_push_data_panics_on_document_stream() {
    // Verify that push_chunk returns error when called on a stream from document
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    let doc = builder.finalize();

    let mut stream = VgmStream::from_document(doc);

    // This should return an error
    let result = stream.push_chunk(&[0x62]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string()
            .contains("push_chunk() cannot be called on a VgmStream created from a document")
    );
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
    parser.push_chunk(&vgm_data).expect("push chunk");

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
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        },
        data_length: 4,
    });

    builder.add_vgm_command(WaitSamples(50));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut parser = VgmStream::new();
    parser.set_loop_count(Some(1));
    parser.set_fadeout_samples(Some(100)); // Allow 100 samples fadeout
    push_vgm_bytes(&mut parser, &vgm_bytes);

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
    push_vgm_bytes(&mut parser, &vgm_bytes);

    let mut commands = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(cmd)) => commands.push(cmd),
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => break,
            Err(_) => break,
        }
    }

    // Should end after one loop without fadeout.
    // EndOfData is handled internally and not returned to the iterator,
    // so only the WaitSamples(50) command is in the output.
    assert_eq!(commands.len(), 1); // Only WaitSamples; EndOfData is not returned
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
    builder.set_loop_index(2);

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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2151,
            instance: Instance::Primary,
        },
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
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Primary,
        },
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
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        }, // command count mode
        data_length: 10,
    });

    // Start stream 1 with normal StartStream from block 1 (play 15 samples)
    // Block 1 starts at offset 16 (after block 0's 16 bytes)
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 1,
        data_start_offset: 16, // Start of block 1 within data bank 0
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: false,
        },
        data_length: 15,
    });

    // Start stream 2 with FastCall (block 2)
    // FastCall plays the entire block
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 2,
        block_id: 2,
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: false,
            looped: false,
        },
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
    push_vgm_bytes(&mut parser, &vgm_bytes);

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

#[test]
fn test_buffer_size_limit_exceeded() {
    // Test that push_chunk returns error when buffer size limit is exceeded
    let mut stream = VgmStream::new();

    // Create a chunk that's 65 MB (exceeds 64 MB limit)
    let large_chunk = vec![0x62; 65 * 1024 * 1024];

    let result = stream.push_chunk(&large_chunk);
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err.to_string().contains("Buffer size limit exceeded"));
}

#[test]
fn test_buffer_size_cumulative_limit() {
    // Test that cumulative pushes can't exceed buffer limit
    let mut stream = VgmStream::new();

    // Push chunks that individually are fine but cumulatively exceed limit
    let chunk_size = 40 * 1024 * 1024; // 40 MB
    stream
        .push_chunk(&vec![0x62; chunk_size])
        .expect("first chunk");

    // Second 40 MB chunk should fail (total would be 80 MB > 64 MB limit)
    let result = stream.push_chunk(&vec![0x62; chunk_size]);
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err.to_string().contains("Buffer size limit exceeded"));
}

#[test]
fn test_set_max_buffer_size() {
    // Test that max buffer size can be configured
    let mut stream = VgmStream::new();

    // Default should be 64 MB
    assert_eq!(stream.max_buffer_size(), 64 * 1024 * 1024);

    // Set to 128 MB
    stream.set_max_buffer_size(128 * 1024 * 1024);
    assert_eq!(stream.max_buffer_size(), 128 * 1024 * 1024);

    // Should now accept 80 MB chunk that would have failed with default limit
    let chunk_size = 80 * 1024 * 1024;
    stream
        .push_chunk(&vec![0x62; chunk_size])
        .expect("large chunk with increased limit");

    // But 50 MB more should fail (total 130 MB > 128 MB limit)
    let result = stream.push_chunk(&vec![0x62; 50 * 1024 * 1024]);
    assert!(result.is_err());
}

#[test]
fn test_set_max_buffer_size_smaller_limit() {
    // Test setting a smaller buffer size limit
    let mut stream = VgmStream::new();

    // Set to 10 MB
    stream.set_max_buffer_size(10 * 1024 * 1024);

    // 5 MB should work
    stream
        .push_chunk(&vec![0x62; 5 * 1024 * 1024])
        .expect("5 MB chunk");

    // Another 6 MB should fail (total 11 MB > 10 MB limit)
    let result = stream.push_chunk(&vec![0x62; 6 * 1024 * 1024]);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Buffer size limit exceeded")
    );
}

#[test]
fn test_push_data_chunked_512_bytes() {
    // Test feeding VGM data in 512-byte chunks via push_data
    // Verify that commands are properly parsed even when split across chunks
    let mut builder = VgmBuilder::new();

    // Register a chip to ensure header is non-trivial
    builder.register_chip(soundlog::chip::Chip::Ym2612, 0, 7670454);

    // Add various commands
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(WaitSamples(1000));
    builder.add_vgm_command(Wait882Samples);
    builder.add_vgm_command(WaitSamples(500));
    builder.add_vgm_command(WaitNSample(123));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    // Create stream and feed data in 512-byte chunks
    let mut stream = VgmStream::new();
    const CHUNK_SIZE: usize = 512;

    let mut offset = 0;
    let mut commands = Vec::new();

    while offset < vgm_bytes.len() {
        let end = (offset + CHUNK_SIZE).min(vgm_bytes.len());
        let chunk = &vgm_bytes[offset..end];

        stream.push_chunk(chunk).expect("push chunk");
        offset = end;

        // Try to parse commands after each chunk
        loop {
            match stream.next() {
                Some(Ok(StreamResult::Command(cmd))) => {
                    commands.push(cmd);
                }
                Some(Ok(StreamResult::NeedsMoreData)) => break,
                Some(Ok(StreamResult::EndOfStream)) => break,
                Some(Err(e)) => panic!("Parse error: {:?}", e),
                None => break,
            }
        }
    }

    // Verify we got wait commands (EndOfData is handled internally)
    let wait_count = commands
        .iter()
        .filter(|c| {
            matches!(
                c,
                VgmCommand::Wait735Samples(_)
                    | VgmCommand::Wait882Samples(_)
                    | VgmCommand::WaitSamples(_)
                    | VgmCommand::WaitNSample(_)
            )
        })
        .count();

    assert!(
        wait_count >= 3,
        "Should parse at least 3 wait commands, got {}",
        wait_count
    );

    // Verify we parsed some commands successfully
    assert!(
        !commands.is_empty(),
        "Should have parsed commands with chunked data"
    );
}

#[test]
fn test_push_data_header_availability() {
    // Test that header information becomes available after enough data is pushed
    // VGM header is at least 0x40 bytes, so we need to push that much
    let mut builder = VgmBuilder::new();

    // Register chips and set metadata
    builder.register_chip(soundlog::chip::Chip::Ym2612, 0, 7670454);
    builder.register_chip(soundlog::chip::Chip::Sn76489, 0, 3579545);

    // Add commands
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();

    // Push first 64 bytes (minimum header size) to ensure header is parsed
    let header_size = 64.min(vgm_bytes.len());
    stream
        .push_chunk(&vgm_bytes[0..header_size])
        .expect("push chunk");

    // Try to parse - this will internally parse the header
    // We can verify the stream is working by checking buffer size
    assert!(
        stream.buffer_size() >= header_size,
        "Stream should have buffered header data"
    );

    // Push remaining data and verify commands can be parsed
    stream
        .push_chunk(&vgm_bytes[header_size..])
        .expect("push chunk");

    let mut command_found = false;
    if let Some(Ok(StreamResult::Command(_))) = stream.next() {
        command_found = true;
    }

    assert!(
        command_found,
        "Should be able to parse commands after header is available"
    );
}

#[test]
fn test_push_data_very_small_chunks() {
    // Test with extremely small chunks (16 bytes) to verify robustness
    let mut builder = VgmBuilder::new();

    builder.register_chip(soundlog::chip::Chip::Ym2612, 0, 7670454);
    builder.add_vgm_command(Wait735Samples);
    builder.add_vgm_command(WaitSamples(2000));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();
    const CHUNK_SIZE: usize = 16;

    let mut offset = 0;
    let mut commands = Vec::new();

    while offset < vgm_bytes.len() {
        let end = (offset + CHUNK_SIZE).min(vgm_bytes.len());
        stream
            .push_chunk(&vgm_bytes[offset..end])
            .expect("push chunk");
        offset = end;

        // Parse available commands
        loop {
            match stream.next() {
                Some(Ok(StreamResult::Command(cmd))) => commands.push(cmd),
                Some(Ok(StreamResult::NeedsMoreData)) => break,
                Some(Ok(StreamResult::EndOfStream)) => break,
                Some(Err(e)) => panic!("Parse error with small chunks: {:?}", e),
                None => break,
            }
        }
    }

    // Verify commands were parsed correctly
    let wait_commands: Vec<_> = commands
        .iter()
        .filter(|c| {
            matches!(
                c,
                VgmCommand::Wait735Samples(_) | VgmCommand::WaitSamples(_)
            )
        })
        .collect();

    assert_eq!(
        wait_commands.len(),
        2,
        "Should parse both wait commands even with tiny chunks"
    );
}

#[test]
fn test_push_data_with_loop() {
    // Test looping with from_document (push_data doesn't support loop_offset)
    // This test verifies loop functionality works with chunked-built VGM
    let mut builder = VgmBuilder::new();

    builder.register_chip(soundlog::chip::Chip::Ym2612, 0, 7670454);

    // Commands before loop
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(WaitSamples(200));

    // Set loop point here
    builder.set_loop_index(2);

    // Commands in loop
    builder.add_vgm_command(WaitSamples(300));
    builder.add_vgm_command(WaitSamples(400));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();

    // Use from_document for proper loop support
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(2)); // 2 playthroughs total (initial + 1 loop)

    let mut wait_values = Vec::new();

    loop {
        match stream.next() {
            Some(Ok(StreamResult::Command(cmd))) => {
                if let VgmCommand::WaitSamples(w) = cmd {
                    wait_values.push(w.0);
                }
            }
            Some(Ok(StreamResult::NeedsMoreData)) => break,
            Some(Ok(StreamResult::EndOfStream)) => break,
            Some(Err(e)) => panic!("Parse error: {:?}", e),
            None => break,
        }
    }

    // Expected: intro (100, 200) + loop section (300, 400) + loop again (300, 400)
    assert_eq!(
        wait_values,
        vec![100, 200, 300, 400, 300, 400],
        "Should play intro once then loop section twice"
    );
}

#[test]
fn test_push_data_buffer_size() {
    // Test buffer_size() method with push_data
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();

    assert_eq!(stream.buffer_size(), 0, "Buffer should be empty initially");

    // Push some data
    stream.push_chunk(&vgm_bytes[0..100]).expect("push chunk");
    assert_eq!(
        stream.buffer_size(),
        100,
        "Buffer size should reflect pushed data"
    );

    // Parse some commands (will consume buffer)
    while let Some(Ok(StreamResult::Command(_))) = stream.next() {}

    // Buffer size should have decreased as commands were parsed
    assert!(
        stream.buffer_size() <= 100,
        "Buffer should be consumed during parsing"
    );
}

#[test]
fn test_push_data_reset() {
    // Test that reset() clears buffer and state
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();
    push_vgm_bytes(&mut stream, &vgm_bytes);

    // Parse a command
    if let Some(Ok(StreamResult::Command(_))) = stream.next() {
        // Command parsed
    }

    // Reset the stream
    stream.reset();

    // Should be able to push data again and parse from beginning
    push_vgm_bytes(&mut stream, &vgm_bytes);
    let mut commands = Vec::new();

    while let Some(Ok(StreamResult::Command(cmd))) = stream.next() {
        commands.push(cmd);
    }

    assert!(
        !commands.is_empty(),
        "Should be able to parse commands after reset"
    );
}

#[test]
fn test_data_block_size_limit_default() {
    let stream = VgmStream::new();

    // Verify default limit is 32MB
    assert_eq!(stream.max_data_block_size(), 32 * 1024 * 1024);
    assert_eq!(stream.total_data_block_size(), 0);
}

#[test]
fn test_data_block_size_limit_setter() {
    let mut stream = VgmStream::new();

    // Set a custom limit
    let custom_limit = 1024 * 1024; // 1 MB
    stream.set_max_data_block_size(custom_limit);

    assert_eq!(stream.max_data_block_size(), custom_limit);
}

#[test]
fn test_data_block_size_tracking() {
    use soundlog::vgm::command::DataBlock;

    let mut builder = VgmBuilder::new();

    // Add a data block
    let block = DataBlock {
        marker: 0x67,
        chip_instance: 0,
        data_type: 0x00,
        size: 1000,
        data: vec![0u8; 1000],
    };
    builder.add_vgm_command(VgmCommand::DataBlock(Box::new(block)));
    builder.add_vgm_command(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();
    push_vgm_bytes(&mut stream, &vgm_bytes);

    // Parse through the stream
    while let Some(Ok(result)) = stream.next() {
        match result {
            StreamResult::Command(_) => {}
            StreamResult::NeedsMoreData => break,
            StreamResult::EndOfStream => break,
        }
    }

    // Verify total size was tracked
    assert!(stream.total_data_block_size() > 0);
}

#[test]
fn test_data_block_size_limit_exceeded() {
    use soundlog::ParseError;
    use soundlog::vgm::command::DataBlock;

    let mut builder = VgmBuilder::new();

    // Create a data block larger than our limit
    let block_size = 2000;
    let block = DataBlock {
        marker: 0x67,
        chip_instance: 0,
        data_type: 0x00,
        size: block_size as u32,
        data: vec![0u8; block_size],
    };
    builder.add_vgm_command(VgmCommand::DataBlock(Box::new(block)));
    builder.add_vgm_command(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();
    // Set a very small limit to trigger the error
    stream.set_max_data_block_size(1000);
    push_vgm_bytes(&mut stream, &vgm_bytes);

    // Parse through the stream - should get an error
    let mut got_error = false;
    for result in &mut stream {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => break,
            Err(ParseError::DataBlockSizeExceeded {
                current_size,
                limit,
                attempted_size,
            }) => {
                got_error = true;
                assert_eq!(limit, 1000);
                assert_eq!(attempted_size, block_size);
                assert_eq!(current_size, 0);
                break;
            }
            Err(e) => {
                panic!("Unexpected error: {:?}", e);
            }
        }
    }

    assert!(got_error, "Expected DataBlockSizeExceeded error");
}

#[test]
fn test_data_block_size_reset() {
    use soundlog::vgm::command::DataBlock;

    let mut builder = VgmBuilder::new();

    let block = DataBlock {
        marker: 0x67,
        chip_instance: 0,
        data_type: 0x00,
        size: 500,
        data: vec![0u8; 500],
    };
    builder.add_vgm_command(VgmCommand::DataBlock(Box::new(block)));
    builder.add_vgm_command(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();
    push_vgm_bytes(&mut stream, &vgm_bytes);

    // Parse to accumulate some data block size
    while let Some(Ok(result)) = stream.next() {
        match result {
            StreamResult::Command(_) => {}
            StreamResult::NeedsMoreData => break,
            StreamResult::EndOfStream => break,
        }
    }

    let size_before_reset = stream.total_data_block_size();
    assert!(
        size_before_reset > 0,
        "Should have accumulated some data block size"
    );

    // Reset should clear the total size
    stream.reset();
    assert_eq!(
        stream.total_data_block_size(),
        0,
        "Reset should clear total data block size"
    );
}

#[test]
fn test_multiple_data_blocks_cumulative_size() {
    use soundlog::vgm::command::DataBlock;

    let mut builder = VgmBuilder::new();

    // Add multiple data blocks with data_type 0x00 (PCM data)
    // These will be stored internally as uncompressed streams
    for _i in 0..5 {
        let block = DataBlock {
            marker: 0x67,
            chip_instance: 0,
            data_type: 0x00, // PCM data type - will be stored internally
            size: 100,
            data: vec![0u8; 100],
        };
        builder.add_vgm_command(VgmCommand::DataBlock(Box::new(block)));
    }
    builder.add_vgm_command(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    let mut stream = VgmStream::new();
    push_vgm_bytes(&mut stream, &vgm_bytes);

    // Parse all blocks - they will be stored internally, not returned as commands
    while let Some(Ok(result)) = stream.next() {
        match result {
            StreamResult::Command(_) => {}
            StreamResult::NeedsMoreData => break,
            StreamResult::EndOfStream => break,
        }
    }

    // Verify that total size was tracked even though blocks weren't returned
    // Total size should be cumulative (5 blocks * 100 bytes each = 500)
    assert!(
        stream.total_data_block_size() >= 500,
        "Total size should be at least 500 bytes, got {}",
        stream.total_data_block_size()
    );
}

#[test]
fn test_push_chunk_wrapper_on_bytes_stream() {
    // Ensure push_chunk forwards to the inner VgmStream when created with new()
    let inner = VgmStream::new();
    let mut callback_stream = VgmCallbackStream::new(inner);
    let chunk = vec![0x56, 0x67, 0x6D, 0x20];
    assert!(callback_stream.push_chunk(&chunk).is_ok());
}

#[test]
fn test_push_chunk_wrapper_on_document_stream_errors() {
    // push_chunk should return an error when the underlying stream is from_document()
    let doc = VgmDocument::default();
    let inner = VgmStream::from_document(doc);
    let mut callback_stream = VgmCallbackStream::new(inner);
    let chunk = vec![0x00];
    assert!(callback_stream.push_chunk(&chunk).is_err());
}

#[test]
fn test_callback_stream_struct_size() {
    // Test to investigate the size of VgmCallbackStream structure
    // VgmCallbackStream is approximately 30KB (29 KB) due to all chip state trackers
    use std::mem::size_of;

    let size = size_of::<VgmCallbackStream>();
    println!(
        "VgmCallbackStream size: {} bytes ({} KB)",
        size,
        size / 1024
    );

    // The struct is large but using setter pattern (&mut self) avoids stack overflow
    assert!(
        size < 1_000_000,
        "VgmCallbackStream is unexpectedly large: {} bytes",
        size
    );
}

#[test]
fn test_callback_stream_with_track_chips() {
    // Test VgmCallbackStream using track_chips() setter method
    let mut builder = VgmBuilder::new();

    // Register YM2612 Primary
    builder.register_chip(chip::Chip::Ym2612, Instance::Primary, 7_670_454);

    // Add a register write
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0,
        },
    );

    // Prevent infinite loop
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let header = doc.header.clone();

    // Create VgmCallbackStream and use track_chips to enable state tracking
    let stream = VgmStream::from_document(doc);
    let mut callback_stream = VgmCallbackStream::new(stream);
    callback_stream.track_chips(&header.chip_instances());

    // Counter for callback invocations
    let write_count = Rc::new(RefCell::new(0));
    let wc = write_count.clone();

    // Register callback
    callback_stream.on_write(move |inst, spec: chip::Ym2612Spec, _sample, _event| {
        *wc.borrow_mut() += 1;
        assert_eq!(inst, Instance::Primary);
        assert_eq!(spec.port, 0);
        assert_eq!(spec.register, 0x28);
        assert_eq!(spec.value, 0xF0);
    });

    // Iterate through stream
    // Note: Iterator returns None on EndOfStream, so loop completion means success
    for result in callback_stream {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) => {
                panic!("Unexpected NeedsMoreData");
            }
            Err(e) => {
                panic!("Stream error: {:?}", e);
            }
            Ok(StreamResult::EndOfStream) => {
                panic!("EndOfStream should not be yielded, iterator should return None");
            }
        }
    }

    // If we get here, the iterator returned None (EndOfStream)
    assert_eq!(*write_count.borrow(), 1, "Should have exactly 1 write");
}

#[test]
fn test_callback_stream_with_single_chip() {
    // Test VgmCallbackStream with a single chip using individual track_*_state method
    let mut builder = VgmBuilder::new();

    // Register only YM2612 Primary
    builder.register_chip(chip::Chip::Ym2612, Instance::Primary, 7_670_454);

    // Add a simple register write
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0,
        },
    );

    // Prevent infinite loop
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();

    // Create VgmCallbackStream and enable state tracking
    let stream = VgmStream::from_document(doc);
    let mut callback_stream = VgmCallbackStream::new(stream);
    callback_stream.track_state::<chip::state::Ym2612State>(Instance::Primary, 7_670_454.0);

    // Counter for callback invocations
    let write_count = Rc::new(RefCell::new(0));
    let wc = write_count.clone();

    // Register callback
    callback_stream.on_write(move |inst, spec: chip::Ym2612Spec, _sample, _event| {
        *wc.borrow_mut() += 1;
        assert_eq!(inst, Instance::Primary);
        assert_eq!(spec.port, 0);
        assert_eq!(spec.register, 0x28);
        assert_eq!(spec.value, 0xF0);
    });

    // Iterate through stream
    // Note: Iterator returns None on EndOfStream, so loop completion means success
    for result in callback_stream {
        match result {
            Ok(StreamResult::Command(_)) => {}
            Ok(StreamResult::NeedsMoreData) => {
                panic!("Unexpected NeedsMoreData");
            }
            Err(e) => {
                panic!("Stream error: {:?}", e);
            }
            Ok(StreamResult::EndOfStream) => {
                panic!("EndOfStream should not be yielded, iterator should return None");
            }
        }
    }

    // If we get here, the iterator returned None (EndOfStream)
    assert_eq!(*write_count.borrow(), 1, "Should have exactly 1 write");
}

#[test]
fn test_callback_stream_multiple_chips_and_instances() {
    // Create VGM document with multiple chips using both Primary and Secondary instances
    let mut builder = VgmBuilder::new();

    // Register YM2612 with both Primary and Secondary instances
    builder.register_chip(chip::Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.register_chip(chip::Chip::Ym2612, Instance::Secondary, 7_670_454);

    // Register SN76489 with both Primary and Secondary instances
    builder.register_chip(chip::Chip::Sn76489, Instance::Primary, 3_579_545);
    builder.register_chip(chip::Chip::Sn76489, Instance::Secondary, 3_579_545);

    // Register YM2151 with both Primary and Secondary instances
    builder.register_chip(chip::Chip::Ym2151, Instance::Primary, 3_579_545);
    builder.register_chip(chip::Chip::Ym2151, Instance::Secondary, 3_579_545);

    // YM2612 Primary: register write (port 0, reg 0x28 = Key On/Off)
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0, // Key On ch0
        },
    );

    // YM2612 Secondary: register write (port 0, reg 0x28)
    builder.add_chip_write(
        Instance::Secondary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF1, // Key On ch1
        },
    );

    // SN76489 Primary: register write
    builder.add_chip_write(
        Instance::Primary,
        chip::PsgSpec {
            value: 0x80, // Tone 0 frequency
        },
    );

    // SN76489 Secondary: register write
    builder.add_chip_write(
        Instance::Secondary,
        chip::PsgSpec {
            value: 0x90, // Tone 1 frequency
        },
    );

    // YM2151 Primary: register write (reg 0x08 = Key On)
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2151Spec {
            register: 0x08,
            value: 0x78, // Key On ch0
        },
    );

    // YM2151 Secondary: register write (reg 0x08)
    builder.add_chip_write(
        Instance::Secondary,
        chip::Ym2151Spec {
            register: 0x08,
            value: 0x79, // Key On ch1
        },
    );

    // Add more register writes to verify state changes
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0xA0, // FNUM1
            value: 0x44,
        },
    );

    builder.add_chip_write(
        Instance::Secondary,
        chip::Ym2151Spec {
            register: 0x20, // RL/FB/CON
            value: 0xC7,
        },
    );

    // Add wait command
    builder.add_vgm_command(WaitSamples(100));

    // Always add EndOfData to prevent infinite loop
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();

    // Create VgmCallbackStream
    // Use individual track_*_state methods
    let stream = VgmStream::from_document(doc);
    let mut callback_stream = VgmCallbackStream::new(stream);
    callback_stream.track_state::<chip::state::Ym2612State>(Instance::Primary, 7_670_454.0);
    callback_stream.track_state::<chip::state::Ym2612State>(Instance::Secondary, 7_670_454.0);
    callback_stream.track_state::<chip::state::Sn76489State>(Instance::Primary, 3_579_545.0);
    callback_stream.track_state::<chip::state::Sn76489State>(Instance::Secondary, 3_579_545.0);
    callback_stream.track_state::<chip::state::Ym2151State>(Instance::Primary, 3_579_545.0);
    callback_stream.track_state::<chip::state::Ym2151State>(Instance::Secondary, 3_579_545.0);

    // Prepare callback counters (wrapped with RefCell for interior mutability)
    let ym2612_primary_writes = Rc::new(RefCell::new(0));
    let ym2612_secondary_writes = Rc::new(RefCell::new(0));
    let sn76489_primary_writes = Rc::new(RefCell::new(0));
    let sn76489_secondary_writes = Rc::new(RefCell::new(0));
    let ym2151_primary_writes = Rc::new(RefCell::new(0));
    let ym2151_secondary_writes = Rc::new(RefCell::new(0));

    // Register YM2612 callback
    let ym2612_p = ym2612_primary_writes.clone();
    let ym2612_s = ym2612_secondary_writes.clone();
    callback_stream.on_write(move |inst, spec: chip::Ym2612Spec, _sample, _event| {
        match inst {
            Instance::Primary => {
                let mut count = ym2612_p.borrow_mut();
                *count += 1;
                // Verify register write content
                if *count == 1 {
                    assert_eq!(spec.port, 0);
                    assert_eq!(spec.register, 0x28);
                    assert_eq!(spec.value, 0xF0);
                } else if *count == 2 {
                    assert_eq!(spec.register, 0xA0);
                    assert_eq!(spec.value, 0x44);
                }
            }
            Instance::Secondary => {
                let mut count = ym2612_s.borrow_mut();
                *count += 1;
                assert_eq!(spec.port, 0);
                assert_eq!(spec.register, 0x28);
                assert_eq!(spec.value, 0xF1);
            }
        }
    });

    // Register SN76489 callback
    let sn_p = sn76489_primary_writes.clone();
    let sn_s = sn76489_secondary_writes.clone();
    callback_stream.on_write(
        move |inst, spec: chip::PsgSpec, _sample, _event| match inst {
            Instance::Primary => {
                *sn_p.borrow_mut() += 1;
                assert_eq!(spec.value, 0x80);
            }
            Instance::Secondary => {
                *sn_s.borrow_mut() += 1;
                assert_eq!(spec.value, 0x90);
            }
        },
    );

    // Register YM2151 callback
    let ym2151_p = ym2151_primary_writes.clone();
    let ym2151_s = ym2151_secondary_writes.clone();
    callback_stream.on_write(
        move |inst, spec: chip::Ym2151Spec, _sample, _event| match inst {
            Instance::Primary => {
                *ym2151_p.borrow_mut() += 1;
                assert_eq!(spec.register, 0x08);
                assert_eq!(spec.value, 0x78);
            }
            Instance::Secondary => {
                let mut count = ym2151_s.borrow_mut();
                *count += 1;
                if *count == 1 {
                    assert_eq!(spec.register, 0x08);
                    assert_eq!(spec.value, 0x79);
                } else if *count == 2 {
                    assert_eq!(spec.register, 0x20);
                    assert_eq!(spec.value, 0xC7);
                }
            }
        },
    );

    // Process commands via iterator (ensure no infinite loop)
    let mut command_count = 0;
    // Note: Iterator returns None on EndOfStream, so loop completion means success
    for result in callback_stream {
        match result {
            Ok(StreamResult::Command(_cmd)) => {
                command_count += 1;
                // Prevent infinite loop: panic if too many commands
                assert!(
                    command_count < 100,
                    "Too many commands, possible infinite loop"
                );
            }
            Ok(StreamResult::NeedsMoreData) => {
                panic!("Unexpected NeedsMoreData from document stream");
            }
            Err(e) => {
                panic!("Stream error: {:?}", e);
            }
            Ok(StreamResult::EndOfStream) => {
                panic!("EndOfStream should not be yielded, iterator should return None");
            }
        }
    }

    // If we get here, the iterator returned None (EndOfStream)

    // Verify expected number of writes for each chip instance
    assert_eq!(
        *ym2612_primary_writes.borrow(),
        2,
        "YM2612 Primary should have 2 writes"
    );
    assert_eq!(
        *ym2612_secondary_writes.borrow(),
        1,
        "YM2612 Secondary should have 1 write"
    );
    assert_eq!(
        *sn76489_primary_writes.borrow(),
        1,
        "SN76489 Primary should have 1 write"
    );
    assert_eq!(
        *sn76489_secondary_writes.borrow(),
        1,
        "SN76489 Secondary should have 1 write"
    );
    assert_eq!(
        *ym2151_primary_writes.borrow(),
        1,
        "YM2151 Primary should have 1 write"
    );
    assert_eq!(
        *ym2151_secondary_writes.borrow(),
        2,
        "YM2151 Secondary should have 2 writes"
    );
}

#[test]
fn test_vgm_callback_stream_push_chunk_large_doc() {
    // This test ensures `VgmCallbackStream::push_chunk` can accept a larger
    // serialized `VgmDocument` streamed in chunks and that registered write
    // callbacks are invoked for chip register writes.
    //
    // We construct a document with many YM2612 writes interleaved with waits,
    // then feed its bytes to a `VgmCallbackStream` created from `VgmStream::new()`
    // (so push_chunk is allowed). The test counts callback invocations to ensure
    // writes were processed.
    use std::cell::RefCell;
    use std::rc::Rc;

    // Build a document with many writes
    let mut builder = VgmBuilder::new();

    // Register a YM2612 instance at id 0 so writes map to a tracked instance
    builder.register_chip(soundlog::chip::Chip::Ym2612, 0, 7987200);

    // Add 200 writes interleaved with small waits to make the document reasonably large
    for i in 0u16..200u16 {
        builder.add_chip_write(
            0usize,
            chip::Ym2612Spec {
                port: 0,
                register: 0x22,
                value: (i & 0xFF) as u8,
            },
        );
        builder.add_vgm_command(WaitSamples(10));
    }

    // Final terminator
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();

    // Create callback stream backed by a byte-mode VgmStream so we can push chunks
    let inner = VgmStream::new();
    let mut cb_stream = VgmCallbackStream::new(inner);

    // Enable state tracking for YM2612 (not strictly necessary for callbacks,
    // but ensures event detection paths are exercised)
    cb_stream.track_state::<crate::chip::state::Ym2612State>(Instance::Primary, 7_987_200.0);

    // Install a callback to count YM2612 write invocations
    let counter = Rc::new(RefCell::new(0usize));
    let counter_clone = counter.clone();
    cb_stream.on_write(move |_inst, _spec: chip::Ym2612Spec, _sample, _event| {
        *counter_clone.borrow_mut() += 1;
    });

    // Feed the serialized bytes in moderate-sized chunks
    let chunk_size = 256usize;
    // Compute initial offset into the serialized bytes from the finalized document header.
    // Use `data_offset` directly: start offset is always `0x34 + data_offset`.
    let header = &doc.header;
    let mut offset = 0x34usize.wrapping_add(header.data_offset as usize);
    while offset < bytes.len() {
        let end = std::cmp::min(offset + chunk_size, bytes.len());
        cb_stream
            .push_chunk(&bytes[offset..end])
            .expect("push_chunk");
        offset = end;

        // Drain available commands after each push to allow callbacks to run.
        // Stop draining when stream indicates it needs more data or ends.
        // Limit inner iterations to avoid pathological infinite loops.
        for _ in 0..1000 {
            match cb_stream.next() {
                Some(Ok(StreamResult::Command(_))) => {
                    // processed a command and callbacks (if any) have been invoked
                    continue;
                }
                Some(Ok(StreamResult::NeedsMoreData)) => break,
                Some(Ok(StreamResult::EndOfStream)) => break,
                Some(Err(e)) => panic!("Stream processing error: {:?}", e),
                None => break,
            }
        }
    }

    // Exhaust any remaining commands after the final chunk
    loop {
        match cb_stream.next() {
            Some(Ok(StreamResult::Command(_))) => {}
            Some(Ok(StreamResult::NeedsMoreData)) => break,
            Some(Ok(StreamResult::EndOfStream)) => break,
            Some(Err(e)) => panic!("Stream error during final drain: {:?}", e),
            None => break,
        }
    }

    // We added 200 YM2612 writes, so expect at least that many callbacks.
    // Some writes may be coalesced/filtered by stream internals; require >= 200 to be safe.
    assert!(
        *counter.borrow() >= 200,
        "Expected at least 200 write callbacks, got {}",
        *counter.borrow()
    );
}

/// Helper: collect the total number of EndOfStream events (i.e. run until end)
/// and return the number of times each sample was emitted so we can assert
/// on the effective loop count.
fn count_effective_loops(mut stream: VgmStream) -> u32 {
    let mut end_count = 0u32;
    loop {
        match stream.next() {
            Some(Ok(StreamResult::EndOfStream)) => {
                end_count += 1;
                break;
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => panic!("Stream error: {:?}", e),
            None => break,
        }
    }
    // current_loop_count() is incremented each time EndOfData is encountered.
    let _ = end_count;
    stream.current_loop_count()
}

/// Build a minimal looping VGM document with a given number of program loops.
fn make_looping_document(program_loops: u32) -> (VgmDocument, u32) {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    // Loop point is at the WaitSamples command (index 0)
    builder.set_loop_index(0);
    builder.add_vgm_command(EndOfData);
    let doc = builder.finalize();
    (doc, program_loops)
}

#[test]
fn test_loop_modifier_default_zero_is_identity() {
    // Both loop_base=0 and loop_modifier=0 (default) → effective loops == program_loops
    let (doc, _) = make_looping_document(3);
    let mut stream = VgmStream::from_document(doc);
    // loop_base and loop_modifier are both 0 from the default header
    stream.set_loop_count(Some(3));

    let loops = count_effective_loops(stream);
    assert_eq!(
        loops, 3,
        "Default (no modifier) should loop exactly program_loops times"
    );
}

#[test]
fn test_loop_modifier_double() {
    // loop_modifier = 0x20 (2× scaling) → effective = 4 * 0x20 / 0x10 = 8 loops
    let (doc, _) = make_looping_document(4);
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(4));
    stream.set_loop_modifier(0x20); // 2× the program loop count

    let loops = count_effective_loops(stream);
    assert_eq!(loops, 8, "loop_modifier=0x20 should double the loop count");
}

#[test]
fn test_loop_modifier_half() {
    // loop_modifier = 0x08 (0.5× scaling) → effective = 4 * 0x08 / 0x10 = 2 loops
    let (doc, _) = make_looping_document(4);
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(4));
    stream.set_loop_modifier(0x08); // 0.5× the program loop count

    let loops = count_effective_loops(stream);
    assert_eq!(loops, 2, "loop_modifier=0x08 should halve the loop count");
}

#[test]
fn test_loop_base_subtract_one() {
    // loop_base = 1 → effective = 4 - 1 = 3 loops
    let (doc, _) = make_looping_document(4);
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(4));
    stream.set_loop_base(1);

    let loops = count_effective_loops(stream);
    assert_eq!(loops, 3, "loop_base=1 should reduce loops by 1");
}

#[test]
fn test_loop_base_negative_adds_loops() {
    // loop_base = -1 (0xFF byte) → effective = 4 - (-1) = 5 loops
    let (doc, _) = make_looping_document(4);
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(4));
    stream.set_loop_base(-1);

    let loops = count_effective_loops(stream);
    assert_eq!(loops, 5, "loop_base=-1 should increase loops by 1");
}

#[test]
fn test_loop_base_and_modifier_combined() {
    // loop_modifier=0x20 doubles → 4×2=8, then loop_base=2 subtracts → 8-2=6
    let (doc, _) = make_looping_document(4);
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(4));
    stream.set_loop_modifier(0x20);
    stream.set_loop_base(2);

    let loops = count_effective_loops(stream);
    assert_eq!(loops, 6, "modifier=0x20, base=2 → 4*2-2=6 loops");
}

#[test]
fn test_loop_base_clamps_to_zero() {
    // loop_base larger than scaled loops → effective must be clamped to 0.
    // effective = 0 means "stop at first EndOfData" (no looping back).
    // current_loop_count() is incremented each time EndOfData is hit, so it
    // will be 1 after the single EndOfData that terminates the stream.
    let (doc, _) = make_looping_document(2);
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(2));
    stream.set_loop_base(10); // would produce 2-10 = -8 → clamped to 0 → stop immediately

    let loops = count_effective_loops(stream);
    assert_eq!(
        loops, 1,
        "Clamped-to-zero effective loops: stream stops at first EndOfData (current_loop_count=1)"
    );
}

#[test]
fn test_loop_modifier_none_loop_count_unchanged() {
    // When loop_count is None the modifier/base must not affect infinite behaviour:
    // set a very short timeout by using loop_count=Some to verify identity still holds.
    let (doc, _) = make_looping_document(2);
    let mut stream = VgmStream::from_document(doc);
    // loop_modifier=0x10 is the same as the effective default (1×)
    stream.set_loop_count(Some(2));
    stream.set_loop_modifier(0x10);
    stream.set_loop_base(0);

    let loops = count_effective_loops(stream);
    assert_eq!(loops, 2, "Explicit 0x10 modifier and 0 base is identity");
}

#[test]
fn test_loop_modifier_getter_setter() {
    let mut stream = VgmStream::new();
    assert_eq!(stream.loop_modifier(), 0);
    stream.set_loop_modifier(0x18);
    assert_eq!(stream.loop_modifier(), 0x18);
}

// ── LengthMode tests ────────────────────────────────────────────────────────

/// Helper: build a VgmBuilder pre-loaded with a DAC stream setup.
///
/// * `data`         – raw sample bytes added to the data block (type 0x00)
/// * `stream_data`  – data block bytes
/// * `freq`         – stream frequency in Hz
/// * `step_size`    – byte stride between consecutive reads
/// * `step_base`    – initial byte offset within each step
///
/// Returns the builder (caller adds StartStream / StartStreamFastCall then waits).
fn build_dac_setup_builder(
    stream_data: Vec<u8>,
    freq: u32,
    step_size: u8,
    step_base: u8,
) -> VgmBuilder {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(soundlog::vgm::command::DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data,
    });
    builder.add_vgm_command(soundlog::vgm::command::SetupStreamControl {
        stream_id: 0,
        chip_type: soundlog::vgm::command::DacStreamChipType {
            chip_id: soundlog::vgm::header::ChipId::Ym2612,
            instance: soundlog::vgm::command::Instance::Primary,
        },
        write_port: 0,
        write_command: 0x2A,
    });
    builder.add_vgm_command(soundlog::vgm::command::SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size,
        step_base,
    });
    builder.add_vgm_command(soundlog::vgm::command::SetStreamFrequency {
        stream_id: 0,
        frequency: freq,
    });
    builder
}

/// Collect all YM2612 DAC writes (register 0x2A) from an assembled VGM document.
fn collect_dac_writes(doc: VgmDocument) -> Vec<u8> {
    let vgm_bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    push_vgm_bytes(&mut parser, &vgm_bytes);
    let mut values = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2612Write(_, spec)))
                if spec.register == 0x2A =>
            {
                values.push(spec.value);
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("parse error: {e:?}"),
            _ => {}
        }
    }
    values
}

/// LengthMode::Milliseconds – stream stops exactly at the specified duration.
///
/// Setup: freq=100 Hz, data_length=30 ms → stream_end_sample = 30*44100/1000 = 1323.
/// sample_interval = 44100/100 = 441.
/// Writes at samples 0, 441, 882; next would be at 1323 which hits the deadline → stop.
/// Expected: exactly 3 writes [0x10, 0x20, 0x30].
#[test]
fn test_length_mode_milliseconds_stops_at_duration() {
    let data = vec![0x10, 0x20, 0x30, 0x40, 0x50];
    let mut builder = build_dac_setup_builder(data, 100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::Milliseconds {
            reverse: false,
            looped: false,
        },
        data_length: 30, // 30 ms
    });
    // Wait long enough to cover the full 30 ms (1323 samples) and a bit beyond.
    builder.add_vgm_command(WaitSamples(2000));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x10, 0x20, 0x30],
        "Milliseconds mode: expected exactly 3 writes"
    );
}

/// LengthMode::Milliseconds with looped=true – stream restarts automatically.
///
/// Same setup (100 Hz, 30 ms, 3 writes per loop).  We wait ≥ 2 full loops + 1 extra write:
/// write 4 fires at the loop-restart sample (1323) → [0x10, 0x20, 0x30, 0x10, 0x20, 0x30, 0x10].
#[test]
fn test_length_mode_milliseconds_looped() {
    let data = vec![0x10, 0x20, 0x30, 0x40, 0x50];
    let mut builder = build_dac_setup_builder(data, 100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::Milliseconds {
            reverse: false,
            looped: true,
        },
        data_length: 30, // 30 ms per loop
    });
    // 2650 samples covers 2 full loops (1323×2 = 2646) and the restart write.
    builder.add_vgm_command(WaitSamples(2650));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    // Pattern: [0x10, 0x20, 0x30] repeating.  After 2 650 samples we expect 7 writes.
    assert_eq!(
        writes,
        vec![0x10, 0x20, 0x30, 0x10, 0x20, 0x30, 0x10],
        "Milliseconds looped: expected two-and-a-half loops of the 3-byte pattern"
    );
}

/// LengthMode::CommandCount with looped=true – stream repeats the sample sequence.
///
/// data=[0x10, 0x20, 0x30, 0x40], count=4, freq=44100 (1 write/sample).
/// After 4 writes remaining hit 0 → loop restart → writes continue from the beginning.
/// WaitSamples(N) fires writes at samples 0..=N (N+1 writes).
/// WaitSamples(8) → 9 writes: [0x10,0x20,0x30,0x40,0x10,0x20,0x30,0x40,0x10].
#[test]
fn test_length_mode_command_count_looped() {
    let data = vec![0x10, 0x20, 0x30, 0x40];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: true,
        },
        data_length: 4,
    });
    // WaitSamples(N) fires writes at samples 0 through N inclusive = N+1 writes.
    builder.add_vgm_command(WaitSamples(8));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x10, 0x20, 0x30, 0x40, 0x10, 0x20, 0x30, 0x40, 0x10],
        "CommandCount looped: data should repeat from the start after 4 writes"
    );
}

/// LengthMode::CommandCount with reverse=true – bytes read backwards.
///
/// data=[0x10, 0x20, 0x30, 0x40], reverse, count=4.
/// Initial position = end of range (index 3).
/// Expected: [0x40, 0x30, 0x20, 0x10].
#[test]
fn test_length_mode_reverse_command_count() {
    let data = vec![0x10, 0x20, 0x30, 0x40];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: true,
            looped: false,
        },
        data_length: 4,
    });
    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x40, 0x30, 0x20, 0x10],
        "CommandCount reverse: expected data read backwards"
    );
}

#[test]
fn test_length_mode_command_count_looped_reverse() {
    // LengthMode::CommandCount reverse + looped
    // data = [0x10,0x20,0x30,0x40], data_length = 4, freq = 44100 (1 write/sample)
    // WaitSamples(8) => 9 writes at samples 0..8 inclusive
    // Reverse pattern [0x40,0x30,0x20,0x10] repeating -> expect:
    // [0x40,0x30,0x20,0x10, 0x40,0x30,0x20,0x10, 0x40]
    let data = vec![0x10, 0x20, 0x30, 0x40];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: true,
            looped: true,
        },
        data_length: 4,
    });
    // WaitSamples(N) fires writes at samples 0..=N (N+1 writes).
    builder.add_vgm_command(WaitSamples(8));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x40, 0x30, 0x20, 0x10, 0x40, 0x30, 0x20, 0x10, 0x40],
        "CommandCount looped reverse: expected reversed pattern repeating"
    );
}

#[test]
fn test_length_mode_milliseconds_looped_reverse() {
    // LengthMode::Milliseconds reverse + looped
    // Setup mirrors test_length_mode_milliseconds_looped but with reverse=true.
    // freq=100 Hz, data_length=30 ms -> 3 writes per loop (values [0x10,0x20,0x30]).
    // Reverse repeating pattern -> expect [0x30,0x20,0x10, 0x30,0x20,0x10, 0x30]
    let data = vec![0x10, 0x20, 0x30, 0x40, 0x50];
    let mut builder = build_dac_setup_builder(data, 100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::Milliseconds {
            reverse: true,
            looped: true,
        },
        data_length: 30, // 30 ms per loop
    });
    // 2650 samples covers 2 full loops (1323×2 = 2646) and the restart write.
    builder.add_vgm_command(WaitSamples(2650));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x30, 0x20, 0x10, 0x30, 0x20, 0x10, 0x30],
        "Milliseconds looped reverse: expected reversed pattern repeating"
    );
}

#[test]
fn test_length_mode_play_until_end_looped_reverse() {
    // StartStream PlayUntilEnd reverse + looped (non-FastCall)
    // Data block [0x01,0x02,0x03,0x04], PlayUntilEnd reverse + looped.
    // WaitSamples(9) -> 10 writes expected; reversed repeating pattern:
    // [0x04,0x03,0x02,0x01, 0x04,0x03,0x02,0x01, 0x04,0x03]
    let data = vec![0x01, 0x02, 0x03, 0x04];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: true,
            looped: true,
        },
        data_length: 0,
    });
    // WaitSamples(9) fires writes at samples 0..=9 = 10 writes.
    builder.add_vgm_command(WaitSamples(9));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x04, 0x03, 0x02, 0x01, 0x04, 0x03, 0x02, 0x01, 0x04, 0x03],
        "PlayUntilEnd looped reverse: expected reversed data repeating (StartStream)"
    );
}

#[test]
fn test_length_mode_play_until_end_looped_reverse_fast_call() {
    // StartStreamFastCall (PlayUntilEnd) reverse + looped
    // Data block [0x01,0x02,0x03,0x04], FastCall block_id=0, looped=true, reverse=true
    // WaitSamples(9) -> 10 writes expected; reversed repeating pattern:
    // [0x04,0x03,0x02,0x01, 0x04,0x03,0x02,0x01, 0x04,0x03]
    let data = vec![0x01, 0x02, 0x03, 0x04];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 0,
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: true,
            looped: true,
        },
    });
    // WaitSamples(9) fires writes at samples 0..=9 = 10 writes.
    builder.add_vgm_command(WaitSamples(9));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x04, 0x03, 0x02, 0x01, 0x04, 0x03, 0x02, 0x01, 0x04, 0x03],
        "PlayUntilEnd (FastCall) looped reverse: expected reversed block repeating"
    );
}

/// StartStreamFastCall with reverse=true (PlayUntilEnd reverse) – plays block backwards.
///
/// Data block [0xA0, 0xB0, 0xC0, 0xD0], FastCall block_id=0, reverse.
/// Initial position is set to block_end - step_size = 3.
/// Expected: [0xD0, 0xC0, 0xB0, 0xA0].
#[test]
fn test_length_mode_reverse_fast_call() {
    let data = vec![0xA0, 0xB0, 0xC0, 0xD0];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 0,
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: true,
            looped: false,
        },
    });
    builder.add_vgm_command(WaitSamples(10));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0xD0, 0xC0, 0xB0, 0xA0],
        "FastCall reverse: expected block played backwards"
    );
}

/// StartStreamFastCall with looped=true (PlayUntilEnd looped) – block repeats.
///
/// Data block [0x01, 0x02, 0x03, 0x04], FastCall block_id=0, looped.
/// After reaching block_end the stream restarts from the beginning of the block.
/// WaitSamples(N) fires writes at samples 0..=N (N+1 writes).
/// WaitSamples(9) → 10 writes: [0x01,0x02,0x03,0x04,0x01,0x02,0x03,0x04,0x01,0x02].
#[test]
fn test_length_mode_play_until_end_looped_fast_call() {
    let data = vec![0x01, 0x02, 0x03, 0x04];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStreamFastCall {
        stream_id: 0,
        block_id: 0,
        flags: soundlog::vgm::command::StartStreamFastCallFlags {
            reverse: false,
            looped: true,
        },
    });
    // WaitSamples(N) fires writes at samples 0 through N inclusive = N+1 writes.
    builder.add_vgm_command(WaitSamples(9));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04, 0x01, 0x02],
        "FastCall PlayUntilEnd looped: block should repeat from the beginning"
    );
}
// ── end of LengthMode tests ─────────────────────────────────────────────────

// Natural-end loop-restart tests:
// These exercise the branch where a read returns `None` because the current
// data position ran past the available data (e.g. when `step_size` > data.len()).
// When `looped = true` the stream should restart correctly (loop-restart branch).
#[test]
fn test_natural_end_loop_restart_forward_step_size_larger_than_bank() {
    // Single-byte data, but step_size = 2. After the first read the position
    // will advance to 2 which is past the end -> read returns None and the
    // loop-restart path should reset current_data_pos to start and continue.
    let data = vec![0x10u8];
    // step_size = 2, so position will jump beyond the bank after one read.
    let mut builder = build_dac_setup_builder(data, 44100, 2, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: false,
            looped: true,
        },
        data_length: 3, // request 3 writes total
    });
    // WaitSamples(2) -> 3 writes at samples 0..=2
    builder.add_vgm_command(WaitSamples(2));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    // Expect the single byte repeated 3 times due to loop-restart when the pos runs past end.
    assert_eq!(
        writes,
        vec![0x10, 0x10, 0x10],
        "Forward natural-end loop-restart: expected repeated single-byte pattern"
    );
}

#[test]
fn test_natural_end_loop_restart_reverse_step_size_larger_than_bank() {
    // Reverse + looped + CommandCount where the initial reverse position lands
    // outside the data bank (step_size=2, data=[0xAA], so initial pos = 4 which
    // is >= data.len()=1).
    //
    // libvgm behaviour: daccontrol_SendCommand guards with
    //   `if (chip->DataStart + chip->RealPos >= chip->DataLen) return;`
    // so every update tick where RealPos is out of range emits nothing.  The
    // stream still decrements RemainCmds and eventually stops – it never
    // restarts into valid data.  No writes are produced.
    //
    // soundlog must match: when read_stream_byte_at returns None the natural-end
    // branch now only resets the position and defers the write to the next tick
    // (no immediate re-read).  Because the initial pos is already out of range
    // and the loop-restart pos for reverse CommandCount is
    //   start_data_pos + (data_length-1)*step_size = 0 + 2*2 = 4  (still out of range)
    // every subsequent read also returns None, so the stream becomes inactive
    // immediately and no writes are produced.
    let data = vec![0xAAu8];
    // step_size = 2 to force positions outside the single-byte bank.
    let mut builder = build_dac_setup_builder(data, 44100, 2, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::CommandCount {
            reverse: true,
            looped: true,
        },
        data_length: 3,
    });
    builder.add_vgm_command(WaitSamples(2));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    // libvgm emits nothing when every RealPos is out of range – match that here.
    assert_eq!(
        writes,
        vec![] as Vec<u8>,
        "Reverse natural-end loop-restart with out-of-range pos: no writes expected (matches libvgm)"
    );
}

#[test]
fn test_loop_base_getter_setter() {
    let mut stream = VgmStream::new();
    assert_eq!(stream.loop_base(), 0);
    stream.set_loop_base(-3);
    assert_eq!(stream.loop_base(), -3);
}

#[test]
fn test_loop_modifier_preserved_across_reset() {
    let mut stream = VgmStream::new();
    stream.set_loop_modifier(0x20);
    stream.set_loop_base(1);
    stream.reset();
    assert_eq!(
        stream.loop_modifier(),
        0x20,
        "loop_modifier preserved after reset"
    );
    assert_eq!(stream.loop_base(), 1, "loop_base preserved after reset");
}

#[test]
fn test_from_document_reads_header_loop_base_modifier() {
    // Build a document whose header has non-default loop_base and loop_modifier
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.set_loop_index(0);
    builder.add_vgm_command(EndOfData);
    let mut doc = builder.finalize();
    // Manually set the header fields (simulate a real VGM file)
    doc.header.loop_base = 1_i8; // +1 reduction
    doc.header.loop_modifier = 0x20; // 2×

    let stream = VgmStream::from_document(doc);
    assert_eq!(
        stream.loop_base(),
        1_i8,
        "from_document should read loop_base from header"
    );
    assert_eq!(
        stream.loop_modifier(),
        0x20_u8,
        "from_document should read loop_modifier from header"
    );
}

// ── from_vgm tests ──────────────────────────────────────────────────────────

#[test]
fn test_from_vgm_basic_commands() {
    // Build a minimal VGM and round-trip it through from_vgm.
    let raw: Vec<u8> = create_vgm_with_various_commands();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm should parse header");
    stream.set_loop_count(Some(1));

    let mut commands: Vec<VgmCommand> = Vec::new();
    for result in &mut stream {
        match result {
            Ok(StreamResult::Command(c)) => commands.push(c),
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            Err(_) => break,
        }
    }

    // Wait735Samples/Wait882Samples are normalised to WaitSamples by the stream processor.
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, VgmCommand::WaitSamples(w) if w.0 == 735))
    );
    assert!(
        commands
            .iter()
            .any(|c| matches!(c, VgmCommand::WaitSamples(w) if w.0 == 882))
    );
    assert!(!commands.is_empty());
}

#[test]
fn test_from_vgm_loops() {
    // Build a document with a loop point, serialise it, then stream via from_vgm.
    let raw: Vec<u8> = create_test_vgm_with_loop();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm should parse header");
    stream.set_loop_count(Some(2));

    let mut end_count = 0u32;
    for result in &mut stream {
        match result {
            Ok(StreamResult::EndOfStream) => {
                end_count += 1;
                break;
            }
            Err(e) => panic!("unexpected error: {e}"),
            _ => {}
        }
    }
    assert_eq!(end_count, 1, "stream should end exactly once");
    // After 2 loops the sample counter should be non-zero.
    assert!(stream.current_sample() > 0);
}

#[test]
fn test_from_vgm_reset() {
    let raw: Vec<u8> = create_vgm_with_various_commands();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));

    // Drain the stream.
    for r in &mut stream {
        if matches!(r, Ok(StreamResult::EndOfStream)) {
            break;
        }
    }

    // Reset and drain again — should yield the same commands.
    stream.reset();
    let mut count = 0usize;
    for result in &mut stream {
        match result.expect("no error after reset") {
            StreamResult::Command(_) => count += 1,
            StreamResult::EndOfStream => break,
            StreamResult::NeedsMoreData => break,
        }
    }
    assert!(count > 0, "should replay commands after reset");
}

#[test]
fn test_from_vgm_reads_loop_base_modifier() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(EndOfData);
    let mut doc = builder.finalize();
    doc.header.loop_base = -2_i8;
    doc.header.loop_modifier = 0x08;
    let raw: Vec<u8> = doc.into();

    let stream = VgmStream::from_vgm(raw).expect("from_vgm");
    assert_eq!(stream.loop_base(), -2_i8);
    assert_eq!(stream.loop_modifier(), 0x08_u8);
}

#[test]
fn test_from_vgm_push_chunk_error() {
    let raw: Vec<u8> = create_vgm_with_various_commands();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    // push_chunk must return an error for Vgm-source streams.
    assert!(stream.push_chunk(&[0x62]).is_err());
}

// ---------------------------------------------------------------------------
// from_vgm + loop + fadeout tests
// ---------------------------------------------------------------------------

/// Build a minimal VGM byte sequence with a loop point using VgmBuilder.
/// The loop is placed at the first WaitSamples command (index 0) so the
/// entire body repeats.
fn create_looped_vgm_bytes() -> Vec<u8> {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitSamples(735));
    b.add_vgm_command(WaitSamples(882));
    b.set_loop_index(0); // loop back to the first WaitSamples
    b.add_vgm_command(EndOfData);
    b.finalize().into()
}

#[test]
fn test_from_vgm_basic() {
    // from_vgm parses successfully and yields WaitSamples commands.
    let raw = create_looped_vgm_bytes();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));

    let mut waits: Vec<u16> = Vec::new();
    for item in &mut stream {
        match item.expect("no error") {
            StreamResult::Command(VgmCommand::WaitSamples(w)) => waits.push(w.0),
            StreamResult::EndOfStream => break,
            StreamResult::NeedsMoreData => break,
            _ => {}
        }
    }
    // With loop_count=1 the body plays once: 735 + 882 samples.
    assert!(waits.contains(&735), "expected WaitSamples(735)");
    assert!(waits.contains(&882), "expected WaitSamples(882)");
}

#[test]
fn test_from_vgm_loop_count() {
    // With loop_count=2 the stream should perform 2 full passes and then stop.
    let raw = create_looped_vgm_bytes();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(2));

    let mut total_samples: u64 = 0;
    for item in &mut stream {
        match item.expect("no error") {
            StreamResult::Command(VgmCommand::WaitSamples(w)) => {
                total_samples += w.0 as u64;
            }
            StreamResult::EndOfStream => break,
            StreamResult::NeedsMoreData => break,
            _ => {}
        }
    }
    // Body = 735 + 882 = 1617 samples; 2 passes = 3234 samples.
    assert_eq!(
        total_samples,
        1617 * 2,
        "expected exactly 2 pass worth of samples"
    );
    assert_eq!(stream.current_loop_count(), 2);
}

#[test]
fn test_from_vgm_fadeout() {
    // With a finite loop count and a fadeout grace period the stream continues
    // emitting commands after the loop ends and then stops at EndOfStream.
    let raw = create_looped_vgm_bytes();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));
    stream.set_fadeout_samples(Some(44100)); // 1 second grace period

    let mut total_samples: u64 = 0;
    let mut reached_end = false;
    for item in &mut stream {
        match item.expect("no error") {
            StreamResult::Command(VgmCommand::WaitSamples(w)) => {
                total_samples += w.0 as u64;
            }
            StreamResult::EndOfStream => {
                reached_end = true;
                break;
            }
            StreamResult::NeedsMoreData => break,
            _ => {}
        }
    }
    // The stream must have advanced beyond a single 1617-sample pass because of
    // the fadeout continuation, and must eventually report EndOfStream.
    assert!(reached_end, "stream should reach EndOfStream after fadeout");
    assert!(
        total_samples >= 1617 + 44100,
        "stream should continue for at least the fadeout period; got {}",
        total_samples
    );
}

#[test]
fn test_from_vgm_callback_stream_loop_and_fadeout() {
    // End-to-end test: VgmCallbackStream::from_vgm with loop + fadeout.
    // Confirm that the iterator terminates (returns None) after the fadeout
    // and that the on_write callback is invoked for chip writes.
    use soundlog::VgmCallbackStream;
    use soundlog::chip::Ym2612Spec;
    use soundlog::vgm::command::Instance;

    let mut b = VgmBuilder::new();
    b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
    b.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0,
        },
    );
    b.add_vgm_command(WaitSamples(735));
    b.set_loop_index(0);
    b.add_vgm_command(EndOfData);
    let raw: Vec<u8> = b.finalize().into();

    let mut cs = VgmCallbackStream::from_vgm(raw).expect("from_vgm");
    cs.set_loop_count(Some(1));
    cs.set_fadeout_samples(Some(735)); // short fadeout

    let write_count = Rc::new(RefCell::new(0u32));
    let wc = write_count.clone();
    cs.on_write(move |_inst, _spec: Ym2612Spec, _sample, _events| {
        *wc.borrow_mut() += 1;
    });

    // Drain the iterator; it must terminate naturally.
    let mut iter_count = 0u32;
    for _result in &mut cs {
        iter_count += 1;
        assert!(iter_count < 100_000, "iterator did not terminate");
    }

    assert!(
        *write_count.borrow() >= 1,
        "on_write callback should have been invoked"
    );
}

// ---------------------------------------------------------------------------
// seek_to_sample / reset_to_loop_point tests
// ---------------------------------------------------------------------------

/// Build a raw VGM bytes that have an *intro* section before the loop point.
///
/// Layout (measured from the loop-point sample-counter, which resets to 0):
///   intro : WaitSamples(500)                ← before loop point
///   ── loop point ──
///   body  : WaitSamples(300) + WaitSamples(400) + EndOfData  (700 samples/loop)
///
/// This is the canonical test fixture for "loop_offset != start of command stream".
fn create_intro_plus_loop_vgm() -> Vec<u8> {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitSamples(500)); // intro (command index 0)
    b.set_loop_index(1); // loop point → command index 1
    b.add_vgm_command(WaitSamples(300)); // loop body: 300 samples
    b.add_vgm_command(WaitSamples(400)); // loop body: 400 samples
    b.add_vgm_command(EndOfData);
    b.finalize().into()
}

/// Collect total wait-sample count from a `VgmStream` until EndOfStream or NeedsMoreData.
fn collect_total_wait_samples(stream: &mut VgmStream) -> u64 {
    let mut total: u64 = 0;
    loop {
        match stream.next().unwrap().unwrap() {
            StreamResult::Command(VgmCommand::WaitSamples(w)) => total += w.0 as u64,
            StreamResult::EndOfStream | StreamResult::NeedsMoreData => break,
            _ => {}
        }
    }
    total
}

// --- VgmStream::reset_to_loop_point ---

#[test]
fn test_reset_to_loop_point_from_document() {
    // seek_to_sample(0) on a from_document stream should succeed and position
    // the cursor at the loop point with current_sample reset to 0.
    let raw = create_intro_plus_loop_vgm();
    let doc = soundlog::VgmDocument::try_from(raw.as_slice()).expect("parse doc");
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(1));

    // Partially advance into the intro.
    match stream.next().unwrap().unwrap() {
        StreamResult::Command(VgmCommand::WaitSamples(w)) => {
            assert_eq!(w.0, 500, "first command should be the intro wait");
        }
        other => panic!("unexpected: {:?}", other),
    }
    assert_eq!(stream.current_sample(), 500);

    // seek_to_sample(0) must bring us back to the loop point (not the start of the file).
    stream.seek_to_sample(0).expect("seek_to_sample(0) failed");
    assert_eq!(
        stream.current_sample(),
        0,
        "sample counter must be 0 after seek_to_sample(0)"
    );

    // The first command after seek must be the first loop-body command.
    match stream.next().unwrap().unwrap() {
        StreamResult::Command(VgmCommand::WaitSamples(w)) => {
            assert_eq!(
                w.0, 300,
                "first command after seek_to_sample(0) must be the first loop-body command"
            );
        }
        other => panic!("unexpected after seek: {:?}", other),
    }
}

#[test]
fn test_reset_to_loop_point_from_vgm() {
    // Same as above but using VgmStream::from_vgm.
    let raw = create_intro_plus_loop_vgm();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));

    // Consume the intro wait.
    let _ = stream.next();
    assert_eq!(stream.current_sample(), 500);

    stream.seek_to_sample(0).expect("seek_to_sample(0) failed");
    assert_eq!(stream.current_sample(), 0);

    match stream.next().unwrap().unwrap() {
        StreamResult::Command(VgmCommand::WaitSamples(w)) => {
            assert_eq!(w.0, 300, "must start at loop-body after seek_to_sample(0)");
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn test_reset_to_loop_point_push_chunk_is_unsupported() {
    // Buffer-backed (push_chunk) streams cannot seek; seek_to_sample(0) must return Err.
    let mut stream = VgmStream::new();
    stream.set_loop_count(Some(1));
    assert!(
        stream.seek_to_sample(0).is_err(),
        "seek_to_sample(0) on a push_chunk stream must return Err"
    );
}

// --- VgmStream::seek_to_sample ---

#[test]
fn test_seek_to_sample_from_document_basic() {
    // Seek to a target that falls midway through the first loop-body wait.
    let raw = create_intro_plus_loop_vgm();
    let doc = soundlog::VgmDocument::try_from(raw.as_slice()).expect("parse doc");
    let mut stream = VgmStream::from_document(doc);
    stream.set_loop_count(Some(1));

    // Target 150 is within the first loop-body wait (0..300).  The seek consumes
    // that wait in one step, so current_sample ends at 300.
    stream.seek_to_sample(150).expect("seek failed");
    assert!(
        stream.current_sample() >= 150,
        "current_sample ({}) must be >= target 150",
        stream.current_sample()
    );
}

#[test]
fn test_seek_to_sample_from_vgm_basic() {
    let raw = create_intro_plus_loop_vgm();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));

    stream.seek_to_sample(150).expect("seek failed");
    assert!(stream.current_sample() >= 150);
}

#[test]
fn test_seek_to_sample_push_chunk_is_unsupported() {
    let mut stream = VgmStream::new();
    assert!(
        stream.seek_to_sample(0).is_err(),
        "seek_to_sample on a push_chunk stream must return Err"
    );
}

#[test]
fn test_seek_to_sample_zero_is_at_loop_point() {
    // seek_to_sample(0) is equivalent to reset_to_loop_point: the cursor moves to
    // the loop point (no fast-forward) and current_sample stays at 0.
    let raw = create_intro_plus_loop_vgm();
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));

    // Advance past the intro.
    let _ = stream.next();
    assert_eq!(stream.current_sample(), 500);

    stream.seek_to_sample(0).expect("seek failed");
    assert_eq!(
        stream.current_sample(),
        0,
        "seek_to_sample(0) must yield current_sample 0"
    );

    // Next command must be the loop-body start, not the intro.
    match stream.next().unwrap().unwrap() {
        StreamResult::Command(VgmCommand::WaitSamples(w)) => {
            assert_eq!(
                w.0, 300,
                "after seek_to_sample(0) the first command must be the first loop-body command"
            );
        }
        other => panic!("unexpected: {:?}", other),
    }
}

/// KEY TEST — loop_offset is NOT at the start of the command stream.
///
/// Verifies that after `seek_to_sample`:
///   1. The intro commands are skipped (not replayed).
///   2. The sample counter is measured from 0 at the loop point.
///   3. Playback continues correctly from the seeked position.
#[test]
fn test_seek_to_sample_loop_offset_not_at_data_start() {
    // Build a VGM where the loop point is clearly NOT at the beginning.
    // Intro: WaitSamples(1000) — one full command before the loop point.
    // Loop body: WaitSamples(400) + WaitSamples(600) + EndOfData (1000 samples/loop).
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitSamples(1000)); // intro (will produce a non-zero loop_offset)
    b.set_loop_index(1); // loop point → command index 1
    b.add_vgm_command(WaitSamples(400)); // loop body part A
    b.add_vgm_command(WaitSamples(600)); // loop body part B
    b.add_vgm_command(EndOfData);
    let raw: Vec<u8> = b.finalize().into();

    // Sanity: when played fully from the beginning with 1 loop, total wait =
    // intro 1000 + body 400 + 600 = 2000.
    {
        let mut s = VgmStream::from_vgm(raw.clone()).expect("from_vgm");
        s.set_loop_count(Some(1));
        let total = collect_total_wait_samples(&mut s);
        assert_eq!(
            total, 2000,
            "full drain without seek must sum to intro+body = 2000"
        );
    }

    // After seek_to_sample(0), the intro is skipped and we replay only the loop body.
    {
        let mut s = VgmStream::from_vgm(raw.clone()).expect("from_vgm");
        s.set_loop_count(Some(1));
        s.seek_to_sample(0).expect("seek failed");
        assert_eq!(
            s.current_sample(),
            0,
            "current_sample must be 0 at loop point"
        );

        // Next command is the first loop-body command (400 samples), NOT the intro (1000).
        match s.next().unwrap().unwrap() {
            StreamResult::Command(VgmCommand::WaitSamples(w)) => {
                assert_eq!(
                    w.0, 400,
                    "first command after seek(0) must be loop-body WaitSamples(400), not intro"
                );
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    // After seek_to_sample(500), the first loop-body wait (400) is consumed, leaving
    // us positioned before the second wait (600).  current_sample ends exactly at 400
    // (since the 400-sample wait overshoots the target of 500 ... wait, 400 < 500).
    // Actually: 400 < 500 so seek consumes 400 and then 600 → lands at 1000 >= 500.
    // Verify: current_sample >= 500 and the entire body has been consumed (EndOfData next).
    {
        let mut s = VgmStream::from_vgm(raw.clone()).expect("from_vgm");
        s.set_loop_count(Some(1));
        s.seek_to_sample(500).expect("seek failed");
        assert!(
            s.current_sample() >= 500,
            "current_sample ({}) must be >= 500",
            s.current_sample()
        );
        // The intro (1000 samples) must NOT have been included in current_sample:
        // the loop body is only 1000 samples total, so current_sample must be < 1000 + 1000.
        // More precisely, since the loop body is 400+600=1000 samples and no intro was replayed,
        // current_sample must be <= 1000.
        assert!(
            s.current_sample() <= 1000,
            "current_sample ({}) must not include the intro 1000 samples",
            s.current_sample()
        );
    }

    // After seek_to_sample(200), only the partial first wait is consumed.
    // WaitSamples(400) is the first command; since 400 >= 200 the seek stops there.
    {
        let mut s = VgmStream::from_vgm(raw.clone()).expect("from_vgm");
        s.set_loop_count(Some(1));
        s.seek_to_sample(200).expect("seek failed");
        // current_sample is 400 (the first loop-body wait consumed atomically)
        assert_eq!(
            s.current_sample(),
            400,
            "after seek(200): WaitSamples(400) is consumed atomically → current_sample=400"
        );
    }
}

// --- VgmCallbackStream::seek_to_sample ---

#[test]
fn test_callback_stream_seek_suppresses_user_callbacks() {
    use soundlog::chip::Ym2612Spec;

    // Build VGM: loop body contains a YM2612 write followed by a long wait.
    // The seek target is AFTER the chip write, so the write is consumed during
    // fast-forward — but the user's on_write must NOT be invoked.
    let mut b = VgmBuilder::new();
    b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
    b.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0,
        },
    );
    b.add_vgm_command(WaitSamples(735));
    b.set_loop_index(0); // loop starts at the very first command (no intro)
    b.add_vgm_command(EndOfData);
    let raw: Vec<u8> = b.finalize().into();

    let mut cs = VgmCallbackStream::from_vgm(raw).expect("from_vgm");
    cs.set_loop_count(Some(1));

    let callback_count = Rc::new(RefCell::new(0u32));
    let cc = callback_count.clone();
    cs.on_write(move |_inst, _spec: Ym2612Spec, _sample, _events| {
        *cc.borrow_mut() += 1;
    });

    // Seek past the only chip write.  Callback must NOT fire during seek.
    cs.seek_to_sample(1).expect("seek failed");
    assert_eq!(
        *callback_count.borrow(),
        0,
        "on_write must NOT fire during seek_to_sample"
    );

    // After seek, the normal iterator must still work and eventually terminate.
    let mut iterated = 0u32;
    for _result in &mut cs {
        iterated += 1;
        assert!(iterated < 10_000, "iterator did not terminate");
    }
}

#[test]
fn test_callback_stream_seek_maintains_chip_state() {
    use soundlog::chip::Ym2612Spec;
    use soundlog::chip::event::StateEvent;
    use soundlog::chip::state::Ym2612State;

    // Build VGM whose loop body contains a key-on write that produces a
    // StateEvent::KeyOn.  After seek_to_sample(1) the state tracker must have
    // processed the write (so it fires KeyOn on the *next* matching key-on write
    // after the seek), and the user callback must not have fired during seek.
    //
    // Layout:
    //   loop body:
    //     YM2612 reg 0x28 = 0xF6  (key-on ch2-slot4, produces KeyOn event)
    //     WaitSamples(735)
    //     EndOfData
    let mut b = VgmBuilder::new();
    b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
    b.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF6, // key-on
        },
    );
    b.add_vgm_command(WaitSamples(735));
    b.set_loop_index(0);
    b.add_vgm_command(EndOfData);
    let raw: Vec<u8> = b.finalize().into();

    let mut cs = VgmCallbackStream::from_vgm(raw).expect("from_vgm");
    cs.set_loop_count(Some(1));
    cs.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);

    // Count how many times the on_write fires during seek (must be 0).
    let seek_fires = Rc::new(RefCell::new(0u32));
    let sf = seek_fires.clone();
    cs.on_write(move |_inst, _spec: Ym2612Spec, _sample, _events| {
        *sf.borrow_mut() += 1;
    });

    cs.seek_to_sample(1).expect("seek_to_sample failed");

    assert_eq!(
        *seek_fires.borrow(),
        0,
        "on_write must not fire during seek_to_sample"
    );

    // After seek, replacing the callback collects events from subsequent iteration.
    // The chip-write (with sample still < 735) was consumed by the seek fast-forward,
    // so no more chip writes happen in this pass → on_write fires 0 times after seek.
    let post_fires = Rc::new(RefCell::new(0u32));
    let pf = post_fires.clone();
    let key_on_after = Rc::new(RefCell::new(false));
    let ka = key_on_after.clone();
    cs.on_write(move |_inst, _spec: Ym2612Spec, _sample, events| {
        *pf.borrow_mut() += 1;
        if events.is_some_and(|evs| evs.iter().any(|e| matches!(e, StateEvent::KeyOn { .. }))) {
            *ka.borrow_mut() = true;
        }
    });

    let mut iterated = 0u32;
    for _result in &mut cs {
        iterated += 1;
        assert!(iterated < 10_000, "iterator did not terminate");
    }
    // chip write was consumed during seek; the remainder of the single loop pass
    // contains only the wait and end-of-data, so post-seek on_write count is 0.
    assert_eq!(
        *post_fires.borrow(),
        0,
        "no chip writes remain after seek in this single-pass test"
    );
}

#[test]
fn test_callback_stream_seek_replays_correctly_from_loop_point_with_intro() {
    // Verify that seek_to_sample on a VGM with an intro section correctly starts
    // from the loop point, not from the beginning.
    //
    // Layout:
    //   intro : WaitSamples(1000)           ← before loop point, sample NOT in loop counter
    //   ── loop point ──
    //   body  : WaitSamples(300) + WaitSamples(400) + EndOfData  (700 samples/loop)

    let raw = create_intro_plus_loop_vgm();

    // Count samples gathered AFTER seek_to_sample(0).  Must equal 700 (loop body only).
    let mut cs = VgmCallbackStream::from_vgm(raw).expect("from_vgm");
    cs.set_loop_count(Some(1));

    cs.seek_to_sample(0).expect("seek failed");
    assert_eq!(
        cs.stream().current_sample(),
        0,
        "after seek(0) the sample counter must be 0 (loop-point relative)"
    );

    let mut samples_after_seek: u64 = 0;
    for result in &mut cs {
        if let Ok(StreamResult::Command(VgmCommand::WaitSamples(w))) = result {
            samples_after_seek += w.0 as u64;
        }
    }
    assert_eq!(
        samples_after_seek, 700,
        "after seek(0) only the 700-sample loop body should be played, not the 500-sample intro"
    );
}

#[test]
fn test_seek_to_sample_beyond_stream_end_reaches_eos() {
    // Seeking past the total loop-body length should consume all commands and reach
    // EndOfStream without panicking.
    let raw = create_intro_plus_loop_vgm(); // body = 700 samples
    let mut stream = VgmStream::from_vgm(raw).expect("from_vgm");
    stream.set_loop_count(Some(1));

    // A target larger than the loop body (700 samples).
    stream.seek_to_sample(99_999).expect("seek failed");
    // Stream must be at end (next call should be EndOfStream or None).
    let result = stream.next();
    assert!(
        matches!(result, Some(Ok(StreamResult::EndOfStream)) | None),
        "after seeking past end, stream must report EndOfStream; got {:?}",
        result
    );
}

#[test]
fn test_callback_stream_stop_at_sample_via_on_write_then_seek_to_resume() {
    use soundlog::chip::Ym2612Spec;

    // VGM layout (loop body = entire body, loop_index = 0):
    //   Write A: reg=0x28 val=0xF0  ← at sample 0
    //   WaitSamples(300)             → sample advances to 300
    //   Write B: reg=0x28 val=0xF6  ← at sample 300
    //   WaitSamples(400)             → sample advances to 700
    //   EndOfData
    let mut b = VgmBuilder::new();
    b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
    b.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0,
        },
    );
    b.add_vgm_command(WaitSamples(300));
    b.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF6,
        },
    );
    b.add_vgm_command(WaitSamples(400));
    b.set_loop_index(0); // entire body is the loop (no intro)
    b.add_vgm_command(EndOfData);
    let raw: Vec<u8> = b.finalize().into();

    let mut cs = VgmCallbackStream::from_vgm(raw).expect("from_vgm");
    cs.set_loop_count(Some(1));

    // Phase 1: iterate; stop when on_write fires at sample > 100
    // Write A fires at sample 0  (< 100 → continue)
    // Write B fires at sample 300 (> 100 → stop and capture)
    const STOP_THRESHOLD: usize = 100;
    let stop_flag = Rc::new(RefCell::new(false));
    let captured_sample: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
    {
        let sf = stop_flag.clone();
        let cap = captured_sample.clone();
        cs.on_write(move |_inst, _spec: Ym2612Spec, sample, _events| {
            if sample > STOP_THRESHOLD && cap.borrow().is_none() {
                *cap.borrow_mut() = Some(sample);
                *sf.borrow_mut() = true;
            }
        });
    }

    for result in &mut cs {
        if let Err(e) = result {
            panic!("stream error in phase 1: {:?}", e);
        }
        if *stop_flag.borrow() {
            break;
        }
    }

    // Write B (sample 300) must have triggered the stop — 300 > 100.
    let captured = captured_sample
        .borrow()
        .expect("on_write must have fired with sample > 100");
    assert_eq!(
        captured, 300,
        "Write B fires at sample 300 (> threshold 100)"
    );

    // Phase 2: seek to the captured sample position
    // seek_to_sample rewinds to the loop point then fast-forwards to `captured`,
    // consuming any prior commands silently (user callbacks suppressed).
    cs.seek_to_sample(captured)
        .expect("seek_to_sample(captured) failed");
    assert!(
        cs.stream().current_sample() >= captured,
        "current_sample ({}) must be >= captured sample {} after seek",
        cs.stream().current_sample(),
        captured
    );

    // Phase 3: resume from the captured sample; install a new callback
    let phase3_writes: Rc<RefCell<Vec<(u8, u8)>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let pw = phase3_writes.clone();
        cs.on_write(move |_inst, spec: Ym2612Spec, _sample, _events| {
            pw.borrow_mut().push((spec.register, spec.value));
        });
    }

    let mut iter_count = 0u32;
    for _result in &mut cs {
        iter_count += 1;
        assert!(iter_count < 10_000, "iterator did not terminate");
    }

    let writes = phase3_writes.borrow();
    // Write A (val=0xF0, sample 0) must NOT appear: seek fast-forwarded past it.
    assert!(
        !writes.iter().any(|&(reg, val)| reg == 0x28 && val == 0xF0),
        "Write A (val=0xF0) must not fire after seeking past sample {}; got {:?}",
        captured,
        *writes
    );
    // Write B (val=0xF6) MUST appear: seek landed at sample 300 and Write B is next.
    assert!(
        writes.iter().any(|&(reg, val)| reg == 0x28 && val == 0xF6),
        "Write B (val=0xF6) must fire after resuming from seek at sample {}; got {:?}",
        captured,
        *writes
    );
}

// Regression tests: Buffer-source multi-loop via chunked push_chunk
//
// These tests cover the bug where `jump_to_loop_point()` on a Buffer source
// did not call `buffer.clear()`, causing stale tail bytes to be re-parsed as
// commands on the second (and later) loop iterations, which produced a
// `ParseError` and surfaced as `PushError::IterParse` in the profiling binary.

/// Build a minimal looping VGM as raw bytes.
/// Layout:
///   intro:  WaitSamples(100), WaitSamples(200)
///   <loop point set here>
///   loop:   WaitSamples(300), WaitSamples(400)
///   EndOfData
fn create_chunked_loop_vgm() -> Vec<u8> {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.add_vgm_command(WaitSamples(200));
    // loop point is placed at index 2, i.e. the WaitSamples(300) command
    builder.set_loop_index(2);
    builder.add_vgm_command(WaitSamples(300));
    builder.add_vgm_command(WaitSamples(400));
    builder.add_vgm_command(EndOfData);
    builder.finalize().into()
}

/// Parse the command region and loop restart position from raw VGM bytes.
/// Returns `(command_region, restart_pos)` where `restart_pos` is a byte
/// offset within `command_region`.
fn parse_command_region(vgm_bytes: &[u8]) -> (&[u8], usize) {
    let header = soundlog::VgmHeader::from_bytes(vgm_bytes).expect("parse header");
    // eof_offset stores (file_size - 4), so absolute EOF = eof_offset + 4.
    let eof = header.eof_offset as usize + 4;
    assert!(eof > 0 && eof <= vgm_bytes.len(), "invalid eof_offset");
    let command_start = soundlog::VgmHeader::command_start(header.version, header.data_offset);
    let region = &vgm_bytes[command_start..eof];
    let restart = soundlog::VgmHeader::loop_pos_in_commands(
        header.version,
        header.loop_offset,
        header.data_offset,
        eof,
    )
    .unwrap_or(0);
    (region, restart)
}

/// Collect all `WaitSamples` values from a `Buffer`-backed `VgmStream` that is
/// fed `command_region` in fixed-size chunks.
///
/// `NeedsMoreData` has two distinct meanings in the Buffer source:
///
/// 1. **Mid-command split** – a chunk boundary landed inside a multi-byte
///    command; the stream just needs the next consecutive bytes.
///    → advance `offset` by `chunk_size` as normal.
///
/// 2. **Post-loop buffer clear** – `jump_to_loop_point()` cleared the buffer
///    after an internal `EndOfData`; the stream needs data re-fed from the
///    loop point.
///    → rewind `offset` to `restart_pos`.
///
/// We distinguish the two cases by watching `current_loop_count()`: if it
/// increased since the last push the stream completed a loop iteration and
/// cleared its buffer, so we rewind; otherwise we just advance.
fn collect_waits_chunked(
    command_region: &[u8],
    restart_pos: usize,
    loop_count: Option<u32>,
    chunk_size: usize,
) -> Result<Vec<u16>, String> {
    let mut stream = VgmStream::new();
    stream.set_loop_count(loop_count);

    let data_len = command_region.len();
    let mut offset = 0usize;
    let mut waits = Vec::new();
    let mut finished = false;
    let mut iteration_guard = 0u32;

    while offset < data_len {
        iteration_guard += 1;
        assert!(
            iteration_guard < 100_000,
            "collect_waits_chunked: iteration limit exceeded (infinite loop?)"
        );

        let end = (offset + chunk_size).min(data_len);
        // Snapshot loop count before push to detect post-loop buffer clears.
        let loops_before_push = stream.current_loop_count();
        stream
            .push_chunk(&command_region[offset..end])
            .map_err(|e| format!("push_chunk error: {e:?}"))?;

        let mut needs_more = false;
        loop {
            match stream.next() {
                Some(Ok(StreamResult::Command(VgmCommand::WaitSamples(w)))) => {
                    waits.push(w.0);
                }
                Some(Ok(StreamResult::Command(_))) => {}
                Some(Ok(StreamResult::NeedsMoreData)) => {
                    needs_more = true;
                    break;
                }
                Some(Ok(StreamResult::EndOfStream)) => {
                    finished = true;
                    break;
                }
                Some(Err(e)) => return Err(format!("stream error: {e:?}")),
                None => {
                    finished = true;
                    break;
                }
            }
        }

        if finished {
            break;
        }

        if needs_more {
            // Distinguish the two NeedsMoreData causes:
            // - loop count increased → jump_to_loop_point() cleared the buffer;
            //   rewind to restart_pos so next push re-feeds from the loop point.
            // - loop count unchanged → mid-command split; advance normally.
            if stream.current_loop_count() > loops_before_push {
                offset = restart_pos;
            } else {
                offset = end;
            }
        } else {
            offset = end;
        }
    }

    Ok(waits)
}

/// Regression test: `Some(1)` (single playthrough, no actual looping) must
/// succeed without any parse error.
#[test]
fn test_push_chunk_loop1_no_error() {
    let vgm = create_chunked_loop_vgm();
    let (region, restart) = parse_command_region(&vgm);

    let waits =
        collect_waits_chunked(region, restart, Some(1), 16).expect("Some(1) must not error");

    // intro (100, 200) + loop section (300, 400) played once → no second loop
    assert_eq!(
        waits,
        vec![100, 200, 300, 400],
        "Some(1): expected intro + one loop section"
    );
}

/// Regression test (primary): `Some(2)` previously triggered `PushError::IterParse`
/// because stale tail bytes remained in the buffer after the first `EndOfData`.
/// After the fix (`buffer.clear()` in `jump_to_loop_point`), this must succeed.
#[test]
fn test_push_chunk_loop2_regression_no_parse_error() {
    let vgm = create_chunked_loop_vgm();
    let (region, restart) = parse_command_region(&vgm);

    let waits =
        collect_waits_chunked(region, restart, Some(2), 16).expect("Some(2) must not error");

    // intro once (100, 200) + loop section twice (300, 400, 300, 400)
    assert_eq!(
        waits,
        vec![100, 200, 300, 400, 300, 400],
        "Some(2): expected intro + loop section played twice"
    );
}

/// Same regression with `Some(3)`: three total playthroughs.
#[test]
fn test_push_chunk_loop3_regression_no_parse_error() {
    let vgm = create_chunked_loop_vgm();
    let (region, restart) = parse_command_region(&vgm);

    let waits =
        collect_waits_chunked(region, restart, Some(3), 16).expect("Some(3) must not error");

    // intro once + loop section three times
    assert_eq!(
        waits,
        vec![100, 200, 300, 400, 300, 400, 300, 400],
        "Some(3): expected intro + loop section played three times"
    );
}

/// Vary the chunk size to 1 byte: ensures the fix holds even when `EndOfData`
/// straddles chunk boundaries in every possible alignment.
#[test]
fn test_push_chunk_loop2_tiny_chunks_regression() {
    let vgm = create_chunked_loop_vgm();
    let (region, restart) = parse_command_region(&vgm);

    let waits = collect_waits_chunked(region, restart, Some(2), 1)
        .expect("Some(2) with 1-byte chunks must not error");

    assert_eq!(
        waits,
        vec![100, 200, 300, 400, 300, 400],
        "1-byte chunks / Some(2): expected intro + loop section played twice"
    );
}

/// Vary the chunk size to 4096 bytes (same as `soundlog_prof.rs`): ensures the
/// fix matches exactly what the profiling binary does.
#[test]
fn test_push_chunk_loop2_prof_chunk_size_regression() {
    let vgm = create_chunked_loop_vgm();
    let (region, restart) = parse_command_region(&vgm);

    let waits = collect_waits_chunked(region, restart, Some(2), 4096)
        .expect("Some(2) with 4096-byte chunks must not error");

    assert_eq!(
        waits,
        vec![100, 200, 300, 400, 300, 400],
        "4096-byte chunks / Some(2): expected intro + loop section played twice"
    );
}

/// Verify that after `jump_to_loop_point` clears the buffer, the stream
/// accepts fresh data and produces exactly the loop-section commands again —
/// i.e. no duplicate or garbled commands from residual bytes.
/// Sweeps multiple chunk sizes so that `EndOfData` lands at different alignments
/// within a chunk, maximising the chance of catching any residual-byte issue.
#[test]
fn test_push_chunk_loop2_command_sequence_is_clean() {
    let vgm = create_chunked_loop_vgm();
    let (region, restart) = parse_command_region(&vgm);

    for chunk_size in [1usize, 3, 7, 13, 32, 128, 4096] {
        let waits = collect_waits_chunked(region, restart, Some(2), chunk_size)
            .unwrap_or_else(|e| panic!("chunk_size={chunk_size}: {e}"));
        assert_eq!(
            waits,
            vec![100, 200, 300, 400, 300, 400],
            "chunk_size={chunk_size}: command sequence must be clean across loop boundary"
        );
    }
}

// ── PlayUntilEnd + looped (StartStream) regression tests ────────────────────
//
// Issue #3: StartStream sets block_end_pos = None, so at_end_before_read always
// returns false for PlayUntilEnd forward mode.  The stream falls through to the
// natural-end path instead of using the pre-read end check.
//
// Issue #4: In the natural-end loop path an extra write was immediately emitted
// at the same sample slot as the wrap.  libvgm defers the first post-restart
// write until the next update tick, so the instant write is wrong.

/// StartStream with PlayUntilEnd + looped=true (forward, non-FastCall).
///
/// Unlike FastCall, StartStream leaves `block_end_pos = None`.  The stream must
/// still detect end-of-bank and loop back to `start_data_pos`, matching the
/// libvgm `DCTRL_LMODE_TOEND` + loop-bit behaviour.
///
/// data=[0x01,0x02,0x03,0x04], freq=44100 (1 write/sample), looped.
/// WaitSamples(9) => 10 writes: [0x01,0x02,0x03,0x04,0x01,0x02,0x03,0x04,0x01,0x02].
#[test]
fn test_length_mode_play_until_end_looped_start_stream() {
    let data = vec![0x01u8, 0x02, 0x03, 0x04];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: false,
            looped: true,
        },
        data_length: 0,
    });
    builder.add_vgm_command(WaitSamples(9));
    builder.add_vgm_command(EndOfData);

    let writes = collect_dac_writes(builder.finalize());
    assert_eq!(
        writes,
        vec![0x01, 0x02, 0x03, 0x04, 0x01, 0x02, 0x03, 0x04, 0x01, 0x02],
        "PlayUntilEnd looped (StartStream): block should repeat from start_data_pos"
    );
}

/// StartStream with PlayUntilEnd + looped=true (forward) must not emit two writes
/// in the same sample slot when wrapping at the natural end of the data bank.
///
/// At 44100 Hz with a 4-byte block, write #5 (sample index 4) is the first write
/// of the second loop.  Each sample slot must contain exactly one write.
#[test]
fn test_length_mode_play_until_end_looped_start_stream_no_double_write_on_wrap() {
    use soundlog::vgm::command::VgmCommand;
    use soundlog::vgm::stream::StreamResult;

    let data = vec![0xA0u8, 0xB0, 0xC0, 0xD0];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: false,
            looped: true,
        },
        data_length: 0,
    });
    // Cover one full loop plus a few extra writes.
    builder.add_vgm_command(WaitSamples(6));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    push_vgm_bytes(&mut parser, &vgm_bytes);

    // Collect (sample_slot, value) by counting Wait splits before each write.
    let mut sample: usize = 0;
    let mut writes_with_sample: Vec<(usize, u8)> = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(VgmCommand::WaitSamples(w))) => {
                sample += w.0 as usize;
            }
            Ok(StreamResult::Command(VgmCommand::Ym2612Write(_, spec)))
                if spec.register == 0x2A =>
            {
                writes_with_sample.push((sample, spec.value));
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("parse error: {e:?}"),
            _ => {}
        }
    }

    // No two writes should share the same sample slot.
    let mut seen_samples = std::collections::HashSet::new();
    for (s, v) in &writes_with_sample {
        assert!(
            seen_samples.insert(*s),
            "double write at sample {s} (value 0x{v:02X}): {writes_with_sample:?}"
        );
    }

    // Values must follow the repeating forward pattern.
    let values: Vec<u8> = writes_with_sample.iter().map(|(_, v)| *v).collect();
    assert_eq!(
        values,
        vec![0xA0, 0xB0, 0xC0, 0xD0, 0xA0, 0xB0, 0xC0],
        "PlayUntilEnd looped (StartStream): wrong value sequence"
    );
}

/// PlayUntilEnd + looped (StartStream, forward): verify per-sample timing across
/// the natural-end wrap boundary.
///
/// At 44100 Hz with a 4-byte block each byte is written on consecutive samples
/// 0, 1, 2, 3.  After the natural end the stream wraps and the first byte of the
/// second loop must appear at sample 4 — not sample 5 (which would happen if
/// `advance_sample_clock` were called twice inside the natural-end branch).
///
/// Expected sample → value mapping:
///   0→0x01, 1→0x02, 2→0x03, 3→0x04, 4→0x01, 5→0x02, 6→0x03
#[test]
fn test_length_mode_play_until_end_looped_start_stream_timing() {
    use soundlog::vgm::command::VgmCommand;
    use soundlog::vgm::stream::StreamResult;

    let data = vec![0x01u8, 0x02, 0x03, 0x04];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: false,
            looped: true,
        },
        data_length: 0,
    });
    // WaitSamples(6) covers samples 0..=6 (7 writes).
    builder.add_vgm_command(WaitSamples(6));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    push_vgm_bytes(&mut parser, &vgm_bytes);

    // Reconstruct the sample slot for every write by accumulating Wait splits.
    let mut sample: usize = 0;
    let mut writes_with_sample: Vec<(usize, u8)> = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(VgmCommand::WaitSamples(w))) => {
                sample += w.0 as usize;
            }
            Ok(StreamResult::Command(VgmCommand::Ym2612Write(_, spec)))
                if spec.register == 0x2A =>
            {
                writes_with_sample.push((sample, spec.value));
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("parse error: {e:?}"),
            _ => {}
        }
    }

    // Each write must land on the expected sample slot with no gaps or skips.
    // If advance_sample_clock() fires twice on the wrap tick, the post-wrap
    // writes would be shifted by one sample.
    let expected: Vec<(usize, u8)> = vec![
        (0, 0x01),
        (1, 0x02),
        (2, 0x03),
        (3, 0x04),
        (4, 0x01), // first write of second loop – must be sample 4, not 5
        (5, 0x02),
        (6, 0x03),
    ];
    assert_eq!(
        writes_with_sample, expected,
        "PlayUntilEnd looped (StartStream): sample timing shifted after natural-end wrap"
    );
}

/// Diagnostic: dump the raw command sequence for PlayUntilEnd looped (StartStream)
/// around the natural-end wrap to see exactly what pending_stream_writes produces.
///
/// data=[0x01,0x02,0x03,0x04], freq=44100.  We collect *every* command item
/// (both WaitSamples splits and Ym2612 writes) in order so the interleaving is
/// visible.  This lets us verify that no extra write is squeezed into sample 3
/// (the wrap tick) before sample 4 starts.
///
/// Expected sequence of items (sample_before → value or wait_n):
///   Wait(0) then write 0x01  @ sample 0
///   Wait(1) then write 0x02  @ sample 1
///   Wait(1) then write 0x03  @ sample 2
///   Wait(1) then write 0x04  @ sample 3
///   Wait(1) then write 0x01  @ sample 4   ← loop restart, no double write at 3
///   Wait(1) then write 0x02  @ sample 5
///
/// If the natural-end branch emits an immediate extra write then we would see
/// two consecutive Ym2612Write items without an intervening WaitSamples.
#[test]
fn test_length_mode_play_until_end_looped_start_stream_raw_sequence() {
    use soundlog::vgm::command::VgmCommand;
    use soundlog::vgm::stream::StreamResult;

    #[derive(Debug, PartialEq)]
    enum Item {
        Wait(u16),
        Write(u8),
    }

    let data = vec![0x01u8, 0x02, 0x03, 0x04];
    let mut builder = build_dac_setup_builder(data, 44100, 1, 0);
    builder.add_vgm_command(soundlog::vgm::command::StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd {
            reverse: false,
            looped: true,
        },
        data_length: 0,
    });
    // Cover the wrap boundary and a couple of writes into the second loop.
    builder.add_vgm_command(WaitSamples(5));
    builder.add_vgm_command(EndOfData);

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    let mut parser = VgmStream::new();
    push_vgm_bytes(&mut parser, &vgm_bytes);

    let mut items: Vec<Item> = Vec::new();
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(VgmCommand::WaitSamples(w))) => {
                items.push(Item::Wait(w.0));
            }
            Ok(StreamResult::Command(VgmCommand::Ym2612Write(_, spec)))
                if spec.register == 0x2A =>
            {
                items.push(Item::Write(spec.value));
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            Err(e) => panic!("parse error: {e:?}"),
            _ => {}
        }
    }

    // The write for the loop-restart (0x01 second time) must be preceded by a
    // WaitSamples, not immediately follow the last write of the first loop (0x04).
    // Two consecutive Write items would indicate the double-write bug.
    let consecutive_writes: Vec<_> = items
        .windows(2)
        .filter(|w| matches!(w, [Item::Write(_), Item::Write(_)]))
        .collect();
    assert!(
        consecutive_writes.is_empty(),
        "double write (no Wait between): {items:?}"
    );

    // Also verify the exact item sequence.
    let expected = vec![
        Item::Write(0x01), // sample 0 – no Wait prefix when current_sample == next_write_sample
        Item::Wait(1),
        Item::Write(0x02), // sample 1
        Item::Wait(1),
        Item::Write(0x03), // sample 2
        Item::Wait(1),
        Item::Write(0x04), // sample 3
        Item::Wait(1),
        Item::Write(0x01), // sample 4 – loop restart
        Item::Wait(1),
        Item::Write(0x02), // sample 5
    ];
    assert_eq!(items, expected, "raw command sequence mismatch: {items:?}");
}
