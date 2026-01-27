//! Detailed parsing of VGM data blocks.
//!
//! This module provides types and parsing functions for detailed interpretation
//! of VGM data blocks (command 0x67). Data blocks can contain:
//!
//! - Uncompressed PCM/ADPCM streams
//! - Compressed streams (bit-packing, DPCM)
//! - Decompression tables
//! - ROM/RAM dumps
//! - RAM write blocks
//!
//! # Examples
//!
//! ## Parsing an uncompressed stream
//!
//! ```
//! use soundlog::vgm::command::DataBlock;
//! use soundlog::vgm::detail::{parse_data_block, DataBlockType};
//!
//! let block = DataBlock {
//!     marker: 0x66,
//!     chip_instance: 0,
//!     data_type: 0x00, // Uncompressed stream, YM2612 PCM
//!     size: 4,
//!     data: vec![0x01, 0x02, 0x03, 0x04],
//! };
//!
//! match parse_data_block(block).unwrap() {
//!     DataBlockType::UncompressedStream(stream) => {
//!         println!("Chip: {:?}", stream.chip_type);
//!         println!("Data length: {}", stream.data.len());
//!     }
//!     _ => panic!("Expected uncompressed stream"),
//! }
//! ```
//!
//! ## Parsing a ROM dump
//!
//! ```
//! use soundlog::vgm::command::DataBlock;
//! use soundlog::vgm::detail::{parse_data_block, DataBlockType};
//!
//! // ROM dump block: type 0x80 (Sega PCM ROM)
//! let mut data = vec![
//!     0x00, 0x10, 0x00, 0x00, // ROM size: 4096 bytes
//!     0x00, 0x00, 0x00, 0x00, // Start address: 0
//! ];
//! data.extend_from_slice(&[0xAA; 4096]); // ROM data
//!
//! let block = DataBlock {
//!     marker: 0x66,
//!     chip_instance: 0,
//!     data_type: 0x80,
//!     size: data.len() as u32,
//!     data,
//! };
//!
//! match parse_data_block(block).unwrap() {
//!     DataBlockType::RomRamDump(dump) => {
//!         println!("Chip: {:?}", dump.chip_type);
//!         println!("ROM size: {}", dump.rom_size);
//!         println!("Start address: 0x{:08X}", dump.start_address);
//!     }
//!     _ => panic!("Expected ROM dump"),
//! }
//! ```
//!
//! ## Decompressing a compressed stream
//!
//! ```
//! use soundlog::vgm::command::DataBlock;
//! use soundlog::vgm::detail::{parse_data_block, DataBlockType, CompressedStreamData};
//!
//! // Compressed stream block with bit-packing (Copy sub-type)
//! let mut data = vec![
//!     0x00,                   // Compression type: 0 (bit-packing)
//!     0x04, 0x00, 0x00, 0x00, // Uncompressed size: 4 bytes
//!     0x08,                   // Bits decompressed: 8
//!     0x04,                   // Bits compressed: 4
//!     0x00,                   // Sub-type: 0 (Copy)
//!     0x00, 0x00,             // Add value: 0 (16-bit)
//!     0x12, 0x34,             // Compressed data: 0x1, 0x2, 0x3, 0x4 (4-bit each)
//! ];
//!
//! let block = DataBlock {
//!     marker: 0x66,
//!     chip_instance: 0,
//!     data_type: 0x40, // Compressed stream, YM2612 PCM
//!     size: data.len() as u32,
//!     data,
//! };
//!
//! match parse_data_block(block).unwrap() {
//!     DataBlockType::CompressedStream(mut stream) => {
//!         if let CompressedStreamData::BitPacking(mut bp) = stream.compression {
//!             bp.decompress(None).unwrap();
//!             println!("Decompressed {} bytes", bp.data.len());
//!             assert_eq!(bp.data, vec![0x01, 0x02, 0x03, 0x04]);
//!         }
//!     }
//!     _ => panic!("Expected compressed stream"),
//! }
//! ```

use crate::binutil::ParseError;
use crate::vgm::command::DataBlock;

/// Stream chip type for uncompressed/compressed streams (data block types 0x00-0x3F and 0x40-0x7E).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamChipType {
    /// YM2612 PCM data
    Ym2612Pcm,
    /// RF5C68 PCM data
    Rf5c68Pcm,
    /// RF5C164 PCM data
    Rf5c164Pcm,
    /// PWM PCM data
    PwmPcm,
    /// OKIM6258 ADPCM data
    Okim6258Adpcm,
    /// HuC6280 PCM data
    Huc6280Pcm,
    /// SCSP PCM data
    ScspPcm,
    /// NES APU DPCM data
    NesApuDpcm,
    /// Mikey PCM data
    MikeyPcm,
    /// Unknown or unsupported chip type
    Unknown(u8),
}

impl From<u8> for StreamChipType {
    fn from(value: u8) -> Self {
        match value & 0x3F {
            0x00 => StreamChipType::Ym2612Pcm,
            0x01 => StreamChipType::Rf5c68Pcm,
            0x02 => StreamChipType::Rf5c164Pcm,
            0x03 => StreamChipType::PwmPcm,
            0x04 => StreamChipType::Okim6258Adpcm,
            0x05 => StreamChipType::Huc6280Pcm,
            0x06 => StreamChipType::ScspPcm,
            0x07 => StreamChipType::NesApuDpcm,
            0x08 => StreamChipType::MikeyPcm,
            _ => StreamChipType::Unknown(value & 0x3F),
        }
    }
}

/// ROM/RAM chip type for ROM/RAM dump blocks (data block types 0x80-0xBF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RomRamChipType {
    /// Sega PCM ROM
    SegaPcmRom,
    /// YM2608 DELTA-T ROM
    Ym2608DeltaTRom,
    /// YM2610 ADPCM ROM
    Ym2610AdpcmRom,
    /// YM2610 DELTA-T ROM
    Ym2610DeltaTRom,
    /// YMF278B ROM
    Ymf278bRom,
    /// YMF271 ROM
    Ymf271Rom,
    /// YMZ280B ROM
    Ymz280bRom,
    /// YMF278B RAM
    Ymf278bRam,
    /// Y8950 DELTA-T ROM
    Y8950DeltaTRom,
    /// MultiPCM ROM
    MultiPcmRom,
    /// uPD7759 ROM
    Upd7759Rom,
    /// OKIM6295 ROM
    Okim6295Rom,
    /// K054539 ROM
    K054539Rom,
    /// C140 ROM
    C140Rom,
    /// K053260 ROM
    K053260Rom,
    /// QSound ROM
    QsoundRom,
    /// ES5505/ES5506 ROM
    Es5505Rom,
    /// X1-010 ROM
    X1010Rom,
    /// C352 ROM
    C352Rom,
    /// GA20 ROM
    Ga20Rom,
    /// Unknown or unsupported chip type
    Unknown(u8),
}

impl From<u8> for RomRamChipType {
    fn from(value: u8) -> Self {
        match value {
            0x80 => RomRamChipType::SegaPcmRom,
            0x81 => RomRamChipType::Ym2608DeltaTRom,
            0x82 => RomRamChipType::Ym2610AdpcmRom,
            0x83 => RomRamChipType::Ym2610DeltaTRom,
            0x84 => RomRamChipType::Ymf278bRom,
            0x85 => RomRamChipType::Ymf271Rom,
            0x86 => RomRamChipType::Ymz280bRom,
            0x87 => RomRamChipType::Ymf278bRam,
            0x88 => RomRamChipType::Y8950DeltaTRom,
            0x89 => RomRamChipType::MultiPcmRom,
            0x8A => RomRamChipType::Upd7759Rom,
            0x8B => RomRamChipType::Okim6295Rom,
            0x8C => RomRamChipType::K054539Rom,
            0x8D => RomRamChipType::C140Rom,
            0x8E => RomRamChipType::K053260Rom,
            0x8F => RomRamChipType::QsoundRom,
            0x90 => RomRamChipType::Es5505Rom,
            0x91 => RomRamChipType::X1010Rom,
            0x92 => RomRamChipType::C352Rom,
            0x93 => RomRamChipType::Ga20Rom,
            _ => RomRamChipType::Unknown(value),
        }
    }
}

/// RAM write chip type for 16-bit RAM writes (data block types 0xC0-0xDF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamWrite16ChipType {
    /// RF5C68
    Rf5c68,
    /// RF5C164
    Rf5c164,
    /// NES APU
    NesApu,
    /// Unknown or unsupported chip type
    Unknown(u8),
}

impl From<u8> for RamWrite16ChipType {
    fn from(value: u8) -> Self {
        match value {
            0xC0 => RamWrite16ChipType::Rf5c68,
            0xC1 => RamWrite16ChipType::Rf5c164,
            0xC2 => RamWrite16ChipType::NesApu,
            _ => RamWrite16ChipType::Unknown(value),
        }
    }
}

/// RAM write chip type for 32-bit RAM writes (data block types 0xE0-0xFF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamWrite32ChipType {
    /// SCSP
    Scsp,
    /// ES5503
    Es5503,
    /// Unknown or unsupported chip type
    Unknown(u8),
}

impl From<u8> for RamWrite32ChipType {
    fn from(value: u8) -> Self {
        match value {
            0xE0 => RamWrite32ChipType::Scsp,
            0xE1 => RamWrite32ChipType::Es5503,
            _ => RamWrite32ChipType::Unknown(value),
        }
    }
}

/// Compression type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionType {
    /// Bit-packing compression
    BitPacking,
    /// DPCM compression
    Dpcm,
    /// Unknown compression type
    Unknown(u8),
}

impl From<u8> for CompressionType {
    fn from(value: u8) -> Self {
        match value {
            0x00 => CompressionType::BitPacking,
            0x01 => CompressionType::Dpcm,
            _ => CompressionType::Unknown(value),
        }
    }
}

/// Bit-packing compression sub-type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitPackingSubType {
    /// Copy: value as-is (high bits unused)
    Copy,
    /// Shift left: value << shift (low bits unused)
    ShiftLeft,
    /// Use table: value is table index
    UseTable,
    /// Unknown sub-type
    Unknown(u8),
}

impl From<u8> for BitPackingSubType {
    fn from(value: u8) -> Self {
        match value {
            0x00 => BitPackingSubType::Copy,
            0x01 => BitPackingSubType::ShiftLeft,
            0x02 => BitPackingSubType::UseTable,
            _ => BitPackingSubType::Unknown(value),
        }
    }
}

/// Uncompressed PCM/ADPCM stream data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncompressedStream {
    pub chip_type: StreamChipType,
    pub data: Vec<u8>,
}

/// Bit-packing compression data and parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitPackingCompression {
    pub bits_decompressed: u8,
    pub bits_compressed: u8,
    pub sub_type: BitPackingSubType,
    pub add_value: u16,
    pub data: Vec<u8>,
}

impl BitPackingCompression {
    /// Decompress bit-packed data in-place.
    ///
    /// After this method succeeds, `self.data` will contain the decompressed data.
    ///
    /// # Arguments
    /// * `table` - Optional decompression table, required when sub_type is UseTable
    ///
    /// # Errors
    /// Returns error if:
    /// - `sub_type` is `UseTable` but `table` is `None`
    /// - Table is provided but doesn't match compression parameters
    pub fn decompress(&mut self, table: Option<&DecompressionTable>) -> Result<(), ParseError> {
        if matches!(self.sub_type, BitPackingSubType::UseTable) && table.is_none() {
            return Err(ParseError::DataInconsistency(
                "Decompression table required for UseTable sub-type".to_string(),
            ));
        }

        let bytes_per_value = self.bits_decompressed.div_ceil(8) as usize;
        let mut result = Vec::new();
        let mut bitstream = BitStreamReader::new(&self.data);

        while bitstream.bits_remaining() >= self.bits_compressed as usize {
            let compressed_value = bitstream.read_bits(self.bits_compressed as usize)?;
            let decompressed_value = match self.sub_type {
                BitPackingSubType::Copy => {
                    // Just use the value as-is (high bits aren't used)
                    compressed_value + (self.add_value as u32)
                }
                BitPackingSubType::ShiftLeft => {
                    // Shift left (low bits aren't used)
                    let shift = self.bits_decompressed - self.bits_compressed;
                    (compressed_value << shift) + (self.add_value as u32)
                }
                BitPackingSubType::UseTable => {
                    // Use table lookup
                    let table = table.unwrap(); // Already checked above
                    let index = compressed_value as usize;
                    read_table_value(table, index, bytes_per_value)? as u32
                }
                BitPackingSubType::Unknown(_) => {
                    return Err(ParseError::Other(format!(
                        "Unknown bit packing sub-type: {:?}",
                        self.sub_type
                    )));
                }
            };
            write_value_bytes(&mut result, decompressed_value, bytes_per_value);
        }

        self.data = result;
        Ok(())
    }
}

/// DPCM compression data and parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpcmCompression {
    pub bits_decompressed: u8,
    pub bits_compressed: u8,
    pub reserved: u8,
    pub start_value: u16,
    pub data: Vec<u8>,
}

impl DpcmCompression {
    /// Decompress DPCM data in-place.
    ///
    /// After this method succeeds, `self.data` will contain the decompressed data.
    ///
    /// DPCM compression uses delta values from a table. Each compressed value
    /// is an index into the delta table, and the delta is added to the running state.
    ///
    /// # Arguments
    /// * `table` - Decompression table containing delta values
    ///
    /// # Errors
    /// Returns error if table doesn't match compression parameters
    pub fn decompress(&mut self, table: &DecompressionTable) -> Result<(), ParseError> {
        let bytes_per_value = self.bits_decompressed.div_ceil(8) as usize;
        let mut result = Vec::new();
        let mut bitstream = BitStreamReader::new(&self.data);
        let mut state = self.start_value as i32;

        while bitstream.bits_remaining() >= self.bits_compressed as usize {
            let delta_index = bitstream.read_bits(self.bits_compressed as usize)? as usize;
            let delta = read_table_value(table, delta_index, bytes_per_value)? as i32;
            state = state.wrapping_add(delta);
            write_value_bytes(&mut result, state as u32, bytes_per_value);
        }

        self.data = result;
        Ok(())
    }
}

/// Compressed stream data block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedStream {
    pub chip_type: StreamChipType,
    pub compression_type: CompressionType,
    pub uncompressed_size: u32,
    pub compression: CompressedStreamData,
}

/// Compression-specific data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressedStreamData {
    BitPacking(BitPackingCompression),
    Dpcm(DpcmCompression),
    Unknown { compression_type: u8, data: Vec<u8> },
}

/// Decompression table (data block type 0x7F).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompressionTable {
    pub compression_type: CompressionType,
    pub sub_type: u8,
    pub bits_decompressed: u8,
    pub bits_compressed: u8,
    pub value_count: u16,
    pub table_data: Vec<u8>,
}

/// ROM/RAM dump data block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RomRamDump {
    pub chip_type: RomRamChipType,
    pub rom_size: u32,
    pub start_address: u32,
    pub data: Vec<u8>,
}

/// RAM write block (16-bit addressing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamWrite16 {
    pub chip_type: RamWrite16ChipType,
    pub start_address: u16,
    pub data: Vec<u8>,
}

/// RAM write block (32-bit addressing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamWrite32 {
    pub chip_type: RamWrite32ChipType,
    pub start_address: u32,
    pub data: Vec<u8>,
}

/// Detailed data block type after parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataBlockType {
    /// Uncompressed PCM/ADPCM stream (types 0x00-0x3F)
    UncompressedStream(UncompressedStream),
    /// Compressed stream (types 0x40-0x7E)
    CompressedStream(CompressedStream),
    /// Decompression table (type 0x7F)
    DecompressionTable(DecompressionTable),
    /// ROM/RAM dump (types 0x80-0xBF)
    RomRamDump(RomRamDump),
    /// RAM write with 16-bit addressing (types 0xC0-0xDF)
    RamWrite16(RamWrite16),
    /// RAM write with 32-bit addressing (types 0xE0-0xFF)
    RamWrite32(RamWrite32),
}

/// Parse a VGM data block into its detailed type.
///
/// This function consumes the input `DataBlock` to avoid copying large data payloads.
///
/// # Arguments
/// * `block` - The data block to parse (ownership is transferred)
///
/// # Returns
/// * `Ok(DataBlockType)` - Parsed data block with detailed type information
/// * `Err(ParseError)` - If the data is insufficient or malformed
///
/// # Errors
/// Returns error if:
/// - Data is too short for the declared block type
/// - Data format is invalid
pub fn parse_data_block(block: DataBlock) -> Result<DataBlockType, ParseError> {
    let data_type = block.data_type;
    let data = block.data;

    match data_type {
        // Uncompressed stream (0x00-0x3F)
        0x00..=0x3F => {
            let chip_type = StreamChipType::from(data_type);
            Ok(DataBlockType::UncompressedStream(UncompressedStream {
                chip_type,
                data,
            }))
        }

        // Compressed stream (0x40-0x7E)
        0x40..=0x7E => {
            if data.len() < 5 {
                return Err(ParseError::UnexpectedEof);
            }

            let chip_type = StreamChipType::from(data_type & 0x3F);
            let compression_type_byte = data[0];
            let compression_type = CompressionType::from(compression_type_byte);
            let uncompressed_size = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);

            let compression = match compression_type {
                CompressionType::BitPacking => {
                    if data.len() < 11 {
                        return Err(ParseError::UnexpectedEof);
                    }
                    let bits_decompressed = data[5];
                    let bits_compressed = data[6];
                    let sub_type = BitPackingSubType::from(data[7]);
                    let add_value = u16::from_le_bytes([data[8], data[9]]);
                    let compressed_data = data[10..].to_vec();
                    CompressedStreamData::BitPacking(BitPackingCompression {
                        bits_decompressed,
                        bits_compressed,
                        sub_type,
                        add_value,
                        data: compressed_data,
                    })
                }
                CompressionType::Dpcm => {
                    if data.len() < 10 {
                        return Err(ParseError::UnexpectedEof);
                    }
                    let bits_decompressed = data[5];
                    let bits_compressed = data[6];
                    let reserved = data[7];
                    let start_value = u16::from_le_bytes([data[8], data[9]]);
                    let compressed_data = data[10..].to_vec();
                    CompressedStreamData::Dpcm(DpcmCompression {
                        bits_decompressed,
                        bits_compressed,
                        reserved,
                        start_value,
                        data: compressed_data,
                    })
                }
                CompressionType::Unknown(_) => CompressedStreamData::Unknown {
                    compression_type: compression_type_byte,
                    data: data[5..].to_vec(),
                },
            };

            Ok(DataBlockType::CompressedStream(CompressedStream {
                chip_type,
                compression_type,
                uncompressed_size,
                compression,
            }))
        }

        // Decompression table (0x7F)
        0x7F => {
            if data.len() < 6 {
                return Err(ParseError::UnexpectedEof);
            }

            let compression_type = CompressionType::from(data[0]);
            let sub_type = data[1];
            let bits_decompressed = data[2];
            let bits_compressed = data[3];
            let value_count = u16::from_le_bytes([data[4], data[5]]);
            let table_data = data[6..].to_vec();

            Ok(DataBlockType::DecompressionTable(DecompressionTable {
                compression_type,
                sub_type,
                bits_decompressed,
                bits_compressed,
                value_count,
                table_data,
            }))
        }

        // ROM/RAM dump (0x80-0xBF)
        0x80..=0xBF => {
            if data.len() < 8 {
                return Err(ParseError::UnexpectedEof);
            }

            let chip_type = RomRamChipType::from(data_type);
            let rom_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let start_address = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            let rom_data = data[8..].to_vec();

            Ok(DataBlockType::RomRamDump(RomRamDump {
                chip_type,
                rom_size,
                start_address,
                data: rom_data,
            }))
        }

        // RAM write (16-bit addressing, 0xC0-0xDF)
        0xC0..=0xDF => {
            if data.len() < 2 {
                return Err(ParseError::UnexpectedEof);
            }

            let chip_type = RamWrite16ChipType::from(data_type);
            let start_address = u16::from_le_bytes([data[0], data[1]]);
            let ram_data = data[2..].to_vec();

            Ok(DataBlockType::RamWrite16(RamWrite16 {
                chip_type,
                start_address,
                data: ram_data,
            }))
        }

        // RAM write (32-bit addressing, 0xE0-0xFF)
        0xE0..=0xFF => {
            if data.len() < 4 {
                return Err(ParseError::UnexpectedEof);
            }

            let chip_type = RamWrite32ChipType::from(data_type);
            let start_address = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let ram_data = data[4..].to_vec();

            Ok(DataBlockType::RamWrite32(RamWrite32 {
                chip_type,
                start_address,
                data: ram_data,
            }))
        }
    }
}

/// MSB-first bitstream reader.
struct BitStreamReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8, // 0-7, where 0 is MSB
}

impl<'a> BitStreamReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn bits_remaining(&self) -> usize {
        if self.byte_pos >= self.data.len() {
            0
        } else {
            (self.data.len() - self.byte_pos) * 8 - self.bit_pos as usize
        }
    }

    fn read_bits(&mut self, num_bits: usize) -> Result<u32, ParseError> {
        if num_bits > 32 {
            return Err(ParseError::Other(
                "Cannot read more than 32 bits".to_string(),
            ));
        }

        if self.bits_remaining() < num_bits {
            return Err(ParseError::UnexpectedEof);
        }

        let mut result: u32 = 0;
        let mut bits_read = 0;

        while bits_read < num_bits {
            let bits_in_current_byte = 8 - self.bit_pos;
            let bits_to_read = (num_bits - bits_read).min(bits_in_current_byte as usize);

            let current_byte = self.data[self.byte_pos];
            let shift = bits_in_current_byte - bits_to_read as u8;
            let mask = if bits_to_read >= 8 {
                0xFF
            } else {
                (1u8 << bits_to_read) - 1
            };
            let bits = (current_byte >> shift) & mask;

            result = (result << bits_to_read) | (bits as u32);
            bits_read += bits_to_read;

            self.bit_pos += bits_to_read as u8;
            if self.bit_pos >= 8 {
                self.byte_pos += 1;
                self.bit_pos = 0;
            }
        }

        Ok(result)
    }
}

/// Read a value from a decompression table.
fn read_table_value(
    table: &DecompressionTable,
    index: usize,
    bytes_per_value: usize,
) -> Result<u32, ParseError> {
    let start = index * bytes_per_value;
    let end = start + bytes_per_value;

    if end > table.table_data.len() {
        return Err(ParseError::DataInconsistency(format!(
            "Table index {} out of bounds (table size: {} bytes, value size: {} bytes)",
            index,
            table.table_data.len(),
            bytes_per_value
        )));
    }

    let mut value: u32 = 0;
    for i in 0..bytes_per_value {
        value |= (table.table_data[start + i] as u32) << (i * 8);
    }
    Ok(value)
}

/// Write a multi-byte value in little-endian format.
fn write_value_bytes(output: &mut Vec<u8>, value: u32, bytes: usize) {
    for i in 0..bytes {
        output.push(((value >> (i * 8)) & 0xFF) as u8);
    }
}
