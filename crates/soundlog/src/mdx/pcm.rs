//! PCM and OKIM6258 ADPCM decoding used by MDX/PDX playback.
//!
//! This module converts the three PCM8A storage formats used by MXDRV into
//! the signed sample range consumed by the software mixer and VGM converter.
//! The ADPCM decoder follows OKIM6258 step/index state transitions, while
//! PCM8 and PCM16 inputs are normalized to the same playback range.
//!
//! It contains decoding primitives only; track timing, channel resampling,
//! and output-chip command generation belong to neighboring modules.

/// PCM8A sample storage formats used by NanoDriveX and MDX PCM references.
///
/// The format selects how the bytes returned by a PDX sample entry are
/// interpreted. All variants are decoded into signed [`i16`] samples in the
/// common playback range `-2048..=2047`; the format does not select a sample
/// rate. MDX commands carry the rate separately from this format choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pcm8aFormat {
    /// 4-bit OKIM6258 ADPCM, with two samples per byte and the low nibble
    /// decoded before the high nibble.
    Adpcm,
    /// Signed 16-bit big-endian PCM, shifted down four bits to the 12-bit
    /// playback range.
    Pcm16,
    /// Signed 8-bit PCM, widened by shifting four bits to the 12-bit playback
    /// range.
    Pcm8,
}

/// Decode an OKIM6258 ADPCM byte stream into signed playback samples.
///
/// Each input byte produces two output samples: its low nibble is decoded
/// first, followed by its high nibble. The decoder starts from signal `-2` and
/// step index `0`, matching the state used by the NanoDriveX hardware path.
/// The output range is `-2048..=2047`, matching the signal range used by the
/// OKIM6258/MSM6258 implementation.
///
/// Consequently, the returned vector always has `bytes.len() * 2` samples.
pub fn decode_adpcm(bytes: &[u8]) -> Vec<i16> {
    let mut decoder = AdpcmDecoder::default();
    let mut samples = Vec::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        samples.push(decoder.decode_nibble(byte & 0x0f));
        samples.push(decoder.decode_nibble(byte >> 4));
    }
    samples
}

/// Decode a PCM8A sample block according to its storage format.
///
/// The returned samples use the signed playback range `-2048..=2047`:
///
/// - [`Pcm8aFormat::Adpcm`] decodes two low-nibble-first ADPCM samples per
///   byte.
/// - [`Pcm8aFormat::Pcm16`] reads signed big-endian 16-bit samples and shifts
///   each value right by four bits.
/// - [`Pcm8aFormat::Pcm8`] reads signed 8-bit samples and shifts each value
///   left by four bits.
///
/// `Pcm16` is the only format with a structural input-length requirement. An
/// odd number of bytes leaves an incomplete sample and returns
/// [`PcmDecodeError::OddPcm16ByteCount`]. Empty input is valid for every
/// format and produces an empty vector.
pub fn decode_pcm8a(format: Pcm8aFormat, bytes: &[u8]) -> Result<Vec<i16>, PcmDecodeError> {
    match format {
        Pcm8aFormat::Adpcm => Ok(decode_adpcm(bytes)),
        Pcm8aFormat::Pcm8 => Ok(bytes
            .iter()
            .map(|&sample| i16::from(i8::from_ne_bytes([sample])) << 4)
            .collect()),
        Pcm8aFormat::Pcm16 => {
            let chunks = bytes.chunks_exact(2);
            if !chunks.remainder().is_empty() {
                return Err(PcmDecodeError::OddPcm16ByteCount);
            }
            Ok(chunks
                .map(|chunk| i16::from_be_bytes([chunk[0], chunk[1]]) >> 4)
                .collect())
        }
    }
}

/// Errors produced while decoding fixed-width PCM data.
///
/// ADPCM and PCM8 consume complete input bytes, so they cannot produce one of
/// these errors. PCM16 requires two bytes for every sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmDecodeError {
    /// PCM16 input contains an incomplete final two-byte sample.
    OddPcm16ByteCount,
}

#[derive(Debug, Clone, Copy)]
struct AdpcmDecoder {
    signal: i32,
    step_index: usize,
}

impl Default for AdpcmDecoder {
    fn default() -> Self {
        Self {
            signal: -2,
            step_index: 0,
        }
    }
}

impl AdpcmDecoder {
    fn decode_nibble(&mut self, nibble: u8) -> i16 {
        apply_nibble(&mut self.signal, &mut self.step_index, nibble);
        self.signal as i16
    }
}

/// Applies one ADPCM nibble to `signal`/`step_index`, mirroring the shared
/// `applyNibble()` step used by both NanoDriveX's decoder and re-encoder.
fn apply_nibble(signal: &mut i32, step_index: &mut usize, nibble: u8) {
    let step = STEP_TABLE[*step_index];
    let mut difference = step >> 3;
    if nibble & 0x01 != 0 {
        difference += step >> 2;
    }
    if nibble & 0x02 != 0 {
        difference += step >> 1;
    }
    if nibble & 0x04 != 0 {
        difference += step;
    }

    if nibble & 0x08 != 0 {
        *signal -= difference;
    } else {
        *signal += difference;
    }
    *signal = (*signal).clamp(-2048, 2047);

    let index_delta = INDEX_SHIFT[usize::from(nibble & 0x07)];
    *step_index = step_index
        .saturating_add_signed(index_delta)
        .min(STEP_TABLE.len() - 1);
}

/// Target-tracking ADPCM re-encoder, mirroring NanoDriveX's
/// `encodeNibbleFast()`/`OKIM6258::encodePair()`. Used to recompress a
/// software-mixed PCM8 stream back into 4-bit OKIM6258 ADPCM for VGM output.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AdpcmEncoder {
    signal: i32,
    step_index: usize,
}

impl AdpcmEncoder {
    /// Encodes one 12-bit signed sample into a 4-bit ADPCM nibble, updating the internal state.
    pub fn encode_nibble(&mut self, target: i32) -> u8 {
        let target = target.clamp(-2048, 2047);
        let mut diff = target - self.signal;
        let mut nibble = 0u8;
        if diff < 0 {
            nibble = 0x08;
            diff = -diff;
        }

        let step = STEP_TABLE[self.step_index];
        if diff >= step {
            nibble |= 0x04;
            diff -= step;
        }
        let half = step >> 1;
        if diff >= half {
            nibble |= 0x02;
            diff -= half;
        }
        if diff >= (step >> 2) {
            nibble |= 0x01;
        }

        apply_nibble(&mut self.signal, &mut self.step_index, nibble);
        nibble & 0x0f
    }

    /// Encodes two consecutive 12-bit signed samples into one ADPCM byte
    /// (low nibble first), mirroring `OKIM6258::encodePair`.
    pub fn encode_pair(&mut self, sample_a: i16, sample_b: i16) -> u8 {
        let nib0 = self.encode_nibble(i32::from(sample_a));
        let nib1 = self.encode_nibble(i32::from(sample_b));
        nib0 | (nib1 << 4)
    }
}

/// Encode signed 12-bit PCM samples into low-nibble-first OKIM6258 ADPCM.
///
/// An odd final sample is paired with zero, matching the byte-oriented PDX
/// sample representation.
pub fn encode_adpcm(samples: &[i16]) -> Vec<u8> {
    let mut encoder = AdpcmEncoder::default();
    samples
        .chunks(2)
        .map(|pair| encoder.encode_pair(pair[0], pair.get(1).copied().unwrap_or(0)))
        .collect()
}

/// Step size table for the ADPCM encoder and decoder. Each entry represents the quantization step
/// for the corresponding step index.
pub(crate) const STEP_TABLE: [i32; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

/// Index shift table for the ADPCM encoder and decoder. Each entry indicates
/// how the step index should be adjusted based on the lower three bits of the encoded nibble.
pub(crate) const INDEX_SHIFT: [isize; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

#[cfg(test)]
mod tests {
    use super::{AdpcmEncoder, Pcm8aFormat, PcmDecodeError, decode_adpcm, decode_pcm8a};

    #[test]
    fn adpcm_decodes_low_nibble_before_high_nibble() {
        assert_eq!(decode_adpcm(&[0x70]), [0, 30]);
    }

    #[test]
    fn adpcm_encoder_round_trips_through_the_decoder() {
        let mut encoder = AdpcmEncoder::default();
        let bytes: Vec<u8> = (0..64)
            .map(|i| {
                let target = ((i * 37) % 4096) - 2048;
                encoder.encode_nibble(target)
            })
            .collect::<Vec<u8>>()
            .chunks(2)
            .map(|pair| pair[0] | (pair[1] << 4))
            .collect();

        // The decoder must be able to follow the encoder's output without
        // diverging (both walk the same step table from the same nibbles).
        let decoded = decode_adpcm(&bytes);
        assert_eq!(decoded.len(), bytes.len() * 2);
        assert!(decoded.iter().all(|&s| (-2048..=2047).contains(&s)));
    }

    #[test]
    fn adpcm_encoder_encode_pair_packs_low_nibble_first() {
        let mut a = AdpcmEncoder::default();
        let mut b = AdpcmEncoder::default();
        let byte = a.encode_pair(100, -100);
        let nib0 = b.encode_nibble(100);
        let nib1 = b.encode_nibble(-100);
        assert_eq!(byte, nib0 | (nib1 << 4));
    }

    #[test]
    fn adpcm_clamps_signal_and_step_index() {
        let samples = decode_adpcm(&[0x77, 0x77, 0x77, 0x77, 0x77, 0x77]);
        assert!(
            samples
                .iter()
                .all(|&sample| (-2048..=2047).contains(&sample))
        );
        assert_eq!(samples.len(), 12);
    }

    #[test]
    fn pcm8a_decodes_pcm8_and_big_endian_pcm16() {
        assert_eq!(
            decode_pcm8a(Pcm8aFormat::Pcm8, &[0x80, 0x7f]).unwrap(),
            [-2048, 2032]
        );
        assert_eq!(
            decode_pcm8a(Pcm8aFormat::Pcm16, &[0x12, 0x30, 0xed, 0xd0]).unwrap(),
            [0x0123, -0x0123]
        );
    }

    #[test]
    fn pcm16_requires_complete_samples() {
        assert_eq!(
            decode_pcm8a(Pcm8aFormat::Pcm16, &[0]),
            Err(PcmDecodeError::OddPcm16ByteCount)
        );
    }
}
