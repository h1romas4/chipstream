/*!
Library crate for the `soundlog-cli` package.

This crate exposes the CUI and logger modules used by its command-line
frontend. The GUI is provided by the separate `soundlog-gui` crate.

The crate re-exports commonly-used items for the `soundlog` binary.
*/

#![allow(dead_code)]

pub mod cui;
pub mod logger;

/// Convenience re-exports to mirror the previous crate layout where the binary
/// could access submodules directly under `crate::...`.
pub use crate::cui::*;
pub use crate::logger::*;
