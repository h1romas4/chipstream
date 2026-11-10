//! Serial NDSIF transport and scheduled playback.

#![doc = include_str!("../README.md")]

mod error;
mod recording;
mod scheduled;
mod session;
mod transport;
mod worker;

pub use error::NdsifTransportError;
pub use recording::{
    NdsifCommandBatch, NdsifRecordingError, NdsifRecordingTransport, NdsifReplayError,
};
pub use scheduled::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent, NdsifPsgEvent, NdsifRegisterEvent,
    NdsifRoutingTransport, NdsifTransport,
};
pub use transport::{NdsifDeviceInfo, NdsifHostTimeline, NdsifOutputGain, NdsifSerialTransport};
pub use worker::{NdsifHardwareWorkers, NdsifRecorder, NdsifWorkerDevice, NdsifWorkerError};
