//! Audio transfer and processing shared by sequencers and engines.
//!
//! Components can be combined independently of an engine or sequencer:
//!
//! ```
//! use chipstream::audio::{
//!     ChipRoute, Gain, StereoBlock, StereoEffect, StereoMatrix, StereoMixer,
//!     StereoResampler,
//! };
//! use chipstream::chip::{ClockDivider, Okim6258, SampleRate};
//!
//! let mut route = ChipRoute::new(Okim6258::new(8_000_000)?);
//! route.chip_mut().set_divider(ClockDivider::Div512)?;
//! let matrix = StereoMatrix::new(vec![[1.0 / 32768.0; 2]])?;
//! let mut src = StereoResampler::new(48_000, 10_000, 16_000, 256)?;
//! let mut samples = [0; 256];
//! let native = route.capture(&mut samples)?;
//! let stereo = matrix.convert(&native)?;
//! let converted = src.process(&stereo)?;
//! let tail = src.finish()?;
//! for block in [converted, tail] {
//!     let mut bus = StereoBlock::silence(SampleRate::from_hz(48_000), block.frames().len())?;
//!     StereoMixer::mix(&mut bus, &block, 1.0)?;
//!     Gain::new(0.5)?.process(&mut bus)?;
//!     // Send bus.frames() to the next processing stage.
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod buffer;
mod effect;
mod mixer;
mod resampler;
mod route;

pub use buffer::{AudioError, StereoBlock, StereoMatrix};
pub use effect::{Gain, StereoBalance, StereoEffect};
pub use mixer::StereoMixer;
pub use resampler::StereoResampler;
pub use route::{ChipRoute, NativeBlock, RouteError};
