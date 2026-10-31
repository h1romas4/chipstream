use crate::Error;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_PAYLOAD_SIZE: usize = 256;
pub const MAX_RAW_SIZE: usize = 267;
pub const MAX_COBS_SIZE: usize = 269;
pub const MAX_ENCODED_SIZE: usize = 271;

const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_3740);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    NoResponse = 0,
    Request = 1,
    Response = 2,
}

impl TryFrom<u8> for MessageType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::NoResponse),
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            _ => Err(Error::InvalidMessageType(value)),
        }
    }
}

/// CRC-16/CCITT-FALSE over the header and payload, excluding the CRC itself.
pub fn checksum(bytes: &[u8]) -> u16 {
    CRC.checksum(bytes)
}

/// An owned, validated v1 frame. Command IDs are not overloaded for responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    message_type: MessageType,
    command_id: u8,
    transaction_id: u16,
    payload: [u8; MAX_PAYLOAD_SIZE],
    length: usize,
}

impl Frame {
    pub fn new(
        message_type: MessageType,
        command_id: u8,
        transaction_id: u16,
        payload: &[u8],
    ) -> Result<Self, Error> {
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(Error::PayloadTooLong {
                length: payload.len(),
            });
        }
        let mut frame = Self {
            message_type,
            command_id,
            transaction_id,
            payload: [0; MAX_PAYLOAD_SIZE],
            length: payload.len(),
        };
        frame.payload[..payload.len()].copy_from_slice(payload);
        Ok(frame)
    }

    pub const fn message_type(&self) -> MessageType {
        self.message_type
    }

    pub const fn command_id(&self) -> u8 {
        self.command_id
    }

    pub const fn transaction_id(&self) -> u16 {
        self.transaction_id
    }

    pub const fn is_response(&self) -> bool {
        matches!(self.message_type, MessageType::Response)
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.length]
    }

    /// Decode the COBS body only, without either zero delimiter.
    pub fn decode(encoded: &[u8]) -> Result<Self, Error> {
        if encoded.len() > MAX_COBS_SIZE {
            return Err(Error::FrameTooLong);
        }
        if encoded.is_empty() || encoded.contains(&0) {
            return Err(Error::InvalidCobs);
        }
        let mut raw = [0; MAX_COBS_SIZE];
        raw[..encoded.len()].copy_from_slice(encoded);
        let length =
            cobs::decode_in_place(&mut raw[..encoded.len()]).map_err(|_| Error::InvalidCobs)?;
        if !(11..=MAX_RAW_SIZE).contains(&length) {
            return Err(Error::InvalidLength);
        }
        if &raw[..2] != b"ND" {
            return Err(Error::InvalidMagic);
        }
        if raw[2] != PROTOCOL_VERSION {
            return Err(Error::UnsupportedVersion(raw[2]));
        }
        let message_type = MessageType::try_from(raw[3])?;
        let payload_length = u16::from_le_bytes([raw[7], raw[8]]) as usize;
        if payload_length > MAX_PAYLOAD_SIZE || length != payload_length + 11 {
            return Err(Error::InvalidLength);
        }
        let expected = checksum(&raw[..length - 2]);
        let received = u16::from_le_bytes([raw[length - 2], raw[length - 1]]);
        if expected != received {
            return Err(Error::CrcMismatch { expected, received });
        }
        Self::new(
            message_type,
            raw[4],
            u16::from_le_bytes([raw[5], raw[6]]),
            &raw[9..length - 2],
        )
    }

    /// Encode with both leading and trailing zero delimiters.
    pub fn encode(&self) -> EncodedFrame {
        let mut raw = [0; MAX_RAW_SIZE];
        raw[..2].copy_from_slice(b"ND");
        raw[2] = PROTOCOL_VERSION;
        raw[3] = self.message_type as u8;
        raw[4] = self.command_id;
        raw[5..7].copy_from_slice(&self.transaction_id.to_le_bytes());
        raw[7..9].copy_from_slice(&(self.length as u16).to_le_bytes());
        raw[9..9 + self.length].copy_from_slice(self.payload());
        let crc = checksum(&raw[..9 + self.length]);
        raw[9 + self.length..11 + self.length].copy_from_slice(&crc.to_le_bytes());
        let mut result = EncodedFrame {
            bytes: [0; MAX_ENCODED_SIZE],
            length: 0,
        };
        result.length = cobs::encode(
            &raw[..11 + self.length],
            &mut result.bytes[1..MAX_ENCODED_SIZE - 1],
        ) + 2;
        result
    }

    pub fn encode_into(&self, output: &mut [u8]) -> Result<usize, Error> {
        let encoded = self.encode();
        if output.len() < encoded.as_bytes().len() {
            return Err(Error::BufferTooSmall {
                required: encoded.length,
            });
        }
        output[..encoded.length].copy_from_slice(encoded.as_bytes());
        Ok(encoded.length)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    bytes: [u8; MAX_ENCODED_SIZE],
    length: usize,
}

impl EncodedFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

impl AsRef<[u8]> for EncodedFrame {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
