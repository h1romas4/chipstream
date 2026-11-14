//! First-order DC blocking for mono native-rate PCM.

use crate::chip::SampleRate;

use super::AudioError;

const CUTOFF_HZ: f64 = 20.0;

/// A stateful 20 Hz first-order high-pass filter for mono `i32` PCM.
///
/// Create a separate filter for each channel. The sample rate may change
/// between calls; filter history is retained until [`Self::reset`]. Output is
/// rounded and saturated to the `i32` range.
#[derive(Clone, Debug, Default)]
pub struct DcBlocker {
    previous_input: f64,
    previous_output: f64,
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
        let k = std::f64::consts::PI * CUTOFF_HZ * f64::from(rate.denominator().get())
            / f64::from(rate.numerator());
        let b = 1.0 / (1.0 + k);
        let a = (1.0 - k) * b;
        for sample in samples {
            let input = f64::from(*sample);
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
