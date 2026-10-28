# chipstream-cli

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

- Provide an output path after the input to render a stereo 16-bit PCM WAV file;
  use `-` to write the WAV stream to standard output. WAV output defaults to
  44,100 Hz; device playback defaults to the device's rate. An explicit device
  rate must be supported. Output samples are clipped.
- Audio-device playback displays elapsed and total time (`elapsed / total`)
  and a terminal-width progress bar (`o` played, `-` remaining, `|` at 50%) on one
  updating standard-output line, preceded by a line with the track title and
  sound chips. VGM uses the English GD3 title. Finite duration is calculated from soundlog's
  command stream without running chip emulators. Infinite playback shows
  `--%`. Press the right-arrow key during playback to seek forward by five
  seconds, or press Ctrl+C to stop playback. `--no-progress` hides the
  progress line but leaves the metadata line.
- Playback defaults to two playthroughs (`--loop-count 0` means one).
  The default `--fadeout-samples 88200` fades for two seconds after the selected
  loop; `--fadeout-samples 44100` fades for one second;
  fadeout has no effect without a loop, with `--infinite`, or with a zero
  duration. `--infinite` cannot be combined with WAV output or `--loop-count`.
- MML files must be UTF-8 and are compiled to MDX in memory. MDX and MML
  playback searches for the named PDX alongside the input; use `--pdx` to
  override it. A missing or ambiguous PDX is an error.
- YM2608 rhythm ROM is not bundled. Supply a compatible raw ROM with
  `--ym2608-rhythm-rom` for VGM/VGZ playback. VGM loop overrides and the PWM
  compatibility option are also VGM/VGZ-only. The PWM workaround is enabled
  by default; `--compat-vgm-pwm false` disables it.

## Troubleshooting and caveats

- WAV output is limited to 4 GiB. Existing output is replaced only after
  successful rendering. Inputs, symlinks, and special files cannot be used as
  output paths.
- Input files and decompressed data are limited to 64 MiB. Unsupported chips
  are silent. Rendering and device playback are not guaranteed real-time safe.
- This CLI targets desktop systems; it is not a browser application.
