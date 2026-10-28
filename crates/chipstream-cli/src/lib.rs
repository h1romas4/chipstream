//! Playback operations used by the chipstream command-line frontend.

mod input;
mod output;
mod playback;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Options for playing or rendering a VGM, VGZ, MDX, or MML file.
pub struct PlayOptions {
    /// Input file.
    pub file: PathBuf,
    /// WAV output path, `-` for stdout, or `None` for device playback.
    pub output: Option<PathBuf>,
    /// Output rate in Hz; defaults to 44,100 for WAV or the device rate.
    pub sample_rate: Option<u32>,
    /// Optional PDX path; otherwise resolve the MDX or MML's filename.
    pub pdx: Option<PathBuf>,
    /// Optional YM2608 rhythm ROM path for VGM.
    pub ym2608_rhythm_rom: Option<PathBuf>,
    /// Finite playthrough count; `None` and zero both mean one.
    pub loop_count: Option<u32>,
    /// Fade duration in 44,100 Hz samples.
    pub fadeout_samples: Option<u32>,
    /// Repeat indefinitely; incompatible with `output` and `loop_count`.
    pub infinite: bool,
    /// Override the VGM loop modifier.
    pub loop_modifier: Option<u8>,
    /// Override the VGM loop base.
    pub loop_base: Option<i8>,
    /// Configure the VGM PWM workaround; `None` enables it by default.
    pub compat_vgm_pwm: Option<bool>,
    /// Suppress playback progress output for audio-device playback.
    pub no_progress: bool,
}

/// Play through the default device, or render to WAV when an output is given.
pub fn play(options: PlayOptions) -> Result<()> {
    let PlayOptions {
        file,
        output,
        sample_rate,
        pdx,
        ym2608_rhythm_rom,
        loop_count,
        fadeout_samples,
        infinite,
        loop_modifier,
        loop_base,
        compat_vgm_pwm,
        no_progress,
    } = options;
    if infinite && (output.is_some() || loop_count.is_some()) {
        anyhow::bail!("infinite playback cannot be combined with WAV output or a loop count");
    }
    if sample_rate == Some(0) {
        anyhow::bail!("sample rate must be nonzero");
    }
    let output_path = output.as_deref().filter(|path| *path != Path::new("-"));
    input::protect_source(&file, output_path)?;
    if let Some(path) = ym2608_rhythm_rom.as_deref() {
        input::protect_source(path, output_path)?;
    }
    let data = input::load(&file, pdx.as_deref(), output_path)
        .with_context(|| format!("failed to load {}", file.display()))?;
    if ym2608_rhythm_rom.is_some() && matches!(data, input::Input::Mdx(_)) {
        anyhow::bail!("--ym2608-rhythm-rom applies only to VGM/VGZ");
    }
    let ym2608_rhythm_rom = ym2608_rhythm_rom
        .as_deref()
        .map(input::load_ym2608_rhythm_rom)
        .transpose()?;
    let options = playback::Options {
        loop_count: if infinite {
            None
        } else {
            Some(loop_count.unwrap_or(1))
        },
        fadeout_samples,
        loop_modifier,
        loop_base,
        compat_vgm_pwm,
        ym2608_rhythm_rom,
    };
    match output {
        Some(path) if path == Path::new("-") => {
            output::write_wav_stdout(data, options, sample_rate.unwrap_or(44_100))
                .with_context(|| format!("failed to render {}", file.display()))
        }
        Some(path) => output::write_wav(data, options, sample_rate.unwrap_or(44_100), &path)
            .with_context(|| format!("failed to render {}", file.display())),
        None => playback::play(data, options, sample_rate, no_progress)
            .with_context(|| format!("failed to play {}", file.display())),
    }
}
