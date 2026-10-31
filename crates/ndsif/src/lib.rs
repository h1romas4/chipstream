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
pub const SPEC_REVISION: u32 = 40;

pub use audio::AudioStreamState;
pub use batch::CommandEncoder;
pub use command::{
    AudioEvent, Command, DAC_PROFILE_YM2612, DAC_TICKS_PER_SECOND, DacRecord, DacRecordKind,
    DacRecords, MAX_AUDIO_DATA_BYTES, MAX_DAC_EVENT_BYTES, MAX_DAC_RECORDS, MAX_REGISTER_BYTES,
    MAX_REGISTER_EVENT_WRITES, MAX_REGISTER_WRITES, Request, RequestEvent,
};
pub use decoder::{Decoder, PARTIAL_FRAME_TIMEOUT_MS};
pub use error::Error;
pub use frame::{
    EncodedFrame, Frame, MAX_COBS_SIZE, MAX_ENCODED_SIZE, MAX_PAYLOAD_SIZE, MAX_RAW_SIZE,
    MessageType, PROTOCOL_VERSION,
};
pub use response::{
    AudioStatus, CAPABILITY_ENTRY_SIZE, CONTROL_FEATURE_DAC_PROFILE, Capabilities, CapabilityEntry,
    ClockMode, DacCapabilities, DacStatus, DeviceInfo, MAX_CAPABILITY_ENTRIES, Reply, Response,
    Status,
};
pub use types::{
    AudioSampleConverter, AudioTiming, BytePosition, Divider, OkiClock, Pan, RegisterWrite,
    RegisterWrites, StatusFlags, WriteId, ZeroPair,
};
