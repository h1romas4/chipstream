use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use cpal::Sample;

use crate::input::Input;
use crate::playback::{Options, Player};

const WAV_MAX_DATA_BYTES: u32 = u32::MAX - 36;

fn wav_data_size(current: u32, frames: usize) -> Result<u32> {
    let bytes = frames
        .checked_mul(4)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| anyhow!("WAV output exceeds the RIFF size limit"))?;
    current
        .checked_add(bytes)
        .filter(|size| *size <= WAV_MAX_DATA_BYTES)
        .ok_or_else(|| anyhow!("WAV output exceeds the RIFF size limit"))
}

fn wav_sample(sample: f32) -> Result<i16> {
    if !sample.is_finite() {
        bail!("sequencer produced a non-finite PCM sample");
    }
    Ok(i16::from_sample(sample.clamp(-1.0, 1.0 - f32::EPSILON)))
}

pub(crate) fn write_wav(input: Input, options: Options, rate: u32, output: &Path) -> Result<()> {
    validate_wav(&input, &options, rate)?;
    match std::fs::symlink_metadata(output) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("WAV output must be a regular file, not a symlink or special file");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect WAV output {}", output.display()));
        }
    }
    let directory = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("failed to create temporary WAV in {}", directory.display()))?;
    render_wav(input, options, rate, temporary.as_file_mut())?;
    temporary
        .persist(output)
        .with_context(|| format!("failed to replace WAV output {}", output.display()))?;
    Ok(())
}

pub(crate) fn write_wav_stdout(input: Input, options: Options, rate: u32) -> Result<()> {
    validate_wav(&input, &options, rate)?;
    let mut temporary = tempfile::tempfile().context("failed to create temporary WAV")?;
    render_wav(input, options, rate, &mut temporary)?;
    temporary
        .seek(SeekFrom::Start(0))
        .context("failed to rewind temporary WAV")?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    io::copy(&mut temporary, &mut stdout).context("failed to write WAV to standard output")?;
    stdout
        .flush()
        .context("failed to flush WAV from standard output")
}

fn validate_wav(input: &Input, options: &Options, rate: u32) -> Result<()> {
    options.validate(input)?;
    if options.loop_count.is_none() {
        bail!("infinite playback cannot be rendered to a WAV file");
    }
    if rate == 0 || rate > u32::MAX / 4 {
        bail!("WAV sample rate must be in 1..={} Hz", u32::MAX / 4);
    }
    Ok(())
}

fn render_wav(
    input: Input,
    options: Options,
    rate: u32,
    output: &mut (impl Write + Seek),
) -> Result<()> {
    let mut player = Player::new(input, rate, &options)?;
    let writer = BufWriter::new(output);
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(writer, spec).context("failed to create WAV header")?;
    let mut bytes_written = 0;
    while let Some(block) = player.render()? {
        bytes_written = wav_data_size(bytes_written, block.frames().len())?;
        for sample in block.frames().iter().flatten() {
            writer
                .write_sample(wav_sample(*sample)?)
                .context("failed to write WAV sample")?;
        }
    }
    writer.finalize().context("failed to finalize WAV output")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_size_limits_are_checked_before_writes() {
        assert_eq!(wav_data_size(0, 1).unwrap(), 4);
        let max_aligned = WAV_MAX_DATA_BYTES & !3;
        assert_eq!(wav_data_size(max_aligned - 4, 1).unwrap(), max_aligned);
        assert!(wav_data_size(max_aligned, 1).is_err());
        assert!(wav_data_size(0, usize::MAX).is_err());
    }

    #[test]
    fn wav_clipping_and_nonfinite_samples() {
        assert_eq!(wav_sample(-2.0).unwrap(), i16::MIN);
        assert_eq!(wav_sample(2.0).unwrap(), i16::MAX);
        assert_eq!(wav_sample(0.0).unwrap(), 0);
        assert!(wav_sample(f32::NAN).is_err());
        assert!(wav_sample(f32::INFINITY).is_err());
    }
}
