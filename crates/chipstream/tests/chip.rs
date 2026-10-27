use std::cell::Cell;
use std::rc::Rc;

use chipstream::chip::{ChipError, ChipRegisterWrite, SoundChip, YmfmChip};
use soundlog::chip::*;
use ymfm_sys::{InterfaceHandler, ffi};

const CHIPS: &[(Chip, ffi::ChipType, u32, u8)] = &[
    (Chip::Ym2151, ffi::ChipType::Ym2151, 64, 1),
    (Chip::Ym2203, ffi::ChipType::Ym2203, 4, 1),
    (Chip::Ym2413, ffi::ChipType::Ym2413, 72, 1),
    (Chip::Ym2608, ffi::ChipType::Ym2608, 8, 2),
    (Chip::Ym2610b, ffi::ChipType::Ym2610B, 16, 2),
    (Chip::Ym2612, ffi::ChipType::Ym2612, 144, 2),
    (Chip::Ym3526, ffi::ChipType::Ym3526, 72, 1),
    (Chip::Y8950, ffi::ChipType::Y8950, 72, 1),
    (Chip::Ym3812, ffi::ChipType::Ym3812, 72, 1),
    (Chip::Ymf262, ffi::ChipType::Ymf262, 288, 2),
    (Chip::Ymf278b, ffi::ChipType::Ymf278B, 768, 3),
];

fn ymfm_write(chip: &Chip, port: u8, register: u8, value: u8) -> ChipRegisterWrite {
    match chip {
        Chip::Ym2151 => ChipRegisterWrite::Ym2151(Ym2151Spec { register, value }),
        Chip::Ym2203 => ChipRegisterWrite::Ym2203(Ym2203Spec { register, value }),
        Chip::Ym2413 => ChipRegisterWrite::Ym2413(Ym2413Spec { register, value }),
        Chip::Ym2608 => ChipRegisterWrite::Ym2608(Ym2608Spec {
            port,
            register,
            value,
        }),
        Chip::Ym2610b => ChipRegisterWrite::Ym2610(Ym2610Spec {
            port,
            register,
            value,
        }),
        Chip::Ym2612 => ChipRegisterWrite::Ym2612(Ym2612Spec {
            port,
            register,
            value,
        }),
        Chip::Ym3526 => ChipRegisterWrite::Ym3526(Ym3526Spec { register, value }),
        Chip::Y8950 => ChipRegisterWrite::Y8950(Y8950Spec { register, value }),
        Chip::Ym3812 => ChipRegisterWrite::Ym3812(Ym3812Spec { register, value }),
        Chip::Ymf262 => ChipRegisterWrite::Ymf262(Ymf262Spec {
            port,
            register,
            value,
        }),
        Chip::Ymf278b => ChipRegisterWrite::Ymf278b(Ymf278bSpec {
            port,
            register,
            value,
        }),
        _ => panic!("not a YMFM chip"),
    }
}

#[test]
fn supported_chips_match_native_configuration_and_output() {
    for (chip_type, native_type, _, banks) in CHIPS {
        let clock = 8_000_000;
        let mut chip = YmfmChip::new(chip_type.clone(), clock).unwrap();
        let mut native = ffi::create_chip(*native_type, clock);
        assert_eq!(chip.chip_type(), chip_type);
        assert_eq!(chip.clock(), clock);
        assert_eq!(chip.sample_rate(), native.sample_rate());
        assert_eq!(chip.channels(), native.channels() as usize);
        assert!(chip.sample_rate() > 0);
        assert!(chip.channels() > 0);

        for bank in 0..*banks {
            chip.apply_register_write(ymfm_write(chip_type, bank, 0x20, 0x01))
                .unwrap();
            native.pin_mut().write(u32::from(bank) * 2, 0x20);
            native.pin_mut().write(u32::from(bank) * 2 + 1, 0x01);
        }
        let mut actual = vec![0; chip.channels() * 32];
        let mut expected = actual.clone();
        chip.generate(&mut actual).unwrap();
        native.pin_mut().generate(&mut expected);
        assert_eq!(actual, expected, "{chip_type:?}");

        if *banks > 1 {
            assert_eq!(
                chip.apply_register_write(ymfm_write(chip_type, *banks, 0, 0)),
                Err(ChipError::InvalidRegisterBank {
                    chip: chip_type.clone(),
                    bank: *banks,
                })
            );
        }
    }
}

#[test]
fn clock_boundaries_and_unsupported_chips_are_checked_before_creation() {
    for (chip_type, _, minimum, _) in CHIPS {
        assert!(matches!(
            YmfmChip::new(chip_type.clone(), minimum - 1),
            Err(ChipError::InvalidClock { clock, minimum: min })
                if clock == minimum - 1 && min == *minimum
        ));
        let chip = YmfmChip::new(chip_type.clone(), *minimum).unwrap();
        assert_eq!(chip.sample_rate(), 1, "{chip_type:?}");
    }
    assert!(matches!(
        YmfmChip::new(Chip::Okim6258, 4_000_000),
        Err(ChipError::UnsupportedChip(Chip::Okim6258))
    ));
}

#[test]
fn invalid_buffer_does_not_change_state_or_samples() {
    let mut chip = YmfmChip::new(Chip::Ym2612, 7_670_000).unwrap();
    let mut reference = YmfmChip::new(Chip::Ym2612, 7_670_000).unwrap();
    let mut invalid = [123; 3];
    assert_eq!(
        chip.generate(&mut invalid),
        Err(ChipError::InvalidBufferLength {
            length: 3,
            channels: 2,
        })
    );
    assert_eq!(invalid, [123; 3]);
    let mut actual = [0; 32];
    let mut expected = [0; 32];
    chip.generate(&mut actual).unwrap();
    reference.generate(&mut expected).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn register_writes_produce_audio_and_reset_matches_native_behavior() {
    let mut chip: Box<dyn SoundChip> = Box::new(YmfmChip::new(Chip::Ym2612, 7_670_000).unwrap());
    let mut native = ffi::create_chip(ffi::ChipType::Ym2612, 7_670_000);
    for (bank, register, value) in [(0, 0x2b, 0x80), (0, 0x2a, 0xff), (1, 0xb6, 0xc0)] {
        chip.write(Ym2612Spec {
            port: bank,
            register,
            value,
        })
        .unwrap();
        native.pin_mut().write(u32::from(bank) * 2, register);
        native.pin_mut().write(u32::from(bank) * 2 + 1, value);
    }
    let mut samples = vec![0; chip.channels() * 100];
    let mut native_samples = samples.clone();
    chip.generate(&mut samples).unwrap();
    native.pin_mut().generate(&mut native_samples);
    assert_eq!(samples, native_samples);
    assert!(samples.iter().any(|&sample| sample != 0));

    chip.reset();
    native.pin_mut().reset();
    let mut actual = vec![0; chip.channels() * 100];
    let mut expected = actual.clone();
    chip.generate(&mut actual).unwrap();
    native.pin_mut().generate(&mut expected);
    assert_eq!(actual, expected);
}

#[test]
fn rendering_in_chunks_preserves_chip_state() {
    let mut one_block = YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap();
    let mut split = YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap();
    let channels = one_block.channels();
    let mut expected = vec![0; channels * 31];
    let mut actual = expected.clone();
    one_block.generate(&mut expected).unwrap();
    let (first, second) = actual.split_at_mut(channels * 7);
    split.generate(first).unwrap();
    split.generate(&mut []).unwrap();
    split.generate(second).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn custom_callbacks_are_used_during_generation() {
    let elapsed = Rc::new(Cell::new(0));
    let callback_elapsed = Rc::clone(&elapsed);
    let handler = InterfaceHandler {
        advance_clock: Some(Box::new(move |clocks| {
            callback_elapsed.set(callback_elapsed.get() + clocks);
            0
        })),
        ..InterfaceHandler::default()
    };
    let mut chip = YmfmChip::with_handler(Chip::Ym2151, 4_000_000, handler).unwrap();
    let mut samples = vec![0; chip.channels() * 10];
    chip.generate(&mut samples).unwrap();
    assert_eq!(elapsed.get(), 640);
}
