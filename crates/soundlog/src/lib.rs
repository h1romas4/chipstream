#![doc = include_str!("../README.md")]
//! soundlog — builder, parser and stream-processor for retro sound-chip register-write logs
//!
//! `soundlog` is a small crate for building and parsing register-write
//! logs for retro sound chips. It currently supports the VGM
//! (Video Game Music) file format.
//!
//! Key features:
//! - Builder API to construct VGM documents programmatically.
//! - Parser support to read VGM data into a structured `VgmDocument`.
//! - Type-safe APIs: chip specifications and VGM commands are modeled as
//!   Rust types to help prevent invalid register writes at compile time.
//! - Stream processing: `VgmStream` provides a low-memory, iterator-based
//!   processor that can accept either chunked binary input (via `push_data`)
//!   or a pre-parsed `VgmDocument` (via `from_document`) and yields parsed
//!   `VgmCommand` values as they become available.
//!
//! VgmStream overview
//!
//! `VgmStream` is designed for streaming/real-time consumption of VGM data:
//! - It yields `VgmCommand` values wrapped in stream results as it parses
//!   input and as it generates writes from DAC streams.
//! - It understands DAC stream control commands (e.g. `SetupStreamControl`,
//!   `SetStreamData`, `SetStreamFrequency`, `StartStream`, `StartStreamFastCall`,
//!   `StopStream`) and will expand stream-generated writes into the output
//!   timeline at the correct sample positions.
//! - During `Wait` commands, the internal scheduler finds upcoming stream-
//!   generated writes and splits waits as necessary so that generated chip
//!   writes are interleaved with parsed commands. This avoids emitting large
//!   bursts and preserves per-sample timing when multiple DAC streams are
//!   active concurrently.
//! - `StartStreamFastCall` block references are resolved by tracking data block
//!   sequence order; the crate also provides helpers to inspect stored
//!   decompression tables and uncompressed stream data where applicable.
//!
//! Examples
//!
//! Example: builder and document round-trip
//!
//! ```rust
//! use soundlog::{VgmBuilder, VgmCommand, VgmDocument};
//! use soundlog::chip::{Chip, Ym2612Spec};
//! use soundlog::vgm::command::{WaitSamples, Instance};
//! use soundlog::meta::Gd3;
//!
//! let mut builder = VgmBuilder::new();
//!
//! // Register the chip's master clock in the VGM header (in Hz)
//! builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
//! // Append chip register writes using a chip-specific spec
//! builder.add_chip_write(
//!     Instance::Primary,
//!     Ym2612Spec {
//!         port: 0,
//!         register: 0x22,
//!         value: 0x91,
//!     },
//! );
//! // Append a VGM command (example: wait)
//! builder.add_vgm_command(WaitSamples(44100));
//! // ... add more commands
//!
//! // Set GD3 metadata for the document
//! builder.set_gd3(Gd3 {
//!     track_name_en: Some("Example Track".to_string()),
//!     game_name_en: Some("soundlog examples".to_string()),
//!     ..Default::default()
//! });
//!
//! // Finalize the document
//! let document: VgmDocument = builder.finalize();
//! // `into()` converts the finalized `VgmDocument` into VGM-format binary bytes
//! let bytes: Vec<u8> = document.into();
//! ```
//!
//! Example: parsing a document
//!
//! ```rust
//! use soundlog::{VgmBuilder, VgmDocument};
//! use soundlog::vgm::command::{Instance, VgmCommand, WaitSamples};
//!
//! // Read VGM bytes from somewhere
//! let bytes: Vec<u8> = /* read a .vgm file */ Vec::new();
//!
//! // For this example we construct a VGM byte sequence using the builder
//! // and then parse it back.
//! let mut b = VgmBuilder::new();
//! b.add_vgm_command(WaitSamples(100));
//! b.add_vgm_command(WaitSamples(200));
//! let doc = b.finalize();
//! let bytes: Vec<u8> = (&doc).into();
//!
//! // Parse the bytes into a `VgmDocument`
//! let document: VgmDocument = (bytes.as_slice())
//!     .try_into()
//!     .expect("failed to parse serialized VGM");
//!
//! // Example: map commands to their sample counts and sum them.
//! let total_wait: u32 = document
//!     .iter()
//!     .map(|cmd| match cmd {
//!         VgmCommand::WaitSamples(s) => s.0 as u32,
//!         _ => 0,
//!     })
//!     .sum();
//!
//! assert_eq!(total_wait, 300);
//! ```
//!
//! Example: using `VgmStream::from_document`
//!
//! The `from_document` constructor is convenient when you already have a
//! parsed `VgmDocument` (for example: constructed programmatically via the
//! `VgmBuilder`). The stream will expand DAC-stream-generated writes into
//! the emitted command sequence and split waits so emitted writes are
//! interleaved at the correct sample positions.
//!
//! ```no_run
//! use soundlog::{VgmBuilder, VgmStream, VgmDocument};
//! use soundlog::vgm::stream::StreamResult;
//! use soundlog::vgm::command::{VgmCommand, WaitSamples, SetupStreamControl, StartStream, Instance};
//! use soundlog::chip::Ym2612Spec;
//!
//! // Build a minimal document that contains a data block and stream control
//! // commands. (Builder helpers for data blocks / stream setup exist on the
//! // `VgmBuilder` type; see the vgm module docs for details.)
//! let mut b = VgmBuilder::new();
//! // Example: append a YM2612 chip register write using the chip-specific spec
//! b.add_chip_write(
//!     Instance::Primary,
//!     Ym2612Spec {
//!         port: 0,
//!         register: 0x22,
//!         value: 0x91,
//!     },
//! );
//! // (pseudo-code) append data block, configure stream and start it
//! // b.add_data_block(...);
//! // b.add_vgm_command(SetupStreamControl { /* ... */ });
//! // b.add_vgm_command(StartStream { /* ... */ });
//! b.add_vgm_command(WaitSamples(8));
//!
//! let doc: VgmDocument = b.finalize();
//!
//! // Create a stream from the parsed document. The iterator will yield
//! // parsed commands as well as any stream-generated writes expanded into
//! // the timeline.
//! let mut stream = VgmStream::from_document(doc);
//! while let Some(result) = stream.next() {
//!     match result {
//!         Ok(StreamResult::Command(cmd)) => match cmd {
//!             VgmCommand::WaitSamples(s) => {
//!                 // Waits may have been split to accommodate stream-generated writes.
//!                 println!("wait {} samples", s.0);
//!             }
//!             VgmCommand::Ym2612Write(inst, spec) => {
//!                 // Handle YM2612 writes here. For example, forward to a device API.
//!                 println!("YM2612 write: {:?} {:?}", inst, spec);
//!             }
//!             other => {
//!                 // Write to the target chips here (e.g. SN76489).
//!                 // Implement actual playback / device I/O in this branch.
//!                 println!("cmd: {:?}", other)
//!             },
//!         },
//!         Ok(StreamResult::NeedsMoreData) => break,
//!         Ok(StreamResult::EndOfStream) => break,
//!         Err(e) => eprintln!("stream error: {:?}", e),
//!     }
//! }
//! ```
//!
//! ```no_run
//! use soundlog::vgm::VgmStream;
//! use soundlog::vgm::stream::StreamResult;
//!
//! let mut parser = VgmStream::new();
//! parser.set_loop_count(Some(2));
//!
//! // Feed VGM data (could be incoming chunks)
//! let vgm_data = vec![0x62, 0x63, 0x66];
//! parser.push_data(&vgm_data);
//!
//! for result in &mut parser {
//!     match result {
//!         Ok(StreamResult::Command(_)) => {
//!             // handle the command
//!         }
//!         Ok(StreamResult::NeedsMoreData) => break,
//!         Ok(StreamResult::EndOfStream) => break,
//!         Err(e) => break,
//!     }
//! }
//! ```
//!
//! From a parsed `VgmDocument` (including stream control commands):
//! ```no_run
//! use soundlog::{VgmBuilder, vgm::stream::VgmStream};
//! use soundlog::vgm::stream::StreamResult;
//! use soundlog::vgm::command::{
//!     WaitSamples, DataBlock, SetupStreamControl, SetStreamData, SetStreamFrequency, StartStream,
//! };
//!
//! // Build a VGM document that contains:
//! // - a DataBlock with an uncompressed PCM stream (data_type 0x00)
//! // - stream control commands to route that bank to a YM2612 stream (chip id 0x02)
//! // - a StartStream and a Wait to allow generated writes to occur
//! let mut builder = VgmBuilder::new();
//!
//! // Simple uncompressed stream data block (type 0x00 = YM2612 PCM)
//! let block = DataBlock {
//!     marker: 0x66,
//!     chip_instance: 0,
//!     data_type: 0x00, // data bank id
//!     size: 4,
//!     data: vec![0x10, 0x20, 0x30, 0x40],
//! };
//! builder.add_vgm_command(block);
//!
//! // Configure stream 0 to write to YM2612 (chip id 0x02), register 0x2A
//! builder.add_vgm_command(SetupStreamControl {
//!     stream_id: 0,
//!     chip_type: 0x02, // YM2612 (DacStreamChipType::Ym2612)
//!     write_port: 0,
//!     write_command: 0x2A,
//! });
//!
//! // Point stream 0 at data bank 0x00 with step size 1
//! builder.add_vgm_command(SetStreamData {
//!     stream_id: 0,
//!     data_bank_id: 0x00,
//!     step_size: 1,
//!     step_base: 0,
//! });
//!
//! // Set a stream frequency so writes will be generated
//! builder.add_vgm_command(SetStreamFrequency {
//!     stream_id: 0,
//!     frequency: 22050, // Hz
//! });
//!
//! // Start the stream (length_mode 3 = play until end of block)
//! builder.add_vgm_command(StartStream {
//!     stream_id: 0,
//!     data_start_offset: 0,
//!     length_mode: 3,
//!     data_length: 0,
//! });
//!
//! // Wait long enough for the stream to generate writes
//! builder.add_vgm_command(WaitSamples(100));
//!
//! let doc = builder.finalize();
//!
//! // Create a stream processor from the parsed document and iterate results.
//! // The iterator will yield the original commands as well as automatically
//! // generated chip write commands produced by the active stream during Waits.
//! let mut stream = VgmStream::from_document(doc);
//! for item in &mut stream {
//!     match item {
//!         Ok(StreamResult::Command(cmd)) => {
//!             // `cmd` may be a parsed command (DataBlock/Setup/Start/Wait) or a
//!             // generated chip write (e.g. `VgmCommand::Ym2612Write`).
//!             println!("command: {:?}", cmd);
//!         }
//!         Ok(StreamResult::NeedsMoreData) => break,
//!         Ok(StreamResult::EndOfStream) => break,
//!         Err(e) => { eprintln!("error: {}", e); break; }
//!     }
//! }
//! ```
//!
//! Streaming from multiple chunks:
//! ```no_run
//! use soundlog::vgm::VgmStream;
//! use soundlog::vgm::stream::StreamResult;
//!
//! let mut parser = VgmStream::new();
//! let chunks = vec![vec![0x61, 0x44], vec![0x01], vec![0x62, 0x63]];
//!
//! for chunk in chunks {
//!     parser.push_data(&chunk);
//!     for result in &mut parser {
//!         match result {
//!             Ok(StreamResult::Command(_)) => {},
//!             Ok(StreamResult::NeedsMoreData) => break,
//!             Ok(StreamResult::EndOfStream) => break,
//!             Err(_) => break,
//!         }
//!     }
//! }
//! ```
//!
//! (For additional low-level details see the `handle_*` helpers and stream state
//! documented on individual methods.)
mod binutil;
pub mod chip;
pub mod meta;
pub mod vgm;

pub use binutil::ParseError;
pub use vgm::command::*;
pub use vgm::{VgmBuilder, VgmDocument, VgmExtraHeader, VgmHeader, VgmStream};
