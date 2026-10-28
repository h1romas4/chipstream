#[cfg(feature = "sn76496")]
use chipstream::chip::ChipError;
use chipstream::sequencer::{VgmSequencer, VgmSequencerError};
#[cfg(feature = "okim6258")]
use soundlog::chip::Okim6258Spec;
use soundlog::chip::{Chip, Ym2608Spec, Ym2610Spec, Ym2612Spec};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};

fn render_all(player: &mut VgmSequencer) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    while let Some(block) = player.render().unwrap() {
        out.extend_from_slice(block.frames());
    }
    out
}

#[test]
fn supplied_vgm_examples_produce_stereo_audio() {
    let fixtures: &[(&str, &[u8])] = &[
        (
            "ym2151",
            include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
        ),
        (
            "ym2203_fm",
            include_bytes!("../../soundlog/assets/vgm/ym2203_fm_a4.vgm"),
        ),
        (
            "ym2203_psg",
            include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm"),
        ),
        (
            "ym2413",
            include_bytes!("../../soundlog/assets/vgm/ym2413_fm_a4.vgm"),
        ),
        (
            "ym2608_fm",
            include_bytes!("../../soundlog/assets/vgm/ym2608_fm_a4.vgm"),
        ),
        (
            "ym2608_psg",
            include_bytes!("../../soundlog/assets/vgm/ym2608_psg_a4.vgm"),
        ),
        (
            "ym2610_fm",
            include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm"),
        ),
        (
            "ym2610_psg",
            include_bytes!("../../soundlog/assets/vgm/ym2610b_psg_a4.vgm"),
        ),
        (
            "ym2612",
            include_bytes!("../../soundlog/assets/vgm/ym2612_a4.vgm"),
        ),
        (
            "ym3526",
            include_bytes!("../../soundlog/assets/vgm/ym3526_fm_a4.vgm"),
        ),
        (
            "y8950",
            include_bytes!("../../soundlog/assets/vgm/y8950_fm_a4.vgm"),
        ),
        (
            "ym3812",
            include_bytes!("../../soundlog/assets/vgm/ym3812_fm_a4.vgm"),
        ),
        (
            "ymf262",
            include_bytes!("../../soundlog/assets/vgm/ymf262_fm_a4.vgm"),
        ),
        (
            "ymf278b",
            include_bytes!("../../soundlog/assets/vgm/ymf278b_fm_a4.vgm"),
        ),
    ];
    for (name, data) in fixtures {
        let mut player = VgmSequencer::from_bytes(data, 48_000, 512)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let samples = render_all(&mut player);
        let document = VgmDocument::try_from(*data).unwrap();
        let mut document_player = VgmSequencer::new(document, 48_000, 257).unwrap();
        assert_eq!(samples, render_all(&mut document_player), "{name}");
        assert!(samples.len() > 8_000, "{name}: {}", samples.len());
        assert!(
            samples.iter().all(|f| f[0].is_finite() && f[1].is_finite()),
            "{name}"
        );
        assert!(
            samples
                .iter()
                .any(|f| f[0].abs() > 0.0001 && f[1].abs() > 0.0001),
            "{name}"
        );
    }
}

#[cfg(feature = "okim6258")]
fn add_oki_data(builder: &mut VgmBuilder, instance: Instance, count: usize) {
    for _ in 0..count {
        builder.add_chip_write(
            instance,
            Okim6258Spec {
                register: 1,
                value: 0x71,
            },
        );
    }
}

#[cfg(feature = "okim6258")]
fn oki_document() -> VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 0,
            value: 2,
        },
    );
    add_oki_data(&mut builder, Instance::Primary, 1024);
    builder.add_vgm_command(WaitSamples(2_000));
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 2,
            value: 1,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 0x0c,
            value: 2,
        },
    );
    builder.add_vgm_command(WaitSamples(3_000));
    builder.finalize()
}

#[test]
#[cfg(feature = "okim6258")]
fn chunks_preserve_oki_pan_rate_and_duration() {
    let doc = oki_document();
    let mut small = VgmSequencer::new(doc.clone(), 48_000, 31).unwrap();
    let mut large = VgmSequencer::new(doc, 48_000, 1024).unwrap();
    let a = render_all(&mut small);
    let b = render_all(&mut large);
    assert_eq!(a.len(), (5000_u32 * 48_000 + 22_050) as usize / 44_100);
    assert_eq!(a, b);
    assert!(
        a[100..2000]
            .iter()
            .any(|f| f[0].abs() > 0.001 && f[1].abs() > 0.001)
    );
    assert!(a[3000..].iter().all(|f| f[0].abs() < 0.00001));
    assert!(a[3000..].iter().any(|f| f[1].abs() > 0.001));
}

#[test]
#[cfg(feature = "sn76496")]
fn sn76489_vgm_produces_audio() {
    let data = include_bytes!("../../soundlog/assets/vgm/sn76489_a4.vgm");
    let mut player = VgmSequencer::from_bytes(data, 44_100, 1000).unwrap();
    let audio = render_all(&mut player);
    assert!(!audio.is_empty());
    assert!(
        audio
            .iter()
            .any(|f| f[0].abs() > 0.001 && f[1].abs() > 0.001)
    );
}

#[test]
#[cfg(feature = "sn76496")]
fn t6w28_header_is_rejected_for_document_and_stream() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
    builder.add_vgm_command(WaitSamples(100));
    let mut doc = builder.finalize();
    assert_eq!(doc.header.misc().t6w28_detected, Some(false));
    assert!(VgmSequencer::new(doc.clone(), 44_100, 128).is_ok());
    doc.header.sn76489_clock |= 0x8000_0000;
    assert_eq!(doc.header.misc().t6w28_detected, Some(true));
    let bytes: Vec<u8> = doc.clone().into();
    for result in [
        VgmSequencer::new(doc, 44_100, 128),
        VgmSequencer::from_vgm(bytes, 44_100, 128),
    ] {
        assert!(matches!(
            result,
            Err(VgmSequencerError::Chip(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "T6W28 PSG is not supported",
            }))
        ));
    }
}

#[test]
fn ym2610_header_variant_selects_correct_fm_channels() {
    let bytes = include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm");
    let mut doc = VgmDocument::try_from(bytes.as_slice()).unwrap();
    for is_b in [false, true] {
        if is_b {
            doc.header.ym2610b_clock |= 0x8000_0000;
        } else {
            doc.header.ym2610b_clock &= !0x8000_0000;
        }
        assert_eq!(doc.header.misc().ym2610b_detected, Some(is_b));
        let mut document_player = VgmSequencer::new(doc.clone(), 44_100, 256).unwrap();
        let bytes: Vec<u8> = doc.clone().into();
        let mut stream_player = VgmSequencer::from_vgm(bytes, 44_100, 512).unwrap();
        let samples = render_all(&mut document_player);
        assert_eq!(samples, render_all(&mut stream_player));
        let audible = samples.iter().flatten().any(|sample| sample.abs() > 0.001);
        assert_eq!(audible, is_b);
    }
}

#[test]
#[cfg(feature = "okim6258")]
fn gain_applies_to_the_correct_chip_and_buffered_audio() {
    let doc = oki_document();
    let mut player = VgmSequencer::new(doc, 48_000, 100).unwrap();
    player.render().unwrap();
    player
        .set_gain(Chip::Okim6258, Instance::Primary, 0.0)
        .unwrap();
    assert!(
        player
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .all(|f| *f == [0.0; 2])
    );
    assert!(matches!(
        player.set_gain(Chip::Okim6258, Instance::Primary, f32::NAN),
        Err(VgmSequencerError::InvalidGain)
    ));
    assert!(matches!(
        player.set_gain(Chip::Ym2151, Instance::Primary, 1.0),
        Err(VgmSequencerError::ChipNotFound { .. })
    ));
}

#[test]
#[cfg(feature = "okim6258")]
fn dual_chip_and_independent_gains_survive_serialization() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Okim6258, Instance::Secondary, 8_000_000);
    for (instance, pan) in [(Instance::Primary, 2), (Instance::Secondary, 1)] {
        builder.add_chip_write(
            instance,
            Okim6258Spec {
                register: 0,
                value: 2,
            },
        );
        add_oki_data(&mut builder, instance, 512);
        builder.add_chip_write(
            instance,
            Okim6258Spec {
                register: 2,
                value: pan,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(4000));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut player = VgmSequencer::from_bytes(&bytes, 48_000, 256).unwrap();
    player
        .set_gain(Chip::Okim6258, Instance::Primary, 0.0)
        .unwrap();
    let samples = render_all(&mut player);
    assert!(samples.iter().all(|f| f[0] == 0.0));
    assert!(samples.iter().any(|f| f[1].abs() > 0.001));
}

#[test]
fn loop_count_and_short_final_chunk() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.set_loop_index(1);
    builder.add_vgm_command(WaitSamples(300));
    let doc = builder.finalize();
    let mut player = VgmSequencer::new(doc, 44_100, 256).unwrap();
    player.set_loop_count(Some(2));
    assert_eq!(player.render().unwrap().unwrap().frames().len(), 256);
    assert_eq!(player.render().unwrap().unwrap().frames().len(), 256);
    assert_eq!(player.render().unwrap().unwrap().frames().len(), 188);
    assert!(player.render().unwrap().is_none());
}

#[test]
fn ym2610_adpcm_reads_vgm_rom_block() {
    use soundlog::vgm::command::DataBlock;
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2610b, Instance::Primary, 8_000_000);
    let mut data = Vec::new();
    data.extend_from_slice(&256_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x77; 256]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0x82,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (1, 0x3f),
        (8, 0xdf),
        (0x10, 0),
        (0x18, 0),
        (0x20, 0),
        (0x28, 0),
        (0, 1),
    ] {
        builder.add_chip_write(
            Instance::Primary,
            Ym2610Spec {
                port: 1,
                register,
                value,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(4410));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut player = VgmSequencer::from_bytes(&bytes, 44_100, 128).unwrap();
    let audio = render_all(&mut player);
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut document_player = VgmSequencer::new(document, 44_100, 257).unwrap();
    assert_eq!(audio, render_all(&mut document_player));
    assert!(
        audio
            .iter()
            .any(|frame| frame[0].abs() > 0.001 && frame[1].abs() > 0.001)
    );
}

#[test]
fn invalid_input_and_missing_end_are_reported() {
    assert!(VgmSequencer::from_bytes(b"not vgm", 48_000, 100).is_err());
    let doc = VgmDocument::default();
    assert!(matches!(
        VgmSequencer::new(doc.clone(), 0, 100),
        Err(VgmSequencerError::InvalidSampleRate)
    ));
    assert!(matches!(
        VgmSequencer::new(doc, 48_000, 0),
        Err(VgmSequencerError::InvalidChunkSize)
    ));
    let mut doc = VgmDocument::default();
    doc.commands.push(WaitSamples(1).into());
    let mut player = VgmSequencer::new(doc, 44_100, 100).unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::UnexpectedEnd)
    ));
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));
}

#[test]
fn ym2612_pcm_dac_stream_is_expanded() {
    use soundlog::vgm::command::{DataBlock, Ym2612Port0Address2AWriteAndWaitN};
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0,
        size: 600,
        data: vec![0xff; 600],
    });
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x2b,
            value: 0x80,
        },
    );
    for _ in 0..600 {
        builder.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(15));
    }
    let bytes: Vec<u8> = builder.finalize().into();
    let mut player = VgmSequencer::from_bytes(&bytes, 48_000, 256).unwrap();
    let audio = render_all(&mut player);
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut document_player = VgmSequencer::new(document, 48_000, 257).unwrap();
    assert_eq!(audio, render_all(&mut document_player));
    assert_eq!(audio.len(), (9000_u32 * 48_000 + 22_050) as usize / 44_100);
    assert!(audio.iter().any(|frame| frame[0].abs() > 0.01));
}

#[test]
fn ym2203_ssg_stereo_mask_is_applied_before_resampling() {
    use soundlog::vgm::command::{Ay8910StereoMask, VgmCommand};
    let bytes = include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm");
    let mut doc = VgmDocument::try_from(&bytes[..]).unwrap();
    // YM2203 SSG: enable all three channels on the right only.
    doc.commands.insert(
        0,
        VgmCommand::AY8910StereoMask(Ay8910StereoMask::from_mask(0b0110_1010)),
    );
    let mut player = VgmSequencer::new(doc, 48_000, 512).unwrap();
    let audio = render_all(&mut player);
    assert!(audio.iter().all(|frame| frame[0].abs() < 0.00001));
    assert!(audio.iter().any(|frame| frame[1].abs() > 0.001));
}

#[test]
#[cfg(feature = "okim6258")]
fn oki_clock_byte_changes_keep_time() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    for (register, value) in [(0, 2), (1, 0x71)] {
        builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
    }

    builder.add_vgm_command(WaitSamples(1000));
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 0x0a,
            value: 0x60,
        },
    );
    builder.add_vgm_command(WaitSamples(2000));
    let mut player = VgmSequencer::new(builder.finalize(), 48_000, 257).unwrap();
    assert_eq!(
        render_all(&mut player).len(),
        (3000_u32 * 48_000 + 22_050) as usize / 44_100
    );
}

#[test]
#[cfg(feature = "okim6258")]
fn extra_header_clock_overrides_chip_instance_clock() {
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::header::{ChipClock, ChipId};
    let mut overridden = oki_document();
    overridden.extra_header = Some(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(
            ChipId::Okim6258,
            Instance::Primary,
            4_000_000,
        )],
        ..Default::default()
    });
    let mut direct = oki_document();
    direct.header.okim6258_clock = 4_000_000;
    let mut a = VgmSequencer::new(overridden, 48_000, 256).unwrap();
    let mut b = VgmSequencer::new(direct, 48_000, 256).unwrap();
    assert_eq!(render_all(&mut a), render_all(&mut b));
}

#[test]
#[cfg(feature = "okim6258")]
fn streamed_dual_oki_matches_document_with_loop_and_extra_header() {
    use soundlog::vgm::VgmExtraHeader;
    use soundlog::vgm::header::{ChipClock, ChipId, ChipVolume};

    let mut builder = VgmBuilder::new();
    for (instance, pan) in [(Instance::Primary, 2), (Instance::Secondary, 1)] {
        builder.register_chip(Chip::Okim6258, instance, 8_000_000);
        for (register, value) in [(0, 2), (1, 0x71), (2, pan)] {
            builder.add_chip_write(instance, Okim6258Spec { register, value });
        }
    }
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![
            ChipClock::new(ChipId::Okim6258, Instance::Primary, 4_000_000),
            ChipClock::new(ChipId::Okim6258, Instance::Secondary, 6_000_000),
        ],
        chip_volumes: vec![
            ChipVolume::new(ChipId::Okim6258, Instance::Primary, 0x70),
            ChipVolume::new(ChipId::Okim6258, Instance::Secondary, 0x1c0),
        ],
        ..Default::default()
    });
    builder.add_vgm_command(WaitSamples(1000));
    builder.set_loop_index(7);
    for (instance, register, value) in [
        (Instance::Primary, 0x0a, 0x60),
        (Instance::Secondary, 0x0a, 0x70),
        (Instance::Secondary, 0x0c, 1),
    ] {
        builder.add_chip_write(instance, Okim6258Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(2000));
    let mut document = builder.finalize();
    document.header.volume_modifier = 0xe0;
    let bytes: Vec<u8> = document.into();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut streamed = VgmSequencer::from_vgm(bytes, 48_000, 73).unwrap();
    let mut parsed = VgmSequencer::new(document, 48_000, 257).unwrap();
    streamed.set_loop_count(Some(2));
    parsed.set_loop_count(Some(2));
    let actual = render_all(&mut streamed);
    assert!(actual.iter().flatten().any(|sample| sample.abs() > 0.001));
    let expected = render_all(&mut parsed);
    assert_eq!(actual.len(), (5000_u32 * 48_000 + 22_050) as usize / 44_100);
    assert_eq!(actual.len(), expected.len(), "loop duration");
    assert!(
        actual.iter().zip(&expected).all(|(a, b)| a == b),
        "first differing frame: {:?}",
        actual.iter().zip(&expected).position(|(a, b)| a != b)
    );
}

#[test]
fn malformed_commands_are_reported_when_scanned_or_rendered() {
    use soundlog::vgm::VgmHeader;

    let mut bytes: Vec<u8> = VgmBuilder::new().finalize().into();
    let header = VgmHeader::from_bytes(&bytes).unwrap();
    let start = VgmHeader::command_start(header.version, header.data_offset);
    bytes.truncate(start);
    bytes.push(0x61);
    let mut player = VgmSequencer::from_vgm(bytes, 44_100, 100).unwrap();
    assert!(player.render().is_err());
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));

    #[cfg(feature = "okim6258")]
    {
        let mut bytes: Vec<u8> = oki_document().into();
        let header = VgmHeader::from_bytes(&bytes).unwrap();
        let start = VgmHeader::command_start(header.version, header.data_offset);
        bytes.truncate(start);
        bytes.extend_from_slice(&[0x67, 0x66, 0, 5, 0, 0, 0, 1]);
        assert!(VgmSequencer::from_vgm(bytes, 44_100, 100).is_err());
    }
}

#[test]
fn supplied_ym2608_rhythm_rom_reaches_the_chip() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2608, Instance::Primary, 8_000_000);
    for (register, value) in [(0x11, 0x3f), (0x18, 0xdf), (0x10, 0x01)] {
        builder.add_chip_write(
            Instance::Primary,
            Ym2608Spec {
                port: 0,
                register,
                value,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(4410));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 256).unwrap();
    player
        .set_ym2608_rhythm_rom(Instance::Primary, &[0x77; 8192])
        .unwrap();
    let audio = render_all(&mut player);
    assert!(
        audio
            .iter()
            .any(|frame| frame[0].abs() > 0.001 && frame[1].abs() > 0.001)
    );
}

#[test]
#[cfg(feature = "okim6258")]
fn very_short_wait_includes_the_first_native_sample() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 0,
            value: 2,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 1,
            value: 0x77,
        },
    );
    builder.add_vgm_command(WaitSamples(1));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 8).unwrap();
    let output = render_all(&mut player);
    assert_eq!(output.len(), 1);
    assert!(output[0][0].abs() > 0.0001);
}

#[test]
#[cfg(feature = "okim6258")]
fn three_bit_okim6258_flag_uses_four_bit_compatibility() {
    let make_document = |three_bit| {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
        for (register, value) in [(0, 2), (1, 0x71), (1, 0x27), (1, 0xe4)] {
            builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(4410));
        let mut document = builder.finalize();
        document.header.okim6258_flags.adpcm_3bit_select = three_bit;
        document
    };

    let mut four_bit = VgmSequencer::new(make_document(false), 44_100, 256).unwrap();
    let mut three_bit = VgmSequencer::new(make_document(true), 44_100, 256).unwrap();
    let four_bit_output = render_all(&mut four_bit);
    let three_bit_output = render_all(&mut three_bit);

    assert_eq!(three_bit_output, four_bit_output);
    assert!(
        three_bit_output
            .iter()
            .any(|frame| frame[0].abs() > 0.0001 && frame[1].abs() > 0.0001)
    );
}

#[test]
#[cfg(feature = "okim6258")]
fn chips_with_different_native_rates_mix_on_the_same_timeline() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    for (register, value) in [(0, 2), (1, 0x71), (2, 2)] {
        builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
    }

    for (register, value) in [(0xb6, 0x40), (0x2a, 0xff), (0x2b, 0x80)] {
        builder.add_chip_write(
            Instance::Primary,
            Ym2612Spec {
                port: 0,
                register,
                value,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(4410));
    let doc = builder.finalize();
    let mut full = VgmSequencer::new(doc.clone(), 48_000, 73).unwrap();
    let mut oki = VgmSequencer::new(doc.clone(), 48_000, 409).unwrap();
    let mut ym = VgmSequencer::new(doc, 48_000, 255).unwrap();
    oki.set_gain(Chip::Ym2612, Instance::Primary, 0.0).unwrap();
    ym.set_gain(Chip::Okim6258, Instance::Primary, 0.0).unwrap();
    let all = render_all(&mut full);
    let left = render_all(&mut oki);
    let right = render_all(&mut ym);
    assert_eq!(all.len(), 4800);
    assert_eq!(all.len(), left.len());
    assert_eq!(all.len(), right.len());
    assert!(left.iter().any(|f| f[0].abs() > 0.001));
    assert!(right.iter().any(|f| f[1].abs() > 0.001));
    for ((a, b), c) in all.iter().zip(left).zip(right) {
        assert!((a[0] - b[0] - c[0]).abs() < 1e-6);
        assert!((a[1] - b[1] - c[1]).abs() < 1e-6);
    }
}
