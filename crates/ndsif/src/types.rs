//! Shared register, audio timing, position, and device status types.

use crate::Error;

/// A single register address/value pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterWrite {
    /// Register address within the selected page.
    pub address: u8,
    /// Value written to the register.
    pub value: u8,
}

impl RegisterWrite {
    /// Construct a register address/value pair.
    pub const fn new(address: u8, value: u8) -> Self {
        Self { address, value }
    }
}

/// Validated address/value pairs borrowed directly from a request payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterWrites<'a>(&'a [u8]);

impl<'a> RegisterWrites<'a> {
    /// Validate the count and framing of borrowed address/value pairs.
    pub(crate) fn decode(bytes: &'a [u8], maximum: usize) -> Result<Self, Error> {
        let pairs = bytes.chunks_exact(2);
        if bytes.is_empty() || !pairs.remainder().is_empty() || pairs.len() > maximum {
            return Err(Error::InvalidArgument);
        }
        Ok(Self(bytes))
    }
    /// Borrow the alternating address/value payload bytes.
    pub const fn as_bytes(self) -> &'a [u8] {
        self.0
    }
    /// Return the number of register pairs.
    pub const fn len(self) -> usize {
        self.0.len() / 2
    }
    /// Return whether there are no pairs. Valid decoded requests are nonempty.
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
    /// Iterate over the decoded register pairs in wire order.
    pub fn iter(self) -> impl ExactSizeIterator<Item = RegisterWrite> + 'a {
        self.0
            .chunks_exact(2)
            .map(|pair| RegisterWrite::new(pair[0], pair[1]))
    }
}

/// ADPCM byte position, not a sample, nibble or tick count. Natural wrapping is forbidden.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BytePosition(u32);

impl BytePosition {
    /// Construct an absolute ADPCM byte position.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    /// Return the absolute byte position.
    pub const fn get(self) -> u32 {
        self.0
    }
    /// Advance by a byte count, rejecting values that would overflow `u32`.
    pub fn checked_advance(self, bytes: usize) -> Result<Self, Error> {
        let bytes = u32::try_from(bytes).map_err(|_| Error::InvalidArgument)?;
        self.0
            .checked_add(bytes)
            .map(Self)
            .ok_or(Error::InvalidArgument)
    }
}

/// VGM sound command ID used by clock configuration and register-write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteId(u8);

impl WriteId {
    /// Primary YM2612 register-write command ID.
    pub const YM2612: Self = Self(0x52);
    /// YM2151 register-write command ID.
    pub const YM2151: Self = Self(0x54);
    /// OKIM6258 register-write command ID.
    pub const OKIM6258: Self = Self(0xb7);
    /// Primary SN76489 raw-byte command ID.
    pub const SN76489: Self = Self(0x50);
    /// Secondary SN76489 raw-byte command ID.
    pub const SN76489_SECOND: Self = Self(0x30);

    /// Return the underlying VGM-style command ID.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Return whether this target accepts address/value register pairs.
    pub const fn supports_register_pairs(self) -> bool {
        matches!(
            self.0,
            0x51..=0x5f
                | 0xa0..=0xa5
                | 0xaa
                | 0xab
                | 0xae
                | 0xaf
                | 0xb7
                | 0xd0
        )
    }

    /// Whether this target accepts raw chip bytes instead of register pairs.
    pub const fn supports_raw_bytes(self) -> bool {
        matches!(self.0, 0x30 | 0x50)
    }
}

impl TryFrom<u8> for WriteId {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 0x30 {
            return Err(Error::InvalidArgument);
        }
        Ok(Self(value))
    }
}

/// Supported OKIM6258 input clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum OkiClock {
    /// 4 MHz input clock.
    Mhz4 = 4_000_000,
    /// 8 MHz input clock.
    Mhz8 = 8_000_000,
}

impl TryFrom<u32> for OkiClock {
    type Error = Error;
    fn try_from(value: u32) -> Result<Self, Error> {
        match value {
            4_000_000 => Ok(Self::Mhz4),
            8_000_000 => Ok(Self::Mhz8),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// OKIM6258 input clock divisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Divider {
    /// Divide the input clock by 512.
    Div512 = 512,
    /// Divide the input clock by 768.
    Div768 = 768,
    /// Divide the input clock by 1024.
    Div1024 = 1024,
}

impl TryFrom<u16> for Divider {
    type Error = Error;
    fn try_from(value: u16) -> Result<Self, Error> {
        match value {
            512 => Ok(Self::Div512),
            768 => Ok(Self::Div768),
            1024 => Ok(Self::Div1024),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// Output-channel selection for an ADPCM stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Pan {
    /// Enable both output channels.
    Both = 0,
    /// Enable the left output channel only.
    Left = 1,
    /// Enable the right output channel only.
    Right = 2,
    /// Disable both output channels.
    Off = 3,
}

/// Exact clock/divider timing: two ADPCM samples per byte, rounded down to whole units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioTiming {
    clock: OkiClock,
    divider: Divider,
}

impl AudioTiming {
    /// Construct timing from a supported input clock and divider.
    pub const fn new(clock: OkiClock, divider: Divider) -> Self {
        Self { clock, divider }
    }

    /// Return the input clock.
    pub const fn clock(self) -> OkiClock {
        self.clock
    }

    /// Return the input clock divider.
    pub const fn divider(self) -> Divider {
        self.divider
    }

    /// Create a stateful converter for a nonzero tick rate in ticks per second.
    pub fn sample_converter(self, tick_rate: u32) -> Result<AudioSampleConverter, Error> {
        if tick_rate == 0 {
            return Err(Error::InvalidArgument);
        }
        Ok(AudioSampleConverter {
            numerator: self.clock as u64,
            denominator: u64::from(tick_rate) * self.divider as u64,
            remainder: 0,
        })
    }

    /// Convert a duration to samples, rounding down.
    pub fn samples_from_ticks(self, ticks: u64, tick_rate: u32) -> Result<u64, Error> {
        self.sample_converter(tick_rate)?.advance(ticks)
    }

    /// Convert a duration to whole ADPCM bytes, rounding down.
    pub fn bytes_from_ticks(self, ticks: u64, tick_rate: u32) -> Result<u64, Error> {
        let converter = self.sample_converter(tick_rate)?;
        let bytes = u128::from(ticks) * u128::from(converter.numerator)
            / (u128::from(converter.denominator) * 2);
        u64::try_from(bytes).map_err(|_| Error::InvalidArgument)
    }

    /// Convert an ADPCM byte count to whole ticks, rounding down.
    pub fn ticks_from_bytes(self, bytes: u64, tick_rate: u32) -> Result<u64, Error> {
        if tick_rate == 0 {
            return Err(Error::InvalidArgument);
        }
        let ticks = u128::from(bytes) * 2 * self.divider as u128 * u128::from(tick_rate)
            / self.clock as u128;
        u64::try_from(ticks).map_err(|_| Error::InvalidArgument)
    }
}

/// Converts consecutive tick durations without losing fractional samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSampleConverter {
    numerator: u64,
    denominator: u64,
    remainder: u64,
}

impl AudioSampleConverter {
    /// Overflow leaves the conversion remainder unchanged.
    pub fn advance(&mut self, ticks: u64) -> Result<u64, Error> {
        let total = u128::from(ticks) * u128::from(self.numerator) + u128::from(self.remainder);
        let samples = u64::try_from(total / u128::from(self.denominator))
            .map_err(|_| Error::InvalidArgument)?;
        self.remainder = (total % u128::from(self.denominator)) as u64;
        Ok(samples)
    }
}

impl TryFrom<u8> for Pan {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Both),
            1 => Ok(Self::Left),
            2 => Ok(Self::Right),
            3 => Ok(Self::Off),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// State-dependent END repetition. Neither value is a universal silence code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ZeroPair {
    /// Repeat byte `0x80`.
    Byte80 = 0x80,
    /// Repeat byte `0x08`.
    Byte08 = 0x08,
}

impl TryFrom<u8> for ZeroPair {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0x80 => Ok(Self::Byte80),
            0x08 => Ok(Self::Byte08),
            _ => Err(Error::InvalidArgument),
        }
    }
}

/// Preserves unknown bits for future firmware extensions. Only FAULT signifies a fault.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatusFlags(u32);

impl StatusFlags {
    /// The audio stream is running.
    pub const RUNNING: Self = Self(0x001);
    /// The audio stream has ended.
    pub const ENDED: Self = Self(0x002);
    /// The device reports a fault.
    pub const FAULT: Self = Self(0x004);
    /// The transport was reset.
    pub const TRANSPORT_RESET: Self = Self(0x008);
    /// Receive buffering overflowed.
    pub const RECEIVE_OVERFLOW: Self = Self(0x010);
    /// Received data was discarded.
    pub const RECEIVE_DISCARDED: Self = Self(0x020);
    /// The PCM stream underflowed.
    pub const PCM_UNDERFLOW: Self = Self(0x040);
    /// The PCM stream overflowed.
    pub const PCM_OVERFLOW: Self = Self(0x080);
    /// The event queue overflowed.
    pub const EVENT_OVERFLOW: Self = Self(0x100);
    /// An argument was rejected.
    pub const ARGUMENT_REJECTED: Self = Self(0x200);

    /// Construct flags while retaining unknown bits.
    pub const fn from_bits_retain(bits: u32) -> Self {
        Self(bits)
    }
    /// Return all flag bits, including unknown bits.
    pub const fn bits(self) -> u32 {
        self.0
    }
    /// Return whether all bits in `flag` are present.
    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
    /// Return whether [`Self::RUNNING`] is set.
    pub const fn running(self) -> bool {
        self.contains(Self::RUNNING)
    }
    /// Return whether [`Self::ENDED`] is set.
    pub const fn ended(self) -> bool {
        self.contains(Self::ENDED)
    }
    /// Return whether [`Self::FAULT`] is set.
    pub const fn fault(self) -> bool {
        self.contains(Self::FAULT)
    }
}
