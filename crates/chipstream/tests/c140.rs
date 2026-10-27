use chipstream::chip::{C140, C140Type, ChipError, SoundChip};
use chipstream::sequencer::{VgmEventFilter, VgmSequencer};
use soundlog::chip::{C140Spec, Chip};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::header::C140ChipType;
use soundlog::vgm::{VgmBuilder, VgmDocument};

fn write(chip: &mut C140, address: u16, value: u8) {
    chip.write(C140Spec {
        register: address,
        value,
    })
    .unwrap();
}

fn start_voice(chip: &mut C140, bank: u8, mode: u8) {
    for (address, value) in [
        (0, 24),
        (1, 24),
        (2, 0x80),
        (3, 0),
        (4, bank),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 4),
        (10, 0),
        (11, 0),
        (5, mode),
    ] {
        write(chip, address, value);
    }
}

#[test]
fn c140_board_banks_interpolation_and_reset() {
    for (kind, bank) in [(C140Type::System2, 0x20), (C140Type::System21, 0x10)] {
        let mut chip = C140::new(12_288_000, kind).unwrap();
        assert_eq!(chip.sample_rate_ratio().denominator().get(), 576);
        chip.write_rom(0x80000, &[0, 0x20, 0x40, 0x60]).unwrap();
        start_voice(&mut chip, bank, 0x80);
        let mut output = [0; 8];
        chip.generate(&mut output).unwrap();
        assert_eq!(output, [0, 0, 128, 128, 256, 256, 0, 0]);
        chip.reset();
        output.fill(1);
        chip.generate(&mut output).unwrap();
        assert_eq!(output, [0; 8]);
    }
}

#[test]
fn asic219_bank_mirror_and_left_inversion() {
    let mut chip = C140::new(25_056_500, C140Type::Asic219).unwrap();
    chip.write_rom(0x20000, &[0, 0x20, 0, 0x40, 0, 0x60, 0, 0x7f])
        .unwrap();
    write(&mut chip, 0x1ff, 1);
    assert_eq!(chip.read_register(0x1f7), 1);
    start_voice(&mut chip, 0, 0x88);
    let mut output = [0; 6];
    chip.generate(&mut output).unwrap();
    assert_eq!(output, [0, 0, -128, 128, -256, 256]);
}

#[test]
fn compressed_samples_and_asic_noise_survive_chunking() {
    let mut chip = C140::new(12_288_000, C140Type::System2).unwrap();
    chip.write_rom(0, &[0, 7, 7, 7]).unwrap();
    start_voice(&mut chip, 0, 0x88);
    let mut samples = [0; 4];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [0, 0, 256, 256]);

    let mut whole = C140::new(25_056_500, C140Type::Asic219).unwrap();
    let mut chunks = C140::new(25_056_500, C140Type::Asic219).unwrap();
    for chip in [&mut whole, &mut chunks] {
        start_voice(chip, 0, 0xc4);
    }
    let mut a = [0; 1024];
    let mut b = [0; 1024];
    whole.generate(&mut a).unwrap();
    for part in b.chunks_mut(32) {
        chunks.generate(part).unwrap();
    }
    assert_eq!(a, b);
    assert!(a.iter().any(|sample| *sample != 0));
}

#[test]
fn bad_input_does_not_change_output_or_state() {
    assert!(matches!(
        C140::new(575, C140Type::System2),
        Err(ChipError::InvalidClock { .. })
    ));
    let mut chip = C140::new(12_288_000, C140Type::System2).unwrap();
    let mut bad = [123; 3];
    assert!(chip.generate(&mut bad).is_err());
    assert_eq!(bad, [123; 3]);
    assert!(chip.write_rom(usize::MAX, &[1]).is_err());
    assert_eq!(chip.read_register(5), 0);
}

#[test]
fn vgm_chip_rom_and_wide_events() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::C140, Instance::Primary, 12_288_000);
    let mut data = Vec::new();
    data.extend_from_slice(&256_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x60; 256]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x8d,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (0x1f7, 1),
        (0, 255),
        (1, 255),
        (2, 0x40),
        (3, 0),
        (4, 0),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 128),
        (5, 0x80),
    ] {
        builder.add_chip_write(Instance::Primary, C140Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(2000));
    let mut doc = builder.finalize();
    doc.header.c140_chip_type = C140ChipType::C140System2;
    let bytes: Vec<u8> = doc.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 31).unwrap();
    let mut document = VgmSequencer::new(parsed, 44_100, 256).unwrap();
    for player in [&mut stream, &mut document] {
        player
            .configure_events(VgmEventFilter {
                chip: Some(Chip::C140),
                port: Some(1),
                register: Some(0xf7),
                register_writes: true,
                ..Default::default()
            })
            .unwrap();
    }
    let render = |player: &mut VgmSequencer| {
        let mut output = Vec::new();
        let mut events = Vec::new();
        while let Some(block) = player
            .render_with_events(|event| events.push(event.value))
            .unwrap()
        {
            output.extend_from_slice(block.frames());
        }
        (output, events)
    };
    let (a, events) = render(&mut stream);
    let (b, other_events) = render(&mut document);
    assert_eq!(a, b);
    assert_eq!(events, vec![1]);
    assert_eq!(events, other_events);
    assert!(
        a.iter()
            .any(|frame| frame[0].abs() > 0.001 && frame[1].abs() > 0.001)
    );
}
