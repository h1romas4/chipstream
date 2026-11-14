//! Draft YM2612 live-DAC and ordered-live record codecs.

use crate::{Error, MAX_PAYLOAD_SIZE};

/// Draft live-DAC profile identifier.
pub const LIVE_DAC_PROFILE: u8 = 2;
/// Draft ordered-live profile identifier.
pub const ORDERED_LIVE_PROFILE: u8 = 3;
/// Live profile time base, in ticks per second.
pub const LIVE_TICKS_PER_SECOND: u32 = 44_100;
/// Maximum live-DAC records in a frame.
pub const MAX_LIVE_DAC_RECORDS: usize = 50;
/// Maximum ordered-live records in a frame.
pub const MAX_ORDERED_LIVE_RECORDS: usize = 100;

/// One page of draft live-DAC capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveDacCapabilities<'a> {
    /// Advertised profile.
    pub profile: u8,
    /// Advertised profile revision.
    pub revision: u8,
    /// Time base in ticks per second.
    pub ticks_per_second: u32,
    /// Total device queue capacity in records.
    pub record_capacity: u32,
    /// Maximum records accepted in one frame.
    pub max_records: u8,
    /// Next page offset, or `u16::MAX` at the end.
    pub next_offset: u16,
    entries: &'a [u8],
}

impl<'a> LiveDacCapabilities<'a> {
    /// Validate metadata and a slice of two-byte `(write_id, instance)` entries.
    pub fn new(
        profile: u8,
        revision: u8,
        ticks_per_second: u32,
        record_capacity: u32,
        max_records: u8,
        next_offset: u16,
        entries: &'a [u8],
    ) -> Result<Self, Error> {
        if ticks_per_second == 0
            || record_capacity == 0
            || max_records == 0
            || usize::from(max_records) > MAX_LIVE_DAC_RECORDS
            || u32::from(max_records) > record_capacity
            || !entries.len().is_multiple_of(2)
            || entries.len() > MAX_PAYLOAD_SIZE - 15
        {
            return Err(Error::InvalidArgument);
        }
        let pairs = entries.as_chunks::<2>().0;
        if pairs
            .iter()
            .enumerate()
            .any(|(i, entry)| pairs[..i].contains(entry))
        {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            profile,
            revision,
            ticks_per_second,
            record_capacity,
            max_records,
            next_offset,
            entries,
        })
    }

    /// Iterate over `(write_id, instance)` entries in wire order.
    pub fn entries(self) -> impl ExactSizeIterator<Item = (u8, u8)> + 'a {
        self.entries
            .as_chunks::<2>()
            .0
            .iter()
            .map(|entry| (entry[0], entry[1]))
    }

    pub(crate) fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() < 14 || bytes.len() != 14 + usize::from(bytes[13]) * 2 {
            return Err(Error::InvalidLength);
        }
        Self::new(
            bytes[0],
            bytes[1],
            u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
            u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
            bytes[10],
            u16::from_le_bytes([bytes[11], bytes[12]]),
            &bytes[14..],
        )
    }

    pub(crate) fn encode(self, output: &mut [u8]) -> usize {
        output[0] = self.profile;
        output[1] = self.revision;
        output[2..6].copy_from_slice(&self.ticks_per_second.to_le_bytes());
        output[6..10].copy_from_slice(&self.record_capacity.to_le_bytes());
        output[10] = self.max_records;
        output[11..13].copy_from_slice(&self.next_offset.to_le_bytes());
        output[13] = (self.entries.len() / 2) as u8;
        output[14..14 + self.entries.len()].copy_from_slice(self.entries);
        14 + self.entries.len()
    }
}

/// Device limits for draft ordered-live profile 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedLiveCapabilities {
    /// Advertised profile.
    pub profile: u8,
    /// Profile revision; revision 5 adds PSG records.
    pub revision: u8,
    /// Time base in ticks per second.
    pub ticks_per_second: u32,
    /// Device FIFO capacity in records.
    pub record_capacity: u16,
    /// Maximum records accepted in one frame.
    pub max_records: u8,
}

impl OrderedLiveCapabilities {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 9 {
            return Err(Error::InvalidLength);
        }
        let result = Self {
            profile: bytes[0],
            revision: bytes[1],
            ticks_per_second: u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
            record_capacity: u16::from_le_bytes([bytes[6], bytes[7]]),
            max_records: bytes[8],
        };
        if result.ticks_per_second == 0
            || result.record_capacity == 0
            || result.max_records == 0
            || usize::from(result.max_records) > MAX_ORDERED_LIVE_RECORDS
            || u16::from(result.max_records) > result.record_capacity
        {
            return Err(Error::InvalidArgument);
        }
        Ok(result)
    }

    pub(crate) fn encode(self, output: &mut [u8]) -> usize {
        output[0] = self.profile;
        output[1] = self.revision;
        output[2..6].copy_from_slice(&self.ticks_per_second.to_le_bytes());
        output[6..8].copy_from_slice(&self.record_capacity.to_le_bytes());
        output[8] = self.max_records;
        9
    }
}

/// Ordered-live diagnostic snapshot, not physical I/O completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedLiveStatus {
    /// Advertised profile.
    pub profile: u8,
    /// Next expected frame sequence.
    pub next_sequence: u32,
    /// Cumulative accepted record count.
    pub accepted_records: u32,
    /// Queued records.
    pub queued: u16,
    /// Free record slots.
    pub free: u16,
    /// Cumulative missing frame count.
    pub missing_frames: u32,
}

impl OrderedLiveStatus {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 17 {
            return Err(Error::InvalidLength);
        }
        Ok(Self {
            profile: bytes[0],
            next_sequence: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
            accepted_records: u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
            queued: u16::from_le_bytes([bytes[9], bytes[10]]),
            free: u16::from_le_bytes([bytes[11], bytes[12]]),
            missing_frames: u32::from_le_bytes([bytes[13], bytes[14], bytes[15], bytes[16]]),
        })
    }

    pub(crate) fn encode(self, output: &mut [u8]) -> usize {
        output[0] = self.profile;
        output[1..5].copy_from_slice(&self.next_sequence.to_le_bytes());
        output[5..9].copy_from_slice(&self.accepted_records.to_le_bytes());
        output[9..11].copy_from_slice(&self.queued.to_le_bytes());
        output[11..13].copy_from_slice(&self.free.to_le_bytes());
        output[13..17].copy_from_slice(&self.missing_frames.to_le_bytes());
        17
    }
}

/// A DAC value and the wait since the preceding DAC write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveDacRecord {
    /// Delay in 44,100 Hz ticks.
    pub wait_ticks: u32,
    /// YM2612 DAC value.
    pub value: u8,
}

/// Validated, borrowed live-DAC records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveDacRecords<'a>(pub(crate) &'a [u8]);

impl<'a> LiveDacRecords<'a> {
    pub(crate) fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.is_empty()
            || !bytes.len().is_multiple_of(5)
            || bytes.len() / 5 > MAX_LIVE_DAC_RECORDS
        {
            return Err(Error::InvalidLength);
        }
        Ok(Self(bytes))
    }

    /// Number of records.
    pub fn len(self) -> usize {
        self.0.len() / 5
    }

    /// Whether this record set is empty.
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Iterate over records in wire order.
    pub fn iter(self) -> impl ExactSizeIterator<Item = LiveDacRecord> + 'a {
        self.0
            .as_chunks::<5>()
            .0
            .iter()
            .map(|record| LiveDacRecord {
                wait_ticks: u32::from_le_bytes([record[0], record[1], record[2], record[3]]),
                value: record[4],
            })
    }
}

/// Operation on the ordered YM2612/PSG timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderedLiveWrite {
    /// YM2612 page 0 DAC data register `$2A`.
    Dac(u8),
    /// YM2612 register write on page 0 or 1.
    Register {
        /// YM2612 register page.
        page: u8,
        /// Register address.
        address: u8,
        /// Register value.
        value: u8,
    },
    /// PSG byte; requires ordered profile revision 5.
    Psg(u8),
}

/// An ordered write and the wait since the preceding ordered write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedLiveRecord {
    /// Delay in 44,100 Hz ticks.
    pub wait_ticks: u32,
    /// Operation to perform.
    pub write: OrderedLiveWrite,
}

impl OrderedLiveRecord {
    pub(crate) fn encode(self, output: &mut [u8]) -> Result<usize, Error> {
        let (tag, len) = match self.write {
            OrderedLiveWrite::Dac(_) if self.wait_ticks <= 0xfd => (self.wait_ticks as u8, 2),
            OrderedLiveWrite::Dac(_) => (0xfe, 6),
            OrderedLiveWrite::Register { page, address, .. }
                if page <= 1 && (page != 0 || address != 0x2a) =>
            {
                (0xff, 8)
            }
            OrderedLiveWrite::Psg(_) => (0xff, 8),
            _ => return Err(Error::InvalidArgument),
        };
        if output.len() < len {
            return Err(Error::InvalidLength);
        }
        output[0] = tag;
        if tag <= 0xfd {
            let OrderedLiveWrite::Dac(value) = self.write else {
                unreachable!()
            };
            output[1] = value;
        } else {
            output[1..5].copy_from_slice(&self.wait_ticks.to_le_bytes());
            match self.write {
                OrderedLiveWrite::Dac(value) => output[5] = value,
                OrderedLiveWrite::Register {
                    page,
                    address,
                    value,
                } => {
                    output[5..8].copy_from_slice(&[page, address, value]);
                }
                OrderedLiveWrite::Psg(value) => output[5..8].copy_from_slice(&[2, 0, value]),
            }
        }
        Ok(len)
    }
}

/// Validated, borrowed variable-length ordered-live records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderedLiveRecords<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> OrderedLiveRecords<'a> {
    pub(crate) fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut remaining = bytes;
        let mut count = 0;
        while !remaining.is_empty() {
            let (_, length) = decode_ordered(remaining)?;
            remaining = &remaining[length..];
            count += 1;
            if count > MAX_ORDERED_LIVE_RECORDS {
                return Err(Error::InvalidLength);
            }
        }
        if count == 0 {
            return Err(Error::InvalidLength);
        }
        Ok(Self { bytes, count })
    }

    /// Number of records.
    pub fn len(self) -> usize {
        self.count
    }

    /// Whether this record set is empty.
    pub fn is_empty(self) -> bool {
        self.count == 0
    }

    /// Iterate over records in wire order.
    pub fn iter(self) -> impl Iterator<Item = OrderedLiveRecord> + 'a {
        let mut bytes = self.bytes;
        core::iter::from_fn(move || {
            if bytes.is_empty() {
                return None;
            }
            let (record, length) = decode_ordered(bytes).expect("validated ordered records");
            bytes = &bytes[length..];
            Some(record)
        })
    }
}

fn decode_ordered(bytes: &[u8]) -> Result<(OrderedLiveRecord, usize), Error> {
    let tag = *bytes.first().ok_or(Error::InvalidLength)?;
    let length = if tag <= 0xfd {
        2
    } else if tag == 0xfe {
        6
    } else {
        8
    };
    if bytes.len() < length {
        return Err(Error::InvalidLength);
    }
    let wait_ticks = if tag <= 0xfd {
        u32::from(tag)
    } else {
        u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]])
    };
    let write = match tag {
        0..=0xfd => OrderedLiveWrite::Dac(bytes[1]),
        0xfe => OrderedLiveWrite::Dac(bytes[5]),
        _ => match (bytes[5], bytes[6]) {
            (0, 0x2a) => OrderedLiveWrite::Dac(bytes[7]),
            (page @ 0..=1, address) => OrderedLiveWrite::Register {
                page,
                address,
                value: bytes[7],
            },
            (2, 0) => OrderedLiveWrite::Psg(bytes[7]),
            _ => return Err(Error::InvalidArgument),
        },
    };
    Ok((OrderedLiveRecord { wait_ticks, write }, length))
}
