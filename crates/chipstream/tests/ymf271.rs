#![cfg(feature = "ymf271")]

use chipstream::chip::{ChipError, SoundChip, Ymf271};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, Ymf271Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use std::cell::Cell;
use std::rc::Rc;

fn write(chip: &mut Ymf271, port: u8, register: u8, value: u8) {
    chip.write(Ymf271Spec {
        port,
        register,
        value,
    })
    .unwrap();
}

fn pcm_writes() -> [(u8, u8, u8); 11] {
    [
        (6, 0, 3),
        (4, 0x30, 4),
        (4, 0x60, 1),
        (0, 0x30, 1),
        (0, 0x50, 31),
        (0, 0x80, 15),
        (0, 0xb0, 7),
        (0, 0xa0, 0),
        (0, 0x90, 0),
        (0, 0xd0, 0x0f),
        (0, 0x00, 1),
    ]
}

#[test]
fn pcm_loop_snapshot_and_bounds() {
    let mut chip = Ymf271::new(16_934_400).unwrap();
    assert_eq!(chip.channels(), 8);
    chip.write_rom(0, &[0x20, 0x40, 0x60, 0x7f, 0]).unwrap();
    for (port, register, value) in pcm_writes() {
        write(&mut chip, port, register, value);
    }
    let snapshot = chip.save_state().unwrap();
    let mut samples = [0; 8 * 256];
    chip.generate(&mut samples).unwrap();
    assert_eq!(chip.read_status() & 8, 8);
    assert_eq!(chip.read_status() & 8, 0);
    assert!(samples.chunks_exact(8).any(|s| s[0] > 100));
    assert!(samples.chunks_exact(8).all(|s| s[1] == 0));
    assert_eq!(&samples[200 * 8..201 * 8], &samples[203 * 8..204 * 8]);
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut again = [0; 8 * 256];
    chip.generate(&mut again).unwrap();
    assert_eq!(samples, again);
    assert!(matches!(
        chip.write_rom(0x800000, &[1]),
        Err(ChipError::RomTooLarge)
    ));
    let mut invalid = [123; 3];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [123; 3]);
}

#[test]
fn timers_and_external_memory() {
    let mut chip = Ymf271::new(16_934_400).unwrap();
    chip.write_rom(1, &[0x12, 0x34]).unwrap();
    assert_eq!(chip.read_external(), 0xff);
    for (register, value) in [(0x14, 1), (0x15, 0), (0x16, 0x80)] {
        write(&mut chip, 6, register, value);
    }
    assert_eq!(chip.read_external(), 0x12);
    assert_eq!(chip.read_external(), 0x34);
    assert_eq!(chip.read_external(), 0xff);
    for (register, value) in [(0x10, 0xff), (0x11, 3), (0x13, 1)] {
        write(&mut chip, 6, register, value);
    }
    chip.generate(&mut [0; 8]).unwrap();
    assert_eq!(chip.read_status() & 3, 1);
    write(&mut chip, 6, 0x13, 0x10);
    assert_eq!(chip.read_status() & 3, 0);
}

#[test]
fn packed_pcm_and_alternate_loop_error() {
    let mut chip = Ymf271::new(16_934_400).unwrap();
    chip.write_rom(0, &[0x12, 0x63, 0x45, 0]).unwrap();
    for (port, register, value) in pcm_writes() {
        write(&mut chip, port, register, value);
    }
    write(&mut chip, 4, 0x30, 2);
    write(&mut chip, 4, 0x60, 0);
    write(&mut chip, 4, 0x90, 4);
    write(&mut chip, 0, 0x50, 0xff);
    write(&mut chip, 0, 0, 1);
    let mut samples = [0; 8 * 4];
    chip.generate(&mut samples).unwrap();
    let left: Vec<_> = samples.chunks_exact(8).map(|s| s[0]).collect();
    assert_eq!(left, [1160, 4426, 1160, 4426]);
    assert!(matches!(
        chip.write(Ymf271Spec {
            port: 4,
            register: 0x20,
            value: 0x80
        }),
        Err(ChipError::InvalidConfiguration { .. })
    ));
}

#[test]
fn fm_all_sync_modes_produce_audio_and_reset() {
    for mode in 0..4 {
        let mut chip = Ymf271::new(16_934_400).unwrap();
        write(&mut chip, 6, 0, mode);
        for port in 0..4 {
            for (register, value) in [(0x30, 1), (0x50, 31), (0x80, 15)] {
                write(&mut chip, port, register, value);
            }
        }
        for (register, value) in [(0xc0, 15), (0xa0, 8), (0x90, 0), (0xd0, 0x0f), (0, 1)] {
            write(&mut chip, 0, register, value);
        }
        let mut samples = [0; 8 * 128];
        chip.generate(&mut samples).unwrap();
        assert!(samples.iter().any(|&v| v != 0), "mode {mode}");
        chip.reset();
        chip.generate(&mut samples).unwrap();
        assert!(samples.iter().all(|&v| v == 0));
    }
}

#[test]
fn tracked_frequency_matches_generated_fm() {
    use soundlog::chip::event::StateEvent;
    use soundlog::chip::state::{ChipState, Ymf271State};

    let mut chip = Ymf271::new(16_934_400).unwrap();
    let mut state = Ymf271State::new(16_934_400.0);
    let mut tracked = None;
    for (port, register, value) in [
        (6, 0, 3),
        (2, 0x30, 1),
        (2, 0x50, 0xff),
        (2, 0x80, 15),
        (2, 0xa0, 0x45),
        (2, 0x90, 0x1c),
        (2, 0, 1),
    ] {
        write(&mut chip, port, register, value);
        state.set_port(port);
        if let Some(events) = state.on_register_write(register, value) {
            for event in events {
                if let StateEvent::KeyOn { tone, .. } = event {
                    tracked = tone.freq_hz;
                }
            }
        }
    }
    let mut samples = vec![0; 44_100 * 8];
    chip.generate(&mut samples).unwrap();
    let left: Vec<_> = samples.chunks_exact(8).map(|s| s[0]).collect();
    let cycles = left
        .windows(2)
        .filter(|pair| pair[0] <= 0 && pair[1] > 0)
        .count() as f32;
    assert!((cycles - tracked.unwrap()).abs() < 1.0);
}

#[test]
fn vgm_rom_and_write_playback() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ymf271, Instance::Primary, 16_934_400);
    let mut data = Vec::new();
    data.extend_from_slice(&5_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x20, 0x40, 0x60, 0x7f, 0]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x85,
        size: data.len() as u32,
        data,
    });
    for (port, register, value) in pcm_writes() {
        builder.add_chip_write(
            Instance::Primary,
            Ymf271Spec {
                port,
                register,
                value,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    for mut player in [
        VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
    ] {
        let output = player.render().unwrap().unwrap();
        assert!(output.frames().iter().any(|frame| frame[0].abs() > 0.01));
    }
}

#[test]
fn vgm_secondary_rom_and_callbacks() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ymf271, Instance::Primary, 16_934_400);
    builder.register_chip(Chip::Ymf271, Instance::Secondary, 16_934_400);
    let mut data = Vec::new();
    data.extend_from_slice(&5_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x20, 0x40, 0x60, 0x7f, 0]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x85,
        size: data.len() as u32,
        data,
    });
    for (port, register, value) in pcm_writes() {
        builder.add_chip_write(
            Instance::Secondary,
            Ymf271Spec {
                port,
                register,
                value,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    for mut player in [
        VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
    ] {
        let seen = Rc::new(Cell::new(false));
        let captured = Rc::clone(&seen);
        player.on_write(move |instance, spec: Ymf271Spec, _, _| {
            if instance == Instance::Secondary && spec.port == 0 && spec.register == 0 {
                captured.set(true);
            }
        });
        let output = player.render().unwrap().unwrap();
        assert!(seen.get());
        assert!(output.frames().iter().any(|frame| frame[0].abs() > 0.01));
    }
}
