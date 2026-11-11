//! Borrowed response views, device capabilities, and audio/DAC status.
//!
//! Correlation checks compare request and response identifiers; a completed
//! response is not a readback or proof of a physical chip's state.

use crate::{EncodedFrame, Error, Frame, MAX_PAYLOAD_SIZE, MessageType, StatusFlags};

/// Completion status carried by a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The command completed.
    Complete,
    /// The device rejected the command.
    Rejected,
}

/// Model and firmware strings borrowed from a response payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo<'a> {
    /// Device model identifier.
    pub model: &'a str,
    /// Firmware identifier.
    pub firmware: &'a str,
}

/// Clock configuration advertised for a chip instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClockMode {
    /// The clock is unknown.
    Unknown = 0,
    /// The chip uses the advertised fixed clock.
    Fixed = 1,
    /// The clock can be configured.
    Variable = 2,
}

impl TryFrom<u8> for ClockMode {
    type Error = Error;

    /// Decode the advertised clock mode.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Fixed),
            2 => Ok(Self::Variable),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// One advertised target chip, instance, page, and clock configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityEntry {
    /// Target command ID.
    pub write_id: u8,
    /// Chip instance.
    pub instance: u8,
    /// Register page.
    pub page: u8,
    /// Device-advertised feature bits.
    pub features: u8,
    /// Clock configuration mode.
    pub clock_mode: ClockMode,
    /// Fixed clock frequency in hertz, or zero when not fixed.
    pub fixed_clock_hz: u32,
}

/// Wire size of one capability entry, in bytes.
pub const CAPABILITY_ENTRY_SIZE: usize = 9;
/// Maximum capability entries in a single response.
pub const MAX_CAPABILITY_ENTRIES: usize = (MAX_PAYLOAD_SIZE - 14) / CAPABILITY_ENTRY_SIZE;
/// Control feature bit indicating output attenuation is supported.
pub const CONTROL_FEATURE_OUTPUT_VOLUME: u16 = 1 << 0;
/// Control feature bit indicating timed DAC profile discovery is supported.
pub const CONTROL_FEATURE_DAC_PROFILE: u16 = 1 << 1;
const CONFIGURABLE_CLOCK_FEATURE: u8 = 1 << 2;

/// One page of device capabilities, borrowing its encoded entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities<'a> {
    /// Maximum payload size advertised by the device, in bytes.
    pub max_payload: u16,
    /// Advertised audio-data capacity, in bytes.
    pub data_capacity: u32,
    /// Advertised audio-event capacity.
    pub event_capacity: u16,
    /// Device control feature bits.
    pub control_features: u16,
    /// Offset for the next page, or `u16::MAX` for the final page.
    pub next_offset: u16,
    entries: &'a [u8],
}

impl<'a> Capabilities<'a> {
    /// Validate and borrow encoded capability entries.
    ///
    /// Rejects inconsistent clock metadata, duplicate target pages, and
    /// payloads exceeding the protocol limits.
    pub fn new(
        max_payload: u16,
        data_capacity: u32,
        event_capacity: u16,
        control_features: u16,
        next_offset: u16,
        entries: &'a [u8],
    ) -> Result<Self, Error> {
        if max_payload as usize > MAX_PAYLOAD_SIZE
            || !entries.len().is_multiple_of(CAPABILITY_ENTRY_SIZE)
            || entries.len() > MAX_CAPABILITY_ENTRIES * CAPABILITY_ENTRY_SIZE
        {
            return Err(Error::InvalidArgument);
        }
        validate_capability_entries(entries)?;
        Ok(Self {
            max_payload,
            data_capacity,
            event_capacity,
            control_features,
            next_offset,
            entries,
        })
    }

    /// Iterate over the validated entries in wire order.
    pub fn entries(self) -> impl ExactSizeIterator<Item = CapabilityEntry> + 'a {
        self.entries
            .as_chunks::<CAPABILITY_ENTRY_SIZE>()
            .0
            .iter()
            .map(|entry| decode_capability_entry(entry).expect("validated capability entry"))
    }
}

/// Decode one entry and check its clock mode against the advertised flags.
fn decode_capability_entry(entry: &[u8]) -> Result<CapabilityEntry, Error> {
    if entry.len() != CAPABILITY_ENTRY_SIZE {
        return Err(Error::InvalidLength);
    }
    let clock_mode = ClockMode::try_from(entry[4])?;
    let fixed_clock_hz = u32::from_le_bytes([entry[5], entry[6], entry[7], entry[8]]);
    let configurable_clock = entry[3] & CONFIGURABLE_CLOCK_FEATURE != 0;
    match clock_mode {
        ClockMode::Unknown if fixed_clock_hz == 0 => {}
        ClockMode::Fixed if fixed_clock_hz != 0 && !configurable_clock => {}
        ClockMode::Variable if fixed_clock_hz == 0 && configurable_clock => {}
        _ => return Err(Error::InvalidArgument),
    }
    Ok(CapabilityEntry {
        write_id: entry[0],
        instance: entry[1],
        page: entry[2],
        features: entry[3],
        clock_mode,
        fixed_clock_hz,
    })
}

/// Reject duplicate pages and inconsistent clocks for the same chip instance.
fn validate_capability_entries(entries: &[u8]) -> Result<(), Error> {
    for (index, entry) in entries
        .as_chunks::<CAPABILITY_ENTRY_SIZE>()
        .0
        .iter()
        .enumerate()
    {
        let current = decode_capability_entry(entry)?;
        for previous in entries[..index * CAPABILITY_ENTRY_SIZE]
            .as_chunks::<CAPABILITY_ENTRY_SIZE>()
            .0
        {
            let previous = decode_capability_entry(previous)?;
            if current.write_id == previous.write_id
                && current.instance == previous.instance
                && (current.page == previous.page
                    || current.clock_mode != previous.clock_mode
                    || current.fixed_clock_hz != previous.fixed_clock_hz)
            {
                return Err(Error::InvalidArgument);
            }
        }
    }
    Ok(())
}

/// Device-reported status of an ADPCM stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStatus {
    /// Accepted ADPCM byte position.
    pub accepted: u32,
    /// Played ADPCM byte position.
    pub played: u32,
    /// Pending ADPCM byte count.
    pub pending: u32,
    /// Reported underflow count.
    pub underflows: u32,
    /// Reported overflow count.
    pub overflows: u32,
    /// Reported rejection count.
    pub rejected: u32,
    /// Maximum reported pending byte count.
    pub max_pending: u32,
    /// Device state and diagnostic flags.
    pub flags: StatusFlags,
    /// Number of late events.
    pub late_events: u32,
    /// Maximum reported event lag.
    pub max_event_lag: u32,
}

impl AudioStatus {
    /// Decode the fixed-width audio status counters and flags.
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 40 {
            return Err(Error::InvalidLength);
        }
        let mut values = [0; 10];
        for (value, bytes) in values.iter_mut().zip(bytes.as_chunks::<4>().0.iter()) {
            *value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let [
            accepted,
            played,
            pending,
            underflows,
            overflows,
            rejected,
            max_pending,
            flags,
            late_events,
            max_event_lag,
        ] = values;
        Ok(Self {
            accepted,
            played,
            pending,
            underflows,
            overflows,
            rejected,
            max_pending,
            flags: StatusFlags::from_bits_retain(flags),
            late_events,
            max_event_lag,
        })
    }
}

/// Device-advertised limits for an optional timed DAC profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DacCapabilities {
    /// Profile identifier.
    pub profile: u8,
    /// API specification revision.
    pub revision: u32,
    /// Profile clock frequency in ticks per second.
    pub ticks_per_second: u32,
    /// DAC record queue capacity.
    pub record_capacity: u32,
    /// Positioned-event queue capacity.
    pub event_capacity: u16,
    /// Maximum DAC records accepted per request.
    pub max_records: u8,
    /// Encoded size of one DAC record in bytes.
    pub record_bytes: u8,
    /// Maximum DAC lateness in microseconds advertised by the device.
    pub max_dac_late_us: u32,
}

impl DacCapabilities {
    /// Decode the fixed-width timed-DAC limits and reject zero capacities.
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 21 {
            return Err(Error::InvalidLength);
        }
        let capabilities = Self {
            profile: bytes[0],
            revision: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
            ticks_per_second: u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
            record_capacity: u32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]),
            event_capacity: u16::from_le_bytes([bytes[13], bytes[14]]),
            max_records: bytes[15],
            record_bytes: bytes[16],
            max_dac_late_us: u32::from_le_bytes([bytes[17], bytes[18], bytes[19], bytes[20]]),
        };
        if capabilities.ticks_per_second == 0
            || capabilities.record_capacity == 0
            || capabilities.event_capacity == 0
            || capabilities.max_records == 0
            || capabilities.record_bytes == 0
        {
            return Err(Error::InvalidArgument);
        }
        Ok(capabilities)
    }
}

/// Device-reported timed DAC queue, state, and timing counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DacStatus {
    /// Profile identifier.
    pub profile: u8,
    /// Device-defined profile state.
    pub state: u8,
    /// Device-defined fault code.
    pub fault: u8,
    /// Whether the stream has been sealed.
    pub sealed: bool,
    /// Number of accepted DAC records.
    pub accepted: u32,
    /// Number of consumed DAC records.
    pub consumed: u32,
    /// Number of queued DAC records.
    pub queued: u32,
    /// Available DAC record capacity.
    pub free: u32,
    /// Number of queued positioned events.
    pub event_queued: u32,
    /// Number of consumed positioned events.
    pub event_consumed: u32,
    /// Reported underflow count.
    pub underflows: u32,
    /// Reported overflow count.
    pub overflows: u32,
    /// Reported rejection count.
    pub rejected: u32,
    /// Number of late DAC operations.
    pub late_dac: u32,
    /// Maximum observed lateness in microseconds.
    pub max_late_us: u32,
    /// Accepted position in profile ticks.
    pub accepted_tick: u64,
    /// Played position in profile ticks.
    pub played_tick: u64,
    /// Executed position in profile ticks.
    pub executed_tick: u64,
}

impl DacStatus {
    /// Decode the fixed-width timed-DAC status and validate the sealed flag.
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 72 || bytes[3] > 1 {
            return Err(Error::InvalidLength);
        }
        let mut values = [0; 11];
        for (value, bytes) in values
            .iter_mut()
            .zip(bytes[4..48].as_chunks::<4>().0.iter())
        {
            *value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        Ok(Self {
            profile: bytes[0],
            state: bytes[1],
            fault: bytes[2],
            sealed: bytes[3] != 0,
            accepted: values[0],
            consumed: values[1],
            queued: values[2],
            free: values[3],
            event_queued: values[4],
            event_consumed: values[5],
            underflows: values[6],
            overflows: values[7],
            rejected: values[8],
            late_dac: values[9],
            max_late_us: values[10],
            accepted_tick: u64::from_le_bytes([
                bytes[48], bytes[49], bytes[50], bytes[51], bytes[52], bytes[53], bytes[54],
                bytes[55],
            ]),
            played_tick: u64::from_le_bytes([
                bytes[56], bytes[57], bytes[58], bytes[59], bytes[60], bytes[61], bytes[62],
                bytes[63],
            ]),
            executed_tick: u64::from_le_bytes([
                bytes[64], bytes[65], bytes[66], bytes[67], bytes[68], bytes[69], bytes[70],
                bytes[71],
            ]),
        })
    }
}

/// Typed response body; borrowed data remains tied to its source frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reply<'a> {
    /// Successful command with no reply payload.
    Complete,
    /// Rejected command with no reply payload.
    Rejected,
    /// Echoed ping bytes.
    Ping(&'a [u8]),
    /// Device model and firmware.
    Info(DeviceInfo<'a>),
    /// A page of device capabilities.
    Capabilities(Capabilities<'a>),
    /// ADPCM stream status.
    AudioStatus(AudioStatus),
    /// Timed DAC capabilities.
    DacCapabilities(DacCapabilities),
    /// Timed DAC status.
    DacStatus(DacStatus),
    /// Unrecognized command response payload.
    Unknown(&'a [u8]),
}

/// Typed response. Parsed data borrows the frame payload; outgoing data borrows caller data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response<'a> {
    /// Command ID of the corresponding request.
    pub command_id: u8,
    /// Transaction ID of the corresponding request.
    pub transaction_id: u16,
    /// Completion or rejection status.
    pub status: Status,
    /// Typed response payload.
    pub reply: Reply<'a>,
}

impl<'a> Response<'a> {
    /// Builds a reply with the request's command ID and transaction ID.
    pub fn for_request(request: &Frame, reply: Reply<'a>) -> Result<Self, Error> {
        let response = Self {
            command_id: request.command_id(),
            transaction_id: request.transaction_id(),
            status: if matches!(reply, Reply::Rejected) {
                Status::Rejected
            } else {
                Status::Complete
            },
            reply,
        };
        response.matches_request(request)?;
        response.to_frame()?;
        Ok(response)
    }

    /// Encodes a locally valid response. A completion is not hardware readback.
    pub fn to_frame(&self) -> Result<Frame, Error> {
        if (self.status == Status::Rejected) != matches!(self.reply, Reply::Rejected) {
            return Err(Error::InvalidArgument);
        }
        let mut payload = [0; MAX_PAYLOAD_SIZE];
        let length = if self.status == Status::Rejected {
            payload[0] = 1;
            1
        } else {
            payload[0] = 0;
            match self.reply {
                Reply::Complete | Reply::Rejected => 1,
                Reply::Ping(bytes) | Reply::Unknown(bytes) => {
                    if bytes.len() > MAX_PAYLOAD_SIZE - 1 {
                        return Err(Error::PayloadTooLong {
                            length: bytes.len().saturating_add(1),
                        });
                    }
                    payload[1..1 + bytes.len()].copy_from_slice(bytes);
                    1 + bytes.len()
                }
                Reply::Info(info) => {
                    if info.model.len() > u8::MAX as usize
                        || info.firmware.len() > u8::MAX as usize
                        || info.model.contains('\0')
                        || info.firmware.contains('\0')
                    {
                        return Err(Error::InvalidArgument);
                    }
                    let length = 3 + info.model.len() + info.firmware.len();
                    if length > MAX_PAYLOAD_SIZE {
                        return Err(Error::PayloadTooLong { length });
                    }
                    payload[1] = info.model.len() as u8;
                    let model_end = 2 + info.model.len();
                    payload[2..model_end].copy_from_slice(info.model.as_bytes());
                    payload[model_end] = info.firmware.len() as u8;
                    payload[model_end + 1..length].copy_from_slice(info.firmware.as_bytes());
                    length
                }
                Reply::Capabilities(capabilities) => {
                    let count = capabilities.entries.len() / CAPABILITY_ENTRY_SIZE;
                    if count > MAX_CAPABILITY_ENTRIES
                        || capabilities.max_payload as usize > MAX_PAYLOAD_SIZE
                        || validate_capability_entries(capabilities.entries).is_err()
                    {
                        return Err(Error::InvalidArgument);
                    }
                    payload[1..3].copy_from_slice(&capabilities.max_payload.to_le_bytes());
                    payload[3..7].copy_from_slice(&capabilities.data_capacity.to_le_bytes());
                    payload[7..9].copy_from_slice(&capabilities.event_capacity.to_le_bytes());
                    payload[9..11].copy_from_slice(&capabilities.control_features.to_le_bytes());
                    payload[11..13].copy_from_slice(&capabilities.next_offset.to_le_bytes());
                    payload[13] = count as u8;
                    payload[14..14 + capabilities.entries.len()]
                        .copy_from_slice(capabilities.entries);
                    14 + capabilities.entries.len()
                }
                Reply::AudioStatus(status) => {
                    let values = [
                        status.accepted,
                        status.played,
                        status.pending,
                        status.underflows,
                        status.overflows,
                        status.rejected,
                        status.max_pending,
                        status.flags.bits(),
                        status.late_events,
                        status.max_event_lag,
                    ];
                    for (bytes, value) in
                        payload[1..41].as_chunks_mut::<4>().0.iter_mut().zip(values)
                    {
                        bytes.copy_from_slice(&value.to_le_bytes());
                    }
                    41
                }
                Reply::DacCapabilities(capabilities) => {
                    payload[1] = capabilities.profile;
                    payload[2..6].copy_from_slice(&capabilities.revision.to_le_bytes());
                    payload[6..10].copy_from_slice(&capabilities.ticks_per_second.to_le_bytes());
                    payload[10..14].copy_from_slice(&capabilities.record_capacity.to_le_bytes());
                    payload[14..16].copy_from_slice(&capabilities.event_capacity.to_le_bytes());
                    payload[16] = capabilities.max_records;
                    payload[17] = capabilities.record_bytes;
                    payload[18..22].copy_from_slice(&capabilities.max_dac_late_us.to_le_bytes());
                    22
                }
                Reply::DacStatus(status) => {
                    payload[1] = status.profile;
                    payload[2] = status.state;
                    payload[3] = status.fault;
                    payload[4] = u8::from(status.sealed);
                    let values = [
                        status.accepted,
                        status.consumed,
                        status.queued,
                        status.free,
                        status.event_queued,
                        status.event_consumed,
                        status.underflows,
                        status.overflows,
                        status.rejected,
                        status.late_dac,
                        status.max_late_us,
                    ];
                    for (bytes, value) in
                        payload[5..49].as_chunks_mut::<4>().0.iter_mut().zip(values)
                    {
                        bytes.copy_from_slice(&value.to_le_bytes());
                    }
                    for (bytes, value) in payload[49..73].as_chunks_mut::<8>().0.iter_mut().zip([
                        status.accepted_tick,
                        status.played_tick,
                        status.executed_tick,
                    ]) {
                        bytes.copy_from_slice(&value.to_le_bytes());
                    }
                    73
                }
            }
        };
        let frame = Frame::new(
            MessageType::Response,
            self.command_id,
            self.transaction_id,
            &payload[..length],
        )?;
        if Response::decode(&frame)? != *self {
            return Err(Error::InvalidArgument);
        }
        Ok(frame)
    }

    /// Validate and encode a response including both zero delimiters.
    pub fn encode(&self) -> Result<EncodedFrame, Error> {
        Ok(self.to_frame()?.encode())
    }

    /// Encode into a caller-provided buffer, returning the number of bytes.
    pub fn encode_into(&self, output: &mut [u8]) -> Result<usize, Error> {
        self.to_frame()?.encode_into(output)
    }

    /// Decode and locally validate a response, borrowing from `frame`.
    pub fn decode(frame: &'a Frame) -> Result<Self, Error> {
        if frame.message_type() != MessageType::Response {
            return Err(Error::UnexpectedOpcode(frame.command_id()));
        }
        let (&status, body) = frame.payload().split_first().ok_or(Error::InvalidLength)?;
        let status = match status {
            0 => Status::Complete,
            1 => Status::Rejected,
            value => return Err(Error::InvalidStatus(value)),
        };
        let reply = if status == Status::Rejected {
            if !body.is_empty() {
                return Err(Error::InvalidLength);
            }
            Reply::Rejected
        } else {
            match frame.command_id() {
                0x00 | 0x03 | 0x04 | 0x1a => {
                    if !body.is_empty() {
                        return Err(Error::InvalidLength);
                    }
                    Reply::Complete
                }
                0x01 => {
                    if body.len() > 32 {
                        return Err(Error::InvalidLength);
                    }
                    Reply::Ping(body)
                }
                0x02 => Reply::Info(decode_info(body)?),
                0x05 => Reply::Capabilities(decode_capabilities(body)?),
                0x13 => Reply::AudioStatus(AudioStatus::decode(body)?),
                0x18 => Reply::DacCapabilities(DacCapabilities::decode(body)?),
                0x1c => Reply::DacStatus(DacStatus::decode(body)?),
                _ => Reply::Unknown(body),
            }
        };
        Ok(Self {
            command_id: frame.command_id(),
            transaction_id: frame.transaction_id(),
            status,
            reply,
        })
    }

    /// Check request type, command and transaction IDs, and any ping echo.
    ///
    /// This does not verify physical chip state.
    pub fn matches_request(&self, request: &Frame) -> Result<(), Error> {
        if request.message_type() != MessageType::Request
            || self.command_id != request.command_id()
            || self.transaction_id != request.transaction_id()
        {
            return Err(Error::ResponseMismatch);
        }
        if matches!(self.reply, Reply::Ping(echo) if echo != request.payload()) {
            return Err(Error::ResponseMismatch);
        }
        Ok(())
    }
}

/// Decode length-prefixed UTF-8 device model and firmware names.
fn decode_info(body: &[u8]) -> Result<DeviceInfo<'_>, Error> {
    let (&model_length, rest) = body.split_first().ok_or(Error::InvalidLength)?;
    let (model, rest) = rest
        .split_at_checked(model_length as usize)
        .ok_or(Error::InvalidLength)?;
    let (&firmware_length, firmware) = rest.split_first().ok_or(Error::InvalidLength)?;
    if firmware.len() != firmware_length as usize || model.contains(&0) || firmware.contains(&0) {
        return Err(Error::InvalidLength);
    }
    Ok(DeviceInfo {
        model: core::str::from_utf8(model).map_err(|_| Error::InvalidUtf8)?,
        firmware: core::str::from_utf8(firmware).map_err(|_| Error::InvalidUtf8)?,
    })
}

/// Decode capability metadata and validate its borrowed entry page.
fn decode_capabilities(body: &[u8]) -> Result<Capabilities<'_>, Error> {
    if body.len() < 13 {
        return Err(Error::InvalidLength);
    }
    let max_payload = u16::from_le_bytes([body[0], body[1]]);
    if max_payload as usize > MAX_PAYLOAD_SIZE {
        return Err(Error::InvalidArgument);
    }
    let data_capacity = u32::from_le_bytes([body[2], body[3], body[4], body[5]]);
    let event_capacity = u16::from_le_bytes([body[6], body[7]]);
    let control_features = u16::from_le_bytes([body[8], body[9]]);
    let next_offset = u16::from_le_bytes([body[10], body[11]]);
    let count = usize::from(body[12]);
    let entries = &body[13..];
    if count > MAX_CAPABILITY_ENTRIES || entries.len() != count * CAPABILITY_ENTRY_SIZE {
        return Err(Error::InvalidLength);
    }
    Capabilities::new(
        max_payload,
        data_capacity,
        event_capacity,
        control_features,
        next_offset,
        entries,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, DAC_PROFILE_YM2612};

    #[test]
    fn dac_capabilities_response_round_trips() {
        let request = Command::GetDacCapabilities.to_frame(7).unwrap();
        let capabilities = DacCapabilities {
            profile: DAC_PROFILE_YM2612,
            revision: 40,
            ticks_per_second: 44_100,
            record_capacity: 8192,
            event_capacity: 512,
            max_records: 60,
            record_bytes: 4,
            max_dac_late_us: 2_000,
        };
        let response =
            Response::for_request(&request, Reply::DacCapabilities(capabilities)).unwrap();
        let frame = response.to_frame().unwrap();
        let decoded = Response::decode(&frame).unwrap();
        assert_eq!(decoded.reply, Reply::DacCapabilities(capabilities));
        decoded.matches_request(&request).unwrap();
    }

    #[test]
    fn dac_start_completion_response_is_typed() {
        let request = Command::DacStart {
            profile: DAC_PROFILE_YM2612,
            minimum_lead_ticks: 3_528,
        }
        .to_frame(8)
        .unwrap();
        let response = Response::for_request(&request, Reply::Complete).unwrap();
        let frame = response.to_frame().unwrap();
        let decoded = Response::decode(&frame).unwrap();
        assert_eq!(decoded.reply, Reply::Complete);
        decoded.matches_request(&request).unwrap();
    }

    #[test]
    fn dac_status_response_round_trips() {
        let request = Command::DacStatus {
            profile: DAC_PROFILE_YM2612,
        }
        .to_frame(9)
        .unwrap();
        let status = DacStatus {
            profile: DAC_PROFILE_YM2612,
            state: 1,
            fault: 0,
            sealed: false,
            accepted: 300,
            consumed: 100,
            queued: 200,
            free: 7_992,
            event_queued: 12,
            event_consumed: 4,
            underflows: 0,
            overflows: 0,
            rejected: 0,
            late_dac: 1,
            max_late_us: 12,
            accepted_tick: 44_100,
            played_tick: 33_000,
            executed_tick: 32_000,
        };
        let response = Response::for_request(&request, Reply::DacStatus(status)).unwrap();
        let frame = response.to_frame().unwrap();
        let decoded = Response::decode(&frame).unwrap();
        assert_eq!(decoded.reply, Reply::DacStatus(status));
        decoded.matches_request(&request).unwrap();
    }
}
