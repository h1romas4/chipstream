//! In-place native-output and stereo effects.
//!
//! [`NativeEffect`] processes one chip output before stereo routing, while
//! [`StereoEffect`] processes a stereo block. [`Gain`] implements both;
//! [`StereoBalance`] attenuates either side of a stereo block.

use crate::chip::SampleRate;

use super::{AudioError, StereoBlock};

/// An in-place effect on one native output, before stereo routing.
///
/// Samples use the backend's native amplitude units as `f32`; no normalization
/// or clipping is applied. The rate may change between calls. On error the
/// samples must be unchanged.
pub trait NativeEffect {
    /// Process one output's samples at its current native rate.
    fn process_native(&mut self, samples: &mut [f32], rate: SampleRate) -> Result<(), AudioError>;
}

/// A stereo PCM effect.
///
/// [`StereoBlock::try_transform`] provides reusable scratch space and preserves
/// the block if processing fails.
pub trait StereoEffect {
    /// Process one block; on error the block is unchanged.
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError>;
}

/// A constant, nonnegative linear amplitude gain.
#[derive(Clone)]
pub struct Gain {
    /// Current linear amplitude multiplier.
    gain: f32,
}

impl Gain {
    /// Create a gain with a finite, nonnegative linear amplitude factor.
    pub fn new(gain: f32) -> Result<Self, AudioError> {
        if !gain.is_finite() || gain < 0.0 {
            return Err(AudioError::InvalidGain);
        }
        Ok(Self { gain })
    }

    /// Change the gain; an invalid value leaves it unchanged.
    pub fn set_gain(&mut self, gain: f32) -> Result<(), AudioError> {
        *self = Self::new(gain)?;
        Ok(())
    }

    /// Return the current linear amplitude factor.
    pub fn gain(&self) -> f32 {
        self.gain
    }
}

impl StereoEffect for Gain {
    /// Apply this gain equally to both channels.
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError> {
        apply_gains(block, [self.gain; 2])
    }
}

impl NativeEffect for Gain {
    fn process_native(&mut self, samples: &mut [f32], _rate: SampleRate) -> Result<(), AudioError> {
        if samples
            .iter()
            .any(|sample| !(*sample * self.gain).is_finite())
        {
            return Err(AudioError::NonFiniteSample);
        }
        for sample in samples {
            *sample *= self.gain;
        }
        Ok(())
    }
}

/// Linear stereo balance; the center leaves both channels unchanged.
///
/// `-1` mutes right, `1` mutes left. This does not fold either channel into
/// the other and is therefore distinct from positioning a mono source.
#[derive(Clone)]
pub struct StereoBalance {
    /// Current balance from left (-1) to right (1).
    balance: f32,
}

impl StereoBalance {
    /// Create a balance using a finite value in `-1..=1`.
    pub fn new(balance: f32) -> Result<Self, AudioError> {
        if !balance.is_finite() || !(-1.0..=1.0).contains(&balance) {
            return Err(AudioError::InvalidBalance);
        }
        Ok(Self { balance })
    }

    /// Change the balance; an invalid value leaves it unchanged.
    pub fn set_balance(&mut self, balance: f32) -> Result<(), AudioError> {
        *self = Self::new(balance)?;
        Ok(())
    }

    /// Return the current balance value.
    pub fn balance(&self) -> f32 {
        self.balance
    }
}

impl StereoEffect for StereoBalance {
    /// Attenuate the side opposite the selected balance.
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError> {
        apply_gains(
            block,
            [1.0 - self.balance.max(0.0), 1.0 + self.balance.min(0.0)],
        )
    }
}

/// Apply channel gains only after verifying that all results are finite.
fn apply_gains(block: &mut StereoBlock, gains: [f32; 2]) -> Result<(), AudioError> {
    for frame in &block.frames {
        if !(frame[0] * gains[0]).is_finite() || !(frame[1] * gains[1]).is_finite() {
            return Err(AudioError::NonFiniteSample);
        }
    }
    for frame in &mut block.frames {
        frame[0] *= gains[0];
        frame[1] *= gains[1];
    }
    Ok(())
}
