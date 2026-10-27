use chipstream::chip::{ChipError, ChipRegisterWrite, Sn76496, Sn76496Config, SoundChip};
use chipstream::sequencer::{VgmEventFilter, VgmSequencer};
use soundlog::chip::Chip;
use soundlog::chip::{GameGearPsgSpec, PsgSpec};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};

#[test]
fn tone_stereo_and_reset() {
    let config = Sn76496Config {
        stereo: true,
        ..Default::default()
    };
    let mut chip = Sn76496::with_config(3_579_545, config).unwrap();
    assert_eq!(chip.sample_rate_ratio().numerator(), 3_579_545);
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 16);
    chip.write(PsgSpec { value: 0x82 }).unwrap();
    chip.write(PsgSpec { value: 0x00 }).unwrap();
    chip.write(PsgSpec { value: 0x90 }).unwrap();
    chip.write(GameGearPsgSpec { value: 0x10 }).unwrap();
    let mut samples = [0; 16];
    chip.generate(&mut samples).unwrap();
    assert_eq!(
        samples,
        [
            -8191, 0, -8191, 0, 0, 0, 0, 0, -8191, 0, -8191, 0, 0, 0, 0, 0
        ]
    );
    chip.reset();
    let mut silence = [1; 16];
    chip.generate(&mut silence).unwrap();
    assert_eq!(silence, [0; 16]);
}

#[test]
fn invalid_input_does_not_change_state_or_buffer() {
    assert!(matches!(
        Sn76496::new(15),
        Err(ChipError::InvalidClock { .. })
    ));
    let bad_config = Sn76496Config {
        feedback: 1,
        ..Default::default()
    };
    assert!(matches!(
        Sn76496::with_config(1_000_000, bad_config),
        Err(ChipError::InvalidConfiguration {
            chip: Chip::Sn76489,
            ..
        })
    ));
    let mut first = Sn76496::new(3_579_545).unwrap();
    let mut second = Sn76496::new(3_579_545).unwrap();
    for chip in [&mut first, &mut second] {
        chip.write(PsgSpec { value: 0x90 }).unwrap();
    }
    let mut invalid = [42; 3];
    assert!(first.generate(&mut invalid).is_err());
    assert_eq!(invalid, [42; 3]);
    assert!(matches!(
        first.apply_register_write(ChipRegisterWrite::Okim6258(soundlog::chip::Okim6258Spec {
            register: 0,
            value: 0
        })),
        Err(ChipError::ChipMismatch { .. })
    ));
    let mut a = [0; 32];
    let mut b = [0; 32];
    first.generate(&mut a).unwrap();
    second.generate(&mut b).unwrap();
    assert_eq!(a, b);
}

#[test]
fn noise_and_chunking_are_deterministic() {
    let mut a = Sn76496::new(3_579_545).unwrap();
    let mut b = Sn76496::new(3_579_545).unwrap();
    for chip in [&mut a, &mut b] {
        chip.write(PsgSpec { value: 0xe4 }).unwrap();
        chip.write(PsgSpec { value: 0xf0 }).unwrap();
    }
    let mut whole = vec![0; 4096];
    let mut chunks = vec![0; 4096];
    a.generate(&mut whole).unwrap();
    for chunk in chunks.chunks_mut(32) {
        b.generate(chunk).unwrap();
    }
    assert_eq!(whole, chunks);
    assert!(whole.contains(&-8191));
    assert!(whole.contains(&0));
}

#[test]
fn vgm_dual_psg_and_game_gear_stereo() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Sn76489, Instance::Secondary, 3_579_545);
    for (instance, mask) in [(Instance::Primary, 0x10), (Instance::Secondary, 0x01)] {
        builder.add_chip_write(instance, GameGearPsgSpec { value: mask });
        for value in [0x82, 0x05, 0x90] {
            builder.add_chip_write(instance, PsgSpec { value });
        }
    }
    builder.add_vgm_command(WaitSamples(500));
    let mut doc = builder.finalize();
    doc.header.sn76489_flags.gamegear_stereo = true;
    doc.header.sn76489_flags.clock_divider_8 = true;
    let bytes: Vec<u8> = doc.into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 31).unwrap();
    let mut document = VgmSequencer::new(parsed, 44_100, 256).unwrap();
    stream
        .set_gain(Chip::Sn76489, Instance::Secondary, 0.0)
        .unwrap();
    document
        .set_gain(Chip::Sn76489, Instance::Secondary, 0.0)
        .unwrap();
    for player in [&mut stream, &mut document] {
        player
            .configure_events(VgmEventFilter {
                chip: Some(Chip::Sn76489),
                register: Some(1),
                register_writes: true,
                ..Default::default()
            })
            .unwrap();
    }
    let render = |player: &mut VgmSequencer| {
        let mut audio = Vec::new();
        let mut events = Vec::new();
        while let Some(block) = player
            .render_with_events(|event| events.push((event.instance, event.value)))
            .unwrap()
        {
            audio.extend_from_slice(block.frames());
        }
        (audio, events)
    };
    let (a, events) = render(&mut stream);
    let (b, other_events) = render(&mut document);
    assert_eq!(a, b);
    assert_eq!(
        events,
        vec![(Instance::Primary, 0x10), (Instance::Secondary, 0x01)]
    );
    assert_eq!(events, other_events);
    assert!(a.iter().any(|frame| frame[0].abs() > 0.001));
    assert!(a.iter().all(|frame| frame[1] == 0.0));
}
