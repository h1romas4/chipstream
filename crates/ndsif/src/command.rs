use crate::{
    BytePosition, Divider, EncodedFrame, Error, Frame, MAX_PAYLOAD_SIZE, MessageType, OkiClock,
    Pan, RegisterWrite, WriteId, ZeroPair,
};

pub const MAX_REGISTER_WRITES: usize = (MAX_PAYLOAD_SIZE - 2) / 2;
pub const MAX_REGISTER_EVENT_WRITES: usize = (MAX_PAYLOAD_SIZE - 10) / 2;
pub const MAX_AUDIO_DATA_BYTES: usize = MAX_PAYLOAD_SIZE - 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEvent<'a> {
    Registers {
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: &'a [RegisterWrite],
    },
    Clock {
        clock: OkiClock,
        divider: Divider,
    },
    Pan(Pan),
    End(ZeroPair),
}

/// Request encoding with local argument validation, not device/session state validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Command<'a> {
    Reset,
    Ping(&'a [u8]),
    GetInfo,
    SetOutputVolume {
        attenuation: u8,
        response: bool,
    },
    SetChipClock {
        write_id: WriteId,
        instance: u8,
        hz: u32,
        response: bool,
    },
    GetCapabilities {
        offset: u16,
    },
    WriteRegisters {
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: &'a [RegisterWrite],
    },
    AudioData {
        stream_id: u8,
        instance: u8,
        position: BytePosition,
        adpcm: &'a [u8],
    },
    AudioEvent {
        stream_id: u8,
        instance: u8,
        position: BytePosition,
        event: AudioEvent<'a>,
    },
    AudioStart {
        stream_id: u8,
        instance: u8,
        clock: OkiClock,
        divider: Divider,
    },
    AudioStatus {
        stream_id: u8,
        instance: u8,
    },
}

impl Command<'_> {
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
            Self::WriteRegisters { write_id, .. } => write_id.get(),
        }
    }

    pub const fn expects_response(&self) -> bool {
        match self {
            Self::Reset
            | Self::Ping(_)
            | Self::GetInfo
            | Self::GetCapabilities { .. }
            | Self::AudioStatus { .. } => true,
            Self::SetOutputVolume { response, .. } | Self::SetChipClock { response, .. } => {
                *response
            }
            Self::WriteRegisters { .. }
            | Self::AudioData { .. }
            | Self::AudioEvent { .. }
            | Self::AudioStart { .. } => false,
        }
    }

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
        };
        let message_type = match self {
            Self::WriteRegisters { .. } => MessageType::NoResponse,
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

    pub fn encode(&self, transaction_id: u16) -> Result<EncodedFrame, Error> {
        Ok(self.to_frame(transaction_id)?.encode())
    }

    pub fn encode_into(&self, transaction_id: u16, output: &mut [u8]) -> Result<usize, Error> {
        self.to_frame(transaction_id)?.encode_into(output)
    }
}

fn encode_writes(
    writes: &[RegisterWrite],
    output: &mut [u8],
    maximum: usize,
) -> Result<usize, Error> {
    if writes.is_empty() || writes.len() > maximum {
        return Err(Error::InvalidArgument);
    }
    for (pair, write) in output.chunks_exact_mut(2).zip(writes) {
        pair[0] = write.address;
        pair[1] = write.value;
    }
    Ok(writes.len() * 2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestEvent<'a> {
    Registers {
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: crate::types::RegisterWrites<'a>,
    },
    Clock {
        clock: OkiClock,
        divider: Divider,
    },
    Pan(Pan),
    End(ZeroPair),
}

/// Typed request view. Unknown commands preserve their command ID and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Request<'a> {
    Reset,
    Ping(&'a [u8]),
    GetInfo,
    SetOutputVolume {
        attenuation: u8,
    },
    SetChipClock {
        write_id: WriteId,
        instance: u8,
        hz: u32,
    },
    GetCapabilities {
        offset: u16,
    },
    WriteRegisters {
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: crate::types::RegisterWrites<'a>,
    },
    AudioData {
        stream_id: u8,
        instance: u8,
        position: BytePosition,
        adpcm: &'a [u8],
    },
    AudioEvent {
        stream_id: u8,
        instance: u8,
        position: BytePosition,
        event: RequestEvent<'a>,
    },
    AudioStart {
        stream_id: u8,
        instance: u8,
        clock: OkiClock,
        divider: Divider,
    },
    AudioStatus {
        stream_id: u8,
        instance: u8,
    },
    Unknown {
        message_type: MessageType,
        command_id: u8,
        transaction_id: u16,
        payload: &'a [u8],
    },
}

impl<'a> Request<'a> {
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
                return Ok(Self::Unknown {
                    message_type,
                    command_id,
                    transaction_id,
                    payload: bytes,
                });
            }
            if bytes.len() < 4 || bytes.len() % 2 != 0 {
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
            0x00..=0x02 | 0x05 | 0x13 => Some(MessageType::Request),
            0x03..=0x04 => None,
            0x10..=0x12 => Some(MessageType::NoResponse),
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
                        if data.len() < 5 || data.len() % 2 == 0 {
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
            _ => Ok(Self::Unknown {
                message_type,
                command_id,
                transaction_id,
                payload: bytes,
            }),
        }
    }
}

fn require_length(bytes: &[u8], length: usize) -> Result<(), Error> {
    if bytes.len() != length {
        return Err(Error::InvalidLength);
    }
    Ok(())
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn validate_register_id(write_id: WriteId) -> Result<(), Error> {
    if write_id.supports_register_pairs() {
        Ok(())
    } else {
        Err(Error::InvalidArgument)
    }
}
