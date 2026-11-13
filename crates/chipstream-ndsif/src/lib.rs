//! NDSIF hardware integration for chipstream.

#![doc = include_str!("../README.md")]

mod factory;
pub mod ndsif;
mod oki_adpcm;
mod policy;
mod routing;

pub use factory::{
    create_automatic_transport_factories, create_automatic_transport_factories_with_policy,
    create_chip_factories,
};
pub use ndsif::{
    NdsifAyCompatibility, NdsifOkim6258, NdsifSn76489, NdsifSoundChip, register_ay8910_factory,
    register_okim6258_factory,
};
pub use ndsif_transport::{
    NdsifRoutingTransport, NdsifSerialConfig, NdsifSerialTransport, NdsifTransportError,
};
pub use policy::{NdsifDirectPolicy, NdsifNd7Policy, NdsifRouteOptions, NdsifRoutePolicy};
pub use routing::{
    NdsifNd7Validator, NdsifRoute, NdsifRoutingError, create_routed_chip_factories,
    create_routed_transport_factories, create_routed_transport_factories_with_policy,
    create_shared_routed_chip_factories,
};
