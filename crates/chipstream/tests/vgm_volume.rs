use chipstream::sequencer::VgmSequencer;
use soundlog::chip::Chip;
use soundlog::chip::Okim6258Spec;
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::VgmDocument;
use soundlog::vgm::command::{Ay8910StereoMask, Instance, VgmCommand, WaitSamples};
use soundlog::vgm::header::{ChipId, ChipVolume, VgmExtraHeader};

fn render(document: VgmDocument) -> Vec<[f32; 2]> {
    let mut player = VgmSequencer::new(document, 48_000, 256).unwrap();
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

fn extra(volumes: Vec<ChipVolume>) -> VgmExtraHeader {
    VgmExtraHeader {
        chip_volumes: volumes,
        ..Default::default()
    }
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
    let mut player = VgmSequencer::from_bytes(&bytes, 48_000, 256).unwrap();
    let first = player.render().unwrap().unwrap().frames().to_vec();
    assert_scaled(&first, &baseline[..first.len()], 1.0, 1.0);
    player
        .set_gain(Chip::Okim6258, Instance::Primary, 0.25)
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
