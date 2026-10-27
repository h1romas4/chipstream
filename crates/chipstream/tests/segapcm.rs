use chipstream::chip::{ChipError, SegaPcm, SoundChip};
use chipstream::sequencer::{VgmEventFilter, VgmSequencer};
use soundlog::chip::{Chip, SegaPcmSpec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};

fn write(chip: &mut SegaPcm, address: u16, value: u8) {
    chip.write(SegaPcmSpec {
        offset: address,
        value,
    })
    .unwrap();
}

#[test]
fn sample_playback_banking_loop_and_reset() {
    let mut chip = SegaPcm::with_banking(4_000_000, 12, 0x70).unwrap();
    assert_eq!(chip.sample_rate(), 31_250);
    chip.write_rom(0x10000, &[0x80, 0xff, 0x00]).unwrap();
    for (address, value) in [
        (2, 0x7f),
        (3, 0x40),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0xff),
        (0x84, 0),
        (0x85, 0),
        (0x86, 0x10),
    ] {
        write(&mut chip, address, value);
    }
    let mut samples = [0; 8];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [0, 0, 0, 0, 16129, 8128, -16256, -8192]);
    chip.reset();
    assert_eq!(chip.read_register(0x86), 0xff);
    let mut silence = [1; 8];
    chip.generate(&mut silence).unwrap();
    assert_eq!(silence, [0; 8]);
    write(&mut chip, 2, 1);
    write(&mut chip, 0x84, 1);
    write(&mut chip, 0x85, 0);
    write(&mut chip, 0x86, 0x10);
    chip.generate(&mut silence[..2]).unwrap();
    assert_eq!(silence[0], 127);
}

#[test]
fn looping_and_one_shot_end_are_applied_before_fetch() {
    let mut chip = SegaPcm::new(4_000_000).unwrap();
    chip.write_rom(0, &[0xff]).unwrap();
    for (address, value) in [
        (2, 1),
        (3, 1),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 1),
        (0x84, 0),
        (0x85, 1),
        (0x86, 0),
    ] {
        write(&mut chip, address, value);
    }
    let mut frame = [0; 2];
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [127, 127]);
    write(&mut chip, 0x85, 1);
    write(&mut chip, 0x86, 2);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [0, 0]);
    assert_eq!(chip.read_register(0x86), 3);
}

#[test]
fn invalid_operations_leave_state_unchanged() {
    assert!(matches!(
        SegaPcm::new(127),
        Err(ChipError::InvalidClock { .. })
    ));
    assert!(SegaPcm::with_banking(4_000_000, 22, 0).is_err());
    let mut chip = SegaPcm::new(4_000_000).unwrap();
    let mut invalid = [42; 3];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [42; 3]);
    assert!(matches!(
        chip.write_rom(usize::MAX, &[1]),
        Err(ChipError::RomTooLarge)
    ));
    assert_eq!(chip.read_register(0x86), 0xff);
}

#[test]
fn vgm_rom_and_second_chip_preserve_wide_addresses() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::SegaPcm, Instance::Primary, 4_000_000);
    builder.register_chip(Chip::SegaPcm, Instance::Secondary, 4_000_000);
    let mut data = Vec::new();
    data.extend_from_slice(&256_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0xff; 256]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x80,
        size: data.len() as u32,
        data,
    });
    for (offset, value) in [
        (2, 100),
        (3, 50),
        (6, 0),
        (7, 128),
        (0x84, 0),
        (0x85, 0),
        (0x86, 0),
    ] {
        builder.add_chip_write(Instance::Secondary, SegaPcmSpec { offset, value });
    }
    builder.add_vgm_command(WaitSamples(1000));
    let mut doc = builder.finalize();
    doc.header.spcm_interface = 12 | (0x70 << 16);
    let bytes: Vec<u8> = doc.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 128).unwrap();
    let mut document = VgmSequencer::new(doc, 44_100, 57).unwrap();
    let mut parsed_player = VgmSequencer::new(parsed, 44_100, 128).unwrap();
    let render = |player: &mut VgmSequencer| {
        let mut result = Vec::new();
        while let Some(block) = player.render().unwrap() {
            result.extend_from_slice(block.frames());
        }
        result
    };
    let actual = render(&mut stream);
    assert_eq!(actual, render(&mut document));
    assert_eq!(actual, render(&mut parsed_player));
    assert!(actual.iter().any(|frame| frame[0] > 0.1 && frame[1] > 0.05));
}

#[test]
fn vgm_events_report_both_address_bytes() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::SegaPcm, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        SegaPcmSpec {
            offset: 0x184,
            value: 0x91,
        },
    );
    builder.add_vgm_command(WaitSamples(100));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 128).unwrap();
    player
        .configure_events(VgmEventFilter {
            chip: Some(Chip::SegaPcm),
            port: Some(1),
            register: Some(0x84),
            register_writes: true,
            ..Default::default()
        })
        .unwrap();
    let mut events = Vec::new();
    while player
        .render_with_events(|event| events.push(event.clone()))
        .unwrap()
        .is_some()
    {}
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].port, events[0].register, events[0].value),
        (1, 0x84, 0x91)
    );
}
