use std::time::Duration;

use soundlog::vgm::VGM_SAMPLE_RATE;

use super::VgmSequencerError;

/// Playback behavior shared by VGM and MDX sequencers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackOptions {
    /// Number of song traversals; `None` allows unlimited looping and zero
    /// is treated as one traversal.
    pub loop_count: Option<u32>,
    /// Fade after the final loop boundary; `None` or zero disables fading.
    /// Unlimited playback and songs without loops ignore the fade. Durations
    /// are rounded to the nearest 44,100 Hz sample; values rounding to zero
    /// also disable it, and values exceeding `u32::MAX` samples are rejected.
    pub fadeout: Option<Duration>,
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            loop_count: Some(1),
            fadeout: None,
        }
    }
}

pub(super) fn fadeout_samples(fadeout: Option<Duration>) -> Result<Option<u32>, VgmSequencerError> {
    fadeout
        .map(|duration| {
            let seconds = u32::try_from(duration.as_secs())
                .map_err(|_| VgmSequencerError::DurationOverflow)?;
            let nanos = duration.subsec_nanos();
            let rate = VGM_SAMPLE_RATE / 100;
            let fractional =
                ((nanos / 1_000) * rate + ((nanos % 1_000) * rate + 5_000_000) / 1_000) / 10_000;
            seconds
                .checked_mul(VGM_SAMPLE_RATE)
                .and_then(|samples| samples.checked_add(fractional))
                .ok_or(VgmSequencerError::DurationOverflow)
        })
        .transpose()
        .map(|samples| samples.filter(|samples| *samples != 0))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::fadeout_samples;

    #[test]
    fn fadeout_rounding_matches_exact_calculation() {
        for seconds in [0, 1, 97_391, 97_392, u32::MAX as u64, u64::MAX] {
            for nanos in [0, 1, 11_337, 11_338, 499_999_999, 999_999_999] {
                let duration = Duration::new(seconds, nanos);
                let exact = (duration.as_nanos() * 44_100 + 500_000_000) / 1_000_000_000;
                let expected = u32::try_from(exact)
                    .ok()
                    .map(|value| (value != 0).then_some(value));
                assert_eq!(
                    fadeout_samples(Some(duration)).ok(),
                    expected,
                    "{duration:?}"
                );
            }
        }
    }
}
