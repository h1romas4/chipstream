//! Device-independent sound-chip emulation, sequencing, and audio processing.

#![doc = include_str!("../README.md")]

pub mod audio;
pub mod chip;
pub mod metadata;
pub mod sequencer;

mod fixed;
