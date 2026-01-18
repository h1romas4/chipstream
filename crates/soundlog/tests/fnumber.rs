use soundlog::chip::fnumber::ChipTypeSpec;
use soundlog::chip::fnumber::{
    Opl3Spec, OpnSpec, find_and_tune_fnumber, find_closest_fnumber, generate_12edo_fnum_table,
};

#[test]
fn test_fnote_block_to_freq_ymf262() {
    let expected = [
        (261.625565_f64, 5_u8, 172_u32),
        (277.182631_f64, 5_u8, 182_u32),
        (293.664768_f64, 5_u8, 194_u32),
        (311.126984_f64, 5_u8, 206_u32),
        (329.627557_f64, 5_u8, 218_u32),
        (349.228231_f64, 5_u8, 230_u32),
        (369.994423_f64, 5_u8, 244_u32),
        (391.995436_f64, 5_u8, 258_u32),
        (415.304698_f64, 5_u8, 274_u32),
        (440.0_f64, 5_u8, 290_u32),
        (466.163762_f64, 5_u8, 308_u32),
        (493.883301_f64, 5_u8, 326_u32),
    ];
    for &(ref_freq, block, f_num) in &expected {
        let produced = Opl3Spec::fnum_block_to_freq(f_num, block, 14_318_180.0).unwrap();
        assert!(
            (produced - ref_freq).abs() <= 2.0,
            "f_num {} block {} produced {} Hz, expected {} Hz",
            f_num,
            block,
            produced,
            ref_freq
        );
    }
}

#[test]
fn test_fnote_block_to_freq_ym2203() {
    let expected = [
        (523.885091145833_f64, 6_u8, 309_u32),
        (554.402669270833_f64, 6_u8, 327_u32),
        (586.615668402778_f64, 6_u8, 346_u32),
        (622.219509548611_f64, 6_u8, 367_u32),
        (659.518771701389_f64, 6_u8, 389_u32),
        (698.513454861111_f64, 6_u8, 412_u32),
        (740.898980034722_f64, 6_u8, 437_u32),
        (784.979926215278_f64, 6_u8, 463_u32),
        (832.451714409722_f64, 6_u8, 491_u32),
        (879.923502604167_f64, 6_u8, 519_u32),
        (930.786132812500_f64, 6_u8, 549_u32),
        (986.735026041667_f64, 6_u8, 582_u32),
    ];

    for &(ref_freq, block, fnum) in &expected {
        let master = OpnSpec::default_master_clock();
        let produced = OpnSpec::fnum_block_to_freq(fnum, block, master).unwrap();
        assert!(
            (produced - ref_freq).abs() <= 2.0,
            "YM2203: fnum {} block {} produced {} Hz, expected {} Hz",
            fnum,
            block,
            produced,
            ref_freq
        );
    }
}

#[test]
fn test_find_closest_fnumber_ymf262opl3_440() {
    let table = generate_12edo_fnum_table::<Opl3Spec>(14_318_180.0).unwrap();
    let found = find_closest_fnumber::<Opl3Spec>(&table, 440.0).unwrap();
    assert_eq!(found.block, 5);
    assert!((found.f_num as i32 - 290).abs() <= 1);
}

#[test]
fn test_find_closest_fnumber_ym2203_440() {
    let master = OpnSpec::default_master_clock() / OpnSpec::config().prescaler;
    let table = generate_12edo_fnum_table::<OpnSpec>(master).unwrap();
    let found = find_closest_fnumber::<OpnSpec>(&table, 440.0).unwrap();
    assert_eq!(found.block, 6);
    assert!((found.f_num as i32 - 519).abs() <= 1);
}

#[test]
fn test_find_closest_fnumber_ymf262opl3_off_tune() {
    let table = generate_12edo_fnum_table::<Opl3Spec>(14_318_180.0).unwrap();

    let found_flat = find_closest_fnumber::<Opl3Spec>(&table, 439.0).unwrap();
    assert_eq!(found_flat.block, 5);
    assert!((found_flat.f_num as i32 - 290).abs() <= 1);

    let found_sharp = find_closest_fnumber::<Opl3Spec>(&table, 445.0).unwrap();
    assert_eq!(found_sharp.block, 5);
    assert!((found_flat.f_num as i32 - 290).abs() <= 1);
}

#[test]
fn test_find_closest_fnumber_ym2203_off_tune() {
    let master = OpnSpec::default_master_clock() / OpnSpec::config().prescaler;
    let table = generate_12edo_fnum_table::<OpnSpec>(master).unwrap();

    let found_flat = find_closest_fnumber::<OpnSpec>(&table, 438.0).unwrap();
    assert_eq!(found_flat.block, 6);
    assert!((found_flat.f_num as i32 - 519).abs() <= 1);

    let found_sharp = find_closest_fnumber::<OpnSpec>(&table, 442.0).unwrap();
    assert_eq!(found_sharp.block, 6);
    assert!((found_sharp.f_num as i32 - 519).abs() <= 1);
}

#[test]
fn test_find_and_tune_fnumber_ymf262opl3() {
    let table = generate_12edo_fnum_table::<Opl3Spec>(14_318_180.0).unwrap();
    let target = 441.0_f64;
    let base = find_closest_fnumber::<Opl3Spec>(&table, target).unwrap();
    let tuned = find_and_tune_fnumber::<Opl3Spec>(&table, target, 14_318_180.0).unwrap();
    let base_err = (base.actual_freq_hz - target).abs();
    assert!(tuned.error_hz <= base_err);
    assert!((tuned.f_num as i32 - 291).abs() <= 1);
}

#[test]
fn test_find_and_tune_fnumber_ym2203() {
    let master = OpnSpec::default_master_clock() / OpnSpec::config().prescaler;
    let table = generate_12edo_fnum_table::<OpnSpec>(master).unwrap();
    let target = 442.0_f64;
    let base = find_closest_fnumber::<OpnSpec>(&table, target).unwrap();
    let master = OpnSpec::default_master_clock() / OpnSpec::config().prescaler;
    let tuned = find_and_tune_fnumber::<OpnSpec>(&table, target, master).unwrap();
    let base_err = (base.actual_freq_hz - target).abs();
    assert!(tuned.error_hz <= base_err);
    assert!((tuned.f_num as i32 - 521).abs() <= 1);
}

#[test]
fn test_output_csv_tuned_freq_fnum_block() {
    let a4 = 440.0_f64;
    let start_hz = a4 / 2f64.powf(4.0); // A0 = A4 / 2^4 = 27.5 Hz
    let _end_hz = a4 * 2f64.powf(3.0); // A7 = A4 * 2^3 = 3520 Hz

    let octaves: usize = 7; // A0 -> A7 (7 octaves)
    let steps_per_octave: usize = 24; // (24 steps/octave)
    let total_steps = octaves * steps_per_octave;

    let mut freqs: Vec<f64> = Vec::with_capacity(total_steps + 1);
    let step_ratio = 2f64.powf(1.0 / (steps_per_octave as f64));
    for i in 0..=total_steps {
        freqs.push(start_hz * step_ratio.powf(i as f64));
    }

    let required = [220.0_f64, 440.0_f64, 880.0_f64];
    for &r in &required {
        freqs.push(r);
    }

    freqs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    freqs.dedup_by(|a, b| ((*a) - (*b)).abs() < 1e-9);
}
