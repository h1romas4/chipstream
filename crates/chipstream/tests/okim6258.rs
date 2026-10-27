use chipstream::chip::{ChipError, ClockDivider, Okim6258, OutputBits, SoundChip};
use soundlog::chip::{Chip, Okim6258Spec};
use soundlog::mdx::pcm::decode_adpcm;
use soundlog::vgm::command::{Instance, VgmCommand};
use soundlog::{VgmBuilder, VgmStream, VgmStreamResult};

fn write(chip: &mut Okim6258, register: u8, value: u8) {
    chip.write(Okim6258Spec { register, value }).unwrap();
}

#[test]
fn data_latch_and_control_transitions_match_reference() {
    let mut chip = Okim6258::new(8_000_000).unwrap();
    assert_eq!(chip.chip_type(), &Chip::Okim6258);
    assert_eq!(chip.clock(), 8_000_000);
    assert_eq!(chip.channels(), 1);
    assert_eq!(chip.sample_rate(), 7812);
    assert_eq!(chip.status(), 0x80);
    write(&mut chip, 1, 0x21);
    write(&mut chip, 0, 0x02);
    let mut first = [0; 1];
    chip.generate(&mut first).unwrap();
    assert_eq!(first, [64]);
    chip.generate(&mut first).unwrap();
    assert_eq!(first, [224]);
    write(&mut chip, 0, 0x02);
    write(&mut chip, 1, 0x43);
    let mut next = [0; 2];
    chip.generate(&mut next).unwrap();
    assert_eq!(next, [448, 736]);
    write(&mut chip, 0, 0x07);
    assert_eq!(chip.status(), 0x80);
    let mut stopped = [1; 4];
    chip.generate(&mut stopped[1..3]).unwrap();
    assert_eq!(stopped, [1, 0, 0, 1]);
    write(&mut chip, 0, 0x02);
    chip.generate(&mut next).unwrap();
    assert_eq!(next, [192, 480]);
    write(&mut chip, 0, 0x04);
    assert_eq!(chip.status(), 0x80);
}

#[test]
fn byte_replacement_and_repeating_nibbles_match_reference() {
    let mut chip = Okim6258::new(8_000_000).unwrap();
    write(&mut chip, 0, 0x02);
    write(&mut chip, 1, 0x21);
    write(&mut chip, 1, 0x43);
    let mut samples = [0; 4];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [192, 480, 720, 1024]);
}

#[test]
fn precision_and_decoder_match_soundlog_and_clamp() {
    let bytes = [0x21, 0x43, 0x77, 0xff, 0x08, 0xe4, 0x12, 0x6a];
    let mut chip = Okim6258::new(8_000_000).unwrap();
    chip.set_output_bits(OutputBits::Bits12);
    write(&mut chip, 0, 0x02);
    let mut actual = Vec::new();
    for byte in bytes {
        write(&mut chip, 1, byte);
        let mut pair = [0; 2];
        chip.generate(&mut pair).unwrap();
        actual.extend_from_slice(&pair);
    }
    let expected: Vec<i32> = decode_adpcm(&bytes)
        .into_iter()
        .map(|sample| i32::from(sample) * 16)
        .collect();
    assert_eq!(actual, expected);

    for (bits, high, low) in [
        (OutputBits::Bits10, 8176, -8192),
        (OutputBits::Bits12, 32752, -32768),
    ] {
        let mut chip = Okim6258::new(8_000_000).unwrap();
        chip.set_output_bits(bits);
        assert_eq!(chip.output_bits(), bits);
        write(&mut chip, 0, 2);
        write(&mut chip, 1, 0x77);
        let mut samples = [0; 128];
        chip.generate(&mut samples).unwrap();
        assert_eq!(samples[127], high);
        write(&mut chip, 1, 0xff);
        chip.generate(&mut samples).unwrap();
        assert_eq!(samples[127], low);
    }
}

#[test]
fn configuration_and_vgm_registers_preserve_decoder_state() {
    let mut chip = Okim6258::new(8_000_000).unwrap();
    write(&mut chip, 0, 2);
    write(&mut chip, 1, 0x21);
    let mut sample = [0];
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample, [64]);
    write(&mut chip, 2, 0xfd);
    assert_eq!(chip.pan_mask(), 1);
    write(&mut chip, 12, 1);
    assert_eq!(chip.divider(), ClockDivider::Div768);
    assert_eq!(chip.sample_rate(), 10416);
    write(&mut chip, 12, 3);
    assert_eq!(chip.divider(), ClockDivider::Div512);
    assert_eq!(chip.sample_rate(), 15625);
    for (register, value) in (8..=11).zip(4_000_000_u32.to_le_bytes()) {
        write(&mut chip, register, value);
    }
    assert_eq!(chip.clock(), 4_000_000);
    assert_eq!(chip.sample_rate(), 7812);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample, [224]);

    chip.set_output_bits(OutputBits::Bits12);
    chip.reset();
    assert_eq!(chip.status(), 0x80);
    assert_eq!(chip.clock(), 4_000_000);
    assert_eq!(chip.divider(), ClockDivider::Div512);
    assert_eq!(chip.output_bits(), OutputBits::Bits12);
    assert_eq!(chip.pan_mask(), 1);
    write(&mut chip, 0, 2);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample, [64]);
}

#[test]
fn invalid_configuration_and_registers_are_rejected_without_state_changes() {
    assert!(matches!(
        Okim6258::new(1023),
        Err(ChipError::InvalidClock {
            clock: 1023,
            minimum: 1024
        })
    ));
    let mut chip = Okim6258::new(1024).unwrap();
    assert_eq!(
        chip.set_clock(1023),
        Err(ChipError::InvalidClock {
            clock: 1023,
            minimum: 1024,
        })
    );
    assert_eq!(
        chip.write(Okim6258Spec {
            register: 9,
            value: 0
        }),
        Err(ChipError::InvalidClock {
            clock: 0,
            minimum: 1024,
        })
    );
    assert_eq!(chip.clock(), 1024);
    chip.set_divider(ClockDivider::Div512).unwrap();
    chip.set_clock(512).unwrap();
    assert_eq!(
        chip.set_divider(ClockDivider::Div768),
        Err(ChipError::InvalidClock {
            clock: 512,
            minimum: 768,
        })
    );
    assert_eq!(chip.divider(), ClockDivider::Div512);
    assert_eq!(
        chip.write(Okim6258Spec {
            register: 3,
            value: 0
        }),
        Err(ChipError::InvalidRegister {
            chip: Chip::Okim6258,
            register: 3,
        })
    );
    assert_eq!(chip.status(), 0x80);
}

#[test]
fn soundlog_commands_drive_the_chip() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    for (register, value) in [(0x0c, 2), (0x00, 2), (0x01, 0x21)] {
        builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
    }
    let mut chip = Okim6258::new(8_000_000).unwrap();
    let stream = VgmStream::from_document(builder.finalize());
    for result in stream {
        match result.unwrap() {
            VgmStreamResult::Command(VgmCommand::Okim6258Write(_, spec)) => {
                chip.write(spec).unwrap();
            }
            VgmStreamResult::EndOfStream => break,
            _ => {}
        }
    }
    assert_eq!(chip.sample_rate(), 15625);
    let mut samples = [0; 2];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [64, 224]);
}
