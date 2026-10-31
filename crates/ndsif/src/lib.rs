#![doc = include_str!("../README.md")]
#![no_std]

pub mod audio;
pub mod batch;
pub mod command;
pub mod decoder;
pub mod error;
pub mod frame;
pub mod response;
pub mod types;

/// NDSIF API specification revision implemented by this codec.
pub const SPEC_REVISION: u32 = 39;

pub use audio::AudioStreamState;
pub use batch::CommandEncoder;
pub use command::{
    AudioEvent, Command, MAX_AUDIO_DATA_BYTES, MAX_REGISTER_EVENT_WRITES, MAX_REGISTER_WRITES,
    Request, RequestEvent,
};
pub use decoder::{Decoder, PARTIAL_FRAME_TIMEOUT_MS};
pub use error::Error;
pub use frame::{
    EncodedFrame, Frame, MAX_COBS_SIZE, MAX_ENCODED_SIZE, MAX_PAYLOAD_SIZE, MAX_RAW_SIZE,
    MessageType, PROTOCOL_VERSION,
};
pub use response::{
    AudioStatus, CAPABILITY_ENTRY_SIZE, Capabilities, CapabilityEntry, ClockMode, DeviceInfo,
    MAX_CAPABILITY_ENTRIES, Reply, Response, Status,
};
pub use types::{
    AudioSampleConverter, AudioTiming, BytePosition, Divider, OkiClock, Pan, RegisterWrite,
    RegisterWrites, StatusFlags, WriteId, ZeroPair,
};
