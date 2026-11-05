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

NDDRV restores the previous window size and maximized state on startup. The
window is centered and fitted to the primary display's available area when
that information is available. Otherwise, placement is left to the window
system.

## Library Reference

The search field filters by title, artist, album, notes, or path. Add chip names
or the `MDX` and `VGM` format tags to narrow results; `VGM` includes VGZ files.
Use the clear button on the right to remove all search text and tags.

For VGM/VGZ tracks, General settings select English or original-language
metadata. English is the default, and missing fields fall back to the other
language. Search matches either language; MDX metadata is unaffected.

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

Playlist tracks play in order; previous and next do not wrap. With loop off,
the default is two plays followed by a two-second fade. Enable loop to repeat
indefinitely.

The Playback settings button sets play count (1-99), fade-out (0-600 seconds),
and the emulated audio output. **System default** follows the operating system;
supported 44.1, 48, 96, and 192 kHz rates or **Device default** can be chosen.
Settings are saved and apply to the next track. Changing the output device
resets its sample rate to **Device default**. With NanoDrive routing, emulated
chips use this output; hardware audio is separate.

Track Info shows available metadata. Artwork is loaded from the track's folder.

### Recording

Recording is experimental. Enable it in Recording settings, choose an input
device, input sample rate and WAV output folder, then use the record button in
the playback controls. The selected input must receive the audio to be recorded;
NanoDrive and software output may need external mixing or system loopback.
Recording starts before playback and saves a 16-bit WAV for each playlist track.
The start delay is included. Stopping or changing tracks saves the current file.
Pausing playback does not pause the input recording. A notification reports each
saved file; recording errors stop playback and are reported separately.

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
