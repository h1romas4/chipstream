//! Stereo PCM blocks, native-output conversion matrices, and audio errors.

use std::error::Error;
use std::fmt;

use crate::chip::SampleRate;

use super::NativeBlock;

/// An error in the shared audio processing components.
#[derive(Debug)]
pub enum AudioError {
    /// A sample rate is zero.
    InvalidSampleRate,
    /// The configured input rate range is invalid.
    InvalidRateRange {
        /// Minimum input rate in hertz.
        min: u32,
        /// Maximum input rate in hertz.
        max: u32,
    },
    /// The input rate is outside the configured range.
    RateOutOfRange {
        /// Actual rational input rate.
        rate: SampleRate,
        /// Minimum input rate in hertz.
        min: u32,
        /// Maximum input rate in hertz.
        max: u32,
    },
    /// A maximum input chunk size is zero.
    InvalidChunkSize,
    /// A stereo matrix is empty or contains non-finite coefficients.
    InvalidMatrix,
    /// The input has an unexpected number of channels.
    ChannelMismatch {
        /// Required channel count.
        expected: usize,
        /// Actual channel count.
        actual: usize,
    },
    /// The input has an unexpected sample rate.
    RateMismatch {
        /// Required sample rate.
        expected: SampleRate,
        /// Actual sample rate.
        actual: SampleRate,
    },
    /// Audio blocks have incompatible frame counts.
    FrameMismatch {
        /// Required frame count.
        expected: usize,
        /// Actual frame count.
        actual: usize,
    },
    /// Gain is negative or non-finite.
    InvalidGain,
    /// Balance is non-finite or outside `-1..=1`.
    InvalidBalance,
    /// An input or calculated sample is non-finite.
    NonFiniteSample,
    /// The resampler stream has already finished.
    StreamFinished,
    /// The resampled output length exceeds supported limits.
    OutputLengthOverflow,
    /// The resampler did not drain within the supported padding length.
    ResamplerDrainIncomplete,
}

impl fmt::Display for AudioError {
    /// Format the audio processing error.
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
        self.convert_with_gains(input, None)
    }

    pub(crate) fn convert_with_gains(
        &self,
        input: &NativeBlock<'_>,
        gains: Option<&[f32]>,
    ) -> Result<StereoBlock, AudioError> {
        if input.channels() != self.coefficients.len() {
            return Err(AudioError::ChannelMismatch {
                expected: self.coefficients.len(),
                actual: input.channels(),
            });
        }
        if let Some(gains) = gains {
            if gains.len() != input.channels() {
                return Err(AudioError::ChannelMismatch {
                    expected: input.channels(),
                    actual: gains.len(),
                });
            }
            if gains.iter().any(|gain| !gain.is_finite() || *gain < 0.0) {
                return Err(AudioError::InvalidGain);
            }
        }
        let mute = input.stereo_mute();
        let mut frames = Vec::with_capacity(input.frames());
        for input_frame in input.samples().chunks_exact(input.channels()) {
            let mut output = [0.0_f32; 2];
            for (index, (sample, coeff)) in input_frame.iter().zip(&self.coefficients).enumerate() {
                let gain = gains.map_or(1.0, |gains| gains[index]);
                output[0] += *sample as f32 * coeff[0] * gain;
                output[1] += *sample as f32 * coeff[1] * gain;
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

/// Compare rational sample rates after reducing their fractions.
pub(crate) fn same_rate(a: SampleRate, b: SampleRate) -> bool {
    /// Compute the greatest common divisor for rate reduction.
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
