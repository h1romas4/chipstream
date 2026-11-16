//! Shared ADPCM table based on MAME's OKIM6258 implementation, revision
//! `70743c6fb2602a5c2666c679b618706eabfca2ad`.
//! Copyright (c) Barry Rodewald.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

pub(super) const INDEX_SHIFT: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Stateful OKIM6258 4-bit ADPCM encoder and decoder.
///
/// The predictor has 12-bit precision, independent of the chip's DAC output
/// precision. Byte operations process the low nibble first. Encoding advances
/// the same predictor as decoding, so separate streams need separate states.
#[cfg(feature = "okim6258")]
#[derive(Clone, Copy)]
pub struct Okim6258Adpcm {
    signal: i32,
    step: usize,
    differences: &'static [[i32; 16]; 49],
}

#[cfg(feature = "okim6258")]
impl Default for Okim6258Adpcm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "okim6258")]
impl Okim6258Adpcm {
    /// Create a predictor in its initial state.
    pub fn new() -> Self {
        static DIFFERENCES: std::sync::OnceLock<[[i32; 16]; 49]> = std::sync::OnceLock::new();
        Self {
            signal: -2,
            step: 0,
            differences: DIFFERENCES.get_or_init(difference_table),
        }
    }

    /// Reset the predictor and step index.
    pub fn reset(&mut self) {
        self.signal = -2;
        self.step = 0;
    }

    /// Return the current signed 12-bit predictor.
    pub fn signal(&self) -> i32 {
        self.signal
    }

    /// Return the current ADPCM step index, from 0 to 48.
    pub fn step_index(&self) -> usize {
        self.step
    }

    /// Decode the low four bits and return the updated predictor.
    pub fn decode_low_nibble(&mut self, value: u8) -> i32 {
        let nibble = value & 0x0f;
        self.signal =
            (self.signal + self.differences[self.step][usize::from(nibble)]).clamp(-2048, 2047);
        self.step = (self.step as i32 + INDEX_SHIFT[usize::from(nibble & 7)]).clamp(0, 48) as usize;
        self.signal
    }

    /// Decode a byte into two predictor values, low nibble first.
    pub fn decode_pair(&mut self, value: u8) -> [i32; 2] {
        [
            self.decode_low_nibble(value),
            self.decode_low_nibble(value >> 4),
        ]
    }

    /// Encode two signed samples as a byte, low nibble first.
    ///
    /// Inputs outside the 12-bit predictor range are clamped.
    pub fn encode_pair(&mut self, samples: [i32; 2]) -> u8 {
        let low = self.encode_nibble(samples[0]);
        let high = self.encode_nibble(samples[1]);
        low | (high << 4)
    }

    fn encode_nibble(&mut self, target: i32) -> u8 {
        let mut difference = target.clamp(-2048, 2047) - self.signal;
        let mut nibble = 0;
        if difference < 0 {
            nibble = 8;
            difference = -difference;
        }
        let step = STEPS[self.step];
        if difference >= step {
            nibble |= 4;
            difference -= step;
        }
        if difference >= step / 2 {
            nibble |= 2;
            difference -= step / 2;
        }
        if difference >= step / 4 {
            nibble |= 1;
        }
        self.decode_low_nibble(nibble);
        nibble
    }
}

/// Build signed ADPCM predictor differences for each step and nibble.
const STEPS: [i32; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

pub(super) fn difference_table() -> [[i32; 16]; 49] {
    std::array::from_fn(|step| {
        let step_value = STEPS[step];
        std::array::from_fn(|nibble| {
            let magnitude = step_value / 8
                + if nibble & 1 != 0 { step_value / 4 } else { 0 }
                + if nibble & 2 != 0 { step_value / 2 } else { 0 }
                + if nibble & 4 != 0 { step_value } else { 0 };
            if nibble & 8 != 0 {
                -magnitude
            } else {
                magnitude
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::difference_table;

    #[cfg(feature = "okim6258")]
    use super::Okim6258Adpcm;

    #[test]
    fn difference_table_matches_reference_for_every_step_and_nibble() {
        let steps = [
            16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
            118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544,
            598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552,
        ];
        let table = difference_table();
        for (index, step) in steps.into_iter().enumerate() {
            for nibble in 0..8 {
                let expected = step / 8
                    + step / 4 * (nibble & 1) as i32
                    + step / 2 * ((nibble >> 1) & 1) as i32
                    + step * ((nibble >> 2) & 1) as i32;
                assert_eq!(table[index][nibble], expected);
                assert_eq!(table[index][nibble | 8], -expected);
            }
        }
    }

    #[cfg(feature = "okim6258")]
    #[test]
    fn okim6258_pair_decoding_matches_individual_nibbles_and_reset() {
        let mut pair = Okim6258Adpcm::new();
        let mut nibble = Okim6258Adpcm::new();
        for byte in 0..=u8::MAX {
            assert_eq!(
                pair.decode_pair(byte),
                [
                    nibble.decode_low_nibble(byte),
                    nibble.decode_low_nibble(byte >> 4)
                ]
            );
            assert_eq!(pair.signal(), nibble.signal());
            assert_eq!(pair.step_index(), nibble.step_index());
        }
        pair.reset();
        assert_eq!(pair.signal(), -2);
        assert_eq!(pair.step_index(), 0);
        assert_eq!(pair.decode_low_nibble(0xf1), 4);
    }

    #[cfg(feature = "okim6258")]
    #[test]
    fn okim6258_encoder_and_decoder_remain_in_sync() {
        let mut encoder = Okim6258Adpcm::new();
        let mut decoder = Okim6258Adpcm::new();
        assert_eq!(encoder.encode_pair([0, 0]), 0x00);
        assert_eq!(encoder.encode_pair([2047, -2048]), 0xf7);
        assert_eq!(encoder.signal(), -31);
        assert_eq!(encoder.step_index(), 16);
        encoder.reset();
        let samples = [i32::MIN, -2048, -200, 0, 100, 2047, i32::MAX];
        for i in 0..1000 {
            let byte =
                encoder.encode_pair([samples[i % samples.len()], samples[(i + 1) % samples.len()]]);
            decoder.decode_pair(byte);
            assert_eq!(encoder.signal(), decoder.signal());
            assert_eq!(encoder.step_index(), decoder.step_index());
        }
    }
}
