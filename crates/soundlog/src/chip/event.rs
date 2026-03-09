//! State events and related types.
//!
//! This module defines the events that can be emitted when chip register
//! state changes, along with supporting types for key state and tone information.

/// Key state for a channel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyState {
    /// Channel is not producing sound
    Off,
    /// Channel is producing sound
    On,
}

/// Tone information extracted from register state
///
/// This structure contains the frequency parameters (f-number and block)
/// extracted from chip registers, along with an optional calculated frequency.
/// Note: `freq_hz` is stored as `Option<f32>` to reduce size and match the
/// crate's public API. Additionally, a `total_level` field is reserved for
/// future use / documentation purposes. It is included in the struct for API
/// compatibility but is currently not populated by existing constructors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneInfo {
    /// F-number (frequency number)
    ///
    /// This is the chip's internal frequency value. The actual frequency
    /// depends on the block value and the chip's master clock.
    pub fnum: u16,

    /// Block (octave/frequency range)
    ///
    /// This value acts as an octave selector or frequency range multiplier.
    /// Higher block values produce higher frequencies for the same f-number.
    pub block: u8,

    /// Calculated frequency in Hz (if master clock is known)
    ///
    /// Stored as `Option<f32>`. Use `None` if the frequency could not be
    /// calculated (unknown master clock or other failure).
    pub freq_hz: Option<f32>,

    /// Total level / attenuation (reserved)
    ///
    /// This field is reserved for future use (and for documentation purposes).
    /// Existing constructors leave this as `None`.
    pub total_level: Option<TotalLevel>,
}

/// TotalLevel information extracted from register state.
/// This is reserved for future use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TotalLevel {
    tlnum: u16,
    db: Option<f32>,
}

impl ToneInfo {
    /// Create a new ToneInfo
    ///
    /// This constructor accepts an `Option<f32>` for the frequency. The
    /// `total_level` field is left as `None`.
    ///
    /// # Arguments
    ///
    /// * `fnum` - F-number value
    /// * `block` - Block value
    /// * `freq_hz` - Calculated frequency in Hz (or None).
    pub fn new(fnum: u16, block: u8, freq_hz: Option<f32>) -> Self {
        Self {
            fnum,
            block,
            freq_hz,
            total_level: None,
        }
    }

    /// New constructor that accepts `Option<f32>` for frequency and explicitly
    /// allows specifying `total_level`.
    ///
    /// # Arguments
    ///
    /// * `fnum` - F-number value
    /// * `block` - Block value
    /// * `freq_hz` - Calculated frequency in Hz (or None)
    /// * `total_level` - Optional total level / attenuation (reserved)
    pub fn new_with_total_level(
        fnum: u16,
        block: u8,
        freq_hz: Option<f32>,
        total_level: Option<TotalLevel>,
    ) -> Self {
        Self {
            fnum,
            block,
            freq_hz,
            total_level,
        }
    }

    /// Create a new ToneInfo without frequency calculation
    ///
    /// # Arguments
    ///
    /// * `fnum` - F-number value
    /// * `block` - Block value
    pub fn without_freq(fnum: u16, block: u8) -> Self {
        Self {
            fnum,
            block,
            freq_hz: None,
            total_level: None,
        }
    }
}

/// Events that can be emitted from state tracking
///
/// These events are generated when notable state changes occur,
/// such as key on/off or tone parameter changes.
#[derive(Debug, Clone, PartialEq)]
pub enum StateEvent {
    /// Channel key-on event with tone information
    ///
    /// Emitted when a channel transitions from Off to On state.
    /// Includes the current tone parameters at the moment of key-on.
    KeyOn {
        /// Channel number that was keyed on
        channel: u8,
        /// Tone information at key-on time
        tone: ToneInfo,
    },

    /// Channel key-off event
    ///
    /// Emitted when a channel transitions from On to Off state.
    KeyOff {
        /// Channel number that was keyed off
        channel: u8,
    },

    /// Tone changed while key is still on
    ///
    /// Emitted when frequency parameters (fnum/block) change while
    /// the channel is actively producing sound. This can be used
    /// to detect pitch bends, vibrato, or portamento effects.
    ToneChange {
        /// Channel number with tone change
        channel: u8,
        /// New tone information
        tone: ToneInfo,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toneinfo_new_and_without_freq() {
        let t1 = ToneInfo::new(0x1234, 5, Some(440.0));
        assert_eq!(t1.fnum, 0x1234);
        assert_eq!(t1.block, 5);
        assert_eq!(t1.freq_hz, Some(440.0));
        assert!(t1.total_level.is_none());

        let t2 = ToneInfo::without_freq(0x0100, 3);
        assert_eq!(t2.fnum, 0x0100);
        assert_eq!(t2.block, 3);
        assert_eq!(t2.freq_hz, None);
        assert!(t2.total_level.is_none());
    }

    #[test]
    fn test_toneinfo_new_with_total_level() {
        // TotalLevel fields are private but accessible within the same module tests.
        let tl = TotalLevel {
            tlnum: 7,
            db: Some(-12.0),
        };
        let t = ToneInfo::new_with_total_level(0x0001, 2, Some(261.625), Some(tl));
        assert_eq!(t.fnum, 0x0001);
        assert_eq!(t.block, 2);
        assert_eq!(t.freq_hz, Some(261.625));
        assert_eq!(t.total_level.unwrap(), tl);
    }

    #[test]
    fn test_state_event_variants_and_equality() {
        let tone = ToneInfo::new(100, 4, Some(440.0));

        let keyon = StateEvent::KeyOn { channel: 1, tone };
        match keyon {
            StateEvent::KeyOn { channel, tone: t } => {
                assert_eq!(channel, 1);
                assert_eq!(t, tone);
            }
            _ => panic!("expected KeyOn variant"),
        }

        let keyoff = StateEvent::KeyOff { channel: 2 };
        assert_eq!(keyoff, StateEvent::KeyOff { channel: 2 });

        let tonechg = StateEvent::ToneChange { channel: 3, tone };
        assert_eq!(tonechg, StateEvent::ToneChange { channel: 3, tone });
    }
}
