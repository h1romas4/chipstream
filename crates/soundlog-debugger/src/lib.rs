/*!
Library crate for the `soundlog-debugger` package.

This file exposes the internal `cui`, `gui` and `logger` modules as the public
library surface so the `src/bin/soundlog.rs` binary (and any other consumers)
can import them via `soundlog_debuger::...` if desired.

The crate intentionally re-exports commonly-used items to keep usage sites
concise (for example the binary previously relied on `crate::cui::...` paths).
*/

#![allow(dead_code)]

pub mod cui;
pub mod gui;
pub mod logger;

/// Convenience re-exports to mirror the previous crate layout where the binary
/// could access submodules directly under `crate::...`.
pub use crate::cui::*;
pub use crate::gui::*;
pub use crate::logger::*;
