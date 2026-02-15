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
/// extracted from chip registers, along with the calculated frequency in Hz
/// if the master clock is known.
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
    /// This is the actual output frequency calculated from fnum, block,
    /// and the chip's master clock frequency. None if calculation failed
    /// or master clock is unknown.
    pub freq_hz: Option<f64>,
}

impl ToneInfo {
    /// Create a new ToneInfo with frequency calculation
    ///
    /// # Arguments
    ///
    /// * `fnum` - F-number value
    /// * `block` - Block value
    /// * `freq_hz` - Calculated frequency in Hz (or None)
    pub fn new(fnum: u16, block: u8, freq_hz: Option<f64>) -> Self {
        Self {
            fnum,
            block,
            freq_hz,
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
