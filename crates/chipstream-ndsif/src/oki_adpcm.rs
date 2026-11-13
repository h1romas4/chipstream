//! Near-zero ADPCM output for the continuous NDSIF hardware stream.

use chipstream::chip::Okim6258Adpcm;

/// Encode two near-zero nibbles and advance the predictor.
pub(crate) fn encode_zero_byte(state: &mut Okim6258Adpcm) -> u8 {
    fn encode_zero_nibble(state: &mut Okim6258Adpcm) -> u8 {
        let nibble = (0..16)
            .min_by_key(|&nibble| {
                let mut candidate = *state;
                let signal = candidate.decode_low_nibble(nibble);
                (signal.abs(), candidate.step_index(), nibble)
            })
            .expect("nibble range is nonempty");
        state.decode_low_nibble(nibble);
        nibble
    }

    let low = encode_zero_nibble(state);
    let high = encode_zero_nibble(state);
    low | (high << 4)
}

/// Choose the repeated near-zero byte best suited to the current predictor.
pub(crate) fn zero_pair(state: &Okim6258Adpcm) -> u8 {
    let score = |byte| {
        let mut candidate = *state;
        let mut accumulated = 0_i32;
        for _ in 0..32 {
            candidate.decode_pair(byte);
            accumulated += candidate.signal().abs();
        }
        (
            accumulated,
            candidate.signal().abs(),
            candidate.step_index(),
        )
    };
    if score(0x80) <= score(0x08) {
        0x80
    } else {
        0x08
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_encoder_converges_from_a_loud_state() {
        let mut state = Okim6258Adpcm::new();
        for _ in 0..32 {
            state.decode_pair(0x77);
        }
        for _ in 0..256 {
            encode_zero_byte(&mut state);
        }

        assert!(state.signal().abs() <= 2);
        assert_eq!(state.step_index(), 0);
        let pair = zero_pair(&state);
        assert!(matches!(pair, 0x80 | 0x08));
        for _ in 0..32 {
            state.decode_pair(pair);
        }
        assert!(state.signal().abs() <= 2);
        assert_eq!(state.step_index(), 0);
    }
}
