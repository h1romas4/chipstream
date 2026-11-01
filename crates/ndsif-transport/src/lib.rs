//! Serial NDSIF transport and scheduled chipstream backend integration.

#![doc = include_str!("../README.md")]

mod error;
mod factory;
mod session;
mod transport;

pub use error::NdsifTransportError;
pub use factory::{NdsifSerialConfig, create_chip_factories};
pub use transport::NdsifSerialTransport;
