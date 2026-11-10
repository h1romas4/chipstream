//! Serial NDSIF transport and scheduled chipstream backend integration.

#![doc = include_str!("../README.md")]

mod error;
mod factory;
mod policy;
mod recording;
mod routing;
mod session;
mod transport;
mod worker;

pub use chipstream::chip::NdsifAyCompatibility;
pub use error::NdsifTransportError;
pub use factory::{
    NdsifSerialConfig, create_automatic_transport_factories,
    create_automatic_transport_factories_with_policy, create_chip_factories,
};
pub use policy::{NdsifDirectPolicy, NdsifNd7Policy, NdsifRouteOptions, NdsifRoutePolicy};
pub use recording::{
    NdsifCommandBatch, NdsifRecordingError, NdsifRecordingTransport, NdsifReplayError,
};
pub use routing::{
    NdsifNd7Validator, NdsifRoute, NdsifRoutingError, NdsifRoutingTransport,
    create_routed_chip_factories, create_routed_transport_factories,
    create_routed_transport_factories_with_policy, create_shared_routed_chip_factories,
};
pub use transport::{NdsifDeviceInfo, NdsifHostTimeline, NdsifOutputGain, NdsifSerialTransport};
pub use worker::{NdsifHardwareWorkers, NdsifRecorder, NdsifWorkerDevice, NdsifWorkerError};
