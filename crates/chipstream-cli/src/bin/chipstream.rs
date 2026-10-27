use std::path::PathBuf;

use anyhow::Result;
use chipstream_cli::{PlayOptions, play};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about = "Play retro sound-chip music")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Play a VGM, gzip-compressed VGZ, MDX, or MML file.
    Play {
        /// VGM, VGZ, MDX, or MML input file.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Render to a stereo 16-bit PCM WAV file instead of the audio device.
        #[arg(short, long, value_name = "WAV_FILE", conflicts_with = "infinite")]
        output: Option<PathBuf>,

        /// Output sample rate in Hz (WAV default: 44100; device default otherwise).
        #[arg(long, value_name = "HZ", value_parser = clap::value_parser!(u32).range(1..))]
        sample_rate: Option<u32>,

        /// PDX sample file for MDX/MML (otherwise resolved alongside the input).
        #[arg(long, value_name = "PDX_FILE")]
        pdx: Option<PathBuf>,

        /// Raw YM2608 rhythm ROM for VGM/VGZ playback.
        #[arg(long, value_name = "ROM_FILE")]
        ym2608_rhythm_rom: Option<PathBuf>,

        /// Number of playthroughs (default: 1; 0 means 1).
        #[arg(long, value_name = "COUNT", conflicts_with = "infinite")]
        loop_count: Option<u32>,

        /// Fade after the final loop, in 44,100 Hz samples (0 disables).
        #[arg(long, value_name = "SAMPLES")]
        fadeout_samples: Option<u32>,

        /// Repeat indefinitely when the file has a loop point.
        #[arg(long)]
        infinite: bool,

        /// Override the VGM loop modifier (0 means the default multiplier).
        #[arg(long)]
        loop_modifier: Option<u8>,

        /// Override the VGM signed loop base.
        #[arg(long, allow_hyphen_values = true)]
        loop_base: Option<i8>,

        /// Enable the VGM PWM bad-rip workaround (default: true).
        #[arg(long, value_name = "BOOL")]
        compat_vgm_pwm: Option<bool>,
    },
}

fn main() -> Result<()> {
    let Args { command } = Args::parse();
    match command {
        Command::Play {
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
        } => crate::play(crate::PlayOptions {
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
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_options_follow_soundlog_loop_semantics() {
        let args = Args::try_parse_from([
            "chipstream",
            "play",
            "song.vgz",
            "--loop-count",
            "2",
            "--fadeout-samples",
            "44100",
            "--loop-modifier",
            "16",
            "--loop-base",
            "-1",
            "--compat-vgm-pwm",
            "false",
            "--ym2608-rhythm-rom",
            "ym2608.rom",
        ])
        .unwrap();
        let Command::Play {
            loop_count,
            fadeout_samples,
            loop_modifier,
            loop_base,
            compat_vgm_pwm,
            ym2608_rhythm_rom,
            ..
        } = args.command;
        assert_eq!(
            (loop_count, loop_modifier, loop_base),
            (Some(2), Some(16), Some(-1))
        );
        assert_eq!(fadeout_samples, Some(44_100));
        assert_eq!(compat_vgm_pwm, Some(false));
        assert_eq!(ym2608_rhythm_rom, Some(PathBuf::from("ym2608.rom")));
        assert!(
            Args::try_parse_from([
                "chipstream",
                "play",
                "song.vgm",
                "--loop-count",
                "2",
                "--infinite",
            ])
            .is_err()
        );
    }

    #[test]
    fn compat_vgm_pwm_requires_boolean_value() {
        for (arguments, expected) in [
            (vec!["chipstream", "play", "song.vgm"], None),
            (
                vec!["chipstream", "play", "song.vgm", "--compat-vgm-pwm", "true"],
                Some(true),
            ),
            (
                vec![
                    "chipstream",
                    "play",
                    "song.vgm",
                    "--compat-vgm-pwm",
                    "false",
                ],
                Some(false),
            ),
        ] {
            let Command::Play { compat_vgm_pwm, .. } =
                Args::try_parse_from(arguments).unwrap().command;
            assert_eq!(compat_vgm_pwm, expected);
        }
        for arguments in [
            vec!["chipstream", "play", "song.vgm", "--compat-vgm-pwm"],
            vec![
                "chipstream",
                "play",
                "song.vgm",
                "--compat-vgm-pwm",
                "invalid",
            ],
            vec!["chipstream", "play", "song.vgm", "--compat-pwm", "false"],
            vec!["chipstream", "play", "song.vgm", "--no-pwm-compatibility"],
        ] {
            assert!(Args::try_parse_from(arguments).is_err());
        }
    }
}
