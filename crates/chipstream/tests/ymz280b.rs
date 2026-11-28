#![cfg(feature = "ymz280b")]

use chipstream::chip::{ChipError, SoundChip, Ymz280b};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::state::Ymz280bState;
use soundlog::chip::{Chip, Ymz280bSpec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut Ymz280b, register: u8, value: u8) {
    chip.write(Ymz280bSpec { register, value }).unwrap();
}

fn setup(chip: &mut Ymz280b, mode: u8, stop: u8) {
    for (reg, value) in [
        (0x63, stop),
        (0, 0xff),
        (2, 0xff),
        (3, 1),
        (0xff, 0x90),
        (1, 0x81 | (mode << 5)),
    ] {
        write(chip, reg, value);
    }
}

fn samples(chip: &mut Ymz280b, frames: usize) -> Vec<i32> {
    let mut buffer = vec![0; frames * 2];
    chip.generate(&mut buffer).unwrap();
    buffer
}

#[test]
fn pcm8_pcm16_interpolation_end_and_irq() {
    let mut chip = Ymz280b::new(16_934_400).unwrap();
    assert_eq!(chip.sample_rate(), 16_934_400 / 192);
    chip.write_rom(0, &[0x40, 0x80]).unwrap();
    write(&mut chip, 0xfe, 1);
    setup(&mut chip, 2, 2);
    assert_eq!(samples(&mut chip, 3), [0, 0, 8160, 0, -16320, 0]);
    assert!(chip.irq_active());
    assert_eq!(chip.read_status(), 1);
    assert!(!chip.irq_active());
    assert_eq!(chip.read_status(), 0);
    assert_eq!(samples(&mut chip, 1), [-15300, 0]);
    chip.reset();
    chip.write_rom(0, &[0, 0x40, 0, 0x80]).unwrap();
    setup(&mut chip, 3, 4);
    assert_eq!(samples(&mut chip, 3), [0, 0, 8160, 0, -16320, 0]);
}

#[test]
fn adpcm_nibbles_and_fractional_interpolation() {
    let mut chip = Ymz280b::new(16_934_400).unwrap();
    chip.write_rom(0, &[0x12]).unwrap();
    setup(&mut chip, 1, 1);
    assert_eq!(samples(&mut chip, 5), [0, 0, 11, 0, 23, 0, 42, 0, 62, 0]);
    assert_eq!(chip.read_status(), 1);
    let snapshot = chip.save_state().unwrap();
    let expected = samples(&mut chip, 20);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert_eq!(samples(&mut chip, 20), expected);
}

#[test]
fn adpcm_loop_restores_decoder_state_across_chunks() {
    let mut chip = Ymz280b::new(16_934_400).unwrap();
    chip.write_rom(0, &[0x12, 0x34, 0x56, 0x78]).unwrap();
    for (reg, value) in [
        (0x61, 1),
        (0x62, 2),
        (0x63, 4),
        (0, 0xff),
        (2, 0xff),
        (3, 1),
        (0xff, 0x80),
        (1, 0xb0),
    ] {
        write(&mut chip, reg, value);
    }
    let snapshot = chip.save_state().unwrap();
    let whole = samples(&mut chip, 24);
    assert_eq!(&whole[16..24], &whole[24..32]);
    assert_eq!(chip.read_status(), 0);
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut split = samples(&mut chip, 3);
    split.extend(samples(&mut chip, 7));
    split.extend(samples(&mut chip, 14));
    assert_eq!(split, whole);
}

#[test]
fn pcm_loop_keyoff_and_global_enable() {
    let mut chip = Ymz280b::new(16_934_400).unwrap();
    chip.write_rom(0, &[0x10, 0x20, 0x30, 0x40]).unwrap();
    for (reg, value) in [
        (0x61, 1),
        (0x62, 2),
        (0x63, 4),
        (0, 0xff),
        (2, 0xff),
        (3, 8),
        (0xff, 0x80),
        (1, 0xd1),
    ] {
        write(&mut chip, reg, value);
    }
    assert_eq!(
        samples(&mut chip, 4),
        [0, 0, 2040, 2040, 4080, 4080, 4080, 4080]
    );
    assert_eq!(chip.read_status(), 0);
    write(&mut chip, 0xff, 0);
    samples(&mut chip, 3);
    assert_eq!(samples(&mut chip, 1), [0, 0]);
    write(&mut chip, 0xff, 0x80);
    assert_eq!(samples(&mut chip, 2), [0, 0, 4080, 4080]);
    write(&mut chip, 1, 0x51);
    samples(&mut chip, 3);
    assert_eq!(samples(&mut chip, 1), [0, 0]);
}

#[test]
fn external_read_and_explicit_unsupported_operations() {
    assert!(matches!(
        Ymz280b::new(191),
        Err(ChipError::InvalidClock { .. })
    ));
    let mut chip = Ymz280b::new(16_934_400).unwrap();
    chip.write_rom(0, &[0x12, 0x34, 0x56]).unwrap();
    assert_eq!(chip.read_external(), 0xff);
    for (reg, value) in [(0xff, 0x40), (0x84, 0), (0x85, 0), (0x86, 1)] {
        write(&mut chip, reg, value);
    }
    assert_eq!(chip.read_external(), 0x34);
    assert_eq!(chip.read_external(), 0x34);
    assert_eq!(chip.read_external(), 0x56);
    write(&mut chip, 0x80, 0x88);
    write(&mut chip, 0x81, 1);
    assert!(
        chip.write(Ymz280bSpec {
            register: 0x80,
            value: 0x08
        })
        .is_err()
    );
    assert!(
        chip.write(Ymz280bSpec {
            register: 0x81,
            value: 2
        })
        .is_err()
    );
    assert!(
        chip.write(Ymz280bSpec {
            register: 0x87,
            value: 1
        })
        .is_err()
    );
    assert!(
        chip.write(Ymz280bSpec {
            register: 0x82,
            value: 1
        })
        .is_err()
    );
    let mut invalid = [42; 3];
    assert!(matches!(
        chip.generate(&mut invalid),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(invalid, [42; 3]);
}

#[test]
fn vgm_dsp_bypass_writes_allow_pcm_playback() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ymz280b, Instance::Primary, 16_934_400);
    let mut data = Vec::new();
    data.extend_from_slice(&512_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x40; 512]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x86,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (0x80, 0x88),
        (0x81, 1),
        (0x61, 1),
        (0x62, 0xff),
        (0x63, 0xff),
        (0, 0xff),
        (2, 0xff),
        (3, 8),
        (0xff, 0x80),
        (1, 0xd1),
    ] {
        builder.add_chip_write(Instance::Primary, Ymz280bSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().try_into().unwrap();
    for mut player in [
        VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
    ] {
        let first = player.render().unwrap().unwrap();
        assert!(first.frames().iter().any(|frame| frame[0].abs() > 0.01));
        let expected = 16320.0 / 32768.0;
        assert!((first.frames()[80][0] - expected).abs() < 0.0001);
        assert!((first.frames()[80][1] - expected).abs() < 0.0001);
    }
}

#[test]
fn vgm_secondary_rom_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ymz280b, Instance::Primary, 16_934_400);
    builder.register_chip(Chip::Ymz280b, Instance::Secondary, 16_934_400);
    let mut data = Vec::new();
    data.extend_from_slice(&512_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x40; 512]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x86,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (0x61, 1),
        (0x62, 0xff),
        (0x63, 0xff),
        (0, 0xff),
        (2, 0xff),
        (3, 15),
        (0xff, 0x80),
        (1, 0xd1),
    ] {
        builder.add_chip_write(Instance::Secondary, Ymz280bSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().try_into().unwrap();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Ymz280bState>();
    let seen = Rc::new(RefCell::new(false));
    let captured = Rc::clone(&seen);
    player.on_write(move |instance, spec: Ymz280bSpec, _, _| {
        if instance == Instance::Secondary && spec.register == 0xff {
            *captured.borrow_mut() = true;
        }
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] == 0.0 && frame[1] > 0.01)
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
