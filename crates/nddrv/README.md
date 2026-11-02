# nddrv

`nddrv` is a GPUI Kit desktop music library and player for VGM, VGZ, and MDX.
It uses chipstream's software emulation and the system's default audio output,
and supports experimental host-paced routing to NanoDrive hardware.

## Downloads

Prebuilt binaries are published on [GitHub Releases](https://github.com/h1romas4/chipstream/releases)
under `nddrv-` tags. Linux binaries are built for Ubuntu 26.04 x86-64; Windows
x86-64 and macOS Apple Silicon and Intel binaries are also available. The macOS
downloads are executable files, not `.app` bundles.

At startup, nddrv checks GitHub Releases for newer stable versions. When an
update is available, a button in the status bar opens its release page. The
check requires an internet connection and does not download or install updates.

The Windows and macOS binaries are not code-signed or notarized. Windows
SmartScreen or macOS Gatekeeper may warn about or block them. Linux binaries
also require compatible system libraries, including ALSA and the GPUI platform
dependencies.

## Requirements and Launch

Install Rust and the platform prerequisites in the
[GPUI Kit installation guide](https://gpui-kit.com/docs/installation/).
On Ubuntu, install the build tools and development libraries with:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libasound2-dev \
	libfontconfig-dev libxkbcommon-dev libxkbcommon-x11-dev
```

Then build and run in release mode:

```sh
cargo run --release -p nddrv
```

## Library

### Directories

Use **Add directory** to register folders. Files are scanned recursively
without following directory symlinks. **Rescan** refreshes metadata.
Folders without tracks in their descendants are hidden from the tree.

Expand the directory tree and select a folder to show tracks in it and its
descendants. Select **All tracks** to clear the directory filter. Music results
are ordered by directory and then by filename, rather than track metadata.

Scanning progress and completion are shown as notifications at the bottom right
of the window. Errors are shown in persistent notifications; scan and playlist
metadata errors include scrollable details.

### Search and Chip Tags

The text filter searches the title, artist, album, notes, or path and can be
combined with the directory filter.

Type a chip name in the Music search field and choose a suggestion with the
mouse or arrow keys and Enter to add an inline chip tag. Remove a tag with
Backspace or Delete. Multiple chip tags require all of those chips and are
combined with the text and directory filters. Suggestions use chips found in
the scanned library.

Type `MDX` or `VGM` to add a format tag; VGM includes VGZ files. Format tags
combine with the text, directory, and chip filters. Selecting both formats
matches either format.

Music shows a format tag (VGM, including VGZ, or MDX) alongside right-aligned
chip tags. The library shows chip tags from VGM/VGZ header clocks. MDX tags
show YM2151 and, when the MDX names a PDX sample bank, OKIM6258. These tags describe
declared or inferred hardware, not whether a chip is supported for playback.
Multiple instances are shown as, for example, `YM2203x2`; searching uses the
base chip name. In both the list and the search input, chips implemented by the
ymfm backend use the same teal color as the volume control, while other chip
tags use purple.
Hover over the tags to see the full chip list when it does not fit.
Music and Playlist show the game and track names together, with the artist
below. Hover over the title to see the full names. Playlist rows show their
position, or a play icon and accent when the track is playing.
Music rows are dimmed when the declared chips include a chip not emulated by
chipstream. Hover over the row to see which chips are unsupported. This is
based on the file header and does not check device availability or all other
playback requirements.
Playlist entries with unsupported chips show a red warning after the title;
hover over it to see the chip names. The warning does not prevent playback.

### Selection

Music and Playlist rows share these selection controls:

| Action | Control |
| --- | --- |
| Select a row | Click |
| Toggle a row in the selection | Ctrl-click (Command-click on macOS) |
| Select a range from the last clicked row | Shift-click |
| Add a range to the selection | Ctrl+Shift-click (Command+Shift-click on macOS) |
| Select all visible Music results or Playlist entries | Focus the list and press Ctrl+A (Command+A on macOS) |

Changing the Music filter clears selections that are no longer visible.
The status bar shows the number of selected tracks when multiple Music or
Playlist entries are selected, using the most recently operated list.
Right-click selected Music rows to add them to the playlist, or double-click a
track to add it. Right-click selected Playlist rows to remove them or clear the
playlist.

## Playlist and Playback

Double-click a playlist entry to play it, or use the playlist transport to
start the first item, pause or resume, stop, and play the previous or next item.
Navigation does not wrap at either end and starts the chosen item even when
playback was paused. Subsequent playlist entries play in order after a track
ends. The transport shows the current game and track, scrolling long titles,
as well as elapsed audio time and output volume.

In Music and Playlist, titles appear as `Game - Track` when game metadata is
present; otherwise the track title or filename is shown. The composer appears
below the title when available.

### Looping and Fade

With loop disabled, VGM and MDX songs play twice through their song loop, then
continue for a two-second software fade. VGM loop-count modifiers in the file
are ignored for this policy. Enable loop before playback to follow song loop
points indefinitely. Files without loop points end normally; MDX files with an
embedded fade or natural ending may end earlier.

### Track Info and Artwork

The Track Info panel below the playlist shows the current playlist track while
playing or paused; otherwise it shows the most recently selected Music or
Playlist track. If that selection is cleared, it falls back to the remaining
selection in the other list. It displays the track, game, composer and notes
when available.

Artwork is chosen from image files in the same directory: `cover`, `folder`,
then `front` are preferred (case-insensitive), followed by filename order.
Subdirectories and symbolic links are not searched. Supported formats are
PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, TGA, DDS, EXR, farbfeld, HDR, PNM and
QOI. An unreadable image is indicated in the panel; directory access errors
are reported as notifications.

### YM2608 Rhythm ROM

For VGM/VGZ playback using YM2608 emulation, NDDRV loads the optional
`ym2608_adpcm_rom.bin` from the track's directory, or from the process's current
working directory if it is not beside the track, for its built-in rhythm
instruments. The ROM is not bundled. If it is absent, playback continues without
it; an unreadable, empty, or oversized file is reported as an error.
NanoDrive-routed YM2608 instances do not use this file.

### Layout and Saved State

Drag the dividers beside Library and Playlist to resize them; their widths are
saved in the configuration. Before a playlist width is saved, Music and
Playlist share the remaining space equally. Side panel widths are retained when
the window is resized, as space permits. Library and playlist rows are rendered
on demand as you scroll.

Folders, playlist entries, panel widths, and routing settings are saved in
`library.json` in the platform configuration directory.

## Piano Roll

The piano icon between Library and Routing opens a full-window piano roll; its
indicator is lit while a playlist track is playing or paused. Select **Incoming**
to see up to six seconds of upcoming notes approach the 88-key keyboard, or
**Trail** to see the previous six seconds recede.
The playback controls below the track information operate the playlist.

A separate, bounded sequencer prepares incoming events without sending them to NanoDrive or
altering playback. Preparing or catching up may take time for complex tracks;
preview errors are shown in the view without stopping playback.

Colors identify chip voices using eight colors, and a playing key uses the
color of its note. Use the color buttons on the right to show or hide notes and
key highlights of each color in either
view; this affects the visualization only, not sound. Colors are shared by
multiple voices. When several visible voices share a key, the most recently
started note determines its color.

The view tracks YM2151, YM2203, YM2413, YM2608, YM2610B, YM2612,
YM3526, Y8950, YM3812, YMF262, and YMF278B state for both software and
NanoDrive routes. Pitch changes are shown as semitone steps. PCM, noise, and
other sounds without a tracked pitch are not displayed.

The view follows the estimated audio presentation time; device latency and clock drift can cause
differences from physical NanoDrive output. If the bounded event or note buffer
fills, playback continues and the piano roll reports that notes were skipped.

## NanoDrive Routing

Hardware routing is experimental and host-paced. Select **Emulation
(chipstream)** to use software emulation without a hardware profile.

### Profiles and Devices

The cable icon opens the routing page. At the top, create or select a named
routing profile. Below it, search for serial ports and select one to add a
NanoDrive device. Connect queries its model name and advertised chip
instances; duplicate model names get a numbered suffix. The baud rate is fixed
at 115,200. Unknown target IDs are shown explicitly.

Device settings are saved, but connections are not restored automatically.
Disconnect a device before removing it. Devices used by a profile cannot be
removed until the routes or profiles are removed.

Connection status reflects the initial connection and is not a continuous
physical-disconnection monitor. Devices are opened on a background worker,
so connection requests are processed sequentially without blocking the UI.
Playback exclusively leases the required connections and resets them before
returning them to the connection service.

### Connections

In the routing connections section below the devices, click a sequencer chip
instance, then click a highlighted matching device target or its chipstream
emulation target. Cable editing uses clicks, not dragging. Cables show each
connection; disconnected or unsupported targets and their cables are orange
and dashed without removing the saved route.
NanoDrive targets are to the right of chipstream, with hardware cables passing
behind the chipstream panel. Additional devices are stacked in that column;
scroll horizontally or vertically as needed.

The same profile selector is also available in the Playlist transport; profile
selection and editing are disabled during playback.

For a track using YM2151 and OKIM6258, one temporary option is to route YM2151
to NanoDrive and explicitly connect OKIM6258 to chipstream emulation. The two
outputs still need external mixing and their synchronization is best-effort.

### Timing and Volume

Hardware playback sends untimed register writes from separate device workers
against the system audio's estimated presentation time. OKIM6258 streams and
supported YM2612 timed DAC streams are prebuffered before the PCM stream begins
and started against the same estimated time. This is best-effort, not
sample-accurate: serial bandwidth, device latency, OS scheduling, and independent
clock drift can cause timing
differences. Physical output comes from each NanoDrive and must be mixed
externally.

The playback volume is a common master when every NanoDrive in the route
advertises output-volume control; it adjusts the software output
and each device's global output. If any routed device lacks that capability,
the panel shows `Software (ND fixed)` and the slider adjusts only software
audio; adjust NanoDrive levels externally. While capabilities are being
checked, the panel shows `Checking ND`. The same slider value carries across
tracks and is applied before supported hardware playback starts. Each capable
device receives an initial volume command before playback; if the route has no
hardware master control, it is initialized to zero attenuation. Device
controls are quantized and best-effort, and external mixer settings are not
changed.

For VGM/VGZ and MDX with loop disabled, devices advertising output-volume control
also fade their
global output over the two-second fade interval. Volume updates use immediate
integer-decibel commands, so their timing and smoothness are best-effort.
Devices without that capability do not fade. The master gain and fade multiply
together. After a hardware volume change, the device is restored to zero
attenuation when reset for reuse.

Pause is disabled when a hardware route is active. Stop and track changes cancel
the workers and reset the devices before reuse.

### Hardware Limitations

OKIM6258 clock and divider changes are supported at ADPCM byte boundaries.
NDSIF supports 4 MHz and 8 MHz OKIM6258 clocks; other clocks, incompatible
device capabilities, buffer limits, and device errors stop playback with a
notification. Explicit hardware routes never silently fall back to emulation.

## File Requirements and Limitations

MDX files that name a PDX sample bank require a matching PDX beside the MDX;
matching is case-insensitive, and ambiguous or missing matches are reported.
The scanner and player limit each input and decompressed VGZ to 64 MiB.

The current implementation uses the default audio device. Some VGM chips or
assets may not be supported by chipstream; playback errors are shown in
notifications.

## License

BSD-3-Clause License
