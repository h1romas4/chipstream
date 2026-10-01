//! Coordinates in the finalized soundlog MDX document.

/// A location in a finalized soundlog MDX document, not in the input AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MdxLocation {
    /// A command after builder insertions and synthetic terminators.
    TrackCommand {
        /// Zero-based MDX track: A-H map to 0-7 and P-W to 8-15.
        track: usize,
        /// Zero-based command index within the finalized track.
        index: usize,
    },
    /// A tone definition in the generated tone bank.
    Tone {
        /// Zero-based tone-bank index, not the MML voice number.
        index: usize,
    },
    /// The document title, when supplied by a source directive.
    Title,
    /// The PDX filename, when supplied by a source directive.
    PcmFile,
}

impl MdxLocation {
    pub(super) fn coordinates(&self) -> (usize, usize) {
        match *self {
            Self::TrackCommand { track, index } if track < 16 => (track, index),
            Self::TrackCommand { .. } => (usize::MAX, 0),
            Self::Tone { index } => (16, index),
            Self::Title => (17, 0),
            Self::PcmFile => (17, 1),
        }
    }
}
