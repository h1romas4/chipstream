//! Typed request commands and borrowed views of decoded request payloads.
//!
//! Encoding validates the message shape and arguments, not whether a device
//! supports the command or whether its current stream state permits it.

use crate::{
    BytePosition, Divider, EncodedFrame, Error, Frame, LIVE_DAC_PROFILE, LiveDacRecord,
    LiveDacRecords, MAX_LIVE_DAC_RECORDS, MAX_ORDERED_LIVE_RECORDS, MAX_PAYLOAD_SIZE, MessageType,
    ORDERED_LIVE_PROFILE, OkiClock, OrderedLiveRecord, OrderedLiveRecords, Pan, RegisterWrite,
    WriteId, ZeroPair,
};

/// Maximum immediate address/value pairs in one frame.
pub const MAX_REGISTER_WRITES: usize = (MAX_PAYLOAD_SIZE - 2) / 2;
/// Maximum immediate raw register-write bytes in one frame.
pub const MAX_REGISTER_BYTES: usize = MAX_PAYLOAD_SIZE - 2;
/// Maximum address/value pairs in one positioned audio event.
pub const MAX_REGISTER_EVENT_WRITES: usize = (MAX_PAYLOAD_SIZE - 10) / 2;
/// Maximum ADPCM data bytes in one audio-data frame.
pub const MAX_AUDIO_DATA_BYTES: usize = MAX_PAYLOAD_SIZE - 6;
/// Maximum timed DAC records in one DAC-data frame.
pub const MAX_DAC_RECORDS: usize = (MAX_PAYLOAD_SIZE - 15) / 4;
/// Maximum raw write bytes in one timed DAC register event.
pub const MAX_DAC_EVENT_BYTES: usize = MAX_PAYLOAD_SIZE - 12;
/// Optional timed DAC profile identifier for the primary YM2612.
pub const DAC_PROFILE_YM2612: u8 = 1;
/// Tick frequency of the YM2612 timed DAC profile.
pub const DAC_TICKS_PER_SECOND: u32 = 44_100;

/// Kind of record in a timed YM2612 DAC stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DacRecordKind {
    /// Write a DAC data value.
    Dac = 0,
    /// Set DAC enable using the high bit of the value.
    Select = 1,
    /// Advance time without a register write.
    Wait = 2,
}

/// One timed DAC operation and its associated wait duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DacRecord {
    /// Operation type.
    pub kind: DacRecordKind,
    /// Operation value; its valid bits depend on `kind`.
    pub value: u8,
    /// Wait duration in profile ticks.
    pub wait_ticks: u16,
}

/// Validated timed DAC records borrowed from a decoded request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DacRecords<'a>(&'a [u8]);

impl<'a> DacRecords<'a> {
    /// Validate record framing, operation values, and the per-frame limit.
    fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(4) || bytes.len() / 4 > MAX_DAC_RECORDS {
            return Err(Error::InvalidLength);
        }
        for record in bytes.as_chunks::<4>().0 {
            let kind = DacRecordKind::try_from(record[0])?;
            let value = record[1];
            let wait_ticks = u16::from_le_bytes([record[2], record[3]]);
            match kind {
                DacRecordKind::Select if value & 0x7f != 0 => return Err(Error::InvalidArgument),
                DacRecordKind::Wait if value != 0 || wait_ticks == 0 => {
                    return Err(Error::InvalidArgument);
                }
                _ => {}
            }
        }
        Ok(Self(bytes))
    }

    /// Number of four-byte records.
    pub fn len(self) -> usize {
        self.0.len() / 4
    }

    /// Whether there are no records. Valid decoded data is nonempty.
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over records in wire order.
    pub fn iter(self) -> impl ExactSizeIterator<Item = DacRecord> + 'a {
        self.0.as_chunks::<4>().0.iter().map(|record| DacRecord {
            kind: DacRecordKind::try_from(record[0]).expect("validated DAC record"),
            value: record[1],
            wait_ticks: u16::from_le_bytes([record[2], record[3]]),
        })
    }
}

impl TryFrom<u8> for DacRecordKind {
    type Error = Error;

    /// Decode a DAC record kind from its wire value.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dac),
            1 => Ok(Self::Select),
            2 => Ok(Self::Wait),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// An event positioned on an ADPCM byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEvent<'a> {
    /// Register writes to a chip, distinct from the stream's target.
    Registers {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target register page.
        page: u8,
        /// Nonempty ordered register writes.
        writes: &'a [RegisterWrite],
    },
    /// Change the ADPCM stream clock and divider.
    Clock {
        /// Input clock.
        clock: OkiClock,
        /// Clock divider.
        divider: Divider,
    },
    /// Change output-channel selection.
    Pan(Pan),
    /// End the stream using a state-dependent repeated ADPCM byte.
    End(ZeroPair),
}

/// Request encoding with local argument validation, not device/session state validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command<'a> {
    /// Reset device state.
    Reset,
    /// Request an echo of at most 32 bytes.
    Ping(&'a [u8]),
    /// Request device model and firmware information.
    GetInfo,
    /// Set output attenuation.
    SetOutputVolume {
        /// Attenuation value in the supported range `0..=96`.
        attenuation: u8,
        /// Whether a response is required.
        response: bool,
    },
    /// Configure an instance's chip input clock.
    SetChipClock {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Nonzero clock frequency in hertz.
        hz: u32,
        /// Whether a response is required.
        response: bool,
    },
    /// Request a page of device capabilities.
    GetCapabilities {
        /// Entry offset for pagination.
        offset: u16,
    },
    /// Send immediate register writes.
    WriteRegisters {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target register page.
        page: u8,
        /// Nonempty register pairs, bounded by [`MAX_REGISTER_WRITES`].
        writes: &'a [RegisterWrite],
    },
    /// Send immediate raw bytes to a compatible target.
    WriteBytes {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target page.
        page: u8,
        /// Nonempty raw bytes, bounded by [`MAX_REGISTER_BYTES`].
        bytes: &'a [u8],
    },
    /// Queue a contiguous ADPCM data chunk.
    AudioData {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
        /// Absolute ADPCM byte position of the chunk.
        position: BytePosition,
        /// Nonempty ADPCM bytes, bounded by [`MAX_AUDIO_DATA_BYTES`].
        adpcm: &'a [u8],
    },
    /// Queue an event at an absolute ADPCM byte position.
    AudioEvent {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
        /// Absolute ADPCM byte position.
        position: BytePosition,
        /// Event to execute at the position.
        event: AudioEvent<'a>,
    },
    /// Start an ADPCM stream.
    AudioStart {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
        /// Input clock.
        clock: OkiClock,
        /// Clock divider.
        divider: Divider,
    },
    /// Request audio stream status.
    AudioStatus {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
    },
    /// Request timed DAC profile capabilities.
    GetDacCapabilities,
    /// Queue records for the optional timed DAC profile.
    DacData {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Sequence number of the first record.
        sequence: u32,
        /// Starting position in profile ticks.
        position: u64,
        /// Nonempty ordered records, bounded by [`MAX_DAC_RECORDS`].
        records: &'a [DacRecord],
    },
    /// Start timed DAC playback.
    DacStart {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Minimum buffered lead, in profile ticks.
        minimum_lead_ticks: u32,
    },
    /// Seal timed DAC playback after all submitted records.
    DacEnd {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Sequence number immediately after the final record.
        next_sequence: u32,
        /// End position in profile ticks.
        end_tick: u64,
    },
    /// Request timed DAC profile status.
    DacStatus {
        /// Timed DAC profile identifier.
        profile: u8,
    },
    /// Queue register or PSG writes at a timed DAC profile position.
    DacRegisterEvent {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Absolute position in profile ticks.
        tick: u64,
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target register page.
        page: u8,
        /// Raw write bytes, bounded by [`MAX_DAC_EVENT_BYTES`].
        writes: &'a [u8],
    },
    /// Request a page of draft live-DAC capabilities.
    GetLiveDacCapabilities {
        /// Target entry offset.
        offset: u16,
    },
    /// Send live-DAC records without an acknowledgement.
    LiveDacData {
        /// YM2612 instance.
        instance: u8,
        /// Nonempty DAC records.
        records: &'a [LiveDacRecord],
    },
    /// Request draft ordered-live capabilities.
    GetOrderedLiveCapabilities,
    /// Send ordered-live records without an acknowledgement.
    OrderedLiveData {
        /// Frame sequence number.
        sequence: u32,
        /// Nonempty records, bounded by the payload and record limits.
        records: &'a [OrderedLiveRecord],
    },
    /// Request an ordered-live diagnostic snapshot.
    GetOrderedLiveStatus,
}

impl Command<'_> {
    /// Return the command ID encoded in the frame.
    pub const fn command_id(&self) -> u8 {
        match self {
            Self::Reset => 0x00,
            Self::Ping(_) => 0x01,
            Self::GetInfo => 0x02,
            Self::SetOutputVolume { .. } => 0x03,
            Self::SetChipClock { .. } => 0x04,
            Self::GetCapabilities { .. } => 0x05,
            Self::AudioData { .. } => 0x10,
            Self::AudioEvent { .. } => 0x11,
            Self::AudioStart { .. } => 0x12,
            Self::AudioStatus { .. } => 0x13,
            Self::GetDacCapabilities => 0x18,
            Self::DacData { .. } => 0x19,
            Self::DacStart { .. } => 0x1a,
            Self::DacEnd { .. } => 0x1b,
            Self::DacStatus { .. } => 0x1c,
            Self::DacRegisterEvent { .. } => 0x1d,
            Self::GetLiveDacCapabilities { .. } => 0x1e,
            Self::LiveDacData { .. } => 0x1f,
            Self::GetOrderedLiveCapabilities => 0x21,
            Self::OrderedLiveData { .. } => 0x22,
            Self::GetOrderedLiveStatus => 0x23,
            Self::WriteRegisters { write_id, .. } | Self::WriteBytes { write_id, .. } => {
                write_id.get()
            }
        }
    }

    /// Return whether this request requires a response.
    pub const fn expects_response(&self) -> bool {
        match self {
            Self::Reset
            | Self::Ping(_)
            | Self::GetInfo
            | Self::GetCapabilities { .. }
            | Self::AudioStatus { .. }
            | Self::GetDacCapabilities
            | Self::DacStart { .. }
            | Self::DacStatus { .. } => true,
            Self::GetLiveDacCapabilities { .. }
            | Self::GetOrderedLiveCapabilities
            | Self::GetOrderedLiveStatus => true,
            Self::SetOutputVolume { response, .. } | Self::SetChipClock { response, .. } => {
                *response
            }
            Self::WriteRegisters { .. }
            | Self::WriteBytes { .. }
            | Self::AudioData { .. }
            | Self::AudioEvent { .. }
            | Self::AudioStart { .. }
            | Self::DacData { .. }
            | Self::DacEnd { .. }
            | Self::DacRegisterEvent { .. }
            | Self::LiveDacData { .. }
            | Self::OrderedLiveData { .. } => false,
        }
    }

    /// Validate local arguments and construct a frame for the transaction.
    pub fn to_frame(&self, transaction_id: u16) -> Result<Frame, Error> {
        let mut payload = [0; MAX_PAYLOAD_SIZE];
        let length = match *self {
            Self::Reset | Self::GetInfo => 0,
            Self::Ping(bytes) => {
                if bytes.len() > 32 {
                    return Err(Error::InvalidArgument);
                }
                payload[..bytes.len()].copy_from_slice(bytes);
                bytes.len()
            }
            Self::SetOutputVolume { attenuation, .. } => {
                if attenuation > 96 {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = attenuation;
                1
            }
            Self::SetChipClock {
                write_id,
                instance,
                hz,
                ..
            } => {
                if hz == 0 {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = write_id.get();
                payload[1] = instance;
                payload[2..6].copy_from_slice(&hz.to_le_bytes());
                6
            }
            Self::GetCapabilities { offset } => {
                payload[..2].copy_from_slice(&offset.to_le_bytes());
                2
            }
            Self::GetLiveDacCapabilities { offset } => {
                payload[..2].copy_from_slice(&offset.to_le_bytes());
                2
            }
            Self::GetOrderedLiveCapabilities | Self::GetOrderedLiveStatus => 0,
            Self::LiveDacData { instance, records } => {
                if records.is_empty() || records.len() > MAX_LIVE_DAC_RECORDS {
                    return Err(Error::InvalidArgument);
                }
                payload[..3].copy_from_slice(&[LIVE_DAC_PROFILE, WriteId::YM2612.get(), instance]);
                for (bytes, record) in payload[3..].as_chunks_mut::<5>().0.iter_mut().zip(records) {
                    bytes[..4].copy_from_slice(&record.wait_ticks.to_le_bytes());
                    bytes[4] = record.value;
                }
                3 + records.len() * 5
            }
            Self::OrderedLiveData { sequence, records } => {
                if records.is_empty() || records.len() > MAX_ORDERED_LIVE_RECORDS {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = ORDERED_LIVE_PROFILE;
                payload[1..5].copy_from_slice(&sequence.to_le_bytes());
                let mut length = 5;
                for record in records {
                    length += record.encode(&mut payload[length..])?;
                }
                length
            }
            Self::WriteRegisters {
                write_id,
                instance,
                page,
                writes,
            } => {
                validate_register_id(write_id)?;
                payload[0] = instance;
                payload[1] = page;
                2 + encode_writes(writes, &mut payload[2..], MAX_REGISTER_WRITES)?
            }
            Self::WriteBytes {
                write_id,
                instance,
                page,
                bytes,
            } => {
                validate_byte_id(write_id)?;
                if bytes.is_empty() || bytes.len() > MAX_REGISTER_BYTES {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = instance;
                payload[1] = page;
                payload[2..2 + bytes.len()].copy_from_slice(bytes);
                2 + bytes.len()
            }
            Self::AudioData {
                stream_id,
                instance,
                position,
                adpcm,
            } => {
                if !(1..=MAX_AUDIO_DATA_BYTES).contains(&adpcm.len()) {
                    return Err(Error::InvalidArgument);
                }
                position.checked_advance(adpcm.len())?;
                payload[0] = stream_id;
                payload[1] = instance;
                payload[2..6].copy_from_slice(&position.get().to_le_bytes());
                payload[6..6 + adpcm.len()].copy_from_slice(adpcm);
                6 + adpcm.len()
            }
            Self::AudioEvent {
                stream_id,
                instance,
                position,
                event,
            } => {
                payload[0] = stream_id;
                payload[1] = instance;
                payload[2..6].copy_from_slice(&position.get().to_le_bytes());
                match event {
                    AudioEvent::Registers {
                        write_id,
                        instance: target_instance,
                        page,
                        writes,
                    } => {
                        validate_register_id(write_id)?;
                        payload[6] = 0;
                        payload[7] = write_id.get();
                        payload[8] = target_instance;
                        payload[9] = page;
                        10 + encode_writes(writes, &mut payload[10..], MAX_REGISTER_EVENT_WRITES)?
                    }
                    AudioEvent::Clock { clock, divider } => {
                        payload[6] = 1;
                        payload[7..11].copy_from_slice(&(clock as u32).to_le_bytes());
                        payload[11..13].copy_from_slice(&(divider as u16).to_le_bytes());
                        13
                    }
                    AudioEvent::Pan(pan) => {
                        payload[6] = 2;
                        payload[7] = pan as u8;
                        8
                    }
                    AudioEvent::End(pair) => {
                        payload[6] = 3;
                        payload[7] = pair as u8;
                        8
                    }
                }
            }
            Self::AudioStart {
                stream_id,
                instance,
                clock,
                divider,
            } => {
                payload[0] = stream_id;
                payload[1] = instance;
                payload[2..6].copy_from_slice(&(clock as u32).to_le_bytes());
                payload[6..8].copy_from_slice(&(divider as u16).to_le_bytes());
                8
            }
            Self::AudioStatus {
                stream_id,
                instance,
            } => {
                payload[0] = stream_id;
                payload[1] = instance;
                2
            }
            Self::GetDacCapabilities => 0,
            Self::DacData {
                profile,
                write_id,
                instance,
                sequence,
                position,
                records,
            } => {
                if profile != DAC_PROFILE_YM2612
                    || write_id != WriteId::YM2612
                    || instance != 0
                    || records.is_empty()
                    || records.len() > MAX_DAC_RECORDS
                    || sequence.checked_add(records.len() as u32).is_none()
                    || position > u64::MAX / 1_000_000
                {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = profile;
                payload[1] = write_id.get();
                payload[2] = instance;
                payload[3..7].copy_from_slice(&sequence.to_le_bytes());
                payload[7..15].copy_from_slice(&position.to_le_bytes());
                let mut end_tick = position;
                for (bytes, record) in payload[15..].as_chunks_mut::<4>().0.iter_mut().zip(records)
                {
                    match record.kind {
                        DacRecordKind::Select if record.value & 0x7f != 0 => {
                            return Err(Error::InvalidArgument);
                        }
                        DacRecordKind::Wait if record.value != 0 || record.wait_ticks == 0 => {
                            return Err(Error::InvalidArgument);
                        }
                        _ => {}
                    }
                    end_tick = end_tick
                        .checked_add(u64::from(record.wait_ticks))
                        .filter(|tick| *tick <= u64::MAX / 1_000_000)
                        .ok_or(Error::InvalidArgument)?;
                    bytes[0] = record.kind as u8;
                    bytes[1] = record.value;
                    bytes[2..4].copy_from_slice(&record.wait_ticks.to_le_bytes());
                }
                15 + records.len() * 4
            }
            Self::DacStart {
                profile,
                minimum_lead_ticks,
            } => {
                if profile != DAC_PROFILE_YM2612 || minimum_lead_ticks == 0 {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = profile;
                payload[1..5].copy_from_slice(&minimum_lead_ticks.to_le_bytes());
                5
            }
            Self::DacEnd {
                profile,
                next_sequence,
                end_tick,
            } => {
                if profile != DAC_PROFILE_YM2612 || end_tick > u64::MAX / 1_000_000 {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = profile;
                payload[1..5].copy_from_slice(&next_sequence.to_le_bytes());
                payload[5..13].copy_from_slice(&end_tick.to_le_bytes());
                13
            }
            Self::DacStatus { profile } => {
                if profile != DAC_PROFILE_YM2612 {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = profile;
                1
            }
            Self::DacRegisterEvent {
                profile,
                tick,
                write_id,
                instance,
                page,
                writes,
            } => {
                if profile != DAC_PROFILE_YM2612
                    || tick > u64::MAX / 1_000_000
                    || writes.is_empty()
                    || writes.len() > MAX_DAC_EVENT_BYTES
                    || !valid_dac_event_target(write_id, instance, page, writes)
                {
                    return Err(Error::InvalidArgument);
                }
                payload[0] = profile;
                payload[1..9].copy_from_slice(&tick.to_le_bytes());
                payload[9] = write_id.get();
                payload[10] = instance;
                payload[11] = page;
                payload[12..12 + writes.len()].copy_from_slice(writes);
                12 + writes.len()
            }
        };
        let message_type = match self {
            Self::WriteRegisters { .. }
            | Self::WriteBytes { .. }
            | Self::DacData { .. }
            | Self::DacEnd { .. }
            | Self::DacRegisterEvent { .. } => MessageType::NoResponse,
            _ if self.expects_response() => MessageType::Request,
            _ => MessageType::NoResponse,
        };
        Frame::new(
            message_type,
            self.command_id(),
            transaction_id,
            &payload[..length],
        )
    }

    /// Validate and encode a frame including both zero delimiters.
    pub fn encode(&self, transaction_id: u16) -> Result<EncodedFrame, Error> {
        Ok(self.to_frame(transaction_id)?.encode())
    }

    /// Encode into a caller-provided buffer, returning the number of bytes.
    pub fn encode_into(&self, transaction_id: u16, output: &mut [u8]) -> Result<usize, Error> {
        self.to_frame(transaction_id)?.encode_into(output)
    }
}

/// Encode nonempty register pairs into an already-sized payload buffer.
fn encode_writes(
    writes: &[RegisterWrite],
    output: &mut [u8],
    maximum: usize,
) -> Result<usize, Error> {
    if writes.is_empty() || writes.len() > maximum {
        return Err(Error::InvalidArgument);
    }
    for (pair, write) in output.as_chunks_mut::<2>().0.iter_mut().zip(writes) {
        pair[0] = write.address;
        pair[1] = write.value;
    }
    Ok(writes.len() * 2)
}

/// Decoded ADPCM-positioned event with register pairs borrowed from a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestEvent<'a> {
    /// Ordered register writes to a target chip.
    Registers {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target register page.
        page: u8,
        /// Validated borrowed register pairs.
        writes: crate::types::RegisterWrites<'a>,
    },
    /// Change the ADPCM stream clock and divider.
    Clock {
        /// Input clock.
        clock: OkiClock,
        /// Clock divider.
        divider: Divider,
    },
    /// Change output-channel selection.
    Pan(Pan),
    /// End the stream using a state-dependent repeated ADPCM byte.
    End(ZeroPair),
}

/// Typed request view. Unknown commands preserve their command ID and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Request<'a> {
    /// Reset device state.
    Reset,
    /// Echo the borrowed payload.
    Ping(&'a [u8]),
    /// Request model and firmware information.
    GetInfo,
    /// Set output attenuation.
    SetOutputVolume {
        /// Attenuation value in `0..=96`.
        attenuation: u8,
    },
    /// Set a chip instance's input clock.
    SetChipClock {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Clock frequency in hertz.
        hz: u32,
    },
    /// Request a page of capabilities.
    GetCapabilities {
        /// Entry offset for pagination.
        offset: u16,
    },
    /// Immediate register writes.
    WriteRegisters {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target register page.
        page: u8,
        /// Validated borrowed register pairs.
        writes: crate::types::RegisterWrites<'a>,
    },
    /// Immediate raw chip bytes.
    WriteBytes {
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target page.
        page: u8,
        /// Borrowed raw bytes.
        bytes: &'a [u8],
    },
    /// ADPCM data for a stream.
    AudioData {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
        /// Absolute ADPCM byte position.
        position: BytePosition,
        /// Borrowed ADPCM data.
        adpcm: &'a [u8],
    },
    /// Event positioned on an ADPCM stream.
    AudioEvent {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
        /// Absolute ADPCM byte position.
        position: BytePosition,
        /// Decoded stream event.
        event: RequestEvent<'a>,
    },
    /// Start an ADPCM stream.
    AudioStart {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
        /// Input clock.
        clock: OkiClock,
        /// Clock divider.
        divider: Divider,
    },
    /// Request ADPCM stream status.
    AudioStatus {
        /// Audio stream identifier.
        stream_id: u8,
        /// Audio stream instance.
        instance: u8,
    },
    /// Request timed DAC profile capabilities.
    GetDacCapabilities,
    /// Timed DAC records.
    DacData {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Sequence number of the first record.
        sequence: u32,
        /// Starting position in profile ticks.
        position: u64,
        /// Validated borrowed records.
        records: DacRecords<'a>,
    },
    /// Start timed DAC playback.
    DacStart {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Minimum buffered lead in profile ticks.
        minimum_lead_ticks: u32,
    },
    /// Seal timed DAC playback.
    DacEnd {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Sequence number immediately after the final record.
        next_sequence: u32,
        /// End position in profile ticks.
        end_tick: u64,
    },
    /// Request timed DAC status.
    DacStatus {
        /// Timed DAC profile identifier.
        profile: u8,
    },
    /// Register or PSG writes positioned on the timed DAC clock.
    DacRegisterEvent {
        /// Timed DAC profile identifier.
        profile: u8,
        /// Absolute position in profile ticks.
        tick: u64,
        /// Target chip command ID.
        write_id: WriteId,
        /// Target chip instance.
        instance: u8,
        /// Target register page.
        page: u8,
        /// Borrowed raw write bytes.
        writes: &'a [u8],
    },
    /// Request a page of draft live-DAC capabilities.
    GetLiveDacCapabilities {
        /// Target entry offset.
        offset: u16,
    },
    /// Validated live-DAC records for a YM2612 instance.
    LiveDacData {
        /// Target instance.
        instance: u8,
        /// Borrowed DAC records.
        records: LiveDacRecords<'a>,
    },
    /// Request draft ordered-live capabilities.
    GetOrderedLiveCapabilities,
    /// Validated ordered-live records.
    OrderedLiveData {
        /// Frame sequence number.
        sequence: u32,
        /// Borrowed ordered records.
        records: OrderedLiveRecords<'a>,
    },
    /// Request an ordered-live diagnostic snapshot.
    GetOrderedLiveStatus,
    /// An unrecognized command, preserved without interpreting its payload.
    Unknown {
        /// Request message type.
        message_type: MessageType,
        /// Unrecognized command ID.
        command_id: u8,
        /// Transaction ID.
        transaction_id: u16,
        /// Borrowed payload bytes.
        payload: &'a [u8],
    },
}

impl<'a> Request<'a> {
    /// Decode and locally validate a request, borrowing data from `frame`.
    ///
    /// Response frames are rejected. Unrecognized request commands are
    /// preserved as [`Request::Unknown`].
    pub fn decode(frame: &'a Frame) -> Result<Self, Error> {
        let message_type = frame.message_type();
        let command_id = frame.command_id();
        let transaction_id = frame.transaction_id();
        let bytes = frame.payload();
        if message_type == MessageType::Response {
            return Err(Error::UnexpectedOpcode(command_id));
        }
        if message_type == MessageType::NoResponse && (0x30..=0xff).contains(&command_id) {
            let write_id = WriteId::try_from(command_id)?;
            if !write_id.supports_register_pairs() {
                if write_id.supports_raw_bytes() {
                    if bytes.len() < 3 {
                        return Err(Error::InvalidLength);
                    }
                    return Ok(Self::WriteBytes {
                        write_id,
                        instance: bytes[0],
                        page: bytes[1],
                        bytes: &bytes[2..],
                    });
                }
                return Ok(Self::Unknown {
                    message_type,
                    command_id,
                    transaction_id,
                    payload: bytes,
                });
            }
            if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
                return Err(Error::InvalidLength);
            }
            let writes = crate::types::RegisterWrites::decode(&bytes[2..], MAX_REGISTER_WRITES)?;
            return Ok(Self::WriteRegisters {
                write_id,
                instance: bytes[0],
                page: bytes[1],
                writes,
            });
        }
        if message_type == MessageType::Request && command_id >= 0x30 {
            return Err(Error::InvalidMessageType(message_type as u8));
        }
        let expected_type = match command_id {
            0x00..=0x02 | 0x05 | 0x13 | 0x18 | 0x1a | 0x1c | 0x1e | 0x21 | 0x23 => {
                Some(MessageType::Request)
            }
            0x03..=0x04 => None,
            0x10..=0x12 | 0x19 | 0x1b | 0x1d | 0x1f | 0x22 => Some(MessageType::NoResponse),
            _ => None,
        };
        if expected_type.is_some_and(|expected| message_type != expected) {
            return Err(Error::InvalidMessageType(message_type as u8));
        }
        match (message_type, command_id) {
            (MessageType::Request, 0x00) => {
                require_length(bytes, 0)?;
                Ok(Self::Reset)
            }
            (MessageType::Request, 0x01) => {
                if bytes.len() > 32 {
                    return Err(Error::InvalidLength);
                }
                Ok(Self::Ping(bytes))
            }
            (MessageType::Request, 0x02) => {
                require_length(bytes, 0)?;
                Ok(Self::GetInfo)
            }
            (MessageType::NoResponse | MessageType::Request, 0x03) => {
                require_length(bytes, 1)?;
                if bytes[0] > 96 {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::SetOutputVolume {
                    attenuation: bytes[0],
                })
            }
            (MessageType::NoResponse | MessageType::Request, 0x04) => {
                require_length(bytes, 6)?;
                let write_id = WriteId::try_from(bytes[0])?;
                let hz = read_u32(&bytes[2..]);
                if hz == 0 {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::SetChipClock {
                    write_id,
                    instance: bytes[1],
                    hz,
                })
            }
            (MessageType::Request, 0x05) => {
                require_length(bytes, 2)?;
                Ok(Self::GetCapabilities {
                    offset: u16::from_le_bytes([bytes[0], bytes[1]]),
                })
            }
            (MessageType::NoResponse, 0x10) => {
                if !(7..=MAX_PAYLOAD_SIZE).contains(&bytes.len()) {
                    return Err(Error::InvalidLength);
                }
                let position = BytePosition::new(read_u32(&bytes[2..]));
                let adpcm = &bytes[6..];
                position.checked_advance(adpcm.len())?;
                Ok(Self::AudioData {
                    stream_id: bytes[0],
                    instance: bytes[1],
                    position,
                    adpcm,
                })
            }
            (MessageType::NoResponse, 0x11) => {
                if bytes.len() < 8 {
                    return Err(Error::InvalidLength);
                }
                let stream_id = bytes[0];
                let instance = bytes[1];
                let position = BytePosition::new(read_u32(&bytes[2..]));
                let data = &bytes[7..];
                let event = match bytes[6] {
                    0 => {
                        if data.len() < 5 || data.len().is_multiple_of(2) {
                            return Err(Error::InvalidLength);
                        }
                        RequestEvent::Registers {
                            write_id: {
                                let write_id = WriteId::try_from(data[0])?;
                                validate_register_id(write_id)?;
                                write_id
                            },
                            instance: data[1],
                            page: data[2],
                            writes: crate::types::RegisterWrites::decode(
                                &data[3..],
                                MAX_REGISTER_EVENT_WRITES,
                            )?,
                        }
                    }
                    1 => {
                        require_length(data, 6)?;
                        RequestEvent::Clock {
                            clock: OkiClock::try_from(read_u32(data))?,
                            divider: Divider::try_from(u16::from_le_bytes([data[4], data[5]]))?,
                        }
                    }
                    2 => {
                        require_length(data, 1)?;
                        RequestEvent::Pan(Pan::try_from(data[0])?)
                    }
                    3 => {
                        require_length(data, 1)?;
                        RequestEvent::End(ZeroPair::try_from(data[0])?)
                    }
                    _ => return Err(Error::InvalidArgument),
                };
                Ok(Self::AudioEvent {
                    stream_id,
                    instance,
                    position,
                    event,
                })
            }
            (MessageType::NoResponse, 0x12) => {
                require_length(bytes, 8)?;
                Ok(Self::AudioStart {
                    stream_id: bytes[0],
                    instance: bytes[1],
                    clock: OkiClock::try_from(read_u32(&bytes[2..]))?,
                    divider: Divider::try_from(u16::from_le_bytes([bytes[6], bytes[7]]))?,
                })
            }
            (MessageType::Request, 0x13) => {
                require_length(bytes, 2)?;
                Ok(Self::AudioStatus {
                    stream_id: bytes[0],
                    instance: bytes[1],
                })
            }
            (MessageType::Request, 0x18) => {
                require_length(bytes, 0)?;
                Ok(Self::GetDacCapabilities)
            }
            (MessageType::NoResponse, 0x19) => {
                if !(19..=MAX_PAYLOAD_SIZE).contains(&bytes.len())
                    || !(bytes.len() - 15).is_multiple_of(4)
                {
                    return Err(Error::InvalidLength);
                }
                let profile = bytes[0];
                let write_id = WriteId::try_from(bytes[1])?;
                let instance = bytes[2];
                let sequence = read_u32(&bytes[3..]);
                let position = read_u64(&bytes[7..]);
                let records = DacRecords::decode(&bytes[15..])?;
                validate_dac_data(profile, write_id, instance, sequence, position, records)?;
                Ok(Self::DacData {
                    profile,
                    write_id,
                    instance,
                    sequence,
                    position,
                    records,
                })
            }
            (MessageType::Request, 0x1a) => {
                require_length(bytes, 5)?;
                let profile = bytes[0];
                let minimum_lead_ticks = read_u32(&bytes[1..]);
                if profile != DAC_PROFILE_YM2612 || minimum_lead_ticks == 0 {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::DacStart {
                    profile,
                    minimum_lead_ticks,
                })
            }
            (MessageType::NoResponse, 0x1b) => {
                require_length(bytes, 13)?;
                let profile = bytes[0];
                let next_sequence = read_u32(&bytes[1..]);
                let end_tick = read_u64(&bytes[5..]);
                if profile != DAC_PROFILE_YM2612 || end_tick > u64::MAX / 1_000_000 {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::DacEnd {
                    profile,
                    next_sequence,
                    end_tick,
                })
            }
            (MessageType::Request, 0x1c) => {
                require_length(bytes, 1)?;
                if bytes[0] != DAC_PROFILE_YM2612 {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::DacStatus { profile: bytes[0] })
            }
            (MessageType::NoResponse, 0x1d) => {
                if !(13..=MAX_PAYLOAD_SIZE).contains(&bytes.len()) {
                    return Err(Error::InvalidLength);
                }
                let profile = bytes[0];
                let tick = read_u64(&bytes[1..]);
                let write_id = WriteId::try_from(bytes[9])?;
                let instance = bytes[10];
                let page = bytes[11];
                let writes = &bytes[12..];
                if profile != DAC_PROFILE_YM2612
                    || tick > u64::MAX / 1_000_000
                    || !valid_dac_event_target(write_id, instance, page, writes)
                {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::DacRegisterEvent {
                    profile,
                    tick,
                    write_id,
                    instance,
                    page,
                    writes,
                })
            }
            (MessageType::Request, 0x1e) => {
                require_length(bytes, 2)?;
                Ok(Self::GetLiveDacCapabilities {
                    offset: u16::from_le_bytes([bytes[0], bytes[1]]),
                })
            }
            (MessageType::NoResponse, 0x1f) => {
                if bytes.len() < 3 {
                    return Err(Error::InvalidLength);
                }
                if bytes[0] != LIVE_DAC_PROFILE || bytes[1] != WriteId::YM2612.get() {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::LiveDacData {
                    instance: bytes[2],
                    records: LiveDacRecords::decode(&bytes[3..])?,
                })
            }
            (MessageType::Request, 0x21) => {
                require_length(bytes, 0)?;
                Ok(Self::GetOrderedLiveCapabilities)
            }
            (MessageType::NoResponse, 0x22) => {
                if bytes.len() < 5 {
                    return Err(Error::InvalidLength);
                }
                if bytes[0] != ORDERED_LIVE_PROFILE {
                    return Err(Error::InvalidArgument);
                }
                Ok(Self::OrderedLiveData {
                    sequence: read_u32(&bytes[1..5]),
                    records: OrderedLiveRecords::decode(&bytes[5..])?,
                })
            }
            (MessageType::Request, 0x23) => {
                require_length(bytes, 0)?;
                Ok(Self::GetOrderedLiveStatus)
            }
            _ => Ok(Self::Unknown {
                message_type,
                command_id,
                transaction_id,
                payload: bytes,
            }),
        }
    }
}

/// Reject a message body whose length differs from the expected fixed size.
fn require_length(bytes: &[u8], length: usize) -> Result<(), Error> {
    if bytes.len() != length {
        return Err(Error::InvalidLength);
    }
    Ok(())
}

/// Read a little-endian word after the caller has checked its length.
fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Read a little-endian double word after the caller has checked its length.
fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Validate the profile target and check sequence and tick-position overflow.
fn validate_dac_data(
    profile: u8,
    write_id: WriteId,
    instance: u8,
    sequence: u32,
    position: u64,
    records: DacRecords<'_>,
) -> Result<(), Error> {
    if profile != DAC_PROFILE_YM2612
        || write_id != WriteId::YM2612
        || instance != 0
        || sequence.checked_add(records.len() as u32).is_none()
        || position > u64::MAX / 1_000_000
    {
        return Err(Error::InvalidArgument);
    }
    let mut end_tick = position;
    for record in records.iter() {
        end_tick = end_tick
            .checked_add(u64::from(record.wait_ticks))
            .filter(|tick| *tick <= u64::MAX / 1_000_000)
            .ok_or(Error::InvalidArgument)?;
    }
    Ok(())
}

/// Accept supported timed-event targets while reserving YM2612 DAC registers.
fn valid_dac_event_target(write_id: WriteId, instance: u8, page: u8, writes: &[u8]) -> bool {
    match (write_id.get(), instance, page) {
        (0x52, 0, 0) | (0x53, 0, 1) if writes.len() >= 2 && writes.len().is_multiple_of(2) => {
            write_id.get() != 0x52
                || writes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|pair| !matches!(pair[0], 0x2a | 0x2b))
        }
        (0x50, 0, 0) | (0x30, 1, 0) => !writes.is_empty(),
        _ => false,
    }
}

/// Reject targets that cannot accept register pairs.
fn validate_register_id(write_id: WriteId) -> Result<(), Error> {
    if write_id.supports_register_pairs() {
        Ok(())
    } else {
        Err(Error::InvalidArgument)
    }
}

/// Reject targets that cannot accept raw chip bytes.
fn validate_byte_id(write_id: WriteId) -> Result<(), Error> {
    if write_id.supports_raw_bytes() {
        Ok(())
    } else {
        Err(Error::InvalidArgument)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_psg_write_bytes_round_trip() {
        for (write_id, instance, bytes) in [
            (WriteId::SN76489, 0, &[0x90, 0x20, 0xff][..]),
            (WriteId::SN76489_SECOND, 1, &[0x9f, 0x10][..]),
        ] {
            let frame = Command::WriteBytes {
                write_id,
                instance,
                page: 0,
                bytes,
            }
            .to_frame(7)
            .unwrap();
            assert_eq!(frame.message_type(), MessageType::NoResponse);
            assert_eq!(frame.command_id(), write_id.get());
            assert_eq!(
                Request::decode(&frame).unwrap(),
                Request::WriteBytes {
                    write_id,
                    instance,
                    page: 0,
                    bytes,
                }
            );
        }
    }

    #[test]
    fn raw_write_bytes_reject_invalid_targets_and_empty_data() {
        assert!(
            Command::WriteBytes {
                write_id: WriteId::YM2151,
                instance: 0,
                page: 0,
                bytes: &[0x80],
            }
            .to_frame(0)
            .is_err()
        );
        assert!(
            Command::WriteBytes {
                write_id: WriteId::SN76489,
                instance: 0,
                page: 0,
                bytes: &[],
            }
            .to_frame(0)
            .is_err()
        );
    }

    #[test]
    fn dac_profile_commands_round_trip() {
        let records = [
            DacRecord {
                kind: DacRecordKind::Wait,
                value: 0,
                wait_ticks: 44_100,
            },
            DacRecord {
                kind: DacRecordKind::Dac,
                value: 0x80,
                wait_ticks: 0,
            },
        ];
        let frame = Command::DacData {
            profile: DAC_PROFILE_YM2612,
            write_id: WriteId::YM2612,
            instance: 0,
            sequence: 0,
            position: 0,
            records: &records,
        }
        .to_frame(7)
        .unwrap();
        assert_eq!(frame.command_id(), 0x19);
        assert_eq!(frame.message_type(), MessageType::NoResponse);
        let Request::DacData {
            profile,
            write_id,
            instance,
            sequence,
            position,
            records: decoded,
        } = Request::decode(&frame).unwrap()
        else {
            panic!("expected DAC_DATA");
        };
        assert_eq!(profile, DAC_PROFILE_YM2612);
        assert_eq!(write_id, WriteId::YM2612);
        assert_eq!(instance, 0);
        assert_eq!(sequence, 0);
        assert_eq!(position, 0);
        assert!(decoded.iter().eq(records));

        for command in [
            Command::GetDacCapabilities,
            Command::DacStart {
                profile: DAC_PROFILE_YM2612,
                minimum_lead_ticks: 3_528,
            },
            Command::DacStatus {
                profile: DAC_PROFILE_YM2612,
            },
        ] {
            let frame = command.to_frame(8).unwrap();
            assert_eq!(frame.message_type(), MessageType::Request);
            assert!(command.expects_response());
            assert!(matches!(
                Request::decode(&frame).unwrap(),
                Request::GetDacCapabilities | Request::DacStart { .. } | Request::DacStatus { .. }
            ));
        }

        let frame = Command::DacEnd {
            profile: DAC_PROFILE_YM2612,
            next_sequence: 2,
            end_tick: 44_100,
        }
        .to_frame(9)
        .unwrap();
        assert_eq!(frame.message_type(), MessageType::NoResponse);
        assert!(matches!(
            Request::decode(&frame).unwrap(),
            Request::DacEnd {
                next_sequence: 2,
                end_tick: 44_100,
                ..
            }
        ));
    }

    #[test]
    fn dac_register_events_validate_target_and_dac_control_registers() {
        let writes = [0x22, 0x33, 0x24, 0x55];
        let frame = Command::DacRegisterEvent {
            profile: DAC_PROFILE_YM2612,
            tick: 44_100,
            write_id: WriteId::YM2612,
            instance: 0,
            page: 0,
            writes: &writes,
        }
        .to_frame(1)
        .unwrap();
        assert!(matches!(
            Request::decode(&frame).unwrap(),
            Request::DacRegisterEvent {
                tick: 44_100,
                writes: decoded,
                ..
            } if decoded == writes
        ));

        for writes in [&[0x2a, 0x01][..], &[0x2b, 0x80][..], &[0x20][..]] {
            assert!(
                Command::DacRegisterEvent {
                    profile: DAC_PROFILE_YM2612,
                    tick: 0,
                    write_id: WriteId::YM2612,
                    instance: 0,
                    page: 0,
                    writes,
                }
                .to_frame(0)
                .is_err()
            );
        }
    }
}
