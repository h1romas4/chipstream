//! Serial NDSIF transport and scheduled chipstream backend integration.

#![doc = include_str!("../README.md")]

mod error;
mod factory;
mod recording;
mod routing;
mod session;
mod transport;
mod worker;

pub use error::NdsifTransportError;
pub use factory::{NdsifSerialConfig, create_chip_factories};
pub use recording::{
    NdsifCommandBatch, NdsifRecordingError, NdsifRecordingTransport, NdsifReplayError,
};
pub use routing::{
    NdsifRoute, NdsifRoutingError, NdsifRoutingTransport, create_routed_chip_factories,
    create_routed_transport_factories, create_shared_routed_chip_factories,
};
pub use transport::{NdsifDeviceInfo, NdsifHostTimeline, NdsifOutputGain, NdsifSerialTransport};
pub use worker::{
    NdsifHardwareWorkers, NdsifRecorder, NdsifSharedDevice, NdsifWorkerError, create_recorders,
};
