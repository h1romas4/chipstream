//! Typed XGM v1 music commands.

/// One YM2612 register write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XgmRegisterWrite {
    /// Register address.
    pub register: u8,
    /// Register value.
    pub value: u8,
}

/// A command in an XGM music block.
///
/// Write groups must contain 1–16 entries. PCM channels and priorities are
/// in `0..=3`; sample IDs are in `0..=63`, with zero stopping the channel.
/// A loop target is a zero-based command index within the same track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XgmCommand {
    /// Wait for one PAL or NTSC frame.
    Frame,
    /// Write one or more bytes to the PSG.
    Psg(Vec<u8>),
    /// Write one or more YM2612 registers on port 0.
    Ym2612Port0(Vec<XgmRegisterWrite>),
    /// Write one or more YM2612 registers on port 1.
    Ym2612Port1(Vec<XgmRegisterWrite>),
    /// Write one or more values to YM2612 register `0x28`.
    Ym2612Key(Vec<u8>),
    /// Start a sample, or stop the channel when `sample_id` is zero.
    Pcm {
        /// PCM channel, 0–3.
        channel: u8,
        /// Priority level, 0–3 (encoded in increments of four).
        priority: u8,
        /// Sample ID, 1–63, or zero to stop.
        sample_id: u8,
    },
    /// Jump to a command index within the current music block.
    Loop(usize),
    /// End playback.
    End,
}
