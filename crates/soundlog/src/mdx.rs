//! MDX utilities and command definitions.

pub mod command;
pub mod compat;
pub mod convert;
pub mod document;
pub mod encoding;
pub mod header;
pub mod lz;
pub mod package;
pub mod parser;
pub mod pcm;
pub mod pcm_mixer;
pub mod pdx;
pub mod tone;

/// PCM playback mode inferred from the MDX track layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MdxPcmMode {
    /// Standard nine-track PCM1 playback.
    LegacyAdpcm,
    /// Extended sixteen-track PCM8A playback.
    Pcm8a,
}

impl MdxPcmMode {
    /// Select PCM8A for sixteen tracks and PCM1 otherwise.
    fn from_track_count(track_count: usize) -> Self {
        if track_count == 16 {
            Self::Pcm8a
        } else {
            Self::LegacyAdpcm
        }
    }

    /// Resolve the selected bank, ignoring it for PCM1.
    fn sample_bank(self, selected: u8) -> usize {
        match self {
            Self::LegacyAdpcm => 0,
            Self::Pcm8a => usize::from(selected),
        }
    }
}
