//! Shared ADPCM table based on MAME's OKIM6258 implementation, revision
//! `70743c6fb2602a5c2666c679b618706eabfca2ad`.
//! Copyright (c) Barry Rodewald.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

pub(super) const INDEX_SHIFT: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

#[derive(Clone)]
pub(crate) struct OkiAdpcmState {
    signal: i32,
    step: usize,
    limit: i32,
    differences: [[i32; 16]; 49],
}

impl OkiAdpcmState {
    /// Initialize a decoder with the selected signal width.
    pub(crate) fn new(output_bits: u8) -> Self {
        Self {
            signal: -2,
            step: 0,
            limit: 1_i32 << (output_bits - 1),
            differences: difference_table(),
        }
    }

    /// Decode both nibbles of a byte in OKIM6258 playback order.
    pub(crate) fn decode_byte(&mut self, value: u8) {
        self.decode_nibble(value & 0x0f);
        self.decode_nibble(value >> 4);
    }

    pub(crate) fn reset(&mut self) {
        self.signal = -2;
        self.step = 0;
    }

    pub(crate) fn signal(&self) -> i32 {
        self.signal
    }

    pub(crate) fn decode_pair(&mut self, value: u8) -> [i32; 2] {
        self.decode_nibble(value & 0x0f);
        let low = self.signal;
        self.decode_nibble(value >> 4);
        [low, self.signal]
    }

    pub(crate) fn encode_pair(&mut self, samples: [i32; 2]) -> u8 {
        let low = self.encode_nibble(samples[0]);
        let high = self.encode_nibble(samples[1]);
        low | (high << 4)
    }

    /// Encode one signed sample as a four-bit OKI ADPCM code.
    fn encode_nibble(&mut self, target: i32) -> u8 {
        let target = target.clamp(-self.limit, self.limit - 1);
        let mut difference = target - self.signal;
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
        self.decode_nibble(nibble);
        nibble
    }

    /// Encode two near-zero nibbles and advance the decoder state.
    pub(crate) fn encode_zero_byte(&mut self) -> u8 {
        let low = self.encode_zero_nibble();
        let high = self.encode_zero_nibble();
        low | (high << 4)
    }

    /// Choose the repeated near-zero byte best suited to the current state.
    pub(crate) fn zero_pair(&self) -> u8 {
        let score = |byte| {
            let mut state = self.clone();
            let mut accumulated = 0_i32;
            for _ in 0..32 {
                state.decode_byte(byte);
                accumulated += state.signal.abs();
            }
            (accumulated, state.signal.abs(), state.step)
        };
        if score(0x80) <= score(0x08) {
            0x80
        } else {
            0x08
        }
    }

    /// Advance the predictor and step index by one ADPCM nibble.
    fn decode_nibble(&mut self, nibble: u8) {
        self.signal = (self.signal + self.differences[self.step][usize::from(nibble)])
            .clamp(-self.limit, self.limit - 1);
        self.step = (self.step as i32 + INDEX_SHIFT[usize::from(nibble & 7)]).clamp(0, 48) as usize;
    }

    /// Select and decode the nibble that brings the predictor closest to zero.
    fn encode_zero_nibble(&mut self) -> u8 {
        let nibble = (0..16)
            .min_by_key(|nibble| {
                let signal = (self.signal + self.differences[self.step][*nibble])
                    .clamp(-self.limit, self.limit - 1);
                let step = (self.step as i32 + INDEX_SHIFT[*nibble & 7]).clamp(0, 48) as usize;
                (signal.abs(), step, *nibble)
            })
            .expect("nibble range is nonempty");
        self.decode_nibble(nibble as u8);
        nibble as u8
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
    use super::{OkiAdpcmState, difference_table};

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

    #[test]
    fn zero_encoder_converges_from_a_loud_state() {
        let mut state = OkiAdpcmState::new(12);
        for _ in 0..32 {
            state.decode_byte(0x77);
        }
        for _ in 0..256 {
            state.encode_zero_byte();
        }

        assert!(state.signal.abs() <= 2);
        assert_eq!(state.step, 0);
        let pair = state.zero_pair();
        assert!(matches!(pair, 0x80 | 0x08));
        for _ in 0..32 {
            state.decode_byte(pair);
        }
        assert!(state.signal.abs() <= 2);
        assert_eq!(state.step, 0);
    }
}
