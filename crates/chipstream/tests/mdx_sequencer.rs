use std::error::Error;

use chipstream::sequencer::{
    Chip, Instance, MdxSequencer, MdxSequencerError, MdxSequencerOptions, VgmEventFilter,
    VgmSequencer, VgmSequencerError,
};
#[cfg(feature = "okim6258")]
use soundlog::mdx::command::MdxAdpcmOrNoiseFrequency;
use soundlog::mdx::command::{MdxNote, MdxPan, MdxRest, MdxVoiceOrPcmBank};
use soundlog::mdx::convert::{AdpcmMode, MdxConvertError, to_vgm_document};
use soundlog::mdx::document::MdxBuilder;
use soundlog::mdx::package::MdxPackage;
#[cfg(feature = "okim6258")]
use soundlog::mdx::pdx::PdxBuilder;
use soundlog::mdx::tone::{MdxOperator, MdxTone};

const MDX: &[u8] = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
#[cfg(feature = "okim6258")]
const PDX: &[u8] = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");

fn render_mdx(player: &mut MdxSequencer) -> Vec<[f32; 2]> {
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    assert!(player.render().unwrap().is_none());
    frames
}

fn render_vgm(player: &mut VgmSequencer) -> Vec<[f32; 2]> {
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    frames
}

fn fm_package() -> MdxPackage {
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 7,
            fl: 0,
            op: 0x78,
            operators: [MdxOperator {
                ar: 31,
                rr: 15,
                ml: 1,
                ..Default::default()
            }; 4],
        })
        .add_mdx_command(0, MdxVoiceOrPcmBank { value: 0 })
        .add_mdx_command(0, MdxPan::Center)
        .add_mdx_command(
            0,
            MdxNote {
                note: 0x98,
                length: 32,
            },
        )
        .add_mdx_command(0, MdxRest { ticks: 8 });
    MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: None,
    }
}

#[cfg(feature = "okim6258")]
fn pcm_package(extended: bool) -> MdxPackage {
    let mut builder = MdxBuilder::new();
    for mode in 0..=4 {
        builder
            .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: mode })
            .add_mdx_command(8, MdxPan::Right)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 8,
                },
            )
            .add_mdx_command(8, MdxPan::Left)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 8,
                },
            );
    }
    if extended {
        builder.add_mdx_command(
            15,
            MdxNote {
                note: 0x81,
                length: 24,
            },
        );
    }
    let mut pdx = PdxBuilder::new();
    pdx.set_sample(0, 0, vec![0x71; 300]).unwrap();
    pdx.set_sample(0, 1, vec![0x37; 300]).unwrap();
    MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: Some(pdx.finalize()),
    }
}

#[test]
fn fm_and_pcm_match_eager_conversion_across_chunks() {
    for (package, mode) in [
        (fm_package(), AdpcmMode::Through),
        #[cfg(feature = "okim6258")]
        (pcm_package(false), AdpcmMode::Through),
        #[cfg(feature = "okim6258")]
        (pcm_package(false), AdpcmMode::Resample),
        #[cfg(feature = "okim6258")]
        (pcm_package(true), AdpcmMode::Through),
        #[cfg(feature = "okim6258")]
        (pcm_package(true), AdpcmMode::Lpf),
    ] {
        let options = MdxSequencerOptions {
            loop_count: Some(1),
            adpcm_mode: mode,
            ..Default::default()
        };
        let document = to_vgm_document(&package, &options).unwrap();
        let mut reference = VgmSequencer::new(document, 48_000, 512).unwrap();
        let expected = render_vgm(&mut reference);
        assert!(
            expected
                .iter()
                .flatten()
                .any(|sample| sample.abs() > 0.0001)
        );
        for chunk in [31, 1024] {
            let mut mdx =
                MdxSequencer::with_options(package.clone(), 48_000, chunk, options).unwrap();
            assert_eq!(
                render_mdx(&mut mdx),
                expected,
                "chunk {chunk}, mode {mode:?}"
            );
        }
    }
}

#[test]
#[cfg(feature = "okim6258")]
fn finite_and_infinite_loops_are_driven_incrementally() {
    let package = MdxPackage::parse(MDX, Some(PDX)).unwrap();
    let options = |loop_count| MdxSequencerOptions {
        loop_count,
        max_ticks: None,
        ..Default::default()
    };
    let once = render_mdx(
        &mut MdxSequencer::with_options(package.clone(), 44_100, 256, options(Some(1))).unwrap(),
    );
    let twice = render_mdx(
        &mut MdxSequencer::with_options(package.clone(), 44_100, 257, options(Some(2))).unwrap(),
    );
    let mut expected = VgmSequencer::new(
        to_vgm_document(&package, &options(Some(2))).unwrap(),
        44_100,
        512,
    )
    .unwrap();
    assert_eq!(twice, render_vgm(&mut expected));
    assert!(twice.len() > once.len());
    let mut infinite = MdxSequencer::with_options(package, 44_100, 256, options(None)).unwrap();
    let frames: Vec<_> = (0..(twice.len() / 256 + 2))
        .flat_map(|_| infinite.render().unwrap().unwrap().frames().to_vec())
        .collect();
    assert!(frames.len() > twice.len());
}

#[test]
fn events_and_gain_are_available_for_mdx() {
    let mut player = MdxSequencer::new(fm_package(), 48_000, 128).unwrap();
    player
        .configure_events(VgmEventFilter {
            chip: Some(Chip::Ym2151),
            register: Some(0x08),
            register_writes: true,
            state_changes: true,
            ..Default::default()
        })
        .unwrap();
    assert!(matches!(
        player.render(),
        Err(MdxSequencerError::Playback(
            VgmSequencerError::EventCallbackRequired
        ))
    ));
    let mut events = Vec::new();
    let mut frames = Vec::new();
    while let Some(block) = player
        .render_with_events(|event| events.push(event.clone()))
        .unwrap()
    {
        frames.extend_from_slice(block.frames());
    }
    assert!(
        events
            .iter()
            .any(|event| event.value & 0x78 != 0 && !event.state_events.is_empty())
    );
    assert!(
        events
            .iter()
            .all(|event| event.chip == Chip::Ym2151 && event.register == 0x08)
    );
    let mut muted = MdxSequencer::new(fm_package(), 48_000, 128).unwrap();
    muted
        .set_gain(Chip::Ym2151, Instance::Primary, 0.0)
        .unwrap();
    let muted = render_mdx(&mut muted);
    assert_eq!(muted.len(), frames.len());
    assert!(muted.iter().flatten().all(|sample| *sample == 0.0));
    assert!(frames.iter().flatten().any(|sample| sample.abs() > 0.0001));
}

#[test]
fn invalid_inputs_limits_and_playback_errors_are_reported() {
    assert!(MdxSequencer::from_mdx(b"bad mdx".to_vec(), None, 48_000, 128).is_err());
    assert!(MdxSequencer::from_mdx(MDX.to_vec(), Some(b"bad pdx".to_vec()), 48_000, 128).is_err());
    assert!(MdxSequencer::new(fm_package(), 0, 128).is_err());
    assert!(MdxSequencer::new(fm_package(), 48_000, 0).is_err());
    let options = MdxSequencerOptions {
        ym2151_clock: 0,
        ..Default::default()
    };
    assert!(MdxSequencer::with_options(fm_package(), 48_000, 128, options).is_err());
    let options = MdxSequencerOptions {
        loop_count: Some(1),
        max_ticks: Some(1),
        ..Default::default()
    };
    let mut player = MdxSequencer::with_options(fm_package(), 48_000, 128, options).unwrap();
    let error = loop {
        match player.render() {
            Err(error) => break error,
            Ok(Some(_)) => {}
            Ok(None) => panic!("expected tick limit error"),
        }
    };
    let mut source: &(dyn Error + 'static) = &error;
    loop {
        if let Some(error) = source.downcast_ref::<MdxConvertError>() {
            assert_eq!(*error, MdxConvertError::TickLimitExceeded { max_ticks: 1 });
            break;
        }
        source = source.source().expect("typed MDX error in source chain");
    }
    assert!(player.render().is_err());
}

#[test]
#[cfg(feature = "okim6258")]
fn compressed_mdx_and_pdx_play_like_uncompressed_inputs() {
    let make_mdx = |compressed| {
        let mut builder = MdxBuilder::new();
        builder
            .set_lz_compressed(compressed)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 16,
                },
            )
            .add_mdx_command(8, MdxRest { ticks: 8 });
        builder.finalize().unwrap().to_bytes().unwrap()
    };
    let make_pdx = |compressed| {
        let mut builder = PdxBuilder::new();
        builder.set_sample(0, 0, vec![0x71; 300]).unwrap();
        builder.set_lz_compressed(compressed);
        builder.finalize().to_bytes()
    };
    let plain_mdx = make_mdx(false);
    let plain_pdx = make_pdx(false);
    let compressed_mdx = make_mdx(true);
    let compressed_pdx = make_pdx(true);
    let expected =
        render_mdx(&mut MdxSequencer::from_mdx(plain_mdx, Some(plain_pdx), 48_000, 128).unwrap());
    assert!(
        expected
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 0.0001)
    );
    let actual = render_mdx(
        &mut MdxSequencer::from_mdx(compressed_mdx, Some(compressed_pdx), 48_000, 257).unwrap(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn owned_mdx_without_pdx_matches_parsed_package() {
    let package = fm_package();
    let mdx = package.to_mdx_bytes().unwrap();
    let actual = render_mdx(&mut MdxSequencer::from_mdx(mdx, None, 48_000, 128).unwrap());
    let expected = render_mdx(&mut MdxSequencer::new(package, 48_000, 128).unwrap());
    assert_eq!(actual, expected);
    assert!(actual.iter().flatten().any(|sample| sample.abs() > 0.0001));
}

#[test]
fn owned_mdx_normalizes_mxdrv16y_track_stubs() {
    let mut mdx = vec![0_u8; 286];
    mdx[0] = b'x';
    mdx[1..4].copy_from_slice(&[0x0d, 0x0a, 0x1a]);
    mdx[7..9].copy_from_slice(&20_u16.to_be_bytes());
    for track in 1..9 {
        let entry = 7 + track * 2;
        mdx[entry..entry + 2].copy_from_slice(&0xffff_u16.to_be_bytes());
    }
    mdx[25..28].copy_from_slice(&[0xf1, 0x01, 0x00]);
    mdx[284..286].copy_from_slice(&[0xf1, 0x00]);

    let normalized = soundlog::mdx::compat::normalize_mdx_for_playback(&mdx).unwrap();
    let package = MdxPackage::parse(&normalized, None).unwrap();
    let actual = render_mdx(&mut MdxSequencer::from_mdx(mdx, None, 48_000, 128).unwrap());
    let expected = render_mdx(&mut MdxSequencer::new(package, 48_000, 128).unwrap());
    assert_eq!(actual, expected);
}
