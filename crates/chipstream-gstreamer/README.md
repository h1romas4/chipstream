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

### Playback behavior

| Condition | Behavior |
|---|---|
| Property changes | Available through the `READY` state |
| No loop point | Plays to the natural end; embedded MDX fadeout may end playback earlier |
| Infinite playback (`play-count=0`) | `fadeout-seconds` is not applied |
| Adjacent `ym2608_adpcm_rom.bin` | Automatically loaded for VGM/VGZ and applied to each YM2608 instance |

Example URI: `chipstream:///music/game%20theme.vgm`

## Build requirements

Install Rust and the GStreamer development packages for GStreamer 1.14 or newer.
On Debian or Ubuntu, install `pkg-config`, `libgstreamer1.0-dev`, and
`libgstreamer-plugins-base1.0-dev`.

For real-time playback, build the optimized release plugin from the workspace root.
Debug builds can be too slow to render CPU-intensive VGM files and may cause audio
underruns.

```sh
cargo build --release -p chipstream-gstreamer
```

The build produces `target/release/libgstchipstream.so`.

Inspect the registered element:

```sh
GST_PLUGIN_PATH=target/release gst-inspect-1.0 chipstreamsrc
```

Play a file through an installed audio sink:

```sh
GST_PLUGIN_PATH=target/release gst-launch-1.0 chipstreamsrc location=song.vgm play-count=3 fadeout-seconds=4.5 ! audioconvert ! audioresample ! autoaudiosink
```

Play through `playbin3` using the URI source:

```sh
GST_PLUGIN_PATH=target/release gst-launch-1.0 playbin3 uri=chipstream:///music/song.vgm
```
