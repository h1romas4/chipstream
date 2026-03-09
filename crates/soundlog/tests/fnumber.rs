use soundlog::chip::fnumber::ChipTypeSpec;
use soundlog::chip::fnumber::{
    OplSpec, OpnSpec, find_and_tune_fnumber, find_closest_fnumber, generate_12edo_fnum_table,
};

#[test]
fn test_fnote_block_to_freq_ymf262() {
    let expected = [
        (261.625_58_f32, 4_u8, 86_u32),
        (277.182_62_f32, 4_u8, 91_u32),
        (293.664_76_f32, 4_u8, 97_u32),
        (311.126_98_f32, 4_u8, 103_u32),
        (329.627_56_f32, 4_u8, 109_u32),
        (349.228_24_f32, 4_u8, 115_u32),
        (369.994_42_f32, 4_u8, 122_u32),
        (391.995_42_f32, 4_u8, 129_u32),
        (415.304_7_f32, 4_u8, 137_u32),
        (440.0_f32, 4_u8, 145_u32),
        (466.163_76_f32, 4_u8, 154_u32),
        (493.883_3_f32, 4_u8, 163_u32),
    ];
    for &(ref_freq, block, f_num) in &expected {
        let produced = OplSpec::fnum_block_to_freq(f_num, block, 14_318_180.0f32).unwrap();
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
        (523.885_1_f32, 6_u8, 309_u32),
        (554.402_65_f32, 6_u8, 327_u32),
        (586.615_66_f32, 6_u8, 346_u32),
        (622.219_5_f32, 6_u8, 367_u32),
        (659.518_8_f32, 6_u8, 389_u32),
        (698.513_4_f32, 6_u8, 412_u32),
        (740.899_f32, 6_u8, 437_u32),
        (784.979_9_f32, 6_u8, 463_u32),
        (832.451_7_f32, 6_u8, 491_u32),
        (879.923_5_f32, 6_u8, 519_u32),
        (930.786_13_f32, 6_u8, 549_u32),
        (986.735_05_f32, 6_u8, 582_u32),
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
    let table = generate_12edo_fnum_table::<OplSpec>(14_318_180.0).unwrap();
    let found = find_closest_fnumber::<OplSpec>(&table, 440.0).unwrap();
    assert_eq!(found.block, 4);
    assert!((found.f_num as i32 - 145).abs() <= 1);
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
    let table = generate_12edo_fnum_table::<OplSpec>(14_318_180.0f32).unwrap();

    let found_flat = find_closest_fnumber::<OplSpec>(&table, 439.0).unwrap();
    assert_eq!(found_flat.block, 4);
    assert!((found_flat.f_num as i32 - 145).abs() <= 1);

    let found_sharp = find_closest_fnumber::<OplSpec>(&table, 445.0).unwrap();
    assert_eq!(found_sharp.block, 4);
    assert!((found_flat.f_num as i32 - 145).abs() <= 1);
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
    let table = generate_12edo_fnum_table::<OplSpec>(14_318_180.0f32).unwrap();
    let target = 441.0_f32;
    let base = find_closest_fnumber::<OplSpec>(&table, target).unwrap();
    let tuned = find_and_tune_fnumber::<OplSpec>(&table, target, 14_318_180.0f32).unwrap();
    let base_err = (base.actual_freq_hz - target).abs();
    assert!(tuned.error_hz <= base_err);
    assert!((tuned.f_num as i32 - 146).abs() <= 1);
}

#[test]
fn test_find_and_tune_fnumber_ym2203() {
    let master = OpnSpec::default_master_clock() / OpnSpec::config().prescaler;
    let table = generate_12edo_fnum_table::<OpnSpec>(master).unwrap();
    let target = 442.0_f32;
    let base = find_closest_fnumber::<OpnSpec>(&table, target).unwrap();
    let master = OpnSpec::default_master_clock() / OpnSpec::config().prescaler;
    let tuned = find_and_tune_fnumber::<OpnSpec>(&table, target, master).unwrap();
    let base_err = (base.actual_freq_hz - target).abs();
    assert!(tuned.error_hz <= base_err);
    assert!((tuned.f_num as i32 - 521).abs() <= 1);
}

#[test]
fn test_output_csv_tuned_freq_fnum_block() {
    let a4 = 440.0_f32;
    let start_hz = a4 / 2f32.powf(4.0); // A0 = A4 / 2^4 = 27.5 Hz
    let _end_hz = a4 * 2f32.powf(3.0); // A7 = A4 * 2^3 = 3520 Hz

    let octaves: usize = 7; // A0 -> A7 (7 octaves)
    let steps_per_octave: usize = 24; // (24 steps/octave)
    let total_steps = octaves * steps_per_octave;

    let mut freqs: Vec<f32> = Vec::with_capacity(total_steps + 1);
    let step_ratio = 2f32.powf(1.0 / (steps_per_octave as f32));

    for i in 0..=total_steps {
        freqs.push(start_hz * step_ratio.powf(i as f32));
    }

    let required = [220.0_f32, 440.0_f32, 880.0_f32];
    for &r in &required {
        freqs.push(r);
    }

    freqs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    freqs.dedup_by(|a, b| ((*a) - (*b)).abs() < 1e-6f32);
}

#[test]
fn test_fnumber_error_variants_and_invalid_inputs() {
    use soundlog::chip::fnumber::{
        FNumberError, OplSpec, find_and_tune_fnumber, find_closest_fnumber,
        generate_12edo_fnum_table,
    };

    // Construct and match enum variants directly
    match FNumberError::InvalidInput {
        FNumberError::InvalidInput => {}
        _ => panic!("expected InvalidInput variant"),
    }

    let e = FNumberError::ExcessiveBits {
        param: "fnum_bits",
        bits: 0,
    };
    match e {
        FNumberError::ExcessiveBits { param, bits } => {
            assert_eq!(param, "fnum_bits");
            assert_eq!(bits, 0);
        }
        _ => panic!("expected ExcessiveBits variant"),
    }

    // generate_12edo_fnum_table: invalid master clock values
    assert!(matches!(
        generate_12edo_fnum_table::<OplSpec>(f32::NAN),
        Err(FNumberError::InvalidInput)
    ));
    assert!(matches!(
        generate_12edo_fnum_table::<OplSpec>(0.0),
        Err(FNumberError::InvalidInput)
    ));
    assert!(matches!(
        generate_12edo_fnum_table::<OplSpec>(f32::INFINITY),
        Err(FNumberError::InvalidInput)
    ));

    // With a valid table, invalid frequency inputs for finder functions should error
    let table = generate_12edo_fnum_table::<OplSpec>(14_318_180.0).unwrap();
    assert!(matches!(
        find_closest_fnumber::<OplSpec>(&table, 0.0),
        Err(FNumberError::InvalidInput)
    ));
    assert!(matches!(
        find_closest_fnumber::<OplSpec>(&table, f32::NAN),
        Err(FNumberError::InvalidInput)
    ));

    // find_and_tune_fnumber: invalid freq or invalid master clock
    assert!(matches!(
        find_and_tune_fnumber::<OplSpec>(&table, 0.0, 14_318_180.0),
        Err(FNumberError::InvalidInput)
    ));
    assert!(matches!(
        find_and_tune_fnumber::<OplSpec>(&table, 440.0, f32::NAN),
        Err(FNumberError::InvalidInput)
    ));
    assert!(matches!(
        find_and_tune_fnumber::<OplSpec>(&table, 440.0, 0.0),
        Err(FNumberError::InvalidInput)
    ));
}

#[test]
fn test_default_master_clock_startup_for_specs() {
    // Ensure that default_master_clock() for each spec is usable to generate a table
    use soundlog::chip::fnumber::{
        Opl2Spec, Opl3Spec, OplSpec, OpllSpec, OpnSpec, OpnaSpec, OpxSpec,
        generate_12edo_fnum_table,
    };

    // OpnSpec uses a prescaler in some contexts; its default_master_clock should still be valid
    let t_opn = generate_12edo_fnum_table::<OpnSpec>(OpnSpec::default_master_clock());
    assert!(t_opn.is_ok());

    let t_opna = generate_12edo_fnum_table::<OpnaSpec>(OpnaSpec::default_master_clock());
    assert!(t_opna.is_ok());

    let t_opl = generate_12edo_fnum_table::<OplSpec>(OplSpec::default_master_clock());
    assert!(t_opl.is_ok());

    // Opl2Spec should also be usable with its default master clock
    let t_opl2 = generate_12edo_fnum_table::<Opl2Spec>(Opl2Spec::default_master_clock());
    assert!(t_opl2.is_ok());

    let t_opl3 = generate_12edo_fnum_table::<Opl3Spec>(Opl3Spec::default_master_clock());
    assert!(t_opl3.is_ok());

    let t_opll = generate_12edo_fnum_table::<OpllSpec>(OpllSpec::default_master_clock());
    assert!(t_opll.is_ok());

    let t_opx = generate_12edo_fnum_table::<OpxSpec>(OpxSpec::default_master_clock());
    assert!(t_opx.is_ok());
}
