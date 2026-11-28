#![cfg(feature = "es550x")]

use chipstream::chip::{Es550x, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, Es5506U8Spec, Es5506U16Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};

fn byte(chip: &mut Es550x, register: u8, value: u8) {
    chip.write(Es5506U8Spec { register, value }).unwrap();
}

fn reg5506(chip: &mut Es550x, register: u8, value: u32) {
    for (i, value) in value.to_be_bytes().into_iter().enumerate() {
        byte(chip, register * 4 + i as u8, value);
    }
}

fn rom() -> Vec<u8> {
    (0_i16..16).flat_map(|i| (i * 2000).to_le_bytes()).collect()
}

fn setup5506(chip: &mut Es550x) {
    reg5506(chip, 15, 32);
    reg5506(chip, 1, 0);
    reg5506(chip, 2, 0x7000);
    reg5506(chip, 3, 0);
    reg5506(chip, 15, 0);
    for (register, value) in [
        (11, 0),
        (1, 0x800),
        (2, 0xffff),
        (4, 0xffff),
        (7, 0xffff),
        (9, 0xffff),
        (0, 0x308),
    ] {
        reg5506(chip, register, value);
    }
}

#[test]
fn es5506_reference_waveform_and_snapshot() {
    let mut chip = Es550x::new(Chip::Es5506U8, 16_000_000).unwrap();
    chip.write_rom(0, &rom()).unwrap();
    setup5506(&mut chip);
    assert_eq!(chip.sample_rate(), 1_000_000);
    let mut samples = [0; 32];
    let state = chip.save_state().unwrap();
    chip.generate(&mut samples).unwrap();
    let reference = [
        0, 31873, 63811, 95748, 127686, 159623, 191561, 223498, 255436, 287373, 319311, 351248,
        383186, 415123, 447061, 32065,
    ];
    for (frame, expected) in samples.chunks_exact(2).zip(reference) {
        assert_eq!(frame[0], frame[1]);
        assert!(
            (frame[0] - (expected >> 4)).abs() <= 32,
            "{frame:?} {expected}"
        );
    }
    chip.restore_state(state.as_ref()).unwrap();
    let mut restored = [0; 32];
    chip.generate(&mut restored).unwrap();
    assert_eq!(samples, restored);
    reg5506(&mut chip, 11, 15);
    assert_eq!(chip.sample_rate(), 62_500);
}

#[test]
fn rejected_writes_and_buffers_do_not_change_state() {
    let mut chip = Es550x::new(Chip::Es5506U8, 16_000_000).unwrap();
    chip.write_rom(0, &rom()).unwrap();
    setup5506(&mut chip);
    let state = chip.save_state().unwrap();
    assert!(chip.write_rom(4 << 28, &[1]).is_err());
    assert!(chip.write_rom((1 << 26) - 1, &[1, 2]).is_err());
    let mut invalid = [99; 3];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [99; 3]);
    let mut after_error = [0; 32];
    chip.generate(&mut after_error).unwrap();
    chip.restore_state(state.as_ref()).unwrap();
    let mut baseline = [0; 32];
    chip.generate(&mut baseline).unwrap();
    assert_eq!(after_error, baseline);
}

#[test]
fn es5506_external_voice_bank_and_rom_region_are_independent() {
    let mut baseline = Es550x::new(Chip::Es5506U8, 16_000_000).unwrap();
    baseline.write_rom(0, &rom()).unwrap();
    setup5506(&mut baseline);
    let mut banked = Es550x::new(Chip::Es5506U8, 16_000_000).unwrap();
    banked.write_rom((1 << 28) + (1 << 21), &rom()).unwrap();
    setup5506(&mut banked);
    reg5506(&mut banked, 0, 0x4308);
    byte(&mut banked, 0x40, 1);
    let mut expected = [0; 32];
    let mut actual = [0; 32];
    baseline.generate(&mut expected).unwrap();
    banked.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn es5506_compressed_samples_and_envelope_follow_reference() {
    let mut chip = Es550x::new(Chip::Es5506U8, 16_000_000).unwrap();
    let rom: Vec<u8> = (0_u8..16).flat_map(|i| [0, i * 17]).collect();
    chip.write_rom(0, &rom).unwrap();
    setup5506(&mut chip);
    for (register, value) in [(3, 0xfd00), (5, 0xff00), (6, 12), (0, 0x2308)] {
        reg5506(&mut chip, register, value);
    }
    let reference = [
        [63, 63],
        [-3641, -3641],
        [-7474, -7474],
        [4918, 4918],
        [-13989, -13989],
        [10901, 10922],
        [-25947, -25998],
        [23906, 23953],
        [-47877, -47971],
        [51956, 52058],
        [-87485, -87829],
        [111916, 112356],
        [-158681, -159305],
        [240120, 241064],
        [-284850, -285969],
        [-3818, -3833],
    ];
    let mut samples = [0; 32];
    chip.generate(&mut samples).unwrap();
    for (frame, expected) in samples.chunks_exact(2).zip(reference) {
        for channel in 0..2 {
            assert!(
                (frame[channel] - (expected[channel] >> 4)).abs() <= 64,
                "{frame:?} {expected:?}"
            );
        }
    }
}

#[test]
fn es5505_word_and_byte_writes_share_state() {
    let mut chip = Es550x::new(Chip::Es5505, 16_000_000).unwrap();
    chip.write_rom(0, &rom()).unwrap();
    for (register, value) in [
        (13, 0),
        (1, 0x0400),
        (2, 0),
        (3, 0),
        (4, 0),
        (5, 0x3800),
        (6, 0xfff0),
        (7, 0xfff0),
        (8, 0xff00),
        (9, 0xff00),
        (0, 0x0c08),
    ] {
        chip.write(Es5506U16Spec {
            register: register * 2,
            value,
        })
        .unwrap();
    }

    let mut a = [0; 32];
    chip.generate(&mut a).unwrap();
    assert!(a.iter().any(|sample| *sample > 100));
    assert_eq!(chip.sample_rate(), 1_000_000);
    let reference = [
        0, 31873, 63811, 95748, 127686, 159623, 191561, 223498, 255436, 287373, 319311, 351248,
        383186, 415123, 447061, 478998,
    ];
    for (frame, expected) in a.chunks_exact(2).zip(reference) {
        let expected = expected >> 4;
        assert_eq!(frame[0], frame[1]);
        assert!((frame[0] - expected).abs() <= expected / 25 + 4);
    }
}

#[test]
fn vgm_playback_routes_both_bus_widths_and_physical_variants() {
    for kind in [Chip::Es5505, Chip::Es5506U8] {
        let mut builder = VgmBuilder::new();
        builder.register_chip(kind.clone(), Instance::Secondary, 16_000_000);
        builder.register_chip(kind.clone(), Instance::Primary, 16_000_000);
        let mut data = 32_u32.to_le_bytes().to_vec();
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&rom());
        builder.add_vgm_command(DataBlock {
            marker: 0x66,
            chip_instance: 1,
            data_type: 0x90,
            size: data.len() as u32,
            data,
        });
        if kind == Chip::Es5505 {
            for (register, value) in [
                (13, 7),
                (1, 0x0400),
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0x3800),
                (6, 0xfff0),
                (7, 0xfff0),
                (8, 0xff00),
                (9, 0xff00),
                (0, 0x0c08),
            ] {
                builder.add_chip_write(
                    Instance::Secondary,
                    Es5506U16Spec {
                        register: register * 2,
                        value,
                    },
                );
            }
        } else {
            for (register, value) in [
                (15, 32),
                (1, 0),
                (2, 0x7000),
                (3, 0),
                (15, 0),
                (11, 7),
                (1, 0x800),
                (2, 0xffff),
                (4, 0xffff),
                (7, 0xffff),
                (9, 0xffff),
                (0, 0x308),
            ] {
                let bytes = (value as u32).to_be_bytes();
                builder.add_chip_write(
                    Instance::Secondary,
                    Es5506U16Spec {
                        register: register * 4,
                        value: u16::from_be_bytes([bytes[0], bytes[1]]),
                    },
                );
                builder.add_chip_write(
                    Instance::Secondary,
                    Es5506U8Spec {
                        register: register * 4 + 2,
                        value: bytes[2],
                    },
                );
                builder.add_chip_write(
                    Instance::Secondary,
                    Es5506U8Spec {
                        register: register * 4 + 3,
                        value: bytes[3],
                    },
                );
            }
        }
        builder.add_vgm_command(WaitSamples(1000));
        let document = builder.finalize();
        let bytes: Vec<u8> = document.clone().try_into().unwrap();
        let mut doc_player = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
        let mut byte_player =
            VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
        let mut has_audio = false;
        for _ in 0..5 {
            let doc = doc_player.render().unwrap().unwrap();
            let raw = byte_player.render().unwrap().unwrap();
            assert_eq!(doc, raw);
            has_audio |= doc.frames().iter().any(|frame| frame[0].abs() > 0.001);
        }
        assert!(has_audio, "{kind:?}");
    }
}

#[test]
fn vgm_eight_bit_rom_regions_and_dynamic_rate_match_sixteen_bit() {
    fn document(eight_bit: bool) -> soundlog::vgm::VgmDocument {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Es5506U8, Instance::Primary, 16_000_000);
        let samples = [0_u8, 16, 32, 48, 64, 80, 96, 112, 127];
        let payload = if eight_bit {
            samples.to_vec()
        } else {
            samples.iter().flat_map(|sample| [0, *sample]).collect()
        };
        let offset = if eight_bit { 1 } else { 2 };
        let start = 0x1000_0000_u32 | if eight_bit { 0x8000_0000 } else { 0 } | offset;
        let mut data = (payload.len() as u32 + offset).to_le_bytes().to_vec();
        data.extend_from_slice(&start.to_le_bytes());
        data.extend_from_slice(&payload);
        builder.add_vgm_command(DataBlock {
            marker: 0x66,
            chip_instance: 0,
            data_type: 0x90,
            size: data.len() as u32,
            data,
        });
        for (register, value) in [
            (15, 32),
            (1, 0),
            (2, 0x3800),
            (3, 0),
            (15, 0),
            (11, 7),
            (1, 0x800),
            (2, 0xffff),
            (4, 0xffff),
            (7, 0xffff),
            (9, 0xffff),
            (0, 0x4308),
        ] {
            for (index, value) in (value as u32).to_be_bytes().into_iter().enumerate() {
                builder.add_chip_write(
                    Instance::Primary,
                    Es5506U8Spec {
                        register: register * 4 + index as u8,
                        value,
                    },
                );
            }
        }
        builder.add_vgm_command(WaitSamples(400));
        for (index, value) in 15_u32.to_be_bytes().into_iter().enumerate() {
            builder.add_chip_write(
                Instance::Primary,
                Es5506U8Spec {
                    register: 44 + index as u8,
                    value,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(600));
        builder.finalize()
    }

    let mut a = VgmSequencer::new(document(true), 44_100, 128, Default::default()).unwrap();
    let mut b = VgmSequencer::new(document(false), 44_100, 128, Default::default()).unwrap();
    let mut has_audio = false;
    for _ in 0..7 {
        let a = a.render().unwrap().unwrap();
        let b = b.render().unwrap().unwrap();
        assert_eq!(a, b);
        has_audio |= a.frames().iter().any(|frame| frame[0].abs() > 0.001);
    }
    assert!(has_audio);
}
