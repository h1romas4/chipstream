//! Mixing aligned stereo blocks at a common sample rate.

use super::buffer::same_rate;
use super::{AudioError, StereoBlock};

/// Mixes equal-length stereo blocks at the same sample rate.
///
/// A caller mixing independent sources must queue and align their blocks.
/// [`crate::sequencer::VgmSequencer`] performs this for its own chips.
pub struct StereoMixer;

impl StereoMixer {
    /// Add `input` to `destination` with a finite, nonnegative gain.
    ///
    /// No clipping is applied. On error the destination is unchanged.
    pub fn mix(
        destination: &mut StereoBlock,
        input: &StereoBlock,
        gain: f32,
    ) -> Result<(), AudioError> {
        if !gain.is_finite() || gain < 0.0 {
            return Err(AudioError::InvalidGain);
        }
        if !same_rate(destination.sample_rate, input.sample_rate) {
            return Err(AudioError::RateMismatch {
                expected: destination.sample_rate,
                actual: input.sample_rate,
            });
        }
        if destination.frames.len() != input.frames.len() {
            return Err(AudioError::FrameMismatch {
                expected: destination.frames.len(),
                actual: input.frames.len(),
            });
        }
        for (dst, src) in destination.frames.iter().zip(&input.frames) {
            if !(dst[0] + src[0] * gain).is_finite() || !(dst[1] + src[1] * gain).is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
        }
        for (dst, src) in destination.frames.iter_mut().zip(&input.frames) {
            dst[0] += src[0] * gain;
            dst[1] += src[1] * gain;
        }
        Ok(())
    }
}
