//! Channel state tracking.
//!
//! This module provides the `ChannelState` type for tracking per-channel
//! key/tone information.

use crate::chip::event::{KeyState, ToneInfo};

/// Channel state (chip-agnostic)
///
/// Tracks the current key state and tone information for a single channel.
/// Register data is now stored in the chip's global register storage.
#[derive(Debug, Clone)]
pub struct ChannelState {
    /// Current key state
    pub key_state: KeyState,

    /// Current tone information (if available)
    pub tone: Option<ToneInfo>,
}

impl ChannelState {
    /// Create a new channel state
    pub fn new() -> Self {
        Self {
            key_state: KeyState::Off,
            tone: None,
        }
    }

    /// Clear all channel state
    ///
    /// Resets the channel to its initial state.
    pub fn clear(&mut self) {
        self.key_state = KeyState::Off;
        self.tone = None;
    }
}

impl Default for ChannelState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_and_default() {
        let c = ChannelState::new();
        assert_eq!(c.key_state, KeyState::Off);
        assert!(c.tone.is_none());

        let d = ChannelState::default();
        assert_eq!(d.key_state, KeyState::Off);
        assert!(d.tone.is_none());
    }

    #[test]
    fn test_clear_and_tone_handling() {
        let mut c = ChannelState::new();
        // set some state
        c.key_state = KeyState::On;
        c.tone = Some(ToneInfo::new(0x123, 4, Some(440.0)));
        assert_eq!(c.key_state, KeyState::On);
        assert!(c.tone.is_some());

        // clear should reset to defaults
        c.clear();
        assert_eq!(c.key_state, KeyState::Off);
        assert!(c.tone.is_none());
    }

    #[test]
    fn test_assign_and_compare_tone() {
        let mut c = ChannelState::new();
        let tone = ToneInfo::without_freq(0x10, 2);
        c.tone = Some(tone);
        assert_eq!(c.tone.unwrap(), tone);

        // Replace with a ToneInfo that includes freq
        let tone2 = ToneInfo::new(0x200, 5, Some(523.25));
        c.tone = Some(tone2);
        assert_eq!(c.tone.unwrap().freq_hz, Some(523.25));
    }
}
