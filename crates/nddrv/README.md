# nddrv

`nddrv` is a desktop music library and player for VGM, VGZ, and MDX files.
It uses chipstream for software emulation and your system's default audio
output. Experimental NanoDrive hardware routing is also available.

## Download and Installation

Prebuilt binaries are published on [GitHub Releases](https://github.com/h1romas4/chipstream/releases)
under `nddrv-` tags. Downloads are available for Linux x86-64, Windows x86-64,
and macOS (Apple Silicon and Intel). macOS downloads include an app bundle;
Windows downloads include a standalone executable.

The Windows and macOS binaries are not signed or notarized, so the operating
system may show a security warning. Linux builds require compatible system
libraries, including ALSA and GPUI platform dependencies.

To build from source, install Rust and the platform prerequisites in the
[GPUI Kit installation guide](https://gpui-kit.com/docs/installation/), then run:

```sh
cargo run --release -p nddrv
```

## Quick Start

1. Choose **Add directory** in Library and select a folder containing music.
2. Select a folder or **All tracks**, then use the search field to filter results.
3. Double-click a track to add it to Playlist, or select tracks and press Enter.
4. Use the Playlist transport to play, pause, stop, or change tracks.

Use **Rescan** to refresh a registered folder after its contents change.

## Library Reference

The search field filters by title, artist, album, notes, or path. Add chip names
or the `MDX` and `VGM` format tags to narrow results; `VGM` includes VGZ files.
For VGM/VGZ tracks, General settings select English or original-language metadata;
English is the default, and missing fields fall back to the other language.
Search matches either language. MDX metadata is unaffected.
Multiple chip tags require all listed chips. When a NanoDrive is connected, the
`NanoDrive` tag filters for tracks with chips advertised by the device; it does
not guarantee that the track can be routed to hardware.

Music and Playlist support multi-selection:

| Action | Control |
| --- | --- |
| Select a row | Click |
| Add or remove a row from the selection | Ctrl-click (Command-click on macOS) |
| Select a range | Shift-click |
| Select all visible rows | Focus the list and press Ctrl+A (Command+A on macOS) |
| Add selected Music tracks to Playlist | Focus Music and press Enter |
| Remove selected Playlist entries | Focus Playlist and press Delete |

Right-click a Music row to add selected tracks to Playlist or copy its path.
Right-click selected Playlist rows to remove them or clear the playlist.

## Playback

Playlist tracks play in order. Previous and next do not wrap at either end.
With loop disabled, VGM and MDX tracks play through their song loop twice and
then fade out over two seconds by default. Use the settings button at the
bottom right to change the play count (1-99) and fade-out duration (0-600
seconds). Changes apply to the next track. Enable loop to repeat song loop
points indefinitely, regardless of these settings.

Track Info shows available track, game, composer, and notes metadata. Artwork
is loaded from supported image files in the track's folder.

### Piano Roll

Choose the piano icon to open the full-window Piano Roll. **Incoming** shows
upcoming notes; **Trail** shows recent notes. The **Channels** panel controls
which tracked voices are visible. Chip Gain adjusts software-emulated output;
FM channels also provide mute and solo controls. These controls do not affect
NanoDrive output.

The Piano Roll visualizes selected pitched voices; it does not display every
sound, such as PCM and noise.

## NanoDrive Routing

NanoDrive routing is experimental. Open the routing page with the cable icon,
select a serial port, and connect the device. Matching chip instances are routed
automatically where possible. To change a route, select a sequencer chip and
then a highlighted device or software-emulation target.

Hardware playback is best-effort and may not be sample-accurate. NanoDrive
audio comes from the device and must be mixed externally with software output.
Pause is unavailable while a hardware route is active. Unsupported or
incompatible explicit routes stop playback rather than silently switching to
emulation.

## Supported Files and Notes

- **VGM / VGZ:** VGZ files are decompressed for playback. Input files and
  decompressed VGZ data are limited to 64 MiB.
- **MDX:** A matching PDX sample bank is required when the MDX names one.
- **YM2608 rhythm instruments:** Playback can use the optional
  `ym2608_adpcm_rom.bin`, which is not included with the application.

Chip support depends on the file and available software emulation or connected
hardware. Playback errors are shown in the application.

## License

BSD-3-Clause License
