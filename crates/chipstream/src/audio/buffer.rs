//! Stereo PCM blocks, native-output conversion matrices, and audio errors.

use std::error::Error;
use std::fmt;

use crate::chip::SampleRate;

use super::NativeBlock;

/// An error in the shared audio processing components.
#[derive(Debug)]
pub enum AudioError {
    InvalidSampleRate,
    InvalidRateRange {
        min: u32,
        max: u32,
    },
    RateOutOfRange {
        rate: SampleRate,
        min: u32,
        max: u32,
    },
    InvalidChunkSize,
    InvalidMatrix,
    ChannelMismatch {
        expected: usize,
        actual: usize,
    },
    RateMismatch {
        expected: SampleRate,
        actual: SampleRate,
    },
    FrameMismatch {
        expected: usize,
        actual: usize,
    },
    InvalidGain,
    InvalidBalance,
    NonFiniteSample,
    StreamFinished,
    OutputLengthOverflow,
    ResamplerDrainIncomplete,
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => write!(f, "sample rate must be nonzero"),
            Self::InvalidRateRange { min, max } => {
                write!(f, "invalid input rate range {min}..={max} Hz")
            }
            Self::RateOutOfRange { rate, min, max } => write!(
                f,
                "input rate {}/{} Hz is outside {min}..={max} Hz",
                rate.numerator(),
                rate.denominator()
            ),
            Self::InvalidChunkSize => write!(f, "maximum input chunk size must be nonzero"),
            Self::InvalidMatrix => write!(
                f,
                "stereo matrix must contain finite coefficients and at least one channel"
            ),
            Self::ChannelMismatch { expected, actual } => {
                write!(f, "expected {expected} input channels, got {actual}")
            }
            Self::RateMismatch { expected, actual } => write!(
                f,
                "sample rate mismatch: expected {}/{} Hz, got {}/{} Hz",
                expected.numerator(),
                expected.denominator(),
                actual.numerator(),
                actual.denominator()
            ),
            Self::FrameMismatch { expected, actual } => {
                write!(f, "frame count mismatch: expected {expected}, got {actual}")
            }
            Self::InvalidGain => write!(f, "gain must be finite and nonnegative"),
            Self::InvalidBalance => write!(f, "balance must be finite and between -1 and 1"),
            Self::NonFiniteSample => write!(f, "audio contains a non-finite sample"),
            Self::StreamFinished => write!(f, "resampler stream is already finished"),
            Self::OutputLengthOverflow => {
                write!(f, "resampled output length exceeds supported limits")
            }
            Self::ResamplerDrainIncomplete => write!(
                f,
                "resampler did not drain within the supported padding length"
            ),
        }
    }
}

impl Error for AudioError {}

/// Interleaved stereo PCM, without implicit clipping or normalization.
#[derive(Debug, Clone, PartialEq)]
pub struct StereoBlock {
    pub(crate) sample_rate: SampleRate,
    pub(crate) frames: Vec<[f32; 2]>,
}

impl StereoBlock {
    /// Create a block of finite stereo frames.
    pub fn new(sample_rate: SampleRate, frames: Vec<[f32; 2]>) -> Result<Self, AudioError> {
        if sample_rate.numerator() == 0 {
            return Err(AudioError::InvalidSampleRate);
        }
        if frames.iter().flatten().any(|sample| !sample.is_finite()) {
            return Err(AudioError::NonFiniteSample);
        }
        Ok(Self {
            sample_rate,
            frames,
        })
    }

    /// Create a silent block with the given rate and frame count.
    pub fn silence(sample_rate: SampleRate, frames: usize) -> Result<Self, AudioError> {
        Self::new(sample_rate, vec![[0.0; 2]; frames])
    }

    /// The sample rate of this block.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// Interleaved left/right frames.
    pub fn frames(&self) -> &[[f32; 2]] {
        &self.frames
    }
}

/// Explicit native-output-to-stereo coefficients, including amplitude scaling.
///
/// Provide one coefficient pair per native output. The matrix defines the
/// chip-specific layout and scaling, applies separate chip-controlled stereo
/// mute, and can position mono sources using its coefficients.
#[derive(Debug, Clone)]
pub struct StereoMatrix {
    coefficients: Vec<[f32; 2]>,
}

impl StereoMatrix {
    /// One `[left, right]` coefficient pair for each native output.
    pub fn new(coefficients: Vec<[f32; 2]>) -> Result<Self, AudioError> {
        if coefficients.is_empty()
            || coefficients
                .iter()
                .flatten()
                .any(|value| !value.is_finite())
        {
            return Err(AudioError::InvalidMatrix);
        }
        Ok(Self { coefficients })
    }

    /// Convert a native block to stereo and apply its chip-controlled mutes.
    ///
    /// The coefficients define the native channel layout and numeric scaling;
    /// no chip-specific channel layout or normalization is guessed.
    pub fn convert(&self, input: &NativeBlock<'_>) -> Result<StereoBlock, AudioError> {
        if input.channels() != self.coefficients.len() {
            return Err(AudioError::ChannelMismatch {
                expected: self.coefficients.len(),
                actual: input.channels(),
            });
        }
        let mute = input.stereo_mute();
        let mut frames = Vec::with_capacity(input.frames());
        for input_frame in input.samples().chunks_exact(input.channels()) {
            let mut output = [0.0_f32; 2];
            for (sample, coeff) in input_frame.iter().zip(&self.coefficients) {
                output[0] += *sample as f32 * coeff[0];
                output[1] += *sample as f32 * coeff[1];
            }
            if let Some(mute) = mute {
                if mute.left {
                    output[0] = 0.0;
                }
                if mute.right {
                    output[1] = 0.0;
                }
            }
            frames.push(output);
        }
        StereoBlock::new(input.sample_rate_ratio(), frames)
    }
}

pub(crate) fn same_rate(a: SampleRate, b: SampleRate) -> bool {
    fn gcd(mut a: u32, mut b: u32) -> u32 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let a_divisor = gcd(a.numerator(), a.denominator().get());
    let b_divisor = gcd(b.numerator(), b.denominator().get());
    a.numerator() / a_divisor == b.numerator() / b_divisor
        && a.denominator().get() / a_divisor == b.denominator().get() / b_divisor
}
