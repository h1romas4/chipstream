#![cfg(feature = "nes_apu")]

use chipstream::chip::{ChipError, NesApu, SoundChip};
use chipstream::sequencer::{VgmSequencer, VgmSequencerError};
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::NesApuState;
use soundlog::chip::{Chip, NesApuSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut NesApu, register: u8, value: u8) {
    chip.write(NesApuSpec { register, value }).unwrap();
}

fn setup(mode: usize) -> NesApu {
    let mut chip = NesApu::new(if mode == 4 { 1_662_607 } else { 1_789_773 }).unwrap();
    let memory: Vec<u8> = (0..64).map(|i| (i * 37 + 19) as u8).collect();
    chip.write_ram(0xc000, &memory).unwrap();
    let writes: &[(u8, u8)] = match mode {
        0 | 4 => &[(0x15, 1), (0, 0xbf), (2, 0x40), (3, 8)],
        1 => &[(0x15, 4), (8, 0xff), (0x0a, 0x48), (0x0b, 8)],
        2 => &[(0x15, 8), (0x0c, 0x3f), (0x0e, 4), (0x0f, 8)],
        _ => &[(0x10, 0x4f), (0x11, 0x40), (0x12, 0), (0x13, 3), (0x15, 16)],
    };
    for &(register, value) in writes {
        write(&mut chip, register, value);
    }
    chip
}

#[test]
fn channels_match_jsnes_native_reference() {
    let expected = [
        [3964, 3955, -106, 3150, 1994, 1972, 1960],
        [0, 0, 0, 0, 961, -826, -2825],
        [4559, 4640, 4483, 3644, 1877, -251, -2254],
        [9347, 9502, 7961, 5002, -402, 1945, -579],
        [3964, 3955, -115, -492, 1969, 1958, -2031],
    ];
    let indexes = [0, 1, 16, 128, 1000, 5000, 10000];
    for (mode, expected) in expected.iter().enumerate() {
        let mut chip = setup(mode);
        let mut samples = vec![0; 10001];
        chip.generate(&mut samples).unwrap();
        for (&index, &target) in indexes.iter().zip(expected) {
            assert!(
                (samples[index] - target).abs() <= 1,
                "mode {mode}, sample {index}: {} != {target}",
                samples[index]
            );
        }
    }
}

#[test]
fn envelope_sweep_and_frame_counter_match_reference() {
    let mut chip = NesApu::new(1_789_773).unwrap();
    for (register, value) in [
        (0x15, 3),
        (0, 0x82),
        (1, 0x91),
        (2, 0x70),
        (3, 0x18),
        (4, 0x5c),
        (6, 0x38),
        (7, 0x18),
    ] {
        write(&mut chip, register, value);
    }
    let mut samples = [0; 10001];
    chip.generate(&mut samples[..1000]).unwrap();
    write(&mut chip, 0x17, 0x80);
    chip.generate(&mut samples[1000..]).unwrap();
    for (index, expected) in [
        (0, 3226),
        (1, 3218),
        (16, -31),
        (128, -147),
        (1000, -1303),
        (5000, 0),
        (10000, 0),
    ] {
        assert!(
            (samples[index] - expected).abs() <= 1,
            "sample {index}: {}",
            samples[index]
        );
    }
}

#[test]
fn snapshot_restores_dmc_ram_and_decoder() {
    let mut chip = setup(3);
    let mut prefix = [0; 50];
    chip.generate(&mut prefix).unwrap();
    let state = chip.save_state().unwrap();
    let mut expected = [0; 2000];
    chip.generate(&mut expected).unwrap();
    chip.write_ram(0xc000, &[0; 64]).unwrap();
    chip.restore_state(state.as_ref()).unwrap();
    let mut actual = [0; 2000];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
    assert!(matches!(
        chip.write_ram(65535, &[1, 2]),
        Err(ChipError::RamOutOfRange { .. })
    ));
}

#[test]
fn vgm_rejects_out_of_range_dmc_ram() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::NesApu, Instance::Primary, 1_789_773);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xc2,
        size: 4,
        data: vec![0xff, 0xff, 1, 2],
    });
    builder.add_vgm_command(WaitSamples(20));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::Chip(ChipError::RamOutOfRange {
            chip: Chip::NesApu,
            start: 65535,
            length: 2,
        }))
    ));
}

#[test]
fn vgm_dmc_ram_upload_produces_audio() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::NesApu, Instance::Primary, 1_789_773);
    let mut data = vec![0, 0xc0];
    data.extend((0..64).map(|i| (i * 37 + 19) as u8));
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xc2,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [(0x10, 0x4f), (0x11, 0x40), (0x12, 0), (0x13, 3), (0x15, 16)] {
        builder.add_chip_write(Instance::Primary, NesApuSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(1500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().try_into().unwrap();
    let mut document_player = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
    let mut stream_player = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    let first = document_player.render().unwrap().unwrap();
    assert!(first.frames().iter().any(|frame| frame[0].abs() > 0.01));
    assert_eq!(first, stream_player.render().unwrap().unwrap());
    let checkpoint = document_player.checkpoint().unwrap();
    let second = document_player.render().unwrap().unwrap();
    document_player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(second, document_player.render().unwrap().unwrap());
}

#[test]
fn dual_vgm_uses_independent_ram_and_callbacks() {
    fn document(secondary_byte: u8) -> soundlog::vgm::VgmDocument {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::NesApu, Instance::Primary, 1_789_773);
        builder.register_chip(Chip::NesApu, Instance::Secondary, 1_789_773);
        for (instance, byte) in [
            (Instance::Primary, 0),
            (Instance::Secondary, secondary_byte),
        ] {
            let mut data = vec![0, 0xc0];
            data.extend([byte; 17]);
            builder.add_vgm_command(DataBlock {
                marker: 0x66,
                chip_instance: instance as u8,
                data_type: 0xc2,
                size: data.len() as u32,
                data,
            });
            for (register, value) in [(0x10, 0x4f), (0x11, 0x40), (0x12, 0), (0x13, 1), (0x15, 16)]
            {
                builder.add_chip_write(instance, NesApuSpec { register, value });
            }
        }
        builder.add_vgm_command(WaitSamples(1000));
        builder.finalize()
    }

    fn render(doc: soundlog::vgm::VgmDocument, serialized: bool) -> (Vec<[f32; 2]>, Vec<Instance>) {
        let mut player = if serialized {
            VgmSequencer::from_vgm(
                Vec::<u8>::try_from(doc).unwrap(),
                44_100,
                128,
                Default::default(),
            )
            .unwrap()
        } else {
            VgmSequencer::new(doc, 44_100, 57, Default::default()).unwrap()
        };
        player
            .set_chip_gain(Chip::NesApu, Instance::Primary, 0.0)
            .unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let output = Rc::clone(&events);
        player.on_write(move |instance, spec: NesApuSpec, _, _| {
            if spec.register == 0x15 {
                output.borrow_mut().push(instance);
            }
        });
        let mut frames = Vec::new();
        while let Some(block) = player.render().unwrap() {
            frames.extend_from_slice(block.frames());
        }
        let observed = events.borrow().clone();
        (frames, observed)
    }

    let doc = document(0xff);
    let (expected, events) = render(doc.clone(), false);
    assert_eq!(events, vec![Instance::Primary, Instance::Secondary]);
    assert_eq!(render(doc, true), (expected.clone(), events));
    assert_ne!(expected, render(document(0), false).0);
}

#[test]
fn vgm_tracker_reports_clocked_register_state() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::NesApu, Instance::Primary, 1_662_607);
    for (register, value) in [(2, 0x40), (3, 0), (0x15, 1)] {
        builder.add_chip_write(Instance::Primary, NesApuSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(100));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
    player
        .track_state::<NesApuState>(Instance::Primary, 1_662_607.0)
        .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let output = seen.clone();
    player.on_write(move |_, _: NesApuSpec, _, events| {
        if let Some(events) = events {
            output.borrow_mut().extend(events);
        }
    });
    while player.render().unwrap().is_some() {}
    let events = seen.borrow();
    let StateEvent::KeyOn { channel: 0, tone } = events[0] else {
        panic!("expected pulse key on")
    };
    assert_eq!(tone.freq_hz, Some(1_662_607.0 / (16.0 * 65.0)));
}
