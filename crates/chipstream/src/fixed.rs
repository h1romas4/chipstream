//! Fixed-point timing arithmetic using only 32-bit integer operations.

fn double_remainder(value: u32, divisor: u32, bit: u32) -> (u32, bool) {
    let (doubled, overflow) = value.overflowing_add(value);
    let (sum, extra) = doubled.overflowing_add(bit);
    if overflow || extra || sum >= divisor {
        (sum.wrapping_sub(divisor), true)
    } else {
        (sum, false)
    }
}

fn multiply_divide(a: u32, b: u32, divisor: u32) -> Option<(u32, u32)> {
    if divisor == 0 {
        return None;
    }
    if let Some(product) = a.checked_mul(b) {
        return Some((product / divisor, product % divisor));
    }
    let whole_a = a / divisor;
    let part_a = a % divisor;
    let mut whole = 0_u32;
    let mut remainder = 0_u32;
    for bit in (0..32).rev() {
        whole = whole.checked_mul(2)?;
        let (next, carry) = double_remainder(remainder, divisor, 0);
        remainder = next;
        whole = whole.checked_add(u32::from(carry))?;
        if b & (1 << bit) != 0 {
            whole = whole.checked_add(whole_a)?;
            let (sum, overflow) = remainder.overflowing_add(part_a);
            if overflow || sum >= divisor {
                remainder = sum.wrapping_sub(divisor);
                whole = whole.checked_add(1)?;
            } else {
                remainder = sum;
            }
        }
    }
    Some((whole, remainder))
}

fn rounded_fraction(mut whole: u32, mut remainder: u32, divisor: u32) -> Option<(u32, u32)> {
    let mut fraction = 0_u32;
    for _ in 0..32 {
        let (next, carry) = double_remainder(remainder, divisor, 0);
        remainder = next;
        fraction = (fraction << 1) | u32::from(carry);
    }
    let (_, round_up) = double_remainder(remainder, divisor, 0);
    let (fraction, carry) = fraction.overflowing_add(u32::from(round_up));
    whole = whole.checked_add(u32::from(carry))?;
    Some((whole, fraction))
}

/// Return the integer and Q0.32 fractional parts of `a * b / divisor`.
pub(crate) fn multiply_ratio(a: u32, b: u32, divisor: u32) -> Option<(u32, u32)> {
    let (whole, remainder) = multiply_divide(a, b, divisor)?;
    rounded_fraction(whole, remainder, divisor)
}

/// Return the integer and Q0.32 fractional parts of `n / (a * b)`.
pub(crate) fn divide_ratio(n: u32, a: u32, b: u32) -> Option<(u32, u32)> {
    if a == 0 || b == 0 {
        return None;
    }
    if let Some(divisor) = a.checked_mul(b) {
        return rounded_fraction(n / divisor, n % divisor, divisor);
    }
    let quotient = n / a;
    let mut inner = n % a;
    let mut whole = quotient / b;
    let mut outer = quotient % b;
    let mut fraction = 0_u32;
    // Keep each remainder below its own divisor instead of multiplying them.
    for _ in 0..32 {
        let (next_inner, carry) = double_remainder(inner, a, 0);
        inner = next_inner;
        let (next_outer, carry) = double_remainder(outer, b, u32::from(carry));
        outer = next_outer;
        fraction = (fraction << 1) | u32::from(carry);
    }
    let (_, carry) = double_remainder(inner, a, 0);
    let (_, round_up) = double_remainder(outer, b, u32::from(carry));
    let (fraction, carry) = fraction.overflowing_add(u32::from(round_up));
    whole = whole.checked_add(u32::from(carry))?;
    Some((whole, fraction))
}

pub(crate) fn ceil_multiply_divide(a: u32, b: u32, divisor: u32) -> Option<u32> {
    let (whole, remainder) = multiply_divide(a, b, divisor)?;
    whole.checked_add(u32::from(remainder != 0))
}

pub(crate) fn advance(phase: &mut u32, whole: u32, fraction: u32) -> Option<u32> {
    let (next, carry) = phase.overflowing_add(fraction);
    *phase = next;
    whole.checked_add(u32::from(carry))
}

#[cfg(test)]
mod tests {
    use super::{advance, ceil_multiply_divide, divide_ratio, multiply_ratio};

    #[test]
    fn multiplication_and_division_cover_overflow_boundaries() {
        for (a, b, d, expected) in [
            (u32::MAX, u32::MAX, u32::MAX, Some((u32::MAX, 0))),
            (u32::MAX, 44_100, 8_000_000, Some((23_676_007, 917_780_824))),
            (48_000, 768, 8_000_000, Some((4, 2_611_340_116))),
            (u32::MAX, u32::MAX, u32::MAX - 1, None),
            (
                987_654_321,
                234_567_891,
                1_987_654_321,
                Some((116_555_473, 3_391_103_764)),
            ),
        ] {
            assert_eq!(multiply_ratio(a, b, d), expected);
        }
        assert_eq!(
            ceil_multiply_divide(u32::MAX, 44_100, 8_000_000),
            Some(23_676_008)
        );
        assert_eq!(ceil_multiply_divide(u32::MAX, u32::MAX, 1), None);
        assert_eq!(multiply_ratio(1, 1, 0), None);
    }

    #[test]
    fn division_handles_products_larger_than_u32() {
        for (n, a, b, expected) in [
            (u32::MAX, u32::MAX, u32::MAX, (0, 1)),
            (u32::MAX, u32::MAX - 1, 48_000, (0, 89_478)),
            (8_000_000, 768, 44_100, (0, 1_014_495_299)),
            (1, 1 << 31, 4, (0, 1)),
            (u32::MAX, 1, 1, (u32::MAX, 0)),
            (123_456_789, 987_654_321, 1_234_567, (0, 435)),
        ] {
            assert_eq!(divide_ratio(n, a, b), Some(expected));
        }
        assert_eq!(divide_ratio(1, 0, 1), None);
        assert_eq!(divide_ratio(1, 1, 0), None);
    }

    #[test]
    fn equivalent_ratio_forms_match() {
        let mut state = 0x1234_5678_u32;
        for _ in 0..10_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let a = state % 65_536;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let b = state % 65_536;
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let divisor = (state % 65_535) + 1;
            assert_eq!(
                multiply_ratio(a, b, divisor),
                divide_ratio(a * b, divisor, 1)
            );
            if let Some(product) = (a + 1).checked_mul(b + 1) {
                assert_eq!(
                    divide_ratio(state, a + 1, b + 1),
                    divide_ratio(state, product, 1)
                );
            }
        }
    }

    #[test]
    fn fixed_ratio_has_bounded_error_over_many_steps() {
        let (whole_step, fraction_step) = divide_ratio(48_000, 44_100, 1).unwrap();
        let mut phase = 0;
        let mut whole = 0_u32;
        for _ in 0..44_100_000 {
            whole += advance(&mut phase, whole_step, fraction_step).unwrap();
        }
        assert!(whole.abs_diff(48_000_000) <= 1);
        assert!(phase <= 22_050_000 || phase >= u32::MAX - 22_050_000);
    }

    #[test]
    fn steps_round_to_nearest_and_carry() {
        let (_, third) = divide_ratio(1, 3, 1).unwrap();
        let (_, two_thirds) = divide_ratio(2, 3, 1).unwrap();
        assert_eq!(third, 1_431_655_765);
        assert_eq!(two_thirds, 2_863_311_531);
        let mut phase = 0x8000_0000;
        assert_eq!(advance(&mut phase, 0, third), Some(0));
        assert_eq!(advance(&mut phase, 0, two_thirds), Some(1));
        assert_eq!(phase, 0x8000_0000);
    }
}
