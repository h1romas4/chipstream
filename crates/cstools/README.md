# cstools

`csplay` plays VGM, VGZ, MDX, and MML files through an audio device or renders
them to WAV.

## CLI overview

```text
Play retro sound-chip music

Usage: csplay [OPTIONS] <FILE> [OUTPUT]

Options:
      --sample-rate <HZ>               Output sample rate
      --pdx <PDX_FILE>                 PDX sample file for MDX/MML
      --ym2608-rhythm-rom <ROM_FILE>   YM2608 rhythm ROM for VGM/VGZ
      --loop-count <COUNT>             Number of playthroughs (default: 2)
      --fadeout-samples <SAMPLES>      Fade duration in 44,100 Hz samples (default: 88,200)
      --infinite                       Repeat indefinitely when a loop exists
      --loop-modifier <LOOP_MODIFIER>  Override the VGM loop modifier
      --loop-base <LOOP_BASE>          Override the VGM loop base
      --compat-vgm-pwm <BOOL>          Enable the VGM PWM workaround (default: true)
      --no-progress                    Do not display playback progress
  -h, --help     Print help
  -V, --version  Print version
```

Run `csplay --help` for the full option descriptions.

Examples:

```bash
csplay example.vgm
csplay example.vgz --loop-count 2
csplay example.vgm --infinite
csplay example.mdx --pdx samples.pdx
csplay example.mml
csplay example.vgm --ym2608-rhythm-rom ym2608.rom
csplay example.vgm --compat-vgm-pwm false
csplay example.vgm --no-progress
csplay example.vgz example.wav --sample-rate 48000
csplay example.vgz example.wav --loop-count 2 --fadeout-samples 88200
csplay example.vgm - | ffplay -i -
```

## Playback reference

### Output

| Mode | Behavior |
| --- | --- |
| Audio device | Uses the device's default sample rate. An explicit rate must be supported. |
| WAV file | Add an output path after the input to render stereo 16-bit PCM WAV. Defaults to 44,100 Hz. |
| WAV to stdout | Use `-` as the output path. |
| Samples | Output samples are clipped. |

### Audio-device playback

| Feature | Behavior |
| --- | --- |
| Metadata | Prints the track title and sound chips before the progress line. VGM uses the English GD3 title. |
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
| `--ym2608-rhythm-rom` | YM2608 rhythm ROM is not bundled. Supply a compatible raw ROM for VGM/VGZ playback. |
| VGM/VGZ options | VGM loop overrides and PWM compatibility settings apply only to VGM/VGZ. The PWM workaround is enabled by default; `--compat-vgm-pwm false` disables it. |

## Troubleshooting and caveats

- WAV output is limited to 4 GiB. Existing output is replaced only after
  successful rendering. Inputs, symlinks, and special files cannot be used as
  output paths.
- Input files and decompressed data are limited to 64 MiB. Unsupported chips
  are silent. Rendering and device playback are not guaranteed real-time safe.
- This CLI targets desktop systems; it is not a browser application.

## License

BSD-3-Clause License
