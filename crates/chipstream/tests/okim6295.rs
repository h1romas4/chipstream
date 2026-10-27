use chipstream::chip::{ChipError, ChipRegisterWrite, Okim6295, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, Okim6295Spec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};

fn rom() -> Vec<u8> {
    vec![0, 0, 8, 0, 0, 9, 0, 0, 0x77, 0x88]
}

#[test]
fn sample_commands_decode_and_stop() {
    let mut chip = Okim6295::new(1_000_000).unwrap();
    chip.write_rom(0, &rom()).unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x80,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x10,
    })
    .unwrap();
    assert_eq!(chip.status(), 0xf1);
    let mut output = [0; 4];
    chip.generate(&mut output).unwrap();
    assert_eq!(output, [120, 372, 336, 304]);
    assert_eq!(chip.status(), 0xf0);
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x80,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x30,
    })
    .unwrap();
    assert_eq!(chip.status(), 0xf3);
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x08,
    })
    .unwrap();
    assert_eq!(chip.status(), 0xf2);
    chip.reset();
    assert_eq!(chip.status(), 0xf0);
}

#[test]
fn sample_banking_and_pin7() {
    let mut chip = Okim6295::new(1_000_000).unwrap();
    chip.write_rom(0x40000, &rom()).unwrap();
    chip.write(Okim6295Spec {
        register: 0x0f,
        value: 1,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x80,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x10,
    })
    .unwrap();
    let mut sample = [0];
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample, [120]);
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 165);
    chip.write(Okim6295Spec {
        register: 0x0c,
        value: 1,
    })
    .unwrap();
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 132);
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Okim6258(soundlog::chip::Okim6258Spec {
            register: 0,
            value: 0
        })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(
        chip.write(Okim6295Spec {
            register: 0x07,
            value: 0
        })
        .is_err()
    );
}

#[test]
fn nmk112_banks_the_table_and_sample_data() {
    let mut chip = Okim6295::new(1_000_000).unwrap();
    chip.write_rom(0x10000, &rom()).unwrap();
    chip.write(Okim6295Spec {
        register: 0x10,
        value: 1,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0x0e,
        value: 0x80,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x80,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0,
        value: 0x10,
    })
    .unwrap();
    let mut samples = [0; 4];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [120, 372, 336, 304]);
}

#[test]
fn clock_bytes_are_committed_on_the_final_write() {
    let mut chip = Okim6295::new(0x10000).unwrap();
    chip.write(Okim6295Spec {
        register: 0x0a,
        value: 0,
    })
    .unwrap();
    assert_eq!(chip.clock(), 0x10000);
    assert!(matches!(
        chip.write(Okim6295Spec {
            register: 0x0b,
            value: 0
        }),
        Err(ChipError::InvalidClock { clock: 0, .. })
    ));
    assert_eq!(chip.clock(), 0x10000);
    chip.write(Okim6295Spec {
        register: 0x09,
        value: 1,
    })
    .unwrap();
    chip.write(Okim6295Spec {
        register: 0x0b,
        value: 0,
    })
    .unwrap();
    assert_eq!(chip.clock(), 0x100);
}

#[test]
fn vgm_rom_dynamic_pin_and_document_match() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6295, Instance::Primary, 1_000_000);
    let mut data = Vec::new();
    data.extend_from_slice(&10_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&rom());
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x8b,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [(0, 0x80), (0, 0x10)] {
        builder.add_chip_write(Instance::Primary, Okim6295Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(10));
    builder.add_chip_write(
        Instance::Primary,
        Okim6295Spec {
            register: 0x0c,
            value: 1,
        },
    );
    for (register, value) in [(0x08, 0x80), (0x09, 0x84), (0x0a, 0x1e), (0x0b, 0)] {
        builder.add_chip_write(Instance::Primary, Okim6295Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 31).unwrap();
    let mut document = VgmSequencer::new(parsed, 44_100, 256).unwrap();
    let render = |player: &mut VgmSequencer| {
        let mut result = Vec::new();
        while let Some(block) = player.render().unwrap() {
            result.extend_from_slice(block.frames());
        }
        result
    };
    let audio = render(&mut stream);
    assert_eq!(audio, render(&mut document));
    assert!(audio.iter().any(|frame| frame[0] != 0.0 && frame[1] != 0.0));
}
