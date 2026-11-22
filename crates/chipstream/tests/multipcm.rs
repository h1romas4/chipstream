#![cfg(feature = "multipcm")]

use chipstream::chip::{ChipError, MultiPcm, SoundChip};
use chipstream::sequencer::{ChipFactoryRegistry, VgmSequencer};
use soundlog::chip::state::MultiPcmState;
use soundlog::chip::{Chip, MultiPcmBankSpec, MultiPcmSpec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut MultiPcm, register: u8, value: u8) {
    chip.write(MultiPcmSpec { register, value }).unwrap();
}

fn slot_write(chip: &mut MultiPcm, slot: u8, register: u8, value: u8) {
    write(chip, 1, slot);
    write(chip, 2, register);
    write(chip, 0, value);
}

fn header(start: u32) -> [u8; 12] {
    [
        (start >> 16) as u8,
        (start >> 8) as u8,
        start as u8,
        0,
        0,
        0xff,
        0xfc,
        0,
        0xf0,
        0,
        0xff,
        0,
    ]
}

fn configure(chip: &mut MultiPcm) {
    for (reg, value) in [(1, 0), (3, 0x10), (5, 1), (0, 0), (4, 0x80)] {
        slot_write(chip, 0, reg, value);
    }
}

fn samples(chip: &mut MultiPcm, frames: usize) -> Vec<i32> {
    let mut output = vec![0; frames * 2];
    chip.generate(&mut output).unwrap();
    output
}

#[test]
fn pcm_8bit_header_loop_pan_and_keyoff() {
    let mut chip = MultiPcm::new(8_064_000).unwrap();
    assert_eq!(chip.sample_rate(), 36_000);
    chip.write_rom(0, &header(0x100)).unwrap();
    chip.write_rom(0x100, &[0x40, 0x80, 0x20, 0x7f]).unwrap();
    configure(&mut chip);
    let output = samples(&mut chip, 9);
    assert_eq!(&output[0..2], [0, 0]);
    assert!(output[2] > 15_000 && output[2] == output[3]);
    assert!(output[4] < -30_000 && output[4] == output[5]);
    assert_eq!(&output[2..10], &output[10..18]);
    slot_write(&mut chip, 0, 0, 0x70);
    assert_eq!(samples(&mut chip, 1)[0], 0);
    slot_write(&mut chip, 0, 4, 0);
    assert_eq!(samples(&mut chip, 2), [0; 4]);
    chip.reset();
    configure(&mut chip);
    assert!(samples(&mut chip, 3).iter().any(|value| *value != 0));
}

#[test]
fn pcm12_lfo_envelope_snapshot_and_chunking() {
    let mut chip = MultiPcm::new(8_064_000).unwrap();
    chip.write_rom(0, &header(0x400100)).unwrap();
    chip.write_rom(0x100, &[0x40, 0xf1, 0x80, 0x20, 0x82, 0x70])
        .unwrap();
    configure(&mut chip);
    let output = samples(&mut chip, 3);
    assert!(output[2] > 15_000);
    assert!(output[4] < -30_000);
    slot_write(&mut chip, 0, 6, 0x3f);
    slot_write(&mut chip, 0, 10, 7);
    slot_write(&mut chip, 0, 7, 0x8a);
    let snapshot = chip.save_state().unwrap();
    let whole = samples(&mut chip, 100);
    assert!(whole.iter().any(|value| *value != 0));
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut split = samples(&mut chip, 31);
    split.extend(samples(&mut chip, 69));
    assert_eq!(split, whole);
    chip.restore_state(snapshot.as_ref()).unwrap();
    let modulated = samples(&mut chip, 3_000);
    chip.restore_state(snapshot.as_ref()).unwrap();
    slot_write(&mut chip, 0, 6, 0);
    slot_write(&mut chip, 0, 10, 0);
    assert_ne!(samples(&mut chip, 3_000), modulated);
}

#[test]
fn keyoff_releases_non_instant_envelope() {
    let mut chip = MultiPcm::new(8_064_000).unwrap();
    let mut sample_header = header(0x100);
    sample_header[10] = 0xfe;
    chip.write_rom(0, &sample_header).unwrap();
    chip.write_rom(0x100, &[0x40; 4]).unwrap();
    configure(&mut chip);
    assert!(samples(&mut chip, 3)[4] > 0);
    slot_write(&mut chip, 0, 4, 0);
    assert!(samples(&mut chip, 10).iter().any(|value| *value != 0));
    samples(&mut chip, 3_000);
    assert_eq!(samples(&mut chip, 3), [0; 6]);
}

#[test]
fn board_bank_switching_changes_live_reads() {
    let mut chip = MultiPcm::new(8_064_000).unwrap();
    chip.write_rom(0, &header(0x100000)).unwrap();
    chip.write_rom(0x200000, &[0x40; 4]).unwrap();
    chip.write_rom(0x300000, &[0x80; 4]).unwrap();
    chip.write(MultiPcmBankSpec {
        channel: 3,
        bank_offset: 0x20,
    })
    .unwrap();
    configure(&mut chip);
    assert!(samples(&mut chip, 3)[4] > 0);
    let snapshot = chip.save_state().unwrap();
    chip.write(MultiPcmBankSpec {
        channel: 3,
        bank_offset: 0x30,
    })
    .unwrap();
    assert!(samples(&mut chip, 3)[4] < 0);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert!(samples(&mut chip, 3)[4] > 0);
    chip.write(MultiPcmBankSpec {
        channel: 1,
        bank_offset: 0x30,
    })
    .unwrap();
    assert!(samples(&mut chip, 3)[4] > 0);
    chip.write_rom(0, &header(0x180000)).unwrap();
    slot_write(&mut chip, 0, 1, 0);
    assert!(samples(&mut chip, 3)[4] < 0);
    assert!(
        chip.write(MultiPcmBankSpec {
            channel: 4,
            bank_offset: 0
        })
        .is_err()
    );
}

#[test]
fn legacy_board_banks_support_model1_and_independent_multi32_windows() {
    let mut chip = MultiPcm::new(8_064_000).unwrap();
    chip.write_rom(0, &header(0x100000)).unwrap();
    chip.write_rom(0x180000, &[0x40; 4]).unwrap();
    chip.write_rom(0x200000, &[0x60; 4]).unwrap();
    chip.write_rom(0x280000, &[0x80; 4]).unwrap();

    write(&mut chip, 0x11, 3);
    write(&mut chip, 0x12, 0);
    configure(&mut chip);
    assert!(samples(&mut chip, 3)[4] > 15_000);

    write(&mut chip, 0x12, 3);
    chip.write_rom(0, &header(0x180000)).unwrap();
    slot_write(&mut chip, 0, 1, 0);
    assert!(samples(&mut chip, 3)[4] > 15_000);

    write(&mut chip, 0x10, 2);
    assert!(samples(&mut chip, 3)[4] < -30_000);
    chip.write_rom(0, &header(0x100000)).unwrap();
    slot_write(&mut chip, 0, 1, 0);
    assert!(samples(&mut chip, 3)[4] > 20_000);

    write(&mut chip, 0x11, 3);
    chip.write_rom(0, &header(0x180000)).unwrap();
    slot_write(&mut chip, 0, 1, 0);
    let snapshot = chip.save_state().unwrap();
    assert!(samples(&mut chip, 3)[4] < -30_000);
    write(&mut chip, 0x12, 3);
    assert!(samples(&mut chip, 3)[4] > 15_000);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert!(samples(&mut chip, 3)[4] < -30_000);
}

#[test]
fn vgm_legacy_multi32_bank_writes_play_banked_samples() {
    for instance in [Instance::Primary, Instance::Secondary] {
        let render = |legacy: bool| {
            let mut builder = VgmBuilder::new();
            builder.register_chip(Chip::MultiPcm, instance, 8_064_000);
            for (start, bytes) in [
                (0_u32, header(0x100000).to_vec()),
                (0x180000, vec![0x40; 4]),
            ] {
                let mut data = Vec::new();
                data.extend_from_slice(&0x180004_u32.to_le_bytes());
                data.extend_from_slice(&start.to_le_bytes());
                data.extend_from_slice(&bytes);
                builder.add_vgm_command(DataBlock {
                    marker: 0x66,
                    chip_instance: instance as u8,
                    data_type: 0x89,
                    size: data.len() as u32,
                    data,
                });
            }
            if legacy {
                for (register, value) in [(0x11, 3), (0x12, 0)] {
                    builder.add_chip_write(instance, MultiPcmSpec { register, value });
                }
            } else {
                for (channel, bank_offset) in [(2, 0x18), (1, 0)] {
                    builder.add_chip_write(
                        instance,
                        MultiPcmBankSpec {
                            channel,
                            bank_offset,
                        },
                    );
                }
            }
            for (register, value) in [
                (1, 0),
                (2, 1),
                (0, 0),
                (2, 3),
                (0, 0x10),
                (2, 5),
                (0, 1),
                (2, 4),
                (0, 0x80),
            ] {
                builder.add_chip_write(instance, MultiPcmSpec { register, value });
            }
            builder.add_vgm_command(WaitSamples(500));
            let bytes: Vec<u8> = builder.finalize().into();
            let mut player =
                VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
            player.track_state_with_voice_clocks::<MultiPcmState>();
            player.render().unwrap().unwrap()
        };
        let legacy = render(true);
        assert!(legacy.frames().iter().any(|frame| frame[0] > 0.01));
        assert_eq!(legacy, render(false));
    }
}

#[test]
fn vgm_clock_produces_expected_pitch_and_factory_clock() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::MultiPcm, Instance::Primary, 7_938_000);
    let mut sample_header = header(0x100);
    sample_header[6] = 0xe0;
    let mut rom = vec![0; 0x120];
    rom[..12].copy_from_slice(&sample_header);
    rom[0x100..0x110].fill(0x40);
    rom[0x110..0x120].fill(0xc0);
    let mut data = Vec::new();
    data.extend_from_slice(&(rom.len() as u32).to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&rom);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x89,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (1, 0),
        (2, 1),
        (0, 0),
        (2, 3),
        (0, 0x10),
        (2, 5),
        (0, 1),
        (2, 4),
        (0, 0x80),
    ] {
        builder.add_chip_write(Instance::Primary, MultiPcmSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(44_100));
    let doc = builder.finalize();
    let mut player = VgmSequencer::new(doc.clone(), 44_100, 1024, Default::default()).unwrap();
    let mut previous = 0.0;
    let mut rising_edges = 0;
    while let Some(block) = player.render().unwrap() {
        for frame in block.frames() {
            if previous <= 0.0 && frame[0] > 0.0 {
                rising_edges += 1;
            }
            previous = frame[0];
        }
    }
    assert!((1377..=1380).contains(&rising_edges), "{rising_edges}");

    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::MultiPcm, |config| {
        assert_eq!(config.clock(), 9_878_400);
        Ok(Box::new(MultiPcm::new(config.clock())?))
    });
    VgmSequencer::new_with_factories(doc, 44_100, 1024, Default::default(), factories).unwrap();
}

#[test]
fn invalid_operations_are_explicit() {
    assert!(matches!(
        MultiPcm::new(223),
        Err(ChipError::InvalidClock { .. })
    ));
    let mut chip = MultiPcm::new(8_064_000).unwrap();
    let mut out = [42; 3];
    assert!(matches!(
        chip.generate(&mut out),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(out, [42; 3]);
    assert!(
        chip.write(MultiPcmSpec {
            register: 13,
            value: 1
        })
        .is_err()
    );
    write(&mut chip, 1, 7);
    write(&mut chip, 2, 1);
    write(&mut chip, 0, 0);
    assert_eq!(samples(&mut chip, 2), [0; 4]);
}

#[test]
fn vgm_secondary_rom_banks_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::MultiPcm, Instance::Primary, 8_064_000);
    builder.register_chip(Chip::MultiPcm, Instance::Secondary, 8_064_000);
    for (start, bytes) in [
        (0_u32, header(0x100000).to_vec()),
        (0x200000, vec![0x40; 4]),
    ] {
        let mut data = Vec::new();
        data.extend_from_slice(&0x200004_u32.to_le_bytes());
        data.extend_from_slice(&start.to_le_bytes());
        data.extend_from_slice(&bytes);
        builder.add_vgm_command(DataBlock {
            marker: 0x66,
            chip_instance: 1,
            data_type: 0x89,
            size: data.len() as u32,
            data,
        });
    }
    builder.add_chip_write(
        Instance::Secondary,
        MultiPcmBankSpec {
            channel: 3,
            bank_offset: 0x20,
        },
    );
    for (register, value) in [
        (1, 0),
        (2, 1),
        (0, 0),
        (2, 3),
        (0, 0x10),
        (2, 5),
        (0, 1),
        (2, 4),
        (0, 0x80),
    ] {
        builder.add_chip_write(Instance::Secondary, MultiPcmSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    assert!(bytes.windows(4).any(|bytes| bytes == [0xc3, 0x83, 0x20, 0]));
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<MultiPcmState>();
    let observed = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&observed);
    player.on_write(move |instance, spec: MultiPcmBankSpec, _, changes| {
        captured.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] > 0.01 && frame[1] > 0.01)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(
        observed
            .borrow()
            .iter()
            .any(|(instance, spec, changes)| *instance == Instance::Secondary
                && spec.bank_offset == 0x20
                && changes.is_none())
    );
    for mut other in [
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::new(parsed, 44_100, 128, Default::default()).unwrap(),
    ] {
        assert_eq!(other.render().unwrap().unwrap(), first);
        assert_eq!(other.render().unwrap().unwrap(), second);
    }
}
