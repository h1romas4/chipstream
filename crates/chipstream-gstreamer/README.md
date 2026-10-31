# chipstream-gstreamer

GStreamer plugin for VGM and MDX playback with `chipstream`.

## Element API

### `chipstreamsrc`

| API | Description |
|---|---|
| Source caps | Interleaved stereo `audio/x-raw`, `F32LE` |
| Input formats | `.vgm`, `.vgz`, `.mdx` |
| Tags | VGM GD3 tags; MDX title |
| URI protocol | `chipstream://` with an absolute local file path; supported by `playbin` and `playbin3` |
| Duration query | Exact finite duration when it fits the bounded command scan, including configured loops and fadeout; unbounded or very long playback may be unknown |
| Seek | Time-format, normal-rate seeks; reuses the initial state and two recent checkpoints captured about every 30 seconds, then renders forward from the nearest checkpoint. If snapshots are unavailable, seeks fall back to rendering from the start |

### Properties

| Property | Type | Default | Description |
|---|---|---:|---|
| `location` | string | — | Input file path |
| `pdx-location` | string | — | Optional MDX PDX path; otherwise resolved beside the MDX |
| `sample-rate` | uint | 48000 | Output rate in Hz (8000–192000) |
| `chunk-frames` | uint | 512 | Stereo frames per output buffer (64–8192) |
| `play-count` | uint | 2 | Total traversals; `0` repeats indefinitely |
| `fadeout-seconds` | double | 2.0 | Fade after the final loop point; `0` disables it (maximum 3600) |
| `ndsif-port` | string | — | Optional NDSIF serial port; unset uses emulation for every chip |
| `ndsif-baud-rate` | uint | 115200 | NDSIF serial baud rate |
| `ndsif-timeout-ms` | uint | 500 | Serial request/response timeout |
| `ndsif-reset` | boolean | true | Reset the device when opening the serial port |

### Playback behavior

| Condition | Behavior |
|---|---|
| Property changes | Available through the `READY` state |
| No loop point | Plays to the natural end; embedded MDX fadeout may end playback earlier |
| Infinite playback (`play-count=0`) | `fadeout-seconds` is not applied |
| Adjacent `ym2608_adpcm_rom.bin` | Automatically loaded for VGM/VGZ and applied to each YM2608 instance |

Example URI: `chipstream:///music/game%20theme.vgm`

### NDSIF hardware playback

When `ndsif-port` is set, the plugin opens the serial device, optionally resets
it, and queries its capabilities. YM2151 uses the device when its reported
clock matches. If the file has no OKIM6258 clock, YM2151 writes are sent
directly and paced by the host playback clock. When the file uses OKIM6258, its
audio stream also provides timing for YM2151 events; that path is used only
when the device supports the required clock and stream capabilities.
Unsupported chips or instances continue through their emulator backends. The
OKIM6258 path streams source ADPCM bytes and generates state-matched ADPCM
silence for timing gaps and the track tail. The device produces the audible
hardware output; NDSIF-backed chip voices are silent in the GStreamer PCM
stream.

NDSIF exposes a continuous OKIM6258 stream, so loop/restart behavior and the
GStreamer `fadeout-seconds` ramp are not reproduced on the physical chip.
For predictable hardware playback, use `play-count=1` and
`fadeout-seconds=0`. Only the primary OKIM6258 stream is supported.

```sh
GST_PLUGIN_PATH="$HOME/.local/lib/chipstream" gst-launch-1.0 \
  chipstreamsrc location=song.vgm ndsif-port=/dev/ttyACM0 \
    ndsif-baud-rate=115200 ndsif-timeout-ms=500 \
  ! audioconvert ! audioresample ! autoaudiosink
```

Serial communication and hardware playback require a connected, compatible
NDSIF device. Seeking is disabled while `ndsif-port` is set because the device
stream cannot be restored from a sequencer checkpoint.

## Build requirements

Install Rust and the GStreamer development packages for GStreamer 1.14 or newer.
On Debian or Ubuntu, install `pkg-config`, `libgstreamer1.0-dev`, and
`libgstreamer-plugins-base1.0-dev`.

For real-time playback, build and install the optimized release plugin from the
workspace root. Debug builds can be too slow to render CPU-intensive VGM files and
may cause audio underruns.

```sh
cargo run -p xtasks -- install-gstreamer
```

On Linux, this builds `libgstchipstream.so` and copies it to
`~/.local/lib/chipstream/`.

Inspect the registered element:

```sh
GST_PLUGIN_PATH="$HOME/.local/lib/chipstream" gst-inspect-1.0 chipstreamsrc
```

Play a file through an installed audio sink:

```sh
GST_PLUGIN_PATH="$HOME/.local/lib/chipstream" gst-launch-1.0 chipstreamsrc location=song.vgm play-count=3 fadeout-seconds=4.5 ! audioconvert ! audioresample ! autoaudiosink
```

Play through `playbin3` using the URI source:

```sh
GST_PLUGIN_PATH="$HOME/.local/lib/chipstream" gst-launch-1.0 playbin3 uri=chipstream:///music/song.vgm
```

## Termusic

Termusic requires several patches before it can use the chipstream GStreamer
plugin; these environment variables alone are not sufficient.

```sh
# termusic
export GST_PLUGIN_PATH=/home/hiromasa/.local/lib/chipstream
export TMS_BACKEND=gstreamer
```
