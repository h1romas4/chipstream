//! XGM v1 timing and sample table.

use crate::chip::Chip;
use crate::vgm::VgmHeader;
use crate::vgm::command::Instance;

/// Number of sample table entries in an XGM v1 header.
pub const XGM_SAMPLE_COUNT: usize = 63;

/// Frame timing specified by an XGM file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XgmTiming {
    /// 60 frames per second.
    Ntsc,
    /// 50 frames per second.
    Pal,
}

impl XgmTiming {
    /// Frame duration in 44.1 kHz VGM samples.
    pub const fn frame_samples(self) -> u16 {
        match self {
            Self::Ntsc => 735,
            Self::Pal => 882,
        }
    }

    /// Header describing the YM2612 and SN76489 used for playback.
    pub fn vgm_header(self) -> VgmHeader {
        let (ym, psg) = match self {
            Self::Ntsc => (7_670_454, 3_579_545),
            Self::Pal => (7_600_489, 3_546_893),
        };
        let mut header = VgmHeader::default();
        header.set_chip_clock(Chip::Ym2612, Instance::Primary, ym);
        header.set_chip_clock(Chip::Sn76489, Instance::Primary, psg);
        header
    }
}

/// A sample range within the document's shared sample data, in bytes.
///
/// Both fields must be multiples of 256. Sample ID 1 corresponds to entry 0
/// in [`XgmHeader::samples`]; ID 0 is reserved for stopping playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XgmSample {
    /// Offset in the shared sample data.
    pub offset: u32,
    /// Sample length.
    pub length: u32,
}

/// XGM v1 timing and the sample ranges shared by all tracks.
///
/// The file identifier and version are fixed for XGM v1. The serialized
/// sample bank length and GD3/multitrack flags are derived from the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XgmHeader {
    /// PAL or NTSC frame timing.
    pub timing: XgmTiming,
    /// Sample ranges for IDs 1–63.
    pub samples: [Option<XgmSample>; XGM_SAMPLE_COUNT],
}

impl XgmHeader {
    /// Create a header with the given timing and an empty sample table.
    pub fn new(timing: XgmTiming) -> Self {
        Self {
            timing,
            samples: [None; XGM_SAMPLE_COUNT],
        }
    }
}
