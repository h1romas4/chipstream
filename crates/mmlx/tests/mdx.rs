use mmlx::mdx::{self, MmlCommand};

#[test]
fn parses_readable_mdx_fixture() {
    let source = include_str!("../assets/mdx/readable.mml");
    let document = mdx::parse(source).unwrap();

    assert_eq!(
        document.title.as_deref(),
        Some("Readable MXDRV parser fixture")
    );
    assert_eq!(document.pcm_file.as_deref(), Some("fixture.pdx"));
    assert_eq!(document.voices.len(), 1);
    assert_eq!(document.voices[0].values.len(), 47);
    assert_eq!(document.tracks.len(), 7);
    assert_eq!(document.tracks[0].channel, 'A');
    assert_eq!(document.tracks[5].channel, 'F');
    assert_eq!(document.tracks[6].channel, 'P');
    assert!(matches!(
        document.tracks[6].commands.as_slice(),
        [MmlCommand::PcmFrequency(4)]
    ));
}

#[test]
fn parses_compact_mdx_fixture() {
    let source = include_str!("../assets/mdx/compact.mml");
    let document = mdx::parse(source).unwrap();

    assert_eq!(
        document.title.as_deref(),
        Some("Compact MXDRV parser fixture")
    );
    assert_eq!(document.voices.len(), 1);
    assert_eq!(document.tracks.len(), 3);
    assert_eq!(document.tracks[0].channel, 'A');
    assert_eq!(document.tracks[0].commands.len(), 10);
    assert_eq!(document.tracks[2].channel, 'P');
    assert!(matches!(
        document.tracks[2].commands.as_slice(),
        [MmlCommand::PcmFrequency(4)]
    ));
}

#[test]
fn compiles_readable_fixture_to_serializable_mdx() {
    let source = include_str!("../assets/mdx/readable.mml");
    let compiled = mdx::compile(&mdx::parse(source).unwrap()).unwrap();

    assert_eq!(compiled.header.title, "Readable MXDRV parser fixture");
    assert_eq!(compiled.header.pdx_name.as_deref(), Some("fixture.pdx"));
    assert_eq!(compiled.tracks.len(), 9);
    assert!(!compiled.tracks[0].is_empty());
    assert_eq!(compiled.tone_bank.tones.len(), 1);

    let bytes = compiled.to_bytes();
    let reparsed = soundlog::mdx::document::MdxDocument::parse(&bytes).unwrap();

    assert_eq!(reparsed.header.title, "Readable MXDRV parser fixture");
    assert_eq!(reparsed.tracks.len(), 9);
    assert_eq!(reparsed.tone_bank.tones.len(), 1);
}