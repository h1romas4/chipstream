//! Shared ADPCM table based on MAME's OKIM6258 implementation, revision
//! `70743c6fb2602a5c2666c679b618706eabfca2ad`.
//! Copyright (c) Barry Rodewald.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

pub(super) const INDEX_SHIFT: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

pub(super) fn difference_table() -> [[i32; 16]; 49] {
    const STEPS: [i32; 49] = [
        16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118,
        130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658,
        724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552,
    ];
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
}
