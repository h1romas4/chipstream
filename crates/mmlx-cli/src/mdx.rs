use std::fs;
use std::path::{Path, PathBuf};

use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pcm::encode_adpcm;
use soundlog::mdx::pdx::PdxBuilder;
use soundlog::meta::Gd3;

pub(crate) fn parse_input(input: &Path) -> Result<mmlx::mdx::MmlDocument, String> {
    let source =
        fs::read_to_string(input).map_err(|error| format!("{}: {error}", input.display()))?;
    mmlx::mdx::parse(&source).map_err(|error| format!("parse error: {error}"))
}

pub(crate) fn write_vgm(
    source: &mmlx::mdx::MmlDocument,
    mdx: &soundlog::mdx::document::MdxDocument,
    input: &Path,
    output: &Path,
) -> Result<(), String> {
    let pdx_bytes = source
        .pcm_file
        .as_deref()
        .map(|name| {
            let path = find_pdx_path(input, name).ok_or_else(|| {
                format!(
                    "PDX file not found: {name:?} (referenced by {})",
                    input.display()
                )
            })?;
            fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
        })
        .transpose()?;
    let package = MdxPackage::parse_owned(mdx.to_bytes(), pdx_bytes)
        .map_err(|error| format!("failed to prepare VGM conversion: {error}"))?;
    let mut document = to_vgm_document(&package, &MdxToVgmOptions::default())
        .map_err(|error| format!("MDX to VGM conversion failed: {error}"))?;
    if let Some(title) = source.title.as_deref() {
        document.gd3 = Some(Gd3 {
            track_name_origin: Some(title.to_owned()),
            ..Gd3::default()
        });
    }
    let bytes: Vec<u8> = (&document).into();
    fs::write(output, bytes).map_err(|error| format!("{}: {error}", output.display()))
}

fn find_pdx_path(input: &Path, name: &str) -> Option<PathBuf> {
    let parent = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let path = parent.join(name);
    if path.is_file() {
        return Some(path);
    }
    if Path::new(name).extension().is_none() {
        for extension in ["pdx", "PDX"] {
            let candidate = parent.join(format!("{name}.{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let requested_name = Path::new(name).file_name()?.to_str()?;
    fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.eq_ignore_ascii_case(requested_name))
        })
}

pub(crate) fn build_pdx(files: &[PathBuf]) -> Result<(), String> {
    let (output, inputs) = files
        .split_last()
        .ok_or_else(|| "expected at least one input WAV and one output PDX".to_owned())?;
    let mut builder = PdxBuilder::new();

    for (sample_index, input) in inputs.iter().enumerate() {
        let samples = read_wav_samples(input)?;
        let encoded = encode_adpcm(&samples);
        builder
            .set_sample(sample_index / 96, sample_index % 96, encoded)
            .map_err(|error| format!("{}: {error}", input.display()))?;
    }

    fs::write(output, builder.finalize().to_bytes())
        .map_err(|error| format!("{}: {error}", output.display()))
}

fn read_wav_samples(path: &Path) -> Result<Vec<i16>, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(format!(
            "{}: only mono WAV files are supported (found {} channels)",
            path.display(),
            spec.channels
        ));
    }

    match spec.sample_format {
        hound::SampleFormat::Int => match spec.bits_per_sample {
            8 => reader
                .samples::<i8>()
                .map(|sample| sample.map(|value| i16::from(value) << 4))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("{}: {error}", path.display())),
            16 => reader
                .samples::<i16>()
                .map(|sample| sample.map(|value| value >> 4))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("{}: {error}", path.display())),
            24 | 32 => reader
                .samples::<i32>()
                .map(|sample| {
                    sample.map(|value| {
                        let shift = u32::from(spec.bits_per_sample - 12);
                        (value >> shift).clamp(-2048, 2047) as i16
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("{}: {error}", path.display())),
            bits => Err(format!(
                "{}: unsupported integer WAV bit depth {bits}",
                path.display()
            )),
        },
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| {
                sample.map(|value| (value * 2047.0).round().clamp(-2048.0, 2047.0) as i16)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::mdx::pdx::PdxDocument;

    #[test]
    fn builds_pdx_from_wav_input() {
        let stem = format!("mmlx-pdx-test-{}", std::process::id());
        let wav_path = std::env::temp_dir().join(format!("{stem}.wav"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.pdx"));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for sample in [0_i16, 1024, -1024] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();

        build_pdx(&[wav_path.clone(), pdx_path.clone()]).unwrap();
        let document = PdxDocument::parse(&fs::read(&pdx_path).unwrap()).unwrap();
        assert_eq!(document.sample_bytes(0, 0).unwrap().len(), 2);

        fs::remove_file(wav_path).unwrap();
        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_stem_with_uppercase_extension() {
        let stem = format!("mmlx-pdx-path-{}", std::process::id());
        let input_path = std::env::temp_dir().join(format!("{stem}.mml"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(find_pdx_path(&input_path, &stem), Some(pdx_path.clone()));

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_name_case_insensitively() {
        let stem = format!("mmlx-pdx-case-{}", std::process::id());
        let input_path = std::env::temp_dir().join(format!("{stem}.mml"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(
            find_pdx_path(&input_path, &format!("{stem}.pdx")),
            Some(pdx_path.clone())
        );

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_for_input_without_parent_directory() {
        let stem = format!("mmlx-pdx-relative-{}", std::process::id());
        let input_path = PathBuf::from(format!("{stem}.mml"));
        let pdx_path = PathBuf::from(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(
            find_pdx_path(&input_path, &format!("{stem}.pdx")),
            Some(PathBuf::from(format!("./{stem}.PDX")))
        );

        fs::remove_file(pdx_path).unwrap();
    }
}
