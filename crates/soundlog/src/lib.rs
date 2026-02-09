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
//! let mut stream = VgmStream::from_document(&doc);
//! while let Some(result) = stream.next() {
//!     match result {
//!         Ok(cmd) => match cmd {
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
//!         Err(e) => eprintln!("stream error: {:?}", e),
//!     }
//! }
//! ```
mod binutil;
pub mod chip;
pub mod meta;
pub mod vgm;

pub use binutil::ParseError;
pub use vgm::command::*;
pub use vgm::{VgmBuilder, VgmDocument, VgmExtraHeader, VgmHeader, VgmStream};
