use std::fs;
use std::path::{Path, PathBuf};

use soundlog::mdx::pcm::encode_adpcm;
use soundlog::mdx::pdx::PdxBuilder;

pub(crate) fn parse_input(input: &Path) -> Result<mmlx::mdx::MmlDocument, String> {
    let source =
        fs::read_to_string(input).map_err(|error| format!("{}: {error}", input.display()))?;
    mmlx::mdx::parse(&source).map_err(|error| format!("parse error: {error}"))
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
}
