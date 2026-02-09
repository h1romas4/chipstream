//! soundlog — builder and parser for retro sound-chip register-write logs
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
//!
//! Examples
//!
//! Builder example:
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
//! Parser example:
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
mod binutil;
pub mod chip;
pub mod meta;
pub mod vgm;

pub use binutil::ParseError;
pub use vgm::command::*;
pub use vgm::{VgmBuilder, VgmDocument, VgmExtraHeader, VgmHeader, VgmStream};
