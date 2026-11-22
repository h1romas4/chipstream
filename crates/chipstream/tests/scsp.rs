#![cfg(feature = "scsp")]

use chipstream::chip::{Scsp, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, ScspSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};

const SAMPLES: [i16; 8] = [1000, -1000, 2000, -2000, 3000, -3000, 4000, -4000];

fn word(chip: &mut Scsp, offset: u16, value: u16) {
    for (i, byte) in value.to_be_bytes().into_iter().enumerate() {
        chip.write(ScspSpec {
            offset: offset + i as u16,
            value: byte,
        })
        .unwrap();
    }
}

fn samples() -> Vec<u8> {
    SAMPLES.into_iter().flat_map(i16::to_be_bytes).collect()
}

fn setup(chip: &mut Scsp) {
    chip.write_ram(0, &samples()).unwrap();
    word(chip, 0x400, 15);
    word(chip, 0x006, 8);
    word(chip, 0x00c, 0x100);
    word(chip, 0x016, 0xe000);
    word(chip, 0x000, 0x1820);
}

#[test]
fn pcm_matches_reference_and_restores_snapshot() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    setup(&mut chip);
    let state = chip.save_state().unwrap();
    let mut actual = [0; 48];
    chip.generate(&mut actual).unwrap();
    for (i, frame) in actual.chunks_exact(2).enumerate() {
        assert_eq!(frame, [i32::from(SAMPLES[i % 8]); 2]);
    }
    chip.restore_state(state.as_ref()).unwrap();
    let mut replay = [0; 48];
    chip.generate(&mut replay).unwrap();
    assert_eq!(actual, replay);
}

#[test]
fn fm_ring_modulation_matches_reference() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &[0, 16].repeat(8)).unwrap();
    chip.write_ram(0x100, &samples()).unwrap();
    word(&mut chip, 0x400, 15);
    for (offset, value) in [
        (0x026, 8),
        (0x02c, 0x100),
        (0x02e, 0xcfff),
        (0x036, 0xe000),
        (0x022, 0x100),
        (0x020, 0x0820),
        (0x006, 8),
        (0x00c, 0x100),
        (0x000, 0x1820),
    ] {
        word(&mut chip, offset, value);
    }
    let mut output = [0; 16];
    chip.generate(&mut output).unwrap();
    let reference = [2000, -2000, 3000, -3000, 4000, -4000, 0, 0];
    for (frame, sample) in output.chunks_exact(2).zip(reference) {
        assert_eq!(frame, [sample; 2]);
    }
}

#[test]
fn envelope_matches_reference() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &samples()).unwrap();
    for (offset, value) in [
        (0x400, 15),
        (0x006, 8),
        (0x008, 0x029f),
        (0x00a, 0x3c1f),
        (0x016, 0xe000),
        (0x000, 0x1820),
    ] {
        word(&mut chip, offset, value);
    }
    let mut output = [0; 32];
    chip.generate(&mut output).unwrap();
    let reference = [
        999, -990, 1978, -1979, 2967, -2968, 3957, -3958, 989, -990, 1978, -1979, 2967, -2968,
        3957, -3958,
    ];
    for (frame, sample) in output.chunks_exact(2).zip(reference) {
        assert_eq!(frame, [sample; 2]);
    }
}

#[test]
fn pitch_and_amplitude_lfo_match_reference() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &samples()).unwrap();
    for (offset, value) in [
        (0x400, 15),
        (0x006, 8),
        (0x008, 0x001f),
        (0x00a, 0x3c00),
        (0x012, 0x7ef7),
        (0x016, 0xe000),
        (0x000, 0x1820),
    ] {
        word(&mut chip, offset, value);
    }
    let mut output = [0; 48];
    chip.generate(&mut output).unwrap();
    let reference = [
        61, -66, 129, -126, 194, -183, 235, -255, 52, -35, 84, -51, 82, -23, 32, -127, -17, 94,
        -94, 212, -250, 387, -343, 56,
    ];
    for (frame, sample) in output.chunks_exact(2).zip(reference) {
        assert_eq!(frame, [sample; 2]);
    }
}

#[test]
fn dma_loads_registers_from_shared_ram() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &samples()).unwrap();
    chip.write_ram(0x100, &[0, 8]).unwrap();
    word(&mut chip, 0x400, 15);
    word(&mut chip, 0x412, 0x100);
    word(&mut chip, 0x414, 0x006);
    chip.write(ScspSpec {
        offset: 0x417,
        value: 2,
    })
    .unwrap();
    chip.write(ScspSpec {
        offset: 0x416,
        value: 0x10,
    })
    .unwrap();
    word(&mut chip, 0x00c, 0x100);
    word(&mut chip, 0x016, 0xe000);
    word(&mut chip, 0x000, 0x1820);
    let mut output = [0; 16];
    chip.generate(&mut output).unwrap();
    for (frame, sample) in output.chunks_exact(2).zip(SAMPLES) {
        assert_eq!(frame, [i32::from(sample); 2]);
    }
}

#[test]
fn dsp_mixes_slot_input_to_effect_output() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &samples()).unwrap();
    for (offset, value) in [
        (0x400, 15),
        (0x006, 8),
        (0x00c, 0x1ff),
        (0x014, 7),
        (0x016, 0x00e0),
        (0x700, 0x4000),
        (0x802, 0xa800),
        (0x80c, 0x1000),
        (0xbf0, 0),
        (0x000, 0x1820),
    ] {
        word(&mut chip, offset, value);
    }
    let state = chip.save_state().unwrap();
    let mut output = [0; 16];
    chip.generate(&mut output).unwrap();
    for (frame, sample) in output.chunks_exact(2).zip(SAMPLES) {
        assert_eq!(frame, [i32::from(sample) / 2; 2]);
    }
    chip.restore_state(state.as_ref()).unwrap();
    let mut replay = [0; 16];
    chip.generate(&mut replay).unwrap();
    assert_eq!(output, replay);
}

#[test]
fn invalid_operations_leave_state_and_buffer_unchanged() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    setup(&mut chip);
    let state = chip.save_state().unwrap();
    assert!(chip.write_ram((1 << 20) - 1, &[1, 2]).is_err());
    let mut invalid = [99; 3];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [99; 3]);
    let mut actual = [0; 16];
    chip.generate(&mut actual).unwrap();
    chip.restore_state(state.as_ref()).unwrap();
    let mut expected = [0; 16];
    chip.generate(&mut expected).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn eight_bit_pcm_and_reserved_sample_source() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &[10, 0xf6, 20, 0xec]).unwrap();
    for (offset, value) in [
        (0x400, 15),
        (0x006, 4),
        (0x00c, 0x100),
        (0x016, 0xe000),
        (0x000, 0x1830),
    ] {
        word(&mut chip, offset, value);
    }
    let mut output = [0; 8];
    chip.generate(&mut output).unwrap();
    for (frame, sample) in output.chunks_exact(2).zip([2560, -2560, 5120, -5120]) {
        assert_eq!(frame, [sample; 2]);
    }
    word(&mut chip, 0, 0x09b0);
    let mut invalid = [99; 2];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [99; 2]);
}

#[test]
fn direct_mode_bypasses_total_level_and_respects_pan() {
    let mut chip = Scsp::new(22_579_200).unwrap();
    chip.write_ram(0, &samples()).unwrap();
    for (offset, value) in [
        (0x400, 15),
        (0x006, 8),
        (0x00c, 0x1ff),
        (0x016, 0xef00),
        (0x000, 0x1820),
    ] {
        word(&mut chip, offset, value);
    }
    let mut output = [0; 2];
    chip.generate(&mut output).unwrap();
    assert_eq!(output, [0, 1000]);
}

#[test]
fn vgm_secondary_instance_receives_ram_and_register_writes() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Scsp, instance, 22_579_200);
    }
    let mut data = 0_u32.to_le_bytes().to_vec();
    data.extend_from_slice(&samples());
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0xe0,
        size: data.len() as u32,
        data,
    });
    for (offset, value) in [
        (0x400, 15_u16),
        (0x006, 8),
        (0x00c, 0x100),
        (0x016, 0xe000),
        (0x000, 0x1820),
    ] {
        for (i, byte) in value.to_be_bytes().into_iter().enumerate() {
            builder.add_chip_write(
                Instance::Secondary,
                ScspSpec {
                    offset: offset + i as u16,
                    value: byte,
                },
            );
        }
    }
    builder.add_vgm_command(WaitSamples(1000));
    let document = builder.finalize();
    let bytes = Vec::<u8>::from(document.clone());
    let mut a = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
    let mut b = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    let mut audible = false;
    for _ in 0..5 {
        let expected = a.render().unwrap().unwrap();
        let actual = b.render().unwrap().unwrap();
        assert_eq!(expected, actual);
        audible |= actual.frames().iter().any(|frame| frame[0].abs() > 0.001);
    }
    assert!(audible);
}
