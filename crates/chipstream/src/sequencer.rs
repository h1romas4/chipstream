//! Device-independent VGM and MDX playback.
//!
//! [`VgmSequencer`] and [`MdxSequencer`] render songs into stereo PCM blocks
//! without an audio device or sound engine. They support per-chip gain and
//! optional register-write and state events through [`VgmEventFilter`].
//! Output is not clipped, and rendering may allocate.
//!
//! Render a VGM file containing a YM2151:
//!
//! ```no_run
//! use chipstream::sequencer::{Chip, Instance, VgmSequencer};
//!
//! let bytes = std::fs::read("song.vgm")?;
//! let mut player = VgmSequencer::from_vgm(bytes, 48_000, 512)?;
//! player.set_gain(Chip::Ym2151, Instance::Primary, 0.5)?;
//! while let Some(block) = player.render()? {
//!     // Send block.frames() to the host.
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! For MDX playback, provide the MDX and optional PDX data; PDX lookup is the
//! caller's responsibility:
//!
//! ```no_run
//! use chipstream::sequencer::MdxSequencer;
//!
//! let mdx = std::fs::read("song.mdx")?;
//! let pdx = std::fs::read("song.pdx")?;
//! let mut player = MdxSequencer::from_mdx(mdx, Some(pdx), 48_000, 512)?;
//! while let Some(block) = player.render()? {
//!     // Send block.frames() to the host.
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod compat;
mod event;
mod mdx;
mod vgm;
mod volume;

pub use event::{VgmEventFilter, VgmWriteEvent};
pub use mdx::{MdxSequencer, MdxSequencerError, MdxSequencerOptions};
pub use soundlog::chip::Chip;
pub use soundlog::chip::event::{StateEvent, ToneInfo};
pub use soundlog::vgm::command::Instance;
pub use vgm::{VgmSequencer, VgmSequencerError};
