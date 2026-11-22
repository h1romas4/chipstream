#![cfg(feature = "rf5c")]

use chipstream::chip::{ChipError, Rf5c, SoundChip};
use chipstream::sequencer::{VgmSequencer, VgmSequencerError};
use soundlog::chip::{Chip, Rf5c68U8Spec, Rf5c68U16Spec, Rf5c164U8Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, PcmRamWrite, StreamChipType, WaitSamples};

fn write(chip: &mut Rf5c, offset: u8, value: u8) {
    match chip.chip_type() {
        Chip::Rf5c68 => chip.write(Rf5c68U8Spec { offset, value }).unwrap(),
        Chip::Rf5c164 => chip.write(Rf5c164U8Spec { offset, value }).unwrap(),
        _ => unreachable!(),
    }
}

fn setup(chip: &mut Rf5c) {
    for (offset, value) in [
        (7, 0xc0),
        (0, 0xff),
        (1, 0xff),
        (2, 0),
        (3, 8),
        (4, 0),
        (5, 0),
        (6, 0),
        (8, 0xfe),
    ] {
        write(chip, offset, value);
    }
}

#[test]
fn playback_loops_and_quantizes_each_model() {
    for (kind, expected) in [
        (Chip::Rf5c68, [896, 1856, -960, 896]),
        (Chip::Rf5c164, [956, 1912, -956, 956]),
    ] {
        let mut chip = Rf5c::new(kind, 12_500_000).unwrap();
        chip.write_ram(0, &[0x88, 0x90, 0x08, 0xff]).unwrap();
        setup(&mut chip);
        let mut audio = [0; 8];
        chip.generate(&mut audio).unwrap();
        for (frame, sample) in audio.chunks_exact(2).zip(expected) {
            assert_eq!(frame, [sample, sample]);
        }
    }
}

#[test]
fn ram_windows_and_snapshots_preserve_model() {
    let mut chip = Rf5c::new(Chip::Rf5c68, 12_500_000).unwrap();
    write(&mut chip, 7, 0x81);
    chip.write(Rf5c68U16Spec {
        offset: 0,
        value: 0x9f,
    })
    .unwrap();
    write(&mut chip, 7, 0xc0);
    write(&mut chip, 6, 0x10);
    write(&mut chip, 0, 255);
    write(&mut chip, 1, 255);
    write(&mut chip, 2, 0);
    write(&mut chip, 3, 8);
    write(&mut chip, 8, 0xfe);
    let state = chip.save_state().unwrap();
    let mut expected = [0; 2];
    chip.generate(&mut expected).unwrap();
    chip.write_ram(0x1000, &[0]).unwrap();
    chip.restore_state(state.as_ref()).unwrap();
    let mut actual = [0; 2];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
    assert!(actual[0] > 0);
    let mut other = Rf5c::new(Chip::Rf5c164, 12_500_000).unwrap();
    assert!(matches!(
        other.restore_state(state.as_ref()),
        Err(ChipError::InvalidStateSnapshot(_))
    ));
    assert!(matches!(
        chip.write_ram(65535, &[1, 2]),
        Err(ChipError::RamOutOfRange { .. })
    ));
}

fn document(kind: Chip, block_type: u8, use_copy: bool) -> soundlog::vgm::VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(kind.clone(), Instance::Primary, 12_500_000);
    match kind {
        Chip::Rf5c68 => builder.add_chip_write(
            Instance::Primary,
            Rf5c68U8Spec {
                offset: 7,
                value: 0x81,
            },
        ),
        Chip::Rf5c164 => builder.add_chip_write(
            Instance::Primary,
            Rf5c164U8Spec {
                offset: 7,
                value: 0x81,
            },
        ),
        _ => unreachable!(),
    };
    let data = [0xc0, 0xc0, 0x40, 0xff];
    if use_copy {
        builder.add_vgm_command(DataBlock {
            marker: 0x66,
            chip_instance: 0,
            data_type: block_type - 0xbf,
            size: data.len() as u32,
            data: data.to_vec(),
        });
        builder.add_vgm_command(PcmRamWrite {
            marker: 0x66,
            chip_type: if kind == Chip::Rf5c68 {
                StreamChipType::Rf5c68Pcm
            } else {
                StreamChipType::Rf5c164Pcm
            },
            instance: Instance::Primary,
            read_offset: 0,
            write_offset: 0,
            size: data.len() as u32,
        });
    } else {
        builder.add_vgm_command(DataBlock {
            marker: 0x66,
            chip_instance: 0,
            data_type: block_type,
            size: 6,
            data: vec![0, 0, data[0], data[1], data[2], data[3]],
        });
    }
    for (offset, value) in [
        (7, 0xc0),
        (0, 0xff),
        (1, 0xff),
        (2, 0),
        (3, 8),
        (4, 0),
        (5, 0x10),
        (6, 0x10),
        (8, 0xfe),
    ] {
        match kind {
            Chip::Rf5c68 => {
                builder.add_chip_write(Instance::Primary, Rf5c68U8Spec { offset, value })
            }
            Chip::Rf5c164 => {
                builder.add_chip_write(Instance::Primary, Rf5c164U8Spec { offset, value })
            }
            _ => unreachable!(),
        };
    }
    builder.add_vgm_command(WaitSamples(1000));
    builder.finalize()
}

#[test]
fn vgm_direct_and_pcm_bank_uploads_render_from_document_and_bytes() {
    for (kind, block) in [(Chip::Rf5c68, 0xc0), (Chip::Rf5c164, 0xc1)] {
        let mut outputs = Vec::new();
        for copy in [false, true] {
            let doc = document(kind.clone(), block, copy);
            let bytes: Vec<u8> = doc.clone().into();
            let mut from_doc = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
            let mut from_bytes =
                VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
            let result = from_doc.render().unwrap().unwrap();
            assert!(
                result.frames().iter().any(|frame| frame[0].abs() > 0.01),
                "{kind:?}, copy={copy}: {:?}",
                &result.frames()[..8]
            );
            assert_eq!(result, from_bytes.render().unwrap().unwrap());
            outputs.push(result);
        }
        assert_eq!(outputs[0], outputs[1]);
    }
}

#[test]
fn invalid_pcm_bank_range_fails_explicitly() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Rf5c68, Instance::Primary, 12_500_000);
    builder.add_vgm_command(PcmRamWrite {
        marker: 0x66,
        chip_type: StreamChipType::Rf5c68Pcm,
        instance: Instance::Primary,
        read_offset: 0,
        write_offset: 0,
        size: 1,
    });
    builder.add_vgm_command(WaitSamples(20));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::InvalidRomBlock)
    ));
}

#[test]
fn secondary_pcm_copy_is_isolated_and_survives_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Rf5c68, Instance::Primary, 12_500_000);
    builder.register_chip(Chip::Rf5c68, Instance::Secondary, 12_500_000);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 1,
        size: 4,
        data: vec![0xc0, 0x40, 0xc0, 0xff],
    });
    builder.add_vgm_command(PcmRamWrite {
        marker: 0x66,
        chip_type: StreamChipType::Rf5c68Pcm,
        instance: Instance::Secondary,
        read_offset: 0,
        write_offset: 0,
        size: 4,
    });
    for (offset, value) in [
        (7, 0xc0),
        (0, 255),
        (1, 255),
        (2, 0),
        (3, 8),
        (6, 0),
        (8, 0xfe),
    ] {
        builder.add_chip_write(Instance::Secondary, Rf5c68U8Spec { offset, value });
    }

    builder.add_vgm_command(WaitSamples(2000));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut player = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    player
        .set_chip_gain(Chip::Rf5c68, Instance::Secondary, 0.0)
        .unwrap();
    assert!(
        player
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .all(|frame| *frame == [0.0, 0.0])
    );
    player
        .set_chip_gain(Chip::Rf5c68, Instance::Secondary, 1.0)
        .unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let block = player.render().unwrap().unwrap();
    assert!(block.frames().iter().any(|frame| frame[0].abs() > 0.01));
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(block, player.render().unwrap().unwrap());
}

#[test]
fn checkpoint_restores_bank_for_later_ram_upload() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Rf5c68, Instance::Primary, 12_500_000);
    builder.add_chip_write(
        Instance::Primary,
        Rf5c68U8Spec {
            offset: 7,
            value: 0x82,
        },
    );
    builder.add_vgm_command(WaitSamples(1000));
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0xc0,
        size: 6,
        data: vec![0, 0, 0xc0, 0x40, 0xc0, 0xff],
    });
    for (offset, value) in [
        (7, 0xc0),
        (0, 255),
        (1, 255),
        (2, 0),
        (3, 8),
        (4, 0),
        (5, 0x20),
        (6, 0x20),
        (8, 0xfe),
    ] {
        builder.add_chip_write(Instance::Primary, Rf5c68U8Spec { offset, value });
    }
    builder.add_chip_write(
        Instance::Primary,
        Rf5c68U8Spec {
            offset: 7,
            value: 0x83,
        },
    );
    builder.add_vgm_command(WaitSamples(2000));
    let mut player =
        VgmSequencer::new(builder.finalize(), 44_100, 128, Default::default()).unwrap();
    player.render().unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let expected: Vec<_> = (0..12).map(|_| player.render().unwrap().unwrap()).collect();
    assert!(
        expected
            .iter()
            .any(|block| block.frames().iter().any(|frame| frame[0].abs() > 0.01))
    );
    player.restore_checkpoint(&checkpoint).unwrap();
    let actual: Vec<_> = (0..12).map(|_| player.render().unwrap().unwrap()).collect();
    assert_eq!(actual, expected);
}
