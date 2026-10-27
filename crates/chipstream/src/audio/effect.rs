//! In-place stereo effects, including gain and balance.

use super::{AudioError, StereoBlock};

/// An in-place stereo PCM effect.
pub trait StereoEffect {
    /// Process one block; on error the block is unchanged.
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError>;
}

/// A constant, nonnegative linear amplitude gain.
pub struct Gain {
    gain: f32,
}

impl Gain {
    pub fn new(gain: f32) -> Result<Self, AudioError> {
        if !gain.is_finite() || gain < 0.0 {
            return Err(AudioError::InvalidGain);
        }
        Ok(Self { gain })
    }

    pub fn set_gain(&mut self, gain: f32) -> Result<(), AudioError> {
        *self = Self::new(gain)?;
        Ok(())
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }
}

impl StereoEffect for Gain {
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError> {
        apply_gains(block, [self.gain; 2])
    }
}

/// Linear stereo balance; the center leaves both channels unchanged.
///
/// `-1` mutes right, `1` mutes left. This does not fold either channel into
/// the other and is therefore distinct from positioning a mono source.
pub struct StereoBalance {
    balance: f32,
}

impl StereoBalance {
    pub fn new(balance: f32) -> Result<Self, AudioError> {
        if !balance.is_finite() || !(-1.0..=1.0).contains(&balance) {
            return Err(AudioError::InvalidBalance);
        }
        Ok(Self { balance })
    }

    pub fn set_balance(&mut self, balance: f32) -> Result<(), AudioError> {
        *self = Self::new(balance)?;
        Ok(())
    }

    pub fn balance(&self) -> f32 {
        self.balance
    }
}

impl StereoEffect for StereoBalance {
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError> {
        apply_gains(
            block,
            [1.0 - self.balance.max(0.0), 1.0 + self.balance.min(0.0)],
        )
    }
}

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
