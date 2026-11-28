#![cfg(feature = "qsound")]

use chipstream::chip::{ChipError, Qsound, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, QsoundSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use std::cell::Cell;
use std::rc::Rc;

const WRITES: [(u8, u16); 15] = [
    (0x78, 0x8000),
    (0x01, 1),
    (0x02, 0x1000),
    (0x04, 64),
    (0x05, 65),
    (0x06, 0x4000),
    (0x80, 0x120),
    (0xba, 0x2000),
    (0x93, 0x1000),
    (0xca, 2),
    (0xcb, 8),
    (0xcc, 0x8000),
    (0xcd, 0x4000),
    (0xd6, 1),
    (0x90, 0x120),
];

fn rom() -> Vec<u8> {
    (0..256).map(|i| (i * 37 + 17) as u8).collect()
}

#[test]
fn pcm_adpcm_effects_and_snapshot_match_reference() {
    let mut chip = Qsound::new(60_000_000).unwrap();
    chip.write_rom(0, &rom()).unwrap();
    for (register, value) in WRITES {
        chip.write(QsoundSpec { register, value }).unwrap();
    }
    let mut warmup = [0; 200];
    chip.generate(&mut warmup).unwrap();
    let snapshot = chip.save_state().unwrap();
    let mut output = [0; 32];
    chip.generate(&mut output).unwrap();
    assert_eq!(
        output,
        [
            -225, -13566, 14895, -1119, 32767, 9049, 32285, 13444, -28778, 14408, -24380, 17933,
            -12804, -32767, 281, -13389, 15561, -1065, 32767, 8570, 16241, 12763, 12634, 14324,
            -32340, 2081, -27451, 14809, -14757, -32767, 745, -16954,
        ]
    );
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut again = [0; 32];
    chip.generate(&mut again).unwrap();
    assert_eq!(output, again);
    assert!(matches!(
        chip.generate(&mut [0; 3]),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert!(matches!(
        chip.write_rom(64 * 1024 * 1024, &[0]),
        Err(ChipError::RomTooLarge)
    ));
}

#[test]
fn second_filter_mode_matches_reference() {
    let mut chip = Qsound::new(60_000_000).unwrap();
    chip.write_rom(0, &rom()).unwrap();
    chip.write(QsoundSpec {
        register: 0xe3,
        value: 0x61a,
    })
    .unwrap();
    for (register, value) in WRITES {
        chip.write(QsoundSpec { register, value }).unwrap();
    }
    chip.generate(&mut [0; 200]).unwrap();
    let mut output = [0; 32];
    chip.generate(&mut output).unwrap();
    assert_eq!(
        output,
        [
            -17744, 32767, 1474, -15306, 9452, -32767, 13312, -31582, 10697, -9888, -3132, 11752,
            -12720, 32767, -19007, 32767, 715, -13554, 8498, -32767, 12053, -29827, 9496, -8132,
            -4382, 13511, -13920, 32767, -20267, 32767, -41, -11796,
        ]
    );
}

#[test]
fn vgm_rom_legacy_clock_and_16_bit_writes() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Qsound, Instance::Primary, 4_000_000);
    let mut data = Vec::new();
    data.extend_from_slice(&256_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&rom());
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x8f,
        size: data.len() as u32,
        data,
    });
    for (register, value) in WRITES {
        builder.add_chip_write(Instance::Primary, QsoundSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(1000));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().try_into().unwrap();
    for mut player in [
        VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
    ] {
        let seen = Rc::new(Cell::new(false));
        let captured = Rc::clone(&seen);
        player.on_write(move |instance, spec: QsoundSpec, _, _| {
            if instance == Instance::Primary && spec.register == 0x78 && spec.value == 0x8000 {
                captured.set(true);
            }
        });
        let output = player.render().unwrap().unwrap();
        assert!(seen.get());
        assert!(output.frames().iter().any(|frame| frame[0].abs() > 0.01));
    }
}
