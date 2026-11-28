# cstools

`csplay` plays VGM, VGZ, MDX, MML, XGM v1, gzip-compressed XGZ, and S98 files through an audio device or renders
them to WAV.

## CLI overview

```text
Play retro sound-chip music

Usage: csplay [OPTIONS] <FILE> [OUTPUT]
  csplay device list

Options:
  --output-device <ID>             Audio output device ID from `csplay device list`
      --sample-rate <HZ>               Output sample rate
      --pdx <PDX_FILE>                 PDX sample file for MDX/MML
      --xgm-track <INDEX>              Zero-based XGM/XGZ track index (default: 0)
      --ym2608-rhythm-rom <ROM_FILE>   YM2608 rhythm ROM for VGM/VGZ or S98
      --loop-count <COUNT>             Number of playthroughs (default: 2)
      --fadeout-samples <SAMPLES>      Fade duration in 44,100 Hz samples (default: 88,200)
      --infinite                       Repeat indefinitely when a loop exists
      --loop-modifier <LOOP_MODIFIER>  Override the VGM loop modifier
      --loop-base <LOOP_BASE>          Override the VGM loop base
      --compat-vgm-pwm <BOOL>          Enable the VGM PWM workaround (default: true)
      --ay8910-ym2149 <BOOL>           Approximate VGM AY8910 with YM2149 (default: true)
      --ay8910-native                  Use native AY-3-8910/8912/8913 emulation
      --no-progress                    Do not display playback progress
  -h, --help     Print help
  -V, --version  Print version
```

Run `csplay --help` for the full option descriptions.

Examples:

```bash
csplay example.vgm
csplay device list
csplay example.vgm --output-device 'alsa:device-id'
csplay example.vgz --loop-count 2
csplay example.vgm --infinite
csplay example.mdx --pdx samples.pdx
csplay example.mml
csplay example.xgm --xgm-track 0
csplay example.xgz example.wav
csplay example.s98 example.wav
csplay example.vgm --ym2608-rhythm-rom ym2608.rom
csplay example.vgm --compat-vgm-pwm false
csplay example.vgm --ay8910-ym2149 false
csplay example.vgm --no-progress
csplay example.vgz example.wav --sample-rate 48000
csplay example.vgz example.wav --loop-count 2 --fadeout-samples 88200
csplay example.vgm - | ffplay -i -
```

## Playback reference

### Output

| Mode | Behavior |
| --- | --- |
| Audio device | Uses the default device unless `--output-device <ID>` selects an ID from `csplay device list`. Uses the selected device's default sample rate; an explicit rate must be supported. |
| WAV file | Add an output path after the input to render stereo 16-bit PCM WAV. Defaults to 44,100 Hz. |
| WAV to stdout | Use `-` as the output path. |
| Samples | Output samples are clipped. |

`--output-device` cannot be combined with WAV output. Device IDs are provided
by the audio host and may change when devices or system configuration change.

### Audio-device playback

| Feature | Behavior |
| --- | --- |
| Metadata | Prints the track title and sound chips before the progress line. VGM and XGM use the English GD3 title. |
| Progress | Updates one stdout line with elapsed/total time and a terminal-width bar (`o` played, `-` remaining, `|` at 50%). |
| Duration | Calculated from soundlog's command stream without running chip emulators. Infinite playback displays `--%`. |
| Seek | Press the right arrow to seek forward five seconds. |
| Stop | Press Ctrl+C. |
| `--no-progress` | Hides the progress line but leaves the metadata line. |

### Playback defaults and limits

| Setting | Default / behavior |
| --- | --- |
| `--loop-count` | Defaults to two playthroughs; `0` means one. |
| `--fadeout-samples` | Defaults to 88,200 samples (two seconds at 44,100 Hz). `44,100` samples is one second. Fades after the selected loop. |
| Fadeout | Has no effect without a loop, with `--infinite`, or with a zero duration. |
| `--infinite` | Cannot be combined with WAV output or `--loop-count`. |

### Input and chip-specific options

| Input / option | Behavior |
| --- | --- |
| MML | Must be UTF-8; compiled to MDX in memory. |
| MDX and MML | Search for the named PDX alongside the input. Use `--pdx` to override the path. Missing or ambiguous PDX files are errors. |
| XGM and XGZ | XGM v1 only; XGZ is gzip-compressed XGM. Select a zero-based track with `--xgm-track` (default: 0). PCM mixing approximates the original driver. |
| S98 | Versions 1–3; unsupported devices, nonzero PAN, and differently clocked instances of the same chip are rejected. Tags use BOM-prefixed UTF-8 for v3 or Shift_JIS otherwise; invalid text is an error. |
| `--ym2608-rhythm-rom` | YM2608 rhythm ROM is not bundled. Supply a compatible raw ROM for VGM/VGZ or S98 playback. |
| `--ay8910-ym2149` | Approximate VGM/VGZ or S98 AY8910 with YM2149 by default. Set to `false` to disable the approximation; AY8910 is then silent. Unsupported AY variants or output flags are rejected when enabled. |
| `--ay8910-native` | Use native AY-3-8910/8912/8913 instead of YM2149 approximation; cannot be combined with `--ay8910-ym2149`. Other models and raw/discrete output flags are rejected. |
| VGM/VGZ options | VGM loop overrides and PWM compatibility settings apply only to VGM/VGZ. The PWM workaround is enabled by default; `--compat-vgm-pwm false` disables it. |

## Troubleshooting and caveats

- WAV output is limited to 4 GiB. Existing output is replaced only after
  successful rendering. Inputs, symlinks, and special files cannot be used as
  output paths.
- Input files and decompressed data are limited to 64 MiB. Unsupported chips
  are silent. Rendering and device playback are not guaranteed real-time safe.
- XGM parsing and finite playback use soundlog's default resource limits.
  `--infinite` disables the XGM frame limit, but retains the command-per-frame limit.
- This CLI targets desktop systems; it is not a browser application.

## License

BSD-3-Clause License
