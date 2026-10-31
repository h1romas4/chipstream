use crate::{EncodedFrame, Error, Frame, MAX_PAYLOAD_SIZE, MessageType, StatusFlags};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Complete,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo<'a> {
    pub model: &'a str,
    pub firmware: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClockMode {
    Unknown = 0,
    Fixed = 1,
    Variable = 2,
}

impl TryFrom<u8> for ClockMode {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Fixed),
            2 => Ok(Self::Variable),
            _ => Err(Error::InvalidArgument),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityEntry {
    pub write_id: u8,
    pub instance: u8,
    pub page: u8,
    pub features: u8,
    pub clock_mode: ClockMode,
    pub fixed_clock_hz: u32,
}

pub const CAPABILITY_ENTRY_SIZE: usize = 9;
pub const MAX_CAPABILITY_ENTRIES: usize = (MAX_PAYLOAD_SIZE - 14) / CAPABILITY_ENTRY_SIZE;
const CONFIGURABLE_CLOCK_FEATURE: u8 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities<'a> {
    pub max_payload: u16,
    pub data_capacity: u32,
    pub event_capacity: u16,
    pub control_features: u16,
    pub next_offset: u16,
    entries: &'a [u8],
}

impl<'a> Capabilities<'a> {
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

    pub fn entries(self) -> impl ExactSizeIterator<Item = CapabilityEntry> + 'a {
        self.entries
            .chunks_exact(CAPABILITY_ENTRY_SIZE)
            .map(|entry| decode_capability_entry(entry).expect("validated capability entry"))
    }
}

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

fn validate_capability_entries(entries: &[u8]) -> Result<(), Error> {
    let mut previous_entries = entries.chunks_exact(CAPABILITY_ENTRY_SIZE);
    for (index, entry) in previous_entries.by_ref().enumerate() {
        let current = decode_capability_entry(entry)?;
        for previous in entries[..index * CAPABILITY_ENTRY_SIZE].chunks_exact(CAPABILITY_ENTRY_SIZE)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStatus {
    pub accepted: u32,
    pub played: u32,
    pub pending: u32,
    pub underflows: u32,
    pub overflows: u32,
    pub rejected: u32,
    pub max_pending: u32,
    pub flags: StatusFlags,
    pub late_events: u32,
    pub max_event_lag: u32,
}

impl AudioStatus {
    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 40 {
            return Err(Error::InvalidLength);
        }
        let mut values = [0; 10];
        for (value, bytes) in values.iter_mut().zip(bytes.chunks_exact(4)) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reply<'a> {
    Complete,
    Rejected,
    Ping(&'a [u8]),
    Info(DeviceInfo<'a>),
    Capabilities(Capabilities<'a>),
    AudioStatus(AudioStatus),
    Unknown(&'a [u8]),
}

/// Typed response. Parsed data borrows the frame payload; outgoing data borrows caller data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response<'a> {
    pub command_id: u8,
    pub transaction_id: u16,
    pub status: Status,
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
                    for (bytes, value) in payload[1..41].chunks_exact_mut(4).zip(values) {
                        bytes.copy_from_slice(&value.to_le_bytes());
                    }
                    41
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

    pub fn encode(&self) -> Result<EncodedFrame, Error> {
        Ok(self.to_frame()?.encode())
    }

    pub fn encode_into(&self, output: &mut [u8]) -> Result<usize, Error> {
        self.to_frame()?.encode_into(output)
    }

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
                0x00 | 0x03 | 0x04 => {
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
