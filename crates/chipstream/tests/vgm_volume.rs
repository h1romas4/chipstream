use chipstream::audio::{AudioError, Gain, StereoBalance};
use chipstream::sequencer::{EffectError, NativeOutput, VgmSequencer, VgmSequencerError};
use soundlog::chip::Chip;
use soundlog::chip::Okim6258Spec;
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::VgmDocument;
use soundlog::vgm::command::{Ay8910StereoMask, Instance, VgmCommand, WaitSamples};
use soundlog::vgm::header::{ChipId, ChipVolume, VgmExtraHeader};

fn render(document: VgmDocument) -> Vec<[f32; 2]> {
    render_with_muted(document, None)
}

fn render_with_muted(document: VgmDocument, muted: Option<Chip>) -> Vec<[f32; 2]> {
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    if let Some(chip) = muted {
        player.set_chip_gain(chip, Instance::Primary, 0.0).unwrap();
    }
    let mut samples = Vec::new();
    while let Some(block) = player.render().unwrap() {
        samples.extend_from_slice(block.frames());
    }
    samples
}

fn assert_scaled(actual: &[[f32; 2]], original: &[[f32; 2]], left: f32, right: f32) {
    assert_eq!(actual.len(), original.len());
    assert!(original.iter().flatten().any(|sample| sample.abs() > 0.001));
    for (a, b) in actual.iter().zip(original) {
        for channel in 0..2 {
            let expected = b[channel] * [left, right][channel];
            assert!(
                (a[channel] - expected).abs() <= 0.00001 * (1.0 + expected.abs()),
                "channel {channel}: {} != {expected}",
                a[channel]
            );
        }
    }
}

fn centered_rms(audio: &[[f32; 2]]) -> f32 {
    let frames = &audio[..19_200];
    let mean = frames.iter().map(|frame| frame[0]).sum::<f32>() / frames.len() as f32;
    (frames
        .iter()
        .map(|frame| (frame[0] - mean).powi(2))
        .sum::<f32>()
        / frames.len() as f32)
        .sqrt()
}

#[test]
fn opn_ssg_and_fm_balance_matches_reference_ranges() {
    let cases: &[(&[u8], &[u8], f32, f32)] = &[
        (
            include_bytes!("../../soundlog/assets/vgm/ym2203_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm"),
            0.32,
            0.38,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2608_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2608_psg_a4.vgm"),
            0.65,
            0.77,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2610b_psg_a4.vgm"),
            0.65,
            0.77,
        ),
    ];
    for (fm, ssg, min, max) in cases {
        let mut fm = VgmDocument::try_from(*fm).unwrap();
        let mut ssg = VgmDocument::try_from(*ssg).unwrap();
        fm.header.ym2610b_clock &= !0x4000_0000;
        ssg.header.ym2610b_clock &= !0x4000_0000;
        let ratio = centered_rms(&render(ssg)) / centered_rms(&render(fm));
        assert!(
            (*min..=*max).contains(&ratio),
            "unexpected SSG/FM ratio: {ratio}"
        );
    }
}

#[test]
#[cfg(feature = "sn76496")]
fn sn76489_ym2612_balance_and_headroom() {
    let mut psg =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/sn76489_a4.vgm")[..])
            .unwrap();
    let mut fm =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2612_a4.vgm")[..])
            .unwrap();
    let psg_alone = render(psg.clone());
    let fm_alone = render(fm.clone());
    psg.header.ym2612_clock = fm.header.ym2612_clock;
    fm.header.sn76489_clock = psg.header.sn76489_clock;
    let psg_mixed = render_with_muted(psg, Some(Chip::Ym2612));
    let fm_mixed = render_with_muted(fm, Some(Chip::Sn76489));
    assert_scaled(&psg_mixed, &psg_alone, 2.0 / 3.0, 2.0 / 3.0);
    assert_scaled(&fm_mixed, &fm_alone, 1.0, 1.0);
    let ratio = centered_rms(&psg_mixed) / centered_rms(&fm_mixed);
    assert!(
        (0.32..=0.38).contains(&ratio),
        "unexpected PSG/FM ratio: {ratio}"
    );
}

#[test]
fn ym2612_headroom_preserves_ssg_stereo_mask() {
    let mut document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    document.commands.insert(
        0,
        VgmCommand::AY8910StereoMask(Ay8910StereoMask::from_mask(0b0110_1010)),
    );
    let baseline = render(document.clone());
    document.header.ym2612_clock = 7_670_454;
    let mixed = render_with_muted(document, Some(Chip::Ym2612));
    assert_scaled(&mixed, &baseline, 2.0 / 3.0, 2.0 / 3.0);
    assert!(mixed.iter().all(|frame| frame[0] == 0.0));
    assert!(mixed.iter().any(|frame| frame[1].abs() > 0.001));
}

#[test]
#[cfg(feature = "sn76496")]
fn sn76489_ym2151_balance_matches_reference_range() {
    let mut psg =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/sn76489_a4.vgm")[..])
            .unwrap();
    let mut fm =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm")[..])
            .unwrap();
    psg.header.ym2151_clock = fm.header.ym2151_clock;
    fm.header.sn76489_clock = psg.header.sn76489_clock;
    let psg_audio = render_with_muted(psg, Some(Chip::Ym2151));
    let fm_audio = render_with_muted(fm, Some(Chip::Sn76489));
    let ratio = centered_rms(&psg_audio) / centered_rms(&fm_audio);
    assert!(
        (0.32..=0.38).contains(&ratio),
        "unexpected PSG/FM ratio: {ratio}"
    );
}

fn extra(volumes: Vec<ChipVolume>) -> VgmExtraHeader {
    VgmExtraHeader {
        chip_volumes: volumes,
        ..Default::default()
    }
}

#[test]
fn output_gains_control_fm_and_ssg_independently() {
    let cases: &[(&[u8], &[u8], Chip)] = &[
        (
            include_bytes!("../../soundlog/assets/vgm/ym2203_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm"),
            Chip::Ym2203,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2608_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2608_psg_a4.vgm"),
            Chip::Ym2608,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2610b_psg_a4.vgm"),
            Chip::Ym2610b,
        ),
    ];
    for (fm, ssg, chip) in cases {
        for (bytes, is_ssg) in [(*fm, false), (*ssg, true)] {
            let document = VgmDocument::try_from(bytes).unwrap();
            let baseline = render(document.clone());
            let run = |fm, ssg, chip_gain| {
                let mut player =
                    VgmSequencer::new(document.clone(), 48_000, 256, Default::default()).unwrap();
                let gains = match chip {
                    Chip::Ym2203 => vec![fm, ssg, ssg, ssg],
                    Chip::Ym2608 | Chip::Ym2610b => vec![fm, fm, ssg],
                    _ => unreachable!(),
                };
                player
                    .set_output_gains(chip.clone(), Instance::Primary, &gains)
                    .unwrap();
                player
                    .set_chip_gain(chip.clone(), Instance::Primary, chip_gain)
                    .unwrap();
                let mut audio = Vec::new();
                while let Some(block) = player.render().unwrap() {
                    audio.extend_from_slice(block.frames());
                }
                audio
            };
            assert_scaled(
                &run(0.0, 1.0, 1.0),
                &baseline,
                if is_ssg { 1.0 } else { 0.0 },
                if is_ssg { 1.0 } else { 0.0 },
            );
            assert_scaled(
                &run(1.0, 0.0, 1.0),
                &baseline,
                if is_ssg { 0.0 } else { 1.0 },
                if is_ssg { 0.0 } else { 1.0 },
            );
            assert_scaled(&run(2.0, 2.0, 0.5), &baseline, 1.0, 1.0);
        }
    }
}

#[test]
fn output_gain_changes_and_checkpoints_keep_streaming_state() {
    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    assert_eq!(
        player.chip_instances().collect::<Vec<_>>(),
        vec![(Chip::Ym2203, Instance::Primary)]
    );
    player.render().unwrap();
    player
        .set_output_gains(Chip::Ym2203, Instance::Primary, &[1.0, 0.0, 0.0, 0.0])
        .unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let muted = (0..20)
        .map(|_| player.render().unwrap().unwrap().frames().to_vec())
        .collect::<Vec<_>>();
    assert!(
        muted
            .last()
            .unwrap()
            .iter()
            .all(|frame| frame[0].abs() < 0.00001)
    );
    player
        .set_output_gains(Chip::Ym2203, Instance::Primary, &[1.0, 1.0, 1.0, 1.0])
        .unwrap();
    let resumed = (0..20)
        .map(|_| player.render().unwrap().unwrap().frames().to_vec())
        .collect::<Vec<_>>();
    assert!(
        resumed
            .last()
            .unwrap()
            .iter()
            .any(|frame| frame[0].abs() > 0.001)
    );
    player.restore_checkpoint(&checkpoint).unwrap();
    for expected in muted {
        assert_eq!(player.render().unwrap().unwrap().frames(), expected);
    }
    assert!(
        player
            .set_output_gains(Chip::Ym2203, Instance::Primary, &[f32::NAN, 1.0, 1.0, 1.0])
            .is_err()
    );
    assert!(
        player
            .set_output_gains(Chip::Ym2151, Instance::Primary, &[1.0, 1.0])
            .is_err()
    );
    assert!(
        player
            .set_output_gains(Chip::Ym2203, Instance::Secondary, &[1.0, 1.0, 1.0, 1.0])
            .is_err()
    );
}

#[test]
fn stereo_output_gains_validate_atomically_and_control_each_side() {
    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm")[..])
            .unwrap();
    let baseline = render(document.clone());
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    assert_eq!(
        player
            .output_layout(Chip::Ym2151, Instance::Primary)
            .unwrap(),
        &[NativeOutput::Left, NativeOutput::Right]
    );
    player
        .set_output_gains(Chip::Ym2151, Instance::Primary, &[0.5, 0.0])
        .unwrap();
    assert!(matches!(
        player.set_output_gains(Chip::Ym2151, Instance::Primary, &[1.0]),
        Err(VgmSequencerError::Audio(AudioError::ChannelMismatch {
            expected: 2,
            actual: 1
        }))
    ));
    for gains in [[1.0, f32::NAN], [1.0, f32::INFINITY], [1.0, -1.0]] {
        assert!(matches!(
            player.set_output_gains(Chip::Ym2151, Instance::Primary, &gains),
            Err(VgmSequencerError::InvalidGain)
        ));
    }
    let mut output = Vec::new();
    while let Some(block) = player.render().unwrap() {
        output.extend_from_slice(block.frames());
    }
    assert_scaled(&output, &baseline, 0.5, 0.0);
}

#[test]
fn output_layout_reports_chip_specific_native_outputs() {
    let cases: &[(Chip, &[NativeOutput])] = &[
        (Chip::Ym3812, &[NativeOutput::Mono]),
        (Chip::Ym2413, &[NativeOutput::Melodic, NativeOutput::Rhythm]),
        (
            Chip::Ym2608,
            &[
                NativeOutput::FmAdpcmLeft,
                NativeOutput::FmAdpcmRight,
                NativeOutput::SsgMixed,
            ],
        ),
        (
            Chip::Ymf262,
            &[
                NativeOutput::Left,
                NativeOutput::Right,
                NativeOutput::LeftAux,
                NativeOutput::RightAux,
            ],
        ),
        (
            Chip::Ymf278b,
            &[
                NativeOutput::Unmixed(0),
                NativeOutput::Unmixed(1),
                NativeOutput::Unmixed(2),
                NativeOutput::Unmixed(3),
                NativeOutput::Left,
                NativeOutput::Right,
            ],
        ),
    ];
    for (chip, expected) in cases {
        let mut builder = VgmBuilder::new();
        builder.register_chip(chip.clone(), Instance::Primary, 4_000_000);
        builder.add_vgm_command(WaitSamples(1));
        let mut player =
            VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
        assert_eq!(
            player
                .output_layout(chip.clone(), Instance::Primary)
                .unwrap(),
            *expected
        );
        player
            .set_output_gains(chip.clone(), Instance::Primary, &vec![1.0; expected.len()])
            .unwrap();
        assert!(matches!(
            player.output_layout(chip.clone(), Instance::Secondary),
            Err(VgmSequencerError::ChipNotFound { .. })
        ));
    }
}

#[test]
fn output_gains_survive_vgm_stereo_mask_changes() {
    let mut document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    document.commands.insert(
        0,
        VgmCommand::AY8910StereoMask(Ay8910StereoMask::from_mask(0b0110_1010)),
    );
    let baseline = render(document.clone());
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    assert_eq!(
        player
            .output_layout(Chip::Ym2203, Instance::Primary)
            .unwrap(),
        &[
            NativeOutput::Fm,
            NativeOutput::SsgChannel(0),
            NativeOutput::SsgChannel(1),
            NativeOutput::SsgChannel(2)
        ]
    );
    player
        .set_output_gains(Chip::Ym2203, Instance::Primary, &[1.0, 0.5, 0.5, 0.5])
        .unwrap();
    let mut output = Vec::new();
    while let Some(block) = player.render().unwrap() {
        output.extend_from_slice(block.frames());
    }
    assert_scaled(&output, &baseline, 0.5, 0.5);
}

#[test]
fn native_effects_and_stereo_groups_process_only_selected_outputs() {
    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    let render = |mode: u8| {
        let mut player =
            VgmSequencer::new(document.clone(), 48_000, 256, Default::default()).unwrap();
        let group = (mode != 0).then(|| {
            player
                .create_output_group(Chip::Ym2203, Instance::Primary, &[1, 2, 3])
                .unwrap()
        });
        if mode == 2 {
            for output in 1..=3 {
                player
                    .add_output_effect(
                        Chip::Ym2203,
                        Instance::Primary,
                        output,
                        Gain::new(0.5).unwrap(),
                    )
                    .unwrap();
            }
        } else if mode == 3 {
            let balance = player
                .add_group_effect(&group.unwrap(), StereoBalance::new(0.0).unwrap())
                .unwrap();
            player
                .effect_mut(&balance)
                .unwrap()
                .set_balance(1.0)
                .unwrap();
        }
        let mut frames = Vec::new();
        for _ in 0..8 {
            frames.extend_from_slice(player.render().unwrap().unwrap().frames());
        }
        frames
    };
    let baseline = render(0);
    assert_eq!(render(1), baseline);
    assert_scaled(&render(2), &baseline, 0.5, 0.5);
    assert_scaled(&render(3), &baseline, 0.0, 1.0);
}

#[test]
fn pan_preserves_stereo_masks_and_validates_effect_configuration() {
    let mut document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    document.commands.insert(
        0,
        VgmCommand::AY8910StereoMask(Ay8910StereoMask::from_mask(0b0101_0101)),
    );
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    for output in 1..=3 {
        player
            .set_output_pan(Chip::Ym2203, Instance::Primary, output, Some(1.0))
            .unwrap();
    }
    for _ in 0..4 {
        assert!(
            player
                .render()
                .unwrap()
                .unwrap()
                .frames()
                .iter()
                .flatten()
                .all(|v| v.abs() < 0.00001)
        );
    }
    assert!(matches!(
        player.set_output_pan(Chip::Ym2203, Instance::Primary, 1, Some(f32::NAN)),
        Err(VgmSequencerError::Effect(EffectError::InvalidPan))
    ));
    assert!(matches!(
        player.set_output_pan(Chip::Ym2203, Instance::Primary, 4, Some(0.0)),
        Err(VgmSequencerError::Effect(
            EffectError::OutputOutOfRange { .. }
        ))
    ));
    assert!(matches!(
        player.create_output_group(Chip::Ym2203, Instance::Primary, &[]),
        Err(VgmSequencerError::Effect(EffectError::EmptyGroup))
    ));
    assert!(matches!(
        player.create_output_group(Chip::Ym2203, Instance::Primary, &[1, 1]),
        Err(VgmSequencerError::Effect(
            EffectError::OutputAlreadyGrouped { .. }
        ))
    ));
    let group = player
        .create_output_group(Chip::Ym2203, Instance::Primary, &[1, 2])
        .unwrap();
    assert!(matches!(
        player.create_output_group(Chip::Ym2203, Instance::Primary, &[2, 3]),
        Err(VgmSequencerError::Effect(
            EffectError::OutputAlreadyGrouped { .. }
        ))
    ));
    player.remove_output_group(&group).unwrap();
    player
        .create_output_group(Chip::Ym2203, Instance::Primary, &[2, 3])
        .unwrap();
}

#[test]
fn pan_can_route_a_native_output_to_the_other_side() {
    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    for output in 1..=3 {
        player
            .set_output_pan(Chip::Ym2203, Instance::Primary, output, Some(1.0))
            .unwrap();
    }
    let mut frames = Vec::new();
    for _ in 0..8 {
        frames.extend_from_slice(player.render().unwrap().unwrap().frames());
    }
    assert!(frames.iter().all(|frame| frame[0] == 0.0));
    assert!(frames.iter().any(|frame| frame[1].abs() > 0.001));
}

#[test]
fn effect_order_can_be_changed_and_removed_during_playback() {
    use chipstream::audio::NativeEffect;
    use chipstream::chip::SampleRate;

    #[derive(Clone)]
    struct Offset;
    impl NativeEffect for Offset {
        fn process_native(&mut self, samples: &mut [f32], _: SampleRate) -> Result<(), AudioError> {
            for sample in samples {
                *sample += 1000.0;
            }
            Ok(())
        }
    }

    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    player
        .set_output_gains(Chip::Ym2203, Instance::Primary, &[0.0, 1.0, 0.0, 0.0])
        .unwrap();
    let offset = player
        .add_output_effect(Chip::Ym2203, Instance::Primary, 1, Offset)
        .unwrap();
    let gain = player
        .add_output_effect(Chip::Ym2203, Instance::Primary, 1, Gain::new(0.0).unwrap())
        .unwrap();
    assert!(matches!(
        player.move_effect(&gain, 2),
        Err(VgmSequencerError::Effect(
            EffectError::PositionOutOfRange { .. }
        ))
    ));
    for _ in 0..4 {
        assert!(
            player
                .render()
                .unwrap()
                .unwrap()
                .frames()
                .iter()
                .flatten()
                .all(|v| *v == 0.0)
        );
    }
    player.move_effect(&gain, 0).unwrap();
    let mut audible = false;
    for _ in 0..8 {
        audible |= player
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .flatten()
            .any(|v| v.abs() > 0.001);
    }
    assert!(audible);
    player.remove_effect(&offset).unwrap();
    assert!(player.effect_mut(&offset).is_err());
    for _ in 0..8 {
        player.render().unwrap().unwrap();
    }
    assert!(
        player
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .flatten()
            .all(|v| *v == 0.0)
    );
}

#[test]
fn nonfinite_effect_output_fails_rendering_explicitly() {
    use chipstream::audio::NativeEffect;
    use chipstream::chip::SampleRate;

    #[derive(Clone)]
    struct Invalid;
    impl NativeEffect for Invalid {
        fn process_native(&mut self, samples: &mut [f32], _: SampleRate) -> Result<(), AudioError> {
            samples.fill(f32::NAN);
            Ok(())
        }
    }

    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    player
        .add_output_effect(Chip::Ym2203, Instance::Primary, 1, Invalid)
        .unwrap();
    let checkpoint = player.checkpoint().unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::Audio(AudioError::NonFiniteSample))
    ));
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));
    assert!(matches!(
        player.restore_checkpoint(&checkpoint),
        Err(VgmSequencerError::StreamFailed)
    ));
    assert!(matches!(
        player.seek_to_frame(&checkpoint, 0),
        Err(VgmSequencerError::StreamFailed)
    ));
}

fn oki_builder(dual: bool) -> VgmBuilder {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    if dual {
        builder.register_chip(Chip::Okim6258, Instance::Secondary, 8_000_000);
    }
    for (instance, pan) in [(Instance::Primary, 2), (Instance::Secondary, 1)] {
        if instance == Instance::Secondary && !dual {
            continue;
        }
        builder.add_chip_write(
            instance,
            Okim6258Spec {
                register: 0,
                value: 2,
            },
        );
        for _ in 0..512 {
            builder.add_chip_write(
                instance,
                Okim6258Spec {
                    register: 1,
                    value: 0x71,
                },
            );
        }
        builder.add_chip_write(
            instance,
            Okim6258Spec {
                register: 2,
                value: pan,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(4000));
    builder
}

#[test]
#[cfg(feature = "okim6258")]
fn global_modifier_covers_signed_and_special_values() {
    let original = oki_builder(false).finalize();
    let baseline = render(original.clone());
    for (raw, factor) in [
        (0x00, 1.0),
        (0x20, 2.0),
        (0x80, 16.0),
        (0xc0, 64.0),
        (0xc1, 0.25),
        (0xc2, 2.0_f32.powf(-62.0 / 32.0)),
        (0xe0, 0.5),
        (0xff, 2.0_f32.powf(-1.0 / 32.0)),
    ] {
        let mut document = original.clone();
        document.header.volume_modifier = raw;
        assert_scaled(&render(document), &baseline, factor, factor);
    }
}

#[test]
#[cfg(feature = "okim6258")]
fn extra_volume_and_user_gain_compose_after_serialization() {
    let baseline = render(oki_builder(false).finalize());
    let mut builder = oki_builder(false);
    builder.set_extra_header(extra(vec![ChipVolume::new_relative(
        ChipId::Okim6258,
        Instance::Primary,
        0x80,
    )]));
    let mut document = builder.finalize();
    document.header.volume_modifier = 0x20;
    let bytes: Vec<u8> = document.into();
    let mut player =
        VgmSequencer::from_vgm(bytes.as_slice(), 48_000, 256, Default::default()).unwrap();
    let first = player.render().unwrap().unwrap().frames().to_vec();
    assert_scaled(&first, &baseline[..first.len()], 1.0, 1.0);
    player
        .set_chip_gain(Chip::Okim6258, Instance::Primary, 0.25)
        .unwrap();
    let next = player.render().unwrap().unwrap().frames().to_vec();
    assert_scaled(
        &next,
        &baseline[first.len()..first.len() + next.len()],
        0.25,
        0.25,
    );
}

#[test]
fn absolute_and_relative_chip_volumes_follow_their_own_baselines() {
    let cases: &[(&[u8], ChipId, u16)] = &[
        #[cfg(feature = "sn76496")]
        (
            include_bytes!("../../soundlog/assets/vgm/sn76489_a4.vgm"),
            ChipId::Sn76489,
            0x80,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2413_fm_a4.vgm"),
            ChipId::Ym2413,
            0x200,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2203_fm_a4.vgm"),
            ChipId::Ym2203,
            0x100,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2608_fm_a4.vgm"),
            ChipId::Ym2608,
            0x80,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm"),
            ChipId::Ym2610,
            0x40, // This fixture sets the dual-chip flag, halving the per-instance baseline.
        ),
    ];
    for (bytes, chip, default) in cases {
        let document = VgmDocument::try_from(*bytes).unwrap();
        let baseline = render(document.clone());
        for (entry, factor) in [
            (ChipVolume::new(*chip, Instance::Primary, *default / 2), 0.5),
            (
                ChipVolume::new_relative(*chip, Instance::Primary, 0x200),
                2.0,
            ),
            (ChipVolume::new_relative(*chip, Instance::Primary, 0), 0.0),
        ] {
            let mut modified = document.clone();
            modified.extra_header = Some(extra(vec![entry]));
            assert_scaled(&render(modified), &baseline, factor, factor);
        }
    }
    #[cfg(feature = "okim6258")]
    {
        let document = oki_builder(false).finalize();
        let baseline = render(document.clone());
        let mut modified = document;
        modified.extra_header = Some(extra(vec![ChipVolume::new(
            ChipId::Okim6258,
            Instance::Primary,
            0xe0,
        )]));
        assert_scaled(&render(modified), &baseline, 0.5, 0.5);
    }
}

#[test]
#[cfg(feature = "okim6258")]
fn dual_chip_absolute_volumes_are_independent() {
    let baseline = render(oki_builder(true).finalize());
    assert!(baseline.iter().any(|frame| frame[0].abs() > 0.001));
    assert!(baseline.iter().any(|frame| frame[1].abs() > 0.001));
    let mut builder = oki_builder(true);
    builder.set_extra_header(extra(vec![
        ChipVolume::new(ChipId::Okim6258, Instance::Primary, 0x70),
        ChipVolume::new(ChipId::Okim6258, Instance::Secondary, 0x1c0),
    ]));
    let bytes: Vec<u8> = builder.finalize().into();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    assert_scaled(&render(document), &baseline, 0.5, 2.0);
}

#[test]
fn paired_ssg_and_fm_volumes_are_independent_and_survive_stereo_mask() {
    let cases: &[(&[u8], &[u8], ChipId, u16)] = &[
        (
            include_bytes!("../../soundlog/assets/vgm/ym2203_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm"),
            ChipId::Ym2203,
            0x80,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2608_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2608_psg_a4.vgm"),
            ChipId::Ym2608,
            0x80,
        ),
        (
            include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm"),
            include_bytes!("../../soundlog/assets/vgm/ym2610b_psg_a4.vgm"),
            ChipId::Ym2610,
            0x80,
        ),
    ];
    for (fm, ssg, chip, paired_default) in cases {
        for (bytes, is_ssg) in [(*fm, false), (*ssg, true)] {
            let mut document = VgmDocument::try_from(bytes).unwrap();
            if *chip == ChipId::Ym2203 {
                document.commands.insert(
                    0,
                    VgmCommand::AY8910StereoMask(Ay8910StereoMask::from_mask(0b0110_1010)),
                );
            }
            let baseline = render(document.clone());
            let mut first = document.clone();
            first.extra_header = Some(extra(vec![
                ChipVolume::new_relative(*chip, Instance::Primary, 0),
                ChipVolume::new_paired(*chip, Instance::Primary, *paired_default / 2),
            ]));
            let factor = if is_ssg { 0.5 } else { 0.0 };
            assert_scaled(&render(first), &baseline, factor, factor);
            document.extra_header = Some(extra(vec![
                ChipVolume::new_relative(*chip, Instance::Primary, 0x200),
                ChipVolume::new_paired_relative(*chip, Instance::Primary, 0),
            ]));
            let factor = if is_ssg { 0.0 } else { 2.0 };
            assert_scaled(&render(document), &baseline, factor, factor);
        }
    }
}

#[test]
fn modifier_in_extended_header_space_is_read_for_vgm_150_and_151() {
    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2413_fm_a4.vgm")[..])
            .unwrap();
    let baseline = render(document.clone());
    let mut bytes: Vec<u8> = document.into();
    assert!(bytes.len() > 0x7c);
    bytes[0x7c] = 0x20;
    for version in [0x150_u32, 0x151] {
        bytes[8..12].copy_from_slice(&version.to_le_bytes());
        let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
        assert_eq!(parsed.header.volume_modifier, 0x20);
        assert_scaled(&render(parsed), &baseline, 2.0, 2.0);
    }
}

#[test]
fn command_data_overlapping_modifier_is_not_read_as_header() {
    let mut builder = oki_builder(false);
    for _ in 0..50 {
        builder.add_vgm_command(WaitSamples(123));
    }
    let mut document = builder.finalize();
    document.header.version = 0x150;
    document.header.data_offset = 0x0c;
    let bytes: Vec<u8> = document.into();
    assert_ne!(bytes[0x7c], 0);
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    assert_eq!(parsed.header.volume_modifier, 0);
}
