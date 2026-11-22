use chipstream::sequencer::{
    Chip, Instance, MdxSequencer, MdxSequencerError, VgmSequencer, VgmSequencerError,
};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::WaitSamples;

#[test]
fn vgm_chip_features_apply_to_document_and_owned_stream() {
    let cases = [
        (Chip::C140, "c140", cfg!(feature = "c140")),
        (Chip::C352, "c352", cfg!(feature = "c352")),
        (Chip::Ga20, "ga20", cfg!(feature = "ga20")),
        (Chip::Huc6280, "huc6280", cfg!(feature = "huc6280")),
        (Chip::Pokey, "pokey", cfg!(feature = "pokey")),
        (Chip::K051649, "k051649", cfg!(feature = "k051649")),
        (Chip::K053260, "k053260", cfg!(feature = "k053260")),
        (Chip::K054539, "k054539", cfg!(feature = "k054539")),
        (Chip::X1010, "x1010", cfg!(feature = "x1010")),
        (Chip::Okim6258, "okim6258", cfg!(feature = "okim6258")),
        (Chip::Okim6295, "okim6295", cfg!(feature = "okim6295")),
        (Chip::Pwm, "pwm", cfg!(feature = "pwm")),
        (Chip::Saa1099, "saa1099", cfg!(feature = "saa1099")),
        (Chip::SegaPcm, "segapcm", cfg!(feature = "segapcm")),
        (Chip::Sn76489, "sn76496", cfg!(feature = "sn76496")),
    ];
    for (chip, feature, enabled) in cases {
        let mut builder = VgmBuilder::new();
        builder.register_chip(chip.clone(), Instance::Primary, 4_000_000);
        builder.add_vgm_command(WaitSamples(100));
        let document = builder.finalize();
        let bytes: Vec<u8> = document.clone().into();
        for result in [
            VgmSequencer::new(document, 48_000, 128, Default::default()),
            VgmSequencer::from_vgm(bytes, 48_000, 128, Default::default()),
        ] {
            if enabled {
                let mut player = result.unwrap_or_else(|error| panic!("{chip:?}: {error}"));
                player
                    .set_chip_gain(chip.clone(), Instance::Primary, 0.5)
                    .unwrap();
                assert!(player.render().unwrap().is_some(), "{chip:?}");
            } else {
                match result {
                    Err(VgmSequencerError::FeatureDisabled {
                        chip: actual,
                        feature: actual_feature,
                    }) => {
                        assert_eq!(actual, chip);
                        assert_eq!(actual_feature, feature);
                    }
                    Err(error) => panic!("{chip:?}: unexpected error {error}"),
                    Ok(_) => panic!("{chip:?}: expected disabled feature error"),
                }
            }
        }
    }
}

#[test]
fn mdx_pdx_requires_okim6258_but_fm_only_does_not() {
    let mdx = include_bytes!("../assets/mdx/mdx_fm_loop.mdx");
    let pdx = include_bytes!("../assets/mdx/mdx_fm_loop.pdx");
    let mut fm =
        MdxSequencer::from_mdx(mdx.to_vec(), None, 48_000, 128, Default::default()).unwrap();
    assert!(fm.render().unwrap().is_some());

    let result = MdxSequencer::from_mdx(
        mdx.to_vec(),
        Some(pdx.to_vec()),
        48_000,
        128,
        Default::default(),
    );
    if cfg!(feature = "okim6258") {
        let mut player = result.unwrap_or_else(|error| panic!("PDX playback: {error}"));
        player
            .set_chip_gain(Chip::Okim6258, Instance::Primary, 0.5)
            .unwrap();
        assert!(player.render().unwrap().is_some());
    } else {
        assert!(matches!(
            result,
            Err(MdxSequencerError::Playback(
                VgmSequencerError::FeatureDisabled {
                    chip: Chip::Okim6258,
                    feature: "okim6258"
                }
            ))
        ));
    }
}
