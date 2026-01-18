//! F-number utilities for YAMAHA chip-specific frequency computations.
//!
//! This module provides types and functions for representing and converting
//! chip F-numbers (integer frequency values used by Yamaha and compatible
//! sound chips) to real frequencies in Hertz, and for generating
//! 12-EDO (equal temperament) tables used as tuning references.
//!
//! The public API includes `FNumber`, `FNumberEntry`, helpers for computing
//! produced frequencies, and the `ChipTypeSpec` trait with concrete
//! implementations
//! for supported chips (for example `OpnSpec` and `Opl3Spec`). Also provided
//! are utilities to generate precomputed 12-EDO tables (`generate_12edo_fnum_table`)
//! and to search/tune the nearest integer F-number for a target frequency
//! (`find_closest_fnumber`, `find_and_tune_fnumber`).
//!
//! Error cases are represented by `FNumberError` and invalid inputs are
//! validated and returned accordingly.
//!
//! # Examples
//!
//! ## Get produced frequency from `f_num` and `block`
//!
//! ```rust
//! use soundlog::chip::fnumber::OpnSpec;
//! use soundlog::chip::fnumber::ChipTypeSpec;
//!
//! // Compute produced frequency for a YM2203-like chip (OPN)
//! let freq = OpnSpec::fnum_block_to_freq(0x200, 6, OpnSpec::default_master_clock()).unwrap();
//! println!("frequency = {} Hz", freq);
//! ```
//!
//! ## Get `f_num` and `block` from a target frequency (table lookup + tuning)
//!
//! ```rust
//! use soundlog::chip::fnumber::{OpnSpec, generate_12edo_fnum_table, find_closest_fnumber, find_and_tune_fnumber};
//! use soundlog::chip::fnumber::ChipTypeSpec;
//!
//! // Generate a 12-EDO table for the YM2203 spec using its default master clock.
//! let table = generate_12edo_fnum_table::<OpnSpec>(OpnSpec::default_master_clock()).unwrap();
//!
//! // Find the closest f-number entry to 440 Hz.
//! let closest = find_closest_fnumber::<OpnSpec>(&table, 440.0).unwrap();
//!
//! // Fine-tune the f-number using an explicit master clock value.
//! let tuned = find_and_tune_fnumber::<OpnSpec>(&table, 440.0, OpnSpec::default_master_clock()).unwrap();
//! println!("closest={:?}, tuned={:?}", closest, tuned);
//! ```

/// Reference A4 frequency (default 440 Hz).
///
/// This constant is used as the reference pitch when generating the 12-EDO tables.
const A4_HZ: f64 = 440.0;

/// Representation of an F-number for a chip.
///
/// Fields:
/// - `f_num`: chip-specific integer F-number.
/// - `block`: block (roughly an octave indicator).
/// - `actual_freq_hz`: actual produced frequency (Hz) for this `(block, f_num)`.
/// - `error_hz`: absolute error in Hz from the target frequency.
/// - `error_cents`: error in cents from the target frequency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FNumber {
    pub f_num: u32,
    pub block: u8,
    pub actual_freq_hz: f64,
    pub error_hz: f64,
    pub error_cents: f64,
}

/// Simple error enum used by F-number utilities.
#[derive(Debug)]
pub enum FNumberError {
    InvalidInput,
    ExcessiveBits { param: &'static str, bits: u32 },
}

/// Chip-specific metadata.
///
/// Holds parameters used by `generate_12edo_fnum_table` and tuning utilities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipTypeConfig {
    /// Number of bits available for the F-number field (e.g. 11 for YM2203).
    pub fnum_bits: u8,
    /// Number of bits used for the block field.
    pub block_bits: u8,
    /// Block index that corresponds to A4 (used as table generation baseline).
    pub a4_block: u8,
    /// Prescaler applied to the master clock for this chip (1.0 for OPL3, 4.0 for OPL2-like)
    pub prescaler: f64,
}

/// Trait that exposes chip-specific conversion logic and metadata.
///
/// The public API is generic over this trait so the same functions can be used
/// for different chip marker types (e.g. `OPNSpec`, `Opl3Spec`).
pub trait ChipTypeSpec {
    /// Return chip-specific configuration.
    fn config() -> ChipTypeConfig;

    /// Compute the produced frequency (Hz) for the given `f_num` and `block`, using
    /// the supplied `master_clock_hz`.
    ///
    /// Returns an error for invalid inputs (non-finite master clock, out-of-range
    /// f_num, etc.).
    fn fnum_block_to_freq(f_num: u32, block: u8, master_clock_hz: f64)
    -> Result<f64, FNumberError>;

    /// Compute the ideal (floating-point) `f_num` value for a target frequency and block.
    ///
    /// Used by the table generator to determine integer candidate `f_num` values.
    fn ideal_fnum_for_freq(target_freq: f64, block: u8, master_clock_hz: f64) -> f64;

    /// Default master clock (Hz) to use for this chip when no other value
    /// is supplied. Implementations may override this to reflect common
    /// master clock values used by the chip.
    fn default_master_clock() -> f64 {
        4_000_000.0
    }
}

/// Marker type and implementation for the OPN(YM2203) chip.
pub struct OpnSpec;

impl ChipTypeSpec for OpnSpec {
    fn config() -> ChipTypeConfig {
        ChipTypeConfig {
            fnum_bits: 11,
            block_bits: 3,
            a4_block: 6,
            prescaler: 2.0,
        }
    }

    fn fnum_block_to_freq(
        f_num: u32,
        block: u8,
        master_clock_hz: f64,
    ) -> Result<f64, FNumberError> {
        if !master_clock_hz.is_finite() || master_clock_hz <= 0.0 {
            return Err(FNumberError::InvalidInput);
        }
        if f_num > 0x7FF {
            return Err(FNumberError::InvalidInput);
        }
        let prescaler = Self::config().prescaler;
        let exp = 21_i32 - (block as i32);
        let denom_pow = 2_f64.powi(exp);
        let freq = (f_num as f64) * (master_clock_hz * prescaler) / 144.0 / denom_pow;
        Ok(freq)
    }

    fn ideal_fnum_for_freq(target_freq: f64, block: u8, master_clock_hz: f64) -> f64 {
        let prescaler = Self::config().prescaler;
        let exp = 21_i32 - (block as i32);
        let denom_pow = 2_f64.powi(exp);
        target_freq * 144.0 * denom_pow / (master_clock_hz * prescaler)
    }

    fn default_master_clock() -> f64 {
        4_000_000.0
    }
}

/// Marker type and implementation for the OPL3(YMF262) chip.
pub struct Opl3Spec;

impl ChipTypeSpec for Opl3Spec {
    fn config() -> ChipTypeConfig {
        ChipTypeConfig {
            fnum_bits: 11,
            block_bits: 3,
            a4_block: 5,
            prescaler: 1.0,
        }
    }

    fn fnum_block_to_freq(
        f_num: u32,
        block: u8,
        master_clock_hz: f64,
    ) -> Result<f64, FNumberError> {
        if !master_clock_hz.is_finite() || master_clock_hz <= 0.0 {
            return Err(FNumberError::InvalidInput);
        }
        if f_num > 0x3FF {
            return Err(FNumberError::InvalidInput);
        }
        let prescaler = Self::config().prescaler;
        let freq = (f_num as f64) * (master_clock_hz / prescaler)
            / (288.0 * 2_f64.powi(20 - block as i32));
        Ok(freq)
    }

    fn ideal_fnum_for_freq(target_freq: f64, block: u8, master_clock_hz: f64) -> f64 {
        let prescaler = Self::config().prescaler;
        let exp = 20_i32 - (block as i32);
        let denom_pow = 2_f64.powi(exp);
        target_freq * 288.0 * denom_pow / (master_clock_hz / prescaler)
    }

    fn default_master_clock() -> f64 {
        14_318_180.0
    }
}

/// Type alias for a table entry: (target_frequency_hz, FNumber)
pub type FNumberEntry = (f64, FNumber);

/// Generate an 8×12 F-number table for 12-EDO tuning (A4 = `A4_HZ`).
///
/// - Returns a fixed-size 2D array `[block][semitone]` (no heap allocation).
/// - `master_clock_hz` is the chip's master clock frequency used in chip formulas.
pub fn generate_12edo_fnum_table<C: ChipTypeSpec>(
    master_clock_hz: f64,
) -> Result<[[Option<FNumberEntry>; 12]; 8], FNumberError> {
    let spec = C::config();

    if !master_clock_hz.is_finite() || master_clock_hz <= 0.0 {
        return Err(FNumberError::InvalidInput);
    }

    assert!(
        spec.fnum_bits > 0 && spec.fnum_bits <= 32,
        "invalid fnum_bits {}",
        spec.fnum_bits
    );
    let max_block = ((1 << spec.block_bits as usize) - 1).min(7);
    assert!(
        (spec.a4_block as usize) <= max_block,
        "a4_block {} out of range for block_bits {}",
        spec.a4_block,
        spec.block_bits
    );

    let mut fnum_table: [[Option<FNumberEntry>; 12]; 8] =
        std::array::from_fn(|_| std::array::from_fn(|_| None::<FNumberEntry>));

    for (block, row) in fnum_table.iter_mut().enumerate().take(max_block + 1) {
        for (semitone, slot) in row.iter_mut().enumerate() {
            let semitone_offset =
                (block as i32 - spec.a4_block as i32) * 12 + (semitone as i32 - 9);
            let target_freq = A4_HZ * 2_f64.powf(semitone_offset as f64 / 12.0);

            let ideal_fnum_f = C::ideal_fnum_for_freq(target_freq, block as u8, master_clock_hz);

            let mut best: Option<FNumber> = None;
            let fnum_floor = if ideal_fnum_f.is_finite() && ideal_fnum_f > 0.0 {
                ideal_fnum_f.floor() as i64
            } else {
                0
            };

            let fnum_max = if spec.fnum_bits == 32 {
                u32::MAX
            } else {
                ((1_u64 << spec.fnum_bits as usize) - 1) as u32
            };

            for delta in -1..=1 {
                let cand_i = fnum_floor + delta;
                if cand_i < 1 {
                    continue;
                }
                let cand = cand_i as u32;
                if cand > fnum_max {
                    continue;
                }
                let produced = C::fnum_block_to_freq(cand, block as u8, master_clock_hz)?;
                let err_hz = (produced - target_freq).abs();
                let err_cents = (produced / target_freq).log2() * 1200.0;
                let entry = FNumber {
                    f_num: cand,
                    block: block as u8,
                    actual_freq_hz: produced,
                    error_hz: err_hz,
                    error_cents: err_cents.abs(),
                };
                if best.is_none() || entry.error_hz < best.unwrap().error_hz {
                    best = Some(entry);
                }
            }

            *slot = best.map(|e| (target_freq, e));
        }
    }

    Ok(fnum_table)
}

/// Search the generated 12-EDO f-number `table` for the entry whose
/// produced frequency is closest to `freq` (primary metric: cents,
/// secondary: absolute Hz). The function is generic over `C: ChipTypeSpec`
/// to match the user's requested API shape.
pub fn find_closest_fnumber<C: ChipTypeSpec>(
    fnum_table: &[[Option<FNumberEntry>; 12]; 8],
    freq: f64,
) -> Result<FNumber, FNumberError> {
    if !freq.is_finite() || freq <= 0.0 {
        return Err(FNumberError::InvalidInput);
    }

    let mut best: Option<(FNumber, f64, f64)> = None;

    for row in fnum_table.iter() {
        for entry in row.iter().flatten() {
            let fnum = entry.1;
            let produced = fnum.actual_freq_hz;
            if !produced.is_finite() || produced <= 0.0 {
                continue;
            }
            let ratio = produced / freq;
            let err_cents = ratio.log2().abs() * 1200.0;
            let err_hz = (produced - freq).abs();

            match &best {
                None => {
                    best = Some((fnum, err_cents, err_hz));
                }
                Some((_, best_cents, best_hz)) => {
                    if err_cents < *best_cents || (err_cents == *best_cents && err_hz < *best_hz) {
                        best = Some((fnum, err_cents, err_hz));
                    }
                }
            }
        }
    }

    if let Some((fnum, _, _)) = best {
        Ok(fnum)
    } else {
        Err(FNumberError::InvalidInput)
    }
}

/// Like `find_closest_fnumber` but additionally fine-tunes the returned
/// `f_num` by scanning integer neighbors (keeping the same `block`) to
/// minimize absolute Hz error. The function reconstructs an estimated
/// master clock from the starting table entry so candidate frequencies
/// can be computed with `C::fnum_block_to_freq`.
pub fn find_and_tune_fnumber<C: ChipTypeSpec>(
    fnum_table: &[[Option<FNumberEntry>; 12]; 8],
    freq: f64,
    master_clock_hz: f64,
) -> Result<FNumber, FNumberError> {
    if !freq.is_finite() || freq <= 0.0 {
        return Err(FNumberError::InvalidInput);
    }

    let start = find_closest_fnumber::<C>(fnum_table, freq)?;
    let spec = C::config();
    if !master_clock_hz.is_finite() || master_clock_hz <= 0.0 {
        return Err(FNumberError::InvalidInput);
    }

    let block = start.block;
    let start_fnum = start.f_num;
    let mut best_fnum = start_fnum;
    let mut best_err_hz = (start.actual_freq_hz - freq).abs();

    let scale_k = if start_fnum > 0 {
        start.actual_freq_hz / (start_fnum as f64)
    } else {
        0.0
    };

    assert!(
        spec.fnum_bits > 0 && spec.fnum_bits <= 32,
        "invalid fnum_bits {}",
        spec.fnum_bits
    );
    let fnum_max = if spec.fnum_bits == 32 {
        u32::MAX
    } else {
        ((1u64 << spec.fnum_bits as usize) - 1) as u32
    };

    let mut cand = start_fnum.saturating_add(1);
    while cand <= fnum_max {
        let produced = if scale_k > 0.0 {
            scale_k * (cand as f64)
        } else {
            C::fnum_block_to_freq(cand, block, master_clock_hz)?
        };
        let err = (produced - freq).abs();
        if err < best_err_hz {
            best_err_hz = err;
            best_fnum = cand;
            cand = cand.saturating_add(1);
            continue;
        }
        break;
    }

    let mut cand_down = start_fnum.saturating_sub(1);
    while cand_down >= 1 {
        let produced = if scale_k > 0.0 {
            scale_k * (cand_down as f64)
        } else {
            C::fnum_block_to_freq(cand_down, block, master_clock_hz)?
        };
        let err = (produced - freq).abs();
        if err < best_err_hz {
            best_err_hz = err;
            best_fnum = cand_down;
            if cand_down == 1 {
                break;
            }
            cand_down = cand_down.saturating_sub(1);
            continue;
        }
        break;
    }

    let produced = if scale_k > 0.0 {
        scale_k * (best_fnum as f64)
    } else {
        C::fnum_block_to_freq(best_fnum, block, master_clock_hz)?
    };
    let err_hz = (produced - freq).abs();
    let err_cents = (produced / freq).log2().abs() * 1200.0;
    let result = FNumber {
        f_num: best_fnum,
        block,
        actual_freq_hz: produced,
        error_hz: err_hz,
        error_cents: err_cents,
    };

    Ok(result)
}
