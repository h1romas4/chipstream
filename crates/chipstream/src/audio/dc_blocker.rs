//! First-order DC blocking for mono native-rate PCM.
//!
//! [`DcBlocker`] retains filter history across blocks and sample-rate changes.
//! Use a separate instance for each mono channel and reset it between
//! independent streams.

use crate::chip::SampleRate;

use super::AudioError;

const CUTOFF_HZ: f32 = 20.0;

/// A stateful 20 Hz first-order high-pass filter for mono `i32` PCM.
///
/// Create a separate filter for each channel. The sample rate may change
/// between calls; filter history is retained until [`Self::reset`]. Output is
/// rounded and saturated to the `i32` range. Calculations use `f32`, so large
/// inputs lose integer precision and extreme sample rates can prevent the
/// filter history from decaying.
#[derive(Clone, Debug, Default)]
pub struct DcBlocker {
    previous_input: f32,
    previous_output: f32,
}

impl DcBlocker {
    /// Create a filter with zero history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter mono samples in place at the given native sample rate.
    ///
    /// A zero sample rate returns an error without changing the samples or
    /// filter history.
    pub fn process(&mut self, samples: &mut [i32], rate: SampleRate) -> Result<(), AudioError> {
        if rate.numerator() == 0 {
            return Err(AudioError::InvalidSampleRate);
        }
        let k = std::f32::consts::PI * CUTOFF_HZ * rate.denominator().get() as f32
            / rate.numerator() as f32;
        let b = 1.0 / (1.0 + k);
        let a = (1.0 - k) * b;
        for sample in samples {
            let input = *sample as f32;
            let output = b * (input - self.previous_input) + a * self.previous_output;
            self.previous_input = input;
            self.previous_output = output;
            *sample = output.round() as i32;
        }
        Ok(())
    }

    /// Clear the filter history.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dc_and_preserves_audible_frequencies() {
        let rate = SampleRate::from_hz(15_625);
        let mut filter = DcBlocker::new();
        let mut dc = vec![10_000; 15_625];
        filter.process(&mut dc, rate).unwrap();
        assert!(dc[15_624].abs() <= 1);

        for (frequency, min_gain, max_gain) in [(20.0, 0.70, 0.72), (1_000.0, 0.99, 1.01)] {
            let mut input: Vec<_> = (0..31_250)
                .map(|index| {
                    (10_000.0
                        * (2.0 * std::f64::consts::PI * frequency * index as f64 / 15_625.0).sin())
                    .round() as i32
                })
                .collect();
            let mut filter = DcBlocker::new();
            filter.process(&mut input, rate).unwrap();
            let rms = (input[15_625..]
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / 15_625.0)
                .sqrt();
            let gain = rms / (10_000.0 / 2.0_f64.sqrt());
            assert!(
                (min_gain..=max_gain).contains(&gain),
                "{frequency} Hz: {gain}"
            );
        }
    }

    #[test]
    fn fractional_sample_rate_and_split_blocks_preserve_state() {
        let rate = SampleRate::from_ratio(8_000_000, std::num::NonZeroU32::new(768).unwrap());
        let mut whole = DcBlocker::new();
        let mut split = DcBlocker::new();
        let mut expected = vec![10_000; 2048];
        let mut actual = expected.clone();
        whole.process(&mut expected, rate).unwrap();
        split.process(&mut actual[..511], rate).unwrap();
        split.process(&mut actual[511..], rate).unwrap();
        assert_eq!(actual, expected);
        assert!(actual[2047].abs() < 1_000);
    }

    #[test]
    fn f32_filter_tracks_f64_reference_across_rate_changes() {
        let rates = [
            SampleRate::from_hz(15_625),
            SampleRate::from_ratio(8_000_000, std::num::NonZeroU32::new(768).unwrap()),
            SampleRate::from_hz(48_000),
            SampleRate::from_hz(192_000),
        ];
        let mut filter = DcBlocker::new();
        let mut previous_input = 0.0_f64;
        let mut previous_output = 0.0_f64;
        let mut max_error = 0_i64;
        for rate in rates {
            let k = std::f64::consts::PI * 20.0 * f64::from(rate.denominator().get())
                / f64::from(rate.numerator());
            let b = 1.0 / (1.0 + k);
            let a = (1.0 - k) * b;
            let mut samples: Vec<_> = (0..15_625)
                .map(|n| {
                    let phase = n as f64 / 15_625.0;
                    (12_000.0
                        + 8_000.0 * (2.0 * std::f64::consts::PI * 20.0 * phase).sin()
                        + 4_000.0 * (2.0 * std::f64::consts::PI * 997.0 * phase).sin())
                    .round() as i32
                })
                .collect();
            let original = samples.clone();
            filter.process(&mut samples, rate).unwrap();
            for (input, actual) in original.into_iter().zip(samples) {
                let input = f64::from(input);
                let output = b * (input - previous_input) + a * previous_output;
                previous_input = input;
                previous_output = output;
                let expected = output.round() as i32;
                max_error = max_error.max((i64::from(actual) - i64::from(expected)).abs());
            }
        }
        assert!(max_error <= 2, "maximum error: {max_error} PCM units");
    }

    #[test]
    fn full_scale_steps_saturate_without_nonfinite_state() {
        let mut filter = DcBlocker::new();
        let mut samples = vec![i32::MIN; 15_625];
        filter
            .process(&mut samples, SampleRate::from_hz(15_625))
            .unwrap();
        let mut step = [i32::MAX];
        filter
            .process(&mut step, SampleRate::from_hz(15_625))
            .unwrap();
        assert_eq!(step[0], i32::MAX);
        samples.fill(i32::MAX);
        filter
            .process(&mut samples, SampleRate::from_hz(15_625))
            .unwrap();
        let mut step = [i32::MIN];
        filter
            .process(&mut step, SampleRate::from_hz(15_625))
            .unwrap();
        assert_eq!(step[0], i32::MIN);
        assert!(filter.previous_input.is_finite());
        assert!(filter.previous_output.is_finite());
    }

    #[test]
    fn invalid_rate_preserves_samples_and_filter_state() {
        let rate = SampleRate::from_hz(15_625);
        let mut filter = DcBlocker::new();
        filter.process(&mut [10_000; 8], rate).unwrap();
        let mut reference = filter.clone();
        let mut samples = [1_000; 8];
        assert!(matches!(
            filter.process(&mut samples, SampleRate::from_hz(0)),
            Err(AudioError::InvalidSampleRate)
        ));
        assert_eq!(samples, [1_000; 8]);
        let mut expected = samples;
        filter.process(&mut samples, rate).unwrap();
        reference.process(&mut expected, rate).unwrap();
        assert_eq!(samples, expected);
        filter.reset();
        let mut fresh = DcBlocker::new();
        let mut a = [1_000; 8];
        let mut b = a;
        filter.process(&mut a, rate).unwrap();
        fresh.process(&mut b, rate).unwrap();
        assert_eq!(a, b);
    }
}
