#![cfg(feature = "k054539")]

use chipstream::chip::{ChipError, K054539, K054539Config, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::state::K054539State;
use soundlog::chip::{Chip, K054539Spec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut K054539, register: u16, value: u8) {
    chip.write(K054539Spec { register, value }).unwrap();
}

fn setup(chip: &mut K054539, format: u8, pos: u8) {
    for (reg, value) in [
        (2, 1),
        (5, 0x1f),
        (0x0c, pos),
        (0x200, format),
        (0x22f, 1),
        (0x214, 1),
    ] {
        write(chip, reg, value);
    }
}

fn samples(chip: &mut K054539, count: usize) -> Vec<i32> {
    let mut result = vec![0; count * 2];
    chip.generate(&mut result).unwrap();
    result
}

#[test]
fn pcm_8_and_16_bit_end_loop_reverse() {
    let mut chip = K054539::with_config(
        18_432_000,
        K054539Config {
            disable_reverb: true,
            ..Default::default()
        },
    )
    .unwrap();
    chip.write_rom(0, &[0, 0x40, 0x80, 0x20, 0x00, 0x40, 0x00, 0x80])
        .unwrap();
    setup(&mut chip, 0, 0);
    assert_eq!(samples(&mut chip, 2), [4096, 0, 0, 0]);
    assert_eq!(chip.read_register(0x22c).unwrap(), 0);
    write(&mut chip, 8, 1);
    write(&mut chip, 0x201, 1);
    write(&mut chip, 0x0c, 1);
    write(&mut chip, 0x214, 1);
    assert_eq!(samples(&mut chip, 2), [4096, 0, 4096, 0]);
    write(&mut chip, 0x215, 1);
    write(&mut chip, 0x201, 0);
    write(&mut chip, 0x200, 4);
    write(&mut chip, 0x0c, 2);
    write(&mut chip, 0x214, 1);
    assert_eq!(samples(&mut chip, 2), [4096, 0, 0, 0]);
    write(&mut chip, 0x200, 0x20);
    write(&mut chip, 0x0c, 2);
    write(&mut chip, 0x214, 1);
    assert_eq!(samples(&mut chip, 1), [4096, 0]);
}

#[test]
fn dpcm_reverb_ram_snapshot_and_reset() {
    let config = K054539Config {
        update_at_keyon: false,
        ..Default::default()
    };
    let mut chip = K054539::with_config(18_432_000, config).unwrap();
    chip.write_rom(0, &[0, 0x21, 0x88]).unwrap();
    setup(&mut chip, 8, 0);
    assert_eq!(samples(&mut chip, 2), [0, 0, 64, 0]);
    let snapshot = chip.save_state().unwrap();
    let expected = samples(&mut chip, 4);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert_eq!(samples(&mut chip, 4), expected);
    assert_eq!(chip.read_register(0x22c).unwrap(), 0);
    write(&mut chip, 0x22e, 0x80);
    write(&mut chip, 0x22d, 0x55);
    write(&mut chip, 0x22e, 0x80);
    write(&mut chip, 0x22f, 0x10);
    assert_eq!(chip.read_register(0x22d).unwrap(), 0x55);
    chip.reset();
    write(&mut chip, 0x22e, 0x80);
    write(&mut chip, 0x22f, 0x10);
    assert_eq!(chip.read_register(0x22d).unwrap(), 0);
}

#[test]
fn position_latch_and_reversed_stereo() {
    let config = K054539Config {
        reverse_stereo: true,
        update_at_keyon: true,
        disable_reverb: true,
    };
    let mut chip = K054539::with_config(18_432_000, config).unwrap();
    chip.write_rom(0, &[0, 0x40, 0x20, 0x80]).unwrap();
    setup(&mut chip, 0, 0);
    assert_eq!(samples(&mut chip, 1), [0, 4096]);
    write(&mut chip, 0x0c, 1);
    assert_eq!(chip.read_register(0x0c).unwrap(), 1);
    write(&mut chip, 0x214, 1);
    assert_eq!(samples(&mut chip, 1), [0, 2048]);
    assert!(matches!(
        K054539::new(383),
        Err(ChipError::InvalidClock { .. })
    ));
    assert!(
        chip.write(K054539Spec {
            register: 0x230,
            value: 0
        })
        .is_err()
    );
}

#[test]
fn reverb_delay_and_disable_preserve_ram_behavior() {
    let mut chip = K054539::with_config(
        18_432_000,
        K054539Config {
            update_at_keyon: false,
            ..Default::default()
        },
    )
    .unwrap();
    chip.write_rom(0, &[0x40; 16]).unwrap();
    setup(&mut chip, 0, 0);
    let mut muted = K054539::with_config(
        18_432_000,
        K054539Config {
            update_at_keyon: false,
            disable_reverb: true,
            ..Default::default()
        },
    )
    .unwrap();
    muted.write_rom(0, &[0x40; 16]).unwrap();
    setup(&mut muted, 0, 0);
    assert_eq!(
        samples(&mut chip, 4),
        [4096, 0, 4096, 0, 6144, 2048, 4096, 0]
    );
    assert_eq!(samples(&mut muted, 4), [4096, 0, 4096, 0, 4096, 0, 4096, 0]);
    write(&mut chip, 0x215, 1);
    write(&mut muted, 0x215, 1);
    assert_eq!(samples(&mut chip, 1), [2048, 2048]);
    assert_eq!(samples(&mut muted, 1), [0, 0]);
    for player in [&mut chip, &mut muted] {
        write(player, 0x22e, 0x80);
        write(player, 0x22f, 0x10);
    }
    for _ in 0..16 {
        assert_eq!(
            chip.read_register(0x22d).unwrap(),
            muted.read_register(0x22d).unwrap()
        );
    }
}

#[test]
fn vgm_header_flags_control_output() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::K054539, Instance::Primary, 18_432_000);
    let mut data = Vec::new();
    data.extend_from_slice(&512_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x40; 512]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x8c,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [(2, 1), (5, 0x1f), (0x22f, 1), (0x214, 1)] {
        builder.add_chip_write(Instance::Primary, K054539Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(300));
    let mut doc = builder.finalize();
    doc.header.k054539_flags.reverse_stereo = true;
    doc.header.k054539_flags.disable_reverb = true;
    let bytes: Vec<u8> = doc.clone().into();
    for mut player in [
        VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
    ] {
        let chunk = player.render().unwrap().unwrap();
        assert!(chunk.frames().iter().any(|frame| frame[1] > 0.01));
        assert!(chunk.frames().iter().all(|frame| frame[0] == 0.0));
    }
}

#[test]
fn vgm_legacy_sample_rate_clock_matches_modern_clock() {
    fn document(clock: u32) -> VgmDocument {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::K054539, Instance::Primary, clock);
        let mut data = Vec::new();
        data.extend_from_slice(&512_u32.to_le_bytes());
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&[0x40; 512]);
        builder.add_vgm_command(DataBlock {
            marker: 0x66,
            chip_instance: 0,
            data_type: 0x8c,
            size: data.len() as u32,
            data,
        });
        for (register, value) in [(2, 1), (5, 0x1f), (0x22f, 1), (0x214, 1)] {
            builder.add_chip_write(Instance::Primary, K054539Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(300));
        builder.finalize()
    }
    let mut modern =
        VgmSequencer::new(document(18_432_000), 44_100, 128, Default::default()).unwrap();
    let expected = modern.render().unwrap().unwrap();
    assert!(expected.frames().iter().any(|frame| frame[0].abs() > 0.01));
    let legacy = document(48_000);
    let bytes: Vec<u8> = legacy.clone().into();
    for mut player in [
        VgmSequencer::new(legacy, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
    ] {
        assert_eq!(player.render().unwrap().unwrap(), expected);
    }
}

#[test]
fn vgm_secondary_rom_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::K054539, Instance::Primary, 18_432_000);
    builder.register_chip(Chip::K054539, Instance::Secondary, 18_432_000);
    let mut data = Vec::new();
    data.extend_from_slice(&512_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x40; 512]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x8c,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [(2, 1), (5, 0x1f), (0x22f, 1), (0x214, 1)] {
        builder.add_chip_write(Instance::Secondary, K054539Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<K054539State>();
    let seen = Rc::new(RefCell::new(false));
    let captured = Rc::clone(&seen);
    player.on_write(move |instance, spec: K054539Spec, _, _| {
        if instance == Instance::Secondary && spec.register == 0x214 {
            *captured.borrow_mut() = true;
        }
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] > 0.01 && frame[0] > frame[1] * 1.5)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(*seen.borrow());
    for mut other in [
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::new(parsed, 44_100, 128, Default::default()).unwrap(),
    ] {
        assert_eq!(other.render().unwrap().unwrap(), first);
        assert_eq!(other.render().unwrap().unwrap(), second);
    }
}
