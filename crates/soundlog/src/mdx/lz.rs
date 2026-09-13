//! LZ compression and decompression helpers for MDX and PDX streams.
//!
//! Decompression core based on the token model of Fabrice Bellard's
//! MIT-licensed original LZEXE source code, with X68000 stream-layout
//! differences verified from compressed/decoded MDX and PDX samples.
//!
//! Responsibilities:
//! - Decode MDX/PDX LZ streams with bounded output and input validation.
//! - Encode byte slices using the same marker and back-reference format.
//! - Report structural failures without panicking on truncated input.

use std::collections::HashMap;
use std::result::Result as StdResult;

/// Maximum back-reference distance considered by the encoder, in bytes.
///
/// This is the 4 KiB history window supported by the long-reference token
/// format.
const MAX_DISTANCE: usize = 4096;
/// Maximum match length considered by the encoder, in bytes.
///
/// Long-reference tokens encode lengths up to 256 bytes.
const MAX_MATCH_LENGTH: usize = 256;

/// Result of an LZ decoding operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Result {
    Ok,
    InvalidArgument,
    MarkerNotFound,
    InputOverrun,
    OutputOverrun,
    InvalidBackReference,
}

/// Information returned by [`decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeResult {
    pub result: Result,
    pub bytes_written: usize,
    pub bytes_read: usize,
}

/// Encode bytes as an LZ stream accepted by [`decode`].
///
/// The encoder uses a greedy match search over the preceding 4 KiB. Short
/// references cover distances up to 256 and lengths up to 5; long references
/// cover distances up to 4096 and lengths up to 256.
pub fn encode(source: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::new();
    let mut position = 0;
    let mut candidates = HashMap::<[u8; 3], Vec<usize>>::new();

    while position < source.len() {
        let matched = find_match(source, position, &mut candidates);
        if let Some((distance, length)) = matched {
            if distance <= 256 && length <= 5 {
                writer.write_bit(0);
                writer.write_bit(0);
                writer.write_bits((length - 2) as u8, 2);
                writer.write_byte((256 - distance) as u8);
            } else {
                writer.write_bit(0);
                writer.write_bit(1);
                let short_length = (3..=9).contains(&length);
                let word = ((-(distance as i32)) << 3) as u16
                    | if short_length { (length - 2) as u16 } else { 0 };
                writer.write_byte((word >> 8) as u8);
                writer.write_byte(word as u8);
                if !short_length {
                    writer.write_byte((length - 1) as u8);
                }
            }
            add_candidates(source, position, length, &mut candidates);
            position += length;
        } else {
            writer.write_bit(1);
            writer.write_byte(source[position]);
            add_candidates(source, position, 1, &mut candidates);
            position += 1;
        }
    }

    // Long reference with a zero length is the stream terminator.
    writer.write_bit(0);
    writer.write_bit(1);
    writer.write_byte(0);
    writer.write_byte(0);
    writer.write_byte(0);
    writer.finish()
}

/// Decode an LZ stream into `destination`.
///
/// The destination is written from its beginning. A successful stream ends at
/// the zero-length back-reference token and returns the number of bytes written
/// and consumed from `source`.
pub fn decode(source: &[u8], destination: &mut [u8]) -> DecodeResult {
    let mut reader = BitReader::new(source);
    let mut output_position = 0;

    if source.is_empty() || destination.is_empty() {
        return make_result(Result::InvalidArgument, output_position, &reader);
    }

    loop {
        let bit = match reader.read_bit() {
            Ok(bit) => bit,
            Err(result) => return make_result(result, output_position, &reader),
        };

        if bit != 0 {
            if output_position >= destination.len() {
                return make_result(Result::OutputOverrun, output_position, &reader);
            }
            match reader.read_byte() {
                Ok(byte) => destination[output_position] = byte,
                Err(result) => return make_result(result, output_position, &reader),
            }
            output_position += 1;
            continue;
        }

        let bit = match reader.read_bit() {
            Ok(bit) => bit,
            Err(result) => return make_result(result, output_position, &reader),
        };

        if bit == 0 {
            let b0 = match reader.read_bit() {
                Ok(bit) => bit,
                Err(result) => return make_result(result, output_position, &reader),
            };
            let b1 = match reader.read_bit() {
                Ok(bit) => bit,
                Err(result) => return make_result(result, output_position, &reader),
            };
            let low = match reader.read_byte() {
                Ok(byte) => byte,
                Err(result) => return make_result(result, output_position, &reader),
            };
            let length = usize::from((b0 << 1) | b1) + 2;
            if let Err(result) = copy_from_history(
                destination,
                &mut output_position,
                -256 + i32::from(low),
                length,
            ) {
                return make_result(result, output_position, &reader);
            }
            continue;
        }

        let high = match reader.read_byte() {
            Ok(byte) => byte,
            Err(result) => return make_result(result, output_position, &reader),
        };
        let low = match reader.read_byte() {
            Ok(byte) => byte,
            Err(result) => return make_result(result, output_position, &reader),
        };

        let packed = 0xffff0000u32 | (u32::from(high) << 8) | u32::from(low);
        let offset = (packed as i32) >> 3;
        let code = low & 0x07;
        if code != 0 {
            if let Err(result) = copy_from_history(
                destination,
                &mut output_position,
                offset,
                usize::from(code) + 2,
            ) {
                return make_result(result, output_position, &reader);
            }
            continue;
        }

        let extra = match reader.read_byte() {
            Ok(byte) => byte,
            Err(result) => return make_result(result, output_position, &reader),
        };
        if extra == 0 {
            return make_result(Result::Ok, output_position, &reader);
        }

        if let Err(result) = copy_from_history(
            destination,
            &mut output_position,
            offset,
            usize::from(extra) + 1,
        ) {
            return make_result(result, output_position, &reader);
        }
    }
}

/// A bit-level reader for the LZ-compressed MDX stream.
struct BitReader<'a> {
    source: &'a [u8],
    position: usize,
    bits_left: u8,
    current: u8,
}

/// Implementation of the `BitReader` struct.
impl<'a> BitReader<'a> {
    fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            position: 0,
            bits_left: 0,
            current: 0,
        }
    }

    fn read_byte(&mut self) -> StdResult<u8, Result> {
        let byte = self
            .source
            .get(self.position)
            .copied()
            .ok_or(Result::InputOverrun)?;
        self.position += 1;
        Ok(byte)
    }

    fn read_bit(&mut self) -> StdResult<u8, Result> {
        if self.bits_left == 0 {
            self.current = self.read_byte()?;
            self.bits_left = 8;
        }
        let bit = self.current >> 7;
        self.current <<= 1;
        self.bits_left -= 1;
        Ok(bit)
    }
}

/// Copies a sequence of bytes from the previously decompressed data (history) to the current output position.
/// Returns an error if the back reference is invalid or if the output would overrun the destination buffer.
fn copy_from_history(
    destination: &mut [u8],
    output_position: &mut usize,
    offset: i32,
    length: usize,
) -> StdResult<(), Result> {
    if offset >= 0 {
        return Err(Result::InvalidBackReference);
    }
    let distance = usize::try_from(-offset).map_err(|_| Result::InvalidBackReference)?;
    if distance == 0 || distance > *output_position {
        return Err(Result::InvalidBackReference);
    }
    if length > destination.len() - *output_position {
        return Err(Result::OutputOverrun);
    }

    for source_position in (*output_position - distance..).take(length) {
        destination[*output_position] = destination[source_position];
        *output_position += 1;
    }
    Ok(())
}

/// Finds the longest match for the current position in the source buffer from the candidate positions.
/// Returns the distance and length of the best match if found.
fn find_match(
    source: &[u8],
    position: usize,
    candidates: &mut HashMap<[u8; 3], Vec<usize>>,
) -> Option<(usize, usize)> {
    if position + 2 >= source.len() {
        return None;
    }

    let key = [source[position], source[position + 1], source[position + 2]];
    let positions = candidates.get_mut(&key)?;
    let first_valid = position.saturating_sub(MAX_DISTANCE);
    positions.retain(|&candidate| candidate >= first_valid);

    let mut best = None;
    for &candidate in positions.iter().rev().take(64) {
        let distance = position - candidate;
        let maximum = (source.len() - position).min(MAX_MATCH_LENGTH);
        let mut length = 0;
        while length < maximum && source[candidate + length] == source[position + length] {
            length += 1;
        }
        if length >= 2 && best.is_none_or(|(_, best_length)| length > best_length) {
            best = Some((distance, length));
        }
    }
    best
}

/// Adds candidate positions for potential matches to the hash map.
///
/// `source` is the input buffer, `position` is the starting position of the new data,
/// `length` is the number of bytes to consider, and `candidates` is the hash map storing
/// previously seen positions keyed by 3-byte sequences.
fn add_candidates(
    source: &[u8],
    position: usize,
    length: usize,
    candidates: &mut HashMap<[u8; 3], Vec<usize>>,
) {
    let end = (position + length).min(source.len().saturating_sub(2));
    for candidate in position..end {
        candidates
            .entry([
                source[candidate],
                source[candidate + 1],
                source[candidate + 2],
            ])
            .or_default()
            .push(candidate);
    }
}

/// A bit-level writer for the LZ-compressed MDX stream.
struct BitWriter {
    bytes: Vec<u8>,
    control_position: usize,
    bits_written: u8,
}

/// Implementation of the `BitWriter` struct.
impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: vec![0],
            control_position: 0,
            bits_written: 0,
        }
    }

    fn write_bit(&mut self, bit: u8) {
        if self.bits_written == 8 {
            self.bytes.push(0);
            self.control_position = self.bytes.len() - 1;
            self.bits_written = 0;
        }
        self.bytes[self.control_position] |= (bit & 1) << (7 - self.bits_written);
        self.bits_written += 1;
    }

    fn write_bits(&mut self, value: u8, count: u8) {
        for shift in (0..count).rev() {
            self.write_bit(value >> shift);
        }
    }

    fn write_byte(&mut self, byte: u8) {
        self.bytes.push(byte);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Constructs a `DecodeResult` from the given result, bytes written, and bit reader position.
fn make_result(result: Result, bytes_written: usize, reader: &BitReader<'_>) -> DecodeResult {
    DecodeResult {
        result,
        bytes_written,
        bytes_read: reader.position,
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeResult, Result, decode, encode};
    use std::iter;

    #[test]
    fn rejects_invalid_decode_arguments() {
        let mut output = [0u8; 1];
        assert_eq!(decode(&[], &mut output).result, Result::InvalidArgument);
        assert_eq!(decode(&[0xff], &mut []).result, Result::InvalidArgument);
    }

    #[test]
    fn decodes_literal_and_end_tokens() {
        // Control bits: literal, literal, end. Literal bytes are A and B.
        let source = [0b1101_0000, b'A', b'B', 0, 0, 0];
        let mut output = [0u8; 2];
        let result = decode(&source, &mut output);

        assert_eq!(
            result,
            DecodeResult {
                result: Result::Ok,
                bytes_written: 2,
                bytes_read: 6
            }
        );
        assert_eq!(&output, b"AB");
    }

    #[test]
    fn rejects_invalid_back_reference() {
        // Control bits: back-reference with distance 256 before any output.
        let source = [0b0000_0000, 0, 0, 0, 0];
        let mut output = [0u8; 4];
        assert_eq!(
            decode(&source, &mut output).result,
            Result::InvalidBackReference
        );
    }

    #[test]
    fn encoder_round_trips_literals_and_short_reference() {
        let source = b"0123456701234567";
        let encoded = encode(source);
        let mut decoded = vec![0; source.len()];

        let result = decode(&encoded, &mut decoded);

        assert_eq!(result.result, Result::Ok);
        assert_eq!(result.bytes_written, source.len());
        assert_eq!(decoded, source);
    }

    #[test]
    fn encoder_round_trips_long_reference_and_control_boundaries() {
        let mut source = Vec::from(&b"0123456789abcdef"[..]);
        source.extend(iter::repeat_n(b'X', 300));
        source.extend((0..80).map(|value| value as u8));
        let encoded = encode(&source);
        let mut decoded = vec![0; source.len()];

        let result = decode(&encoded, &mut decoded);

        assert_eq!(result.result, Result::Ok);
        assert_eq!(result.bytes_written, source.len());
        assert_eq!(decoded, source);
        assert!(encoded.len() < source.len());
    }
}
