use std::fs;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_csplay"))
}

#[test]
fn cli_renders_mml_like_compiled_mdx() {
    let dir = tempfile::tempdir().unwrap();
    let mml = dir.path().join("song.MML");
    let mdx = dir.path().join("song.mdx");
    let source = r#"
@1 = {
  28,4,0,5,1,37,2,1,7,0,0,
  22,9,1,2,1,47,2,12,0,0,0,
  29,4,3,6,1,37,1,3,3,0,0,
  15,7,0,5,10,0,2,1,0,0,1,
  2,7,15
}
A t120 @1 l8 c L def
"#;
    fs::write(&mml, source).unwrap();
    let document = mmlx::mdx::compile(&mmlx::mdx::parse(source).unwrap()).unwrap();
    fs::write(&mdx, document.to_bytes().unwrap()).unwrap();
    let mut lengths = Vec::new();
    for (case, options) in [
        ("once", vec!["--loop-count", "1", "--fadeout-samples", "0"]),
        (
            "fade",
            vec!["--loop-count", "2", "--fadeout-samples", "4410"],
        ),
    ] {
        let mut wavs = Vec::new();
        for (index, input) in [&mml, &mdx].into_iter().enumerate() {
            let output = dir.path().join(format!("{case}-{index}.wav"));
            let result = cli()
                .arg(input)
                .arg(&output)
                .args(["--sample-rate", "48000"])
                .args(&options)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let mut wav = hound::WavReader::open(output).unwrap();
            assert_eq!(wav.spec().sample_rate, 48_000);
            wavs.push(wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap());
        }
        assert_eq!(wavs[0], wavs[1]);
        assert!(wavs[0].iter().any(|sample| *sample != 0));
        lengths.push(wavs[0].len());
    }
    assert!(lengths[1] > lengths[0]);
}

#[test]
fn cli_retriggers_adjacent_notes_in_mml_mdx_and_vgm() {
    use chipstream::sequencer::{VgmEventFilter, VgmSequencer};
    use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
    use soundlog::mdx::package::MdxPackage;

    let dir = tempfile::tempdir().unwrap();
    let source = r#"
@1 = {
  31,0,12,15,0,20,0,1,0,0,0,
  31,0,12,15,0,20,0,1,0,0,0,
  31,0,12,15,0,20,0,1,0,0,0,
  31,0,12,15,0,20,0,1,0,0,0,
  7,0,15
}
A t120 @1 q8 l4 cccccccc
"#;
    let mml = dir.path().join("song.mml");
    let mdx = dir.path().join("song.mdx");
    let vgm = dir.path().join("song.vgm");
    fs::write(&mml, source).unwrap();
    let document = mmlx::mdx::compile(&mmlx::mdx::parse(source).unwrap()).unwrap();
    let mdx_bytes = document.to_bytes().unwrap();
    fs::write(&mdx, &mdx_bytes).unwrap();
    let package = MdxPackage::parse(&mdx_bytes, None).unwrap();
    let options = MdxToVgmOptions {
        loop_count: Some(1),
        ..Default::default()
    };
    let vgm_document = to_vgm_document(&package, &options).unwrap();
    let vgm_bytes: Vec<u8> = vgm_document.into();
    fs::write(&vgm, &vgm_bytes).unwrap();

    let mut sequencer = VgmSequencer::from_vgm(vgm_bytes, 44_100, 256).unwrap();
    sequencer
        .configure_events(VgmEventFilter {
            register: Some(0x08),
            register_writes: true,
            state_changes: false,
            ..Default::default()
        })
        .unwrap();
    let mut key_ons = Vec::new();
    let mut key_offs = Vec::new();
    let mut offset = 0;
    while let Some(block) = sequencer
        .render_with_events(|event| {
            let frame = offset + event.frame_offset as usize;
            if event.value & 0x78 != 0 {
                key_ons.push(frame);
            } else {
                key_offs.push(frame);
            }
        })
        .unwrap()
    {
        offset += block.frames().len();
    }
    assert_eq!(key_ons.len(), 8);
    assert!(key_ons[1..].iter().all(|frame| {
        key_offs
            .iter()
            .any(|off| *off <= *frame && frame - off <= 4)
    }));

    let mut rendered = Vec::new();
    for (index, input) in [&mml, &mdx, &vgm].into_iter().enumerate() {
        let output = dir.path().join(format!("{index}.wav"));
        let result = cli().arg(input).arg(&output).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut wav = hound::WavReader::open(output).unwrap();
        rendered.push(wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap());
    }
    assert_eq!(rendered[0], rendered[1]);
    assert_eq!(rendered[0], rendered[2]);
    let peaks: Vec<_> = key_ons
        .iter()
        .map(|&frame| {
            rendered[0][frame * 2..(frame + 2_000) * 2]
                .iter()
                .map(|sample| i32::from(*sample).abs())
                .max()
                .unwrap()
        })
        .collect();
    assert!(peaks[0] > 100, "first note must be audible: {peaks:?}");
    assert!(
        peaks.iter().all(|peak| *peak >= peaks[0] / 2),
        "every note must retrigger: {peaks:?}"
    );
}

#[test]
fn cli_mml_resolves_pdx_and_protects_inputs() {
    use soundlog::mdx::command::MdxCommand;
    use soundlog::mdx::pdx::PdxBuilder;

    let dir = tempfile::tempdir().unwrap();
    let mml = dir.path().join("song.mml");
    let sidecar = dir.path().join("SaMpLe.pDx");
    let override_pdx = dir.path().join("other.pdx");
    let output = dir.path().join("out.wav");
    let source = "#pcmfile \"sample.pdx\"\nP o4 c4\n";
    fs::write(&mml, source).unwrap();
    let document = mmlx::mdx::compile(&mmlx::mdx::parse(source).unwrap()).unwrap();
    let note = document.tracks[8]
        .iter()
        .find_map(|command| match command {
            MdxCommand::Note(note) => Some(usize::from(note.note - 0x80)),
            _ => None,
        })
        .unwrap();
    let mut builder = PdxBuilder::new();
    builder.set_sample(0, note, vec![0x71; 300]).unwrap();
    let pdx = builder.finalize().to_bytes();
    fs::write(&sidecar, &pdx).unwrap();
    fs::write(&override_pdx, &pdx).unwrap();
    let mut rendered = Vec::new();
    for override_path in [None, Some(&override_pdx)] {
        let mut command = cli();
        command.arg(&mml).arg(&output);
        if let Some(path) = override_path {
            command.arg("--pdx").arg(path);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        rendered.push(fs::read(&output).unwrap());
    }
    assert_eq!(rendered[0], rendered[1]);
    assert!(
        hound::WavReader::open(&output)
            .unwrap()
            .samples::<i16>()
            .any(|sample| sample.unwrap() != 0)
    );
    for input in [&mml, &sidecar] {
        let before = fs::read(input).unwrap();
        let result = cli().arg(&mml).arg(input).output().unwrap();
        assert!(!result.status.success());
        assert_eq!(fs::read(input).unwrap(), before);
    }
    let result = cli()
        .arg(&mml)
        .arg("--pdx")
        .arg(&override_pdx)
        .arg(&override_pdx)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&override_pdx).unwrap(), pdx);
    fs::remove_file(&sidecar).unwrap();
    let result = cli().arg(&mml).arg(&output).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("not found"));
    assert_eq!(fs::read(&output).unwrap(), rendered[0]);
}

#[test]
fn cli_mml_reports_errors_without_overwriting_wav() {
    let dir = tempfile::tempdir().unwrap();
    let mml = dir.path().join("invalid.mml");
    let output = dir.path().join("out.wav");
    fs::write(&output, b"existing").unwrap();
    for (source, expected) in [
        (&b"\xff"[..], "not UTF-8"),
        (&b"A ["[..], "failed to parse MML"),
        (&b"A a>>>>>>>>>a"[..], "failed to compile MML"),
    ] {
        fs::write(&mml, source).unwrap();
        let result = cli().arg(&mml).arg(&output).output().unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if expected != "not UTF-8" {
            assert!(
                String::from_utf8_lossy(&result.stderr).contains(&format!("{}:1:", mml.display()))
            );
        }
        assert_eq!(fs::read(&output).unwrap(), b"existing");
    }
    fs::write(&mml, "A r4").unwrap();
    for option in ["--compat-vgm-pwm", "--loop-modifier", "--loop-base"] {
        let result = cli()
            .arg(&mml)
            .arg(&output)
            .args([
                option,
                if option == "--compat-vgm-pwm" {
                    "true"
                } else {
                    "1"
                },
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert_eq!(fs::read(&output).unwrap(), b"existing");
    }
}

#[test]
fn cli_pwm_compatibility_defaults_on_and_can_be_disabled() {
    use soundlog::chip::{Chip, PwmSpec};
    use soundlog::vgm::VgmBuilder;
    use soundlog::vgm::command::{Instance, WaitSamples};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pwm.vgm");
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pwm, Instance::Primary, 44_100);
    builder.add_chip_write(
        Instance::Primary,
        PwmSpec {
            register: 1,
            value: 3,
        },
    );
    for _ in 0..31 {
        builder.add_chip_write(
            Instance::Primary,
            PwmSpec {
                register: 4,
                value: 0x100,
            },
        );
        builder.add_vgm_command(WaitSamples(2));
    }
    fs::write(&source, Vec::<u8>::from(builder.finalize())).unwrap();

    let mut outputs = Vec::new();
    for (name, value) in [
        ("default", None),
        ("true", Some("true")),
        ("false", Some("false")),
    ] {
        let output = dir.path().join(format!("{name}.wav"));
        let mut command = cli();
        command.arg(&source).arg(&output);
        if let Some(value) = value {
            command.args(["--compat-vgm-pwm", value]);
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        outputs.push(
            hound::WavReader::open(output)
                .unwrap()
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );
    }
    assert_eq!(outputs[0], outputs[1]);
    assert!(outputs[0].iter().any(|sample| *sample != 0));
    assert!(outputs[2].iter().all(|sample| *sample == 0));
}

#[test]
fn cli_rejects_pwm_compatibility_settings_for_mdx() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.mdx");
    fs::write(
        &source,
        include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
    )
    .unwrap();
    fs::write(
        dir.path().join("mdx_fm_loop.pdx"),
        include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx"),
    )
    .unwrap();
    for value in ["true", "false"] {
        let output = dir.path().join(format!("{value}.wav"));
        let result = cli()
            .arg(&source)
            .args(["--compat-vgm-pwm", value, "--pdx"])
            .arg(dir.path().join("mdx_fm_loop.pdx"))
            .arg(&output)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("--compat-vgm-pwm"));
        assert!(!output.exists());
    }
}

#[test]
fn library_rejects_conflicting_infinite_options() {
    let options = || chipstream_cli::PlayOptions {
        file: "unused.vgm".into(),
        output: Some("unused.wav".into()),
        sample_rate: None,
        pdx: None,
        ym2608_rhythm_rom: None,
        loop_count: None,
        fadeout_samples: None,
        infinite: true,
        loop_modifier: None,
        loop_base: None,
        compat_vgm_pwm: None,
        no_progress: false,
    };
    assert!(
        chipstream_cli::play(options())
            .unwrap_err()
            .to_string()
            .contains("infinite playback")
    );
    let mut with_count = options();
    with_count.output = None;
    with_count.loop_count = Some(2);
    assert!(
        chipstream_cli::play(with_count)
            .unwrap_err()
            .to_string()
            .contains("infinite playback")
    );
    let mut invalid_rate = options();
    invalid_rate.infinite = false;
    invalid_rate.sample_rate = Some(0);
    assert!(
        chipstream_cli::play(invalid_rate)
            .unwrap_err()
            .to_string()
            .contains("sample rate")
    );
}

#[test]
fn cli_renders_wav_and_protects_input_and_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.vgm");
    let output = dir.path().join("output.wav");
    let bytes = include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm");
    fs::write(&source, bytes).unwrap();
    fs::write(&output, b"old output").unwrap();
    let result = cli()
        .arg(&source)
        .arg(&output)
        .args(["--sample-rate", "48000"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut wav = hound::WavReader::open(&output).unwrap();
    assert_eq!(wav.spec().sample_rate, 48_000);
    assert_eq!(wav.spec().channels, 2);
    assert!(wav.samples::<i16>().any(|sample| sample.unwrap() != 0));
    let rendered = fs::read(&output).unwrap();
    let library_output = dir.path().join("library.wav");
    chipstream_cli::play(chipstream_cli::PlayOptions {
        file: source.clone(),
        output: Some(library_output.clone()),
        sample_rate: Some(48_000),
        pdx: None,
        ym2608_rhythm_rom: None,
        loop_count: None,
        fadeout_samples: None,
        infinite: false,
        loop_modifier: None,
        loop_base: None,
        compat_vgm_pwm: None,
        no_progress: false,
    })
    .unwrap();
    assert_eq!(fs::read(library_output).unwrap(), rendered);
    for args in [
        vec!["--infinite"],
        vec!["--sample-rate", "0"],
        vec!["--sample-rate", "not-a-number"],
    ] {
        let result = cli().arg(&source).arg(&output).args(args).output().unwrap();
        assert!(!result.status.success());
        assert_eq!(fs::read(&output).unwrap(), rendered);
    }
    let result = cli().arg(&source).arg(&source).output().unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&source).unwrap(), bytes);
}

#[test]
fn cli_wav_default_rate_needs_no_audio_device() {
    use soundlog::vgm::command::WaitSamples;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.vgm");
    let output = dir.path().join("output.wav");
    let mut builder = soundlog::vgm::VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.set_loop_index(1);
    builder.add_vgm_command(WaitSamples(1000));
    fs::write(&source, Vec::<u8>::from(builder.finalize())).unwrap();
    let result = cli().arg(&source).arg(&output).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let wav = hound::WavReader::open(output).unwrap();
    assert_eq!(wav.spec().sample_rate, 44_100);
    assert_eq!(wav.duration(), 90_300);
}

#[test]
fn cli_streams_wav_to_stdout_when_output_is_dash() {
    use soundlog::vgm::command::WaitSamples;
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input.vgm");
    let output = dir.path().join("output.wav");
    let mut builder = soundlog::vgm::VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.set_loop_index(1);
    builder.add_vgm_command(WaitSamples(1000));
    fs::write(&source, Vec::<u8>::from(builder.finalize())).unwrap();

    let streamed = cli()
        .arg(&source)
        .arg("-")
        .args(["--loop-count", "1", "--fadeout-samples", "0"])
        .output()
        .unwrap();
    assert!(
        streamed.status.success(),
        "{}",
        String::from_utf8_lossy(&streamed.stderr)
    );
    assert!(streamed.stdout.starts_with(b"RIFF"));
    let wav = hound::WavReader::new(Cursor::new(&streamed.stdout)).unwrap();
    assert_eq!(wav.spec().sample_rate, 44_100);
    assert_eq!(wav.duration(), 1100);

    let saved = cli()
        .arg(&source)
        .arg(&output)
        .args(["--loop-count", "1", "--fadeout-samples", "0"])
        .output()
        .unwrap();
    assert!(saved.status.success());
    assert_eq!(streamed.stdout, fs::read(output).unwrap());
}

#[test]
fn cli_fades_vgm_and_mdx_to_wav() {
    use soundlog::vgm::command::WaitSamples;
    let dir = tempfile::tempdir().unwrap();
    let vgm = dir.path().join("loop.vgm");
    let mut builder = soundlog::vgm::VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(100));
    builder.set_loop_index(1);
    builder.add_vgm_command(WaitSamples(1000));
    fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();
    let mdx = dir.path().join("mdx_fm_loop.mdx");
    fs::write(
        &mdx,
        include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
    )
    .unwrap();
    fs::write(
        dir.path().join("mdx_fm_loop.pdx"),
        include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx"),
    )
    .unwrap();
    for (index, input) in [&vgm, &mdx].into_iter().enumerate() {
        let output = dir.path().join(format!("{index}.wav"));
        let result = cli()
            .arg(input)
            .arg(&output)
            .args([
                "--loop-count",
                "1",
                "--fadeout-samples",
                "4410",
                "--sample-rate",
                "48000",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let wav = hound::WavReader::open(output).unwrap();
        assert_eq!(wav.spec().sample_rate, 48_000);
        if index == 0 {
            assert_eq!(wav.duration(), 5_997);
        } else {
            assert!(wav.duration() > 4_800);
        }
    }
    for value in ["-1", "invalid"] {
        assert!(
            !cli()
                .arg(&vgm)
                .args(["--fadeout-samples", value])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

#[test]
fn cli_renders_mdx_with_pdx_and_compressed_vgm() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let mdx = dir.path().join("mdx_fm_loop.mdx");
    let pdx = dir.path().join("mdx_fm_loop.pdx");
    fs::write(
        &mdx,
        include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
    )
    .unwrap();
    fs::write(
        &pdx,
        include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx"),
    )
    .unwrap();
    let vgz = dir.path().join("music.vgz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"))
        .unwrap();
    fs::write(&vgz, encoder.finish().unwrap()).unwrap();
    for (index, input) in [&mdx, &vgz].into_iter().enumerate() {
        let output = dir.path().join(format!("{index}.wav"));
        let result = cli()
            .arg(input)
            .arg(&output)
            .args(["--sample-rate", "48000"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut wav = hound::WavReader::open(output).unwrap();
        assert_eq!(wav.spec().sample_rate, 48_000);
        assert!(wav.samples::<i16>().any(|sample| sample.unwrap() != 0));
    }
}

#[test]
fn cli_applies_ym2608_rom_when_rendering_wav() {
    use soundlog::chip::{Chip, Ym2608Spec};
    use soundlog::vgm::command::{Instance, WaitSamples};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("rhythm.vgm");
    let rom = dir.path().join("rhythm.rom");
    let output = dir.path().join("rhythm.wav");
    let mut builder = soundlog::vgm::VgmBuilder::new();
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
    let bytes: Vec<u8> = builder.finalize().into();
    fs::write(&input, bytes).unwrap();
    fs::write(&rom, [0x77; 8192]).unwrap();
    let result = cli()
        .arg(&input)
        .arg(&output)
        .arg("--ym2608-rhythm-rom")
        .arg(&rom)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut wav = hound::WavReader::open(output).unwrap();
    assert!(
        wav.samples::<i16>()
            .any(|sample| sample.unwrap().abs() > 32)
    );
}

#[test]
fn cli_protects_pdx_and_rhythm_rom_from_output() {
    let dir = tempfile::tempdir().unwrap();
    let mdx = dir.path().join("song.mdx");
    let pdx = dir.path().join("samples.pdx");
    let rom = dir.path().join("rhythm.rom");
    let mut builder = soundlog::mdx::document::MdxBuilder::new();
    builder.set_pdx_name(Some("samples.pdx"));
    fs::write(&mdx, builder.finalize().unwrap().to_bytes().unwrap()).unwrap();
    let samples = soundlog::mdx::pdx::PdxBuilder::new().finalize().to_bytes();
    fs::write(&pdx, &samples).unwrap();
    let result = cli().arg(&mdx).arg(&pdx).output().unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&pdx).unwrap(), samples);
    let vgm = dir.path().join("song.vgm");
    fs::write(
        &vgm,
        include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
    )
    .unwrap();
    fs::write(&rom, [0x77; 8192]).unwrap();
    let result = cli()
        .arg(&vgm)
        .arg(&rom)
        .arg("--ym2608-rhythm-rom")
        .arg(&rom)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&rom).unwrap(), [0x77; 8192]);
}

#[cfg(unix)]
#[test]
fn cli_does_not_replace_symlink_outputs() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let vgm = dir.path().join("song.vgm");
    let target = dir.path().join("target.wav");
    let output = dir.path().join("link.wav");
    fs::write(
        &vgm,
        include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
    )
    .unwrap();
    fs::write(&target, b"keep").unwrap();
    symlink(&target, &output).unwrap();
    let result = cli().arg(&vgm).arg(&output).output().unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert!(
        fs::symlink_metadata(output)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
