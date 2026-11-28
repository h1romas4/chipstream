#![cfg(feature = "es5503")]

use chipstream::chip::{ChipError, Es5503, SoundChip};
use chipstream::sequencer::{ChipBackendSettings, ChipFactoryRegistry, VgmSequencer};
use soundlog::chip::{Chip, Es5503Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut Es5503, register: u16, value: u8) {
    chip.write(Es5503Spec { register, value }).unwrap();
}

fn oscillator(chip: &mut Es5503, index: u8, pointer: u8, control: u8, frequency: u16) {
    for (register, value) in [
        (index, frequency as u8),
        (0x20 + index, (frequency >> 8) as u8),
        (0x40 + index, 255),
        (0x80 + index, pointer),
        (0xc0 + index, 0),
        (0xa0 + index, control),
    ] {
        write(chip, u16::from(register), value);
    }
}

#[test]
fn sample_rate_channels_and_ram_boundaries() {
    assert!(Es5503::new(271).is_err());
    assert!(Es5503::with_output_channels(7_000_000, 17).is_err());
    let mut chip = Es5503::with_output_channels(7_000_000, 2).unwrap();
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 24);
    write(&mut chip, 0xe1, 62);
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 272);
    assert!(matches!(
        chip.write_ram(0x1ffff, &[1, 2]),
        Err(ChipError::RamOutOfRange { .. })
    ));
    chip.write_ram(0x10000, &[0xc0]).unwrap();
    write(&mut chip, 0xe1, 0);
    oscillator(&mut chip, 0, 0, 0x12, 0);
    write(&mut chip, 0xc0, 0x40);
    let mut frame = [0; 2];
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [0, 6120]);
    let saved = chip.save_state().unwrap();
    chip.write_ram(0x10000, &[0]).unwrap();
    chip.restore_state(saved.as_ref()).unwrap();
    let mut restored = [0; 2];
    chip.generate(&mut restored).unwrap();
    assert_eq!(restored, frame);
}

#[test]
fn zero_terminates_and_even_swap_starts_partner() {
    let mut chip = Es5503::with_output_channels(7_000_000, 2).unwrap();
    chip.write_ram(0, &[0xc0]).unwrap();
    chip.write_ram(0x100, &[0x40]).unwrap();
    write(&mut chip, 0xe1, 2);
    oscillator(&mut chip, 0, 0, 0x06, u16::MAX);
    oscillator(&mut chip, 1, 1, 0x13, 0);
    let mut frames = [0; 6];
    chip.generate(&mut frames).unwrap();
    assert_eq!(frames, [2040, 0, 0, -6120, 0, -6120]);
}

#[test]
fn odd_sync_oscillator_modulates_next_voice() {
    let mut chip = Es5503::with_output_channels(7_000_000, 2).unwrap();
    chip.write_ram(0x100, &[0xc0; 256]).unwrap();
    chip.write_ram(0x200, &[0xc0]).unwrap();
    write(&mut chip, 0xe1, 4);
    oscillator(&mut chip, 1, 1, 4, 0);
    oscillator(&mut chip, 2, 2, 2, 0);
    let mut frame = [0; 2];
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [4312, 0]);
}

fn ram(instance: Instance, address: u32, data: &[u8]) -> DataBlock {
    let mut bytes = address.to_le_bytes().to_vec();
    bytes.extend_from_slice(data);
    DataBlock {
        marker: 0x66,
        chip_instance: instance as u8,
        data_type: 0xe1,
        size: bytes.len() as u32,
        data: bytes,
    }
}

fn document() -> soundlog::vgm::VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Es5503, Instance::Primary, 7_000_000);
    builder.register_chip(Chip::Es5503, Instance::Secondary, 7_000_000);
    builder.add_vgm_command(ram(Instance::Secondary, 0x10000, &[0xc0]));
    for (register, value) in [
        (0x00, 0),
        (0x20, 0),
        (0x40, 255),
        (0x80, 0),
        (0xc0, 0x40),
        (0xa0, 0x12),
        (0xe1, 0),
    ] {
        builder.add_chip_write(Instance::Secondary, Es5503Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(1000));
    builder.add_vgm_command(ram(Instance::Secondary, 0x10000, &[0x40]));
    builder.add_chip_write(
        Instance::Secondary,
        Es5503Spec {
            register: 0xe1,
            value: 62,
        },
    );
    builder.add_vgm_command(WaitSamples(2000));
    let mut doc = builder.finalize();
    doc.header.es5503_output_channels = 2;
    doc
}

#[test]
fn vgm_bytes_document_dynamic_rate_and_checkpoint() {
    let doc = document();
    let bytes: Vec<u8> = doc.clone().try_into().unwrap();
    let mut doc_player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    let mut byte_player = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    for _ in 0..3 {
        assert_eq!(doc_player.render().unwrap(), byte_player.render().unwrap());
    }
    let checkpoint = byte_player.checkpoint().unwrap();
    let blocks: Vec<_> = (0..15)
        .map(|_| byte_player.render().unwrap().unwrap())
        .collect();
    assert!(
        blocks
            .iter()
            .any(|block| block.frames().iter().any(|f| f[1] > 0.01))
    );
    assert!(
        blocks
            .iter()
            .any(|block| block.frames().iter().any(|f| f[1] < -0.01))
    );
    assert!(
        blocks
            .iter()
            .all(|block| block.frames().iter().all(|f| f[0] == 0.0))
    );
    byte_player.restore_checkpoint(&checkpoint).unwrap();
    for block in blocks {
        assert_eq!(Some(block), byte_player.render().unwrap());
    }
}

#[test]
fn header_output_channels_are_passed_to_custom_factory() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&seen);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Es5503, move |config| {
        captured
            .borrow_mut()
            .push((config.instance(), *config.settings()));
        Ok(Box::new(Es5503::with_output_channels(config.clock(), 2)?))
    });
    let _player =
        VgmSequencer::new_with_factories(document(), 44_100, 128, Default::default(), factories)
            .unwrap();
    assert_eq!(
        *seen.borrow(),
        vec![
            (
                Instance::Primary,
                ChipBackendSettings::Es5503 { output_channels: 2 }
            ),
            (
                Instance::Secondary,
                ChipBackendSettings::Es5503 { output_channels: 2 }
            ),
        ]
    );
}

fn vgm_tone_rms(oscillator_count_register: u8) -> f32 {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Es5503, Instance::Primary, 7_000_000);
    let wave: Vec<u8> = (0..256)
        .map(|index| {
            let phase = index as f32 * std::f32::consts::TAU / 256.0;
            (128.0 + 64.0 * phase.sin()).round() as u8
        })
        .collect();
    builder.add_vgm_command(ram(Instance::Primary, 0, &wave));
    for (register, value) in [
        (0x00, 0),
        (0x20, 0xa0),
        (0x40, 255),
        (0x80, 0),
        (0xc0, 0),
        (0xa0, 2),
        (0xe1, oscillator_count_register),
    ] {
        builder.add_chip_write(Instance::Primary, Es5503Spec { register, value });
    }

    builder.add_vgm_command(WaitSamples(4000));
    let mut doc = builder.finalize();
    doc.header.es5503_output_channels = 2;
    let mut player = VgmSequencer::new(doc, 44_100, 256, Default::default()).unwrap();
    let mut samples = Vec::new();
    while let Some(block) = player.render().unwrap() {
        samples.extend(block.frames().iter().map(|frame| frame[0]));
    }
    let steady = &samples[500..3500];
    (steady.iter().map(|x| x * x).sum::<f32>() / steady.len() as f32).sqrt()
}

#[test]
fn vgm_adapts_filter_to_preserve_and_reject_high_frequencies() {
    let audible = vgm_tone_rms(62);
    let ultrasonic = vgm_tone_rms(0);
    assert!(audible > 0.03, "8 kHz tone was attenuated: RMS {audible}");
    assert!(
        ultrasonic < 0.003,
        "ultrasonic tone leaked through: RMS {ultrasonic}"
    );
}

#[test]
fn vgm_rejects_out_of_range_waveform_blocks() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Es5503, Instance::Primary, 7_000_000);
    builder.add_vgm_command(ram(Instance::Primary, 0x1ffff, &[1, 2]));
    builder.add_vgm_command(WaitSamples(10));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().try_into().unwrap();
    let mut players = [
        VgmSequencer::new(doc, 44_100, 64, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 64, Default::default()).unwrap(),
    ];
    for player in &mut players {
        assert!(matches!(
            player.render(),
            Err(chipstream::sequencer::VgmSequencerError::Chip(
                ChipError::RamOutOfRange { .. }
            ))
        ));
    }
}

#[test]
fn vgm_callbacks_report_original_secondary_writes() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&writes);
    let mut player = VgmSequencer::new(document(), 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<soundlog::chip::state::Es5503State>();
    player.on_write::<Es5503Spec, _>(move |instance, spec, _, _| {
        captured.borrow_mut().push((instance, spec));
    });
    player.render().unwrap();
    assert!(writes.borrow().iter().any(|(instance, spec)| {
        *instance == Instance::Secondary && spec.register == 0xe1 && spec.value == 0
    }));
}
