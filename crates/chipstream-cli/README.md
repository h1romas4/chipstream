# chipstream-cli

`csplay` plays VGM, VGZ, MDX, and MML files through an audio device or renders
them to WAV.

## CLI overview

```text
Play retro sound-chip music

Usage: csplay [OPTIONS] <FILE>

Options:
  -o, --output <WAV_FILE>              Render to a stereo 16-bit PCM WAV file
      --sample-rate <HZ>               Output sample rate
      --pdx <PDX_FILE>                 PDX sample file for MDX/MML
      --ym2608-rhythm-rom <ROM_FILE>   YM2608 rhythm ROM for VGM/VGZ
      --loop-count <COUNT>             Number of playthroughs
      --fadeout-samples <SAMPLES>      Fade duration in 44,100 Hz samples
      --infinite                       Repeat indefinitely when a loop exists
      --loop-modifier <LOOP_MODIFIER>  Override the VGM loop modifier
      --loop-base <LOOP_BASE>          Override the VGM loop base
      --compat-vgm-pwm <BOOL>          Enable the VGM PWM workaround (default: true)
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
csplay example.vgz -o example.wav --sample-rate 48000
csplay example.vgz -o example.wav --loop-count 2 --fadeout-samples 88200
```

- WAV output defaults to 44,100 Hz; device playback defaults to the device's
  rate. An explicit device rate must be supported. Output samples are clipped.
- Playback defaults to one playthrough (`--loop-count 0` also means one).
  `--fadeout-samples 44100` fades for one second after the selected loop;
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
