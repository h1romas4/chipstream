//! Compatibility handling for VGM PWM initialization.

const WRITE_THRESHOLD: u8 = 32;

#[derive(Clone, Copy, Default)]
pub(super) enum PwmCompatibility {
    #[default]
    Monitoring,
    Counting(u8),
    Finished,
}

impl PwmCompatibility {
    /// Return true once, before the write that triggers channel initialization.
    pub(super) fn observe(&mut self, register: u8, value: u32) -> bool {
        let count = match *self {
            Self::Monitoring => 0,
            Self::Counting(count) => count,
            Self::Finished => return false,
        };
        if register == 0 {
            // Match the source player's check of the command's low data byte.
            if value & 0xff != 0 {
                *self = Self::Finished;
            }

            return false;
        }
        let count = count + 1;
        if count == WRITE_THRESHOLD {
            *self = Self::Finished;
            true
        } else {
            *self = Self::Counting(count);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PwmCompatibility;

    #[test]
    fn zero_control_preserves_count_and_nonzero_control_ends_detection() {
        let mut state = PwmCompatibility::default();
        for _ in 0..31 {
            assert!(!state.observe(2, 0x123));
        }
        assert!(!state.observe(0, 0));
        assert!(state.observe(4, 0x456));
        assert!(!state.observe(2, 0x789));
        let mut state = PwmCompatibility::default();
        for _ in 0..31 {
            assert!(!state.observe(1, 0x800));
        }
        assert!(!state.observe(0, 0x101));
        assert!(!state.observe(0, 0));
        assert!(!state.observe(4, 0x123));
    }
}
