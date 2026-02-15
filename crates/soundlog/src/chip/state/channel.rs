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
