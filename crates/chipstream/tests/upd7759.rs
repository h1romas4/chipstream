#![cfg(feature = "upd7759")]

use chipstream::chip::{ChipError, SoundChip, Upd7759};
use chipstream::sequencer::{ChipBackendSettings, ChipFactoryRegistry, VgmSequencer};
use soundlog::chip::state::Upd7759State;
use soundlog::chip::{Chip, Upd7759Spec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn write(chip: &mut Upd7759, register: u8, value: u8) {
    chip.write(Upd7759Spec { register, value }).unwrap();
}

fn rom() -> Vec<u8> {
    let mut rom = vec![0; 0x20];
    rom[..7].copy_from_slice(&[0, 0x5a, 0xa5, 0x69, 0x55, 0, 8]);
    rom[0x11..0x16].copy_from_slice(&[0x88, 3, 0x12, 0x78, 0]);
    rom
}

fn start(chip: &mut Upd7759) {
    write(chip, 2, 0);
    write(chip, 1, 0);
    write(chip, 1, 1);
}

fn render(chip: &mut Upd7759, length: usize) -> Vec<i32> {
    let mut samples = vec![0; length];
    chip.generate(&mut samples).unwrap();
    samples
}

#[test]
fn standalone_adpcm_timing_and_snapshot() {
    let mut chip = Upd7759::new(640_000).unwrap();
    assert_eq!(chip.sample_rate(), 160_000);
    chip.write_rom(0, &rom()).unwrap();
    start(&mut chip);
    assert!(chip.busy());
    let snapshot = chip.save_state().unwrap();
    let samples = render(&mut chip, 400);
    assert_eq!(&samples[..100], &[0; 100]);
    assert_eq!(&samples[100..109], &[128; 9]);
    assert_eq!(&samples[109..127], &[1408; 18]);
    assert_eq!(&samples[127..], &[0; 273]);
    assert!(!chip.busy());
    assert_eq!(samples.last(), Some(&0));
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut split = render(&mut chip, 71);
    split.extend(render(&mut chip, 329));
    assert_eq!(split, samples);
}

#[test]
fn bank_reset_and_invalid_start() {
    let mut chip = Upd7759::new(640_000).unwrap();
    chip.write_rom(0x20000, &rom()).unwrap();
    write(&mut chip, 3, 1);
    start(&mut chip);
    assert!(render(&mut chip, 200).iter().any(|sample| *sample != 0));
    write(&mut chip, 0, 0);
    assert!(!chip.busy());
    assert_eq!(render(&mut chip, 10), [0; 10]);
    write(&mut chip, 2, 0);
    write(&mut chip, 1, 0);
    assert!(!chip.busy());
    write(&mut chip, 0, 1);
    write(&mut chip, 1, 1);
    write(&mut chip, 2, 1);
    write(&mut chip, 1, 0);
    render(&mut chip, 300);
    assert!(!chip.busy());
    assert!(
        chip.write(Upd7759Spec {
            register: 4,
            value: 0
        })
        .is_err()
    );
    assert!(matches!(
        Upd7759::new(3),
        Err(ChipError::InvalidClock { .. })
    ));
}

#[test]
fn slave_fifo_plays_same_data_as_standalone() {
    let mut master = Upd7759::new(640_000).unwrap();
    master.write_rom(0, &rom()).unwrap();
    start(&mut master);
    let reference = render(&mut master, 400);
    let mut slave = Upd7759::with_slave_mode(640_000, true).unwrap();
    for data in [0x10, 0, 0, 8, 0, 0x88, 3, 0x12, 0x78, 0] {
        write(&mut slave, 2, data);
    }
    write(&mut slave, 1, 1);
    assert_eq!(render(&mut slave, 400), reference);
    assert!(!slave.busy());
}

#[test]
fn repeat_blocks_and_silence_follow_rom_commands() {
    let mut chip = Upd7759::new(640_000).unwrap();
    let mut data = rom();
    data[0x11..0x18].copy_from_slice(&[0xc1, 0x88, 3, 0x12, 0x78, 1, 0]);
    chip.write_rom(0, &data).unwrap();
    start(&mut chip);
    let output = render(&mut chip, 400);
    assert!(output[100..250].iter().any(|sample| *sample != 0));
    assert!(output[250..].iter().all(|sample| *sample == 0));
    assert!(chip.busy());
    render(&mut chip, 400);
    assert!(!chip.busy());
}

#[test]
fn vgm_slave_mode_preserves_queued_data() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Upd7759, Instance::Primary, 640_000);
    for data in [0x10, 0, 0, 8, 0, 0x88, 3, 0x12, 0x78, 0] {
        builder.add_chip_write(
            Instance::Primary,
            Upd7759Spec {
                register: 2,
                value: data,
            },
        );
    }
    builder.add_chip_write(
        Instance::Primary,
        Upd7759Spec {
            register: 1,
            value: 1,
        },
    );
    builder.add_vgm_command(WaitSamples(500));
    let mut document = builder.finalize();
    document.header.upd7759_clock |= 0x8000_0000;
    let bytes: Vec<u8> = document.clone().into();
    let mut document_player = VgmSequencer::new(document, 44_100, 64, Default::default()).unwrap();
    let mut byte_player = VgmSequencer::from_vgm(bytes, 44_100, 64, Default::default()).unwrap();
    let first = document_player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] != 0.0 && frame[1] != 0.0)
    );
    assert_eq!(byte_player.render().unwrap().unwrap(), first);
    let checkpoint = document_player.checkpoint().unwrap();
    let next = document_player.render().unwrap().unwrap();
    document_player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(document_player.render().unwrap().unwrap(), next);
    assert_eq!(byte_player.render().unwrap().unwrap(), next);
}

#[test]
fn vgm_secondary_rom_bank_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Upd7759, Instance::Primary, 640_000);
    builder.register_chip(Chip::Upd7759, Instance::Secondary, 640_000);
    let mut data = Vec::new();
    data.extend_from_slice(&0x20020_u32.to_le_bytes());
    data.extend_from_slice(&0x20000_u32.to_le_bytes());
    data.extend_from_slice(&rom());
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x8a,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [(3, 1), (2, 0), (1, 0), (1, 1)] {
        builder.add_chip_write(Instance::Secondary, Upd7759Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    let mut player = VgmSequencer::new(document, 44_100, 64, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Upd7759State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&events);
    player.on_write(move |instance, spec: Upd7759Spec, _, changes| {
        captured.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] != 0.0 && frame[1] != 0.0)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(events.borrow().iter().any(|(instance, spec, changes)| {
        *instance == Instance::Secondary && spec.register == 3 && changes.is_none()
    }));
    let mut parsed = VgmSequencer::from_vgm(bytes, 44_100, 64, Default::default()).unwrap();
    assert_eq!(parsed.render().unwrap().unwrap(), first);
    assert_eq!(parsed.render().unwrap().unwrap(), second);
}

#[test]
fn slave_mode_is_supplied_to_external_factories() {
    for slave in [false, true] {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Upd7759, Instance::Primary, 640_000);
        builder.add_vgm_command(WaitSamples(10));
        let mut document = builder.finalize();
        if slave {
            document.header.upd7759_clock |= 0x8000_0000;
        }
        let bytes: Vec<u8> = document.into();
        let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut factories = ChipFactoryRegistry::new();
        let capture = Arc::clone(&seen);
        factories.register(Chip::Upd7759, move |config| {
            capture
                .lock()
                .unwrap()
                .push((*config.settings(), config.clock()));
            Ok(Box::new(Upd7759::with_slave_mode(config.clock(), slave)?))
        });
        VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories)
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [(ChipBackendSettings::Upd7759 { slave }, 640_000)]
        );
    }
}
