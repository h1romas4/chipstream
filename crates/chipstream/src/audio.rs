//! Audio transfer and processing shared by sequencers and engines.
//!
//! [`ChipCapture`] produces a [`NativeBlock`] at the chip's native rate.
//! [`StereoMatrix`] routes native outputs to a [`StereoBlock`], and
//! [`StereoResampler`] converts it to the desired rate. [`StereoMixer`]
//! combines aligned blocks; [`NativeEffect`] and [`StereoEffect`] describe
//! processing at the respective stages. Samples are not implicitly clipped.
//!
//! Resampling is stateful and requires lookahead. Drain a finite stream with
//! [`StereoResampler::finish`] and align independently generated blocks before
//! mixing them.
//!
//! Components can be combined independently of an engine or sequencer; see
//! the [crate documentation](crate) for an example.

mod buffer;
mod dc_blocker;
mod effect;
mod mixer;
mod resampler;
mod route;

pub use buffer::{AudioError, StereoBlock, StereoMatrix};
pub use dc_blocker::DcBlocker;
pub use effect::{Gain, NativeEffect, StereoBalance, StereoEffect};
pub use mixer::StereoMixer;
pub use resampler::StereoResampler;
pub(crate) use resampler::StereoResamplerState;
pub use route::{ChipCapture, NativeBlock, RouteError};
