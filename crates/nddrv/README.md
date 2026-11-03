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
When no directories are registered, Library shows a hint above **Add directory**.

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
Music rows are dimmed when a declared chip instance has neither software
emulation nor a route to a connected NanoDrive. Hover over the row to see the
unsupported chips. Playlist entries show the same information as a red warning
after the title. These indicators update when devices or routes change and do
not prevent playback. They compare chip types and instance counts from the
file header; they do not check chip variants, clock compatibility, shared
hardware conflicts, or other playback requirements.

### Selection

Music and Playlist rows share these selection controls:

| Action | Control |
| --- | --- |
| Select a row | Click |
| Toggle a row in the selection | Ctrl-click (Command-click on macOS) |
| Select a range from the last clicked row | Shift-click |
| Add a range to the selection | Ctrl+Shift-click (Command+Shift-click on macOS) |
| Select all visible Music results or Playlist entries | Focus the list and press Ctrl+A (Command+A on macOS) |
| Move the selection | Focus the list and press Up or Down |
| Add selected Music tracks to Playlist | Focus Music and press Enter |
| Remove selected Playlist entries | Focus Playlist and press Delete |

Changing the Music filter clears selections that are no longer visible.
The status bar shows the number of selected tracks when multiple Music or
Playlist entries are selected, using the most recently operated list.
Right-click selected Music rows to add them to the playlist, or double-click a
track to add it. Right-click selected Playlist rows to remove them or clear the
playlist.
Keyboard navigation selects one row; after deleting Playlist entries, the next
remaining row is selected, or the previous row if the end was reached.

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
The artwork is displayed at up to 192 x 192 logical pixels, scaling down in
narrow playlist panels. Large images are resampled for the display's pixel
density before rendering; animated artwork retains its frames and timing.
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

Folders, playlist entries, panel widths, and NanoDrive device settings are saved in
`library.json` in the platform configuration directory.

## Piano Roll

The piano icon between Library and Routing opens a full-window piano roll; its
indicator is lit while a playlist track is playing or paused. Select **Incoming**
at the lower right to see up to six seconds of upcoming notes approach the
88-key keyboard, or **Trail** to see the previous six seconds recede.
The playback controls below the track information operate the playlist.

When playback is stopped and the playlist is not empty, Piano Roll previews
the first track, which is the track the Play button will start. Incoming shows
the first six seconds of notes; Trail remains empty because there is no
playback history. Chip Gain and Channels can be adjusted before playback:
FM mute and solo selections carry over when that track starts. Changing the
first playlist entry prepares a new preview and resets its FM selections. The
preview remains visible while playback prepares its own Piano Roll data.

The upper-right **Chip Gain** panel adjusts software-emulated chips from 0% to
100%. YM2203, YM2608, and YM2610(B) also have separate FM/ADPCM and SSG
controls; those multiply the chip control. Multiple instances of a chip share
the same settings. Values persist while NDDRV is running, including across
tracks, and return to 100% on application restart. Changes take effect after
the existing audio buffer. NanoDrive outputs are marked and are not
affected by these controls, including when a chip has both hardware and
software instances.

A separate, bounded soundlog stream prepares incoming events without rendering
audio, sending writes to NanoDrive, or altering playback. Preparing or catching
up may take time for complex tracks; preview errors are shown in the view
without stopping playback.

The **Channels** panel below Chip Gain identifies each tracked voice by chip, instance and
channel, with a distinct color. Select a color square to show or hide that
voice's notes and key highlights; this does not change the sound. When several
visible voices share a key, the most recently started note determines its color.

FM channels also have mute (speaker) and solo (headphones) controls, for both
ymfm software playback and NanoDrive routes. Multiple FM channels can be soloed
across chips and instances; while any solo is active, only soloed FM channels
can sound, regardless of their individual mute settings. Turning off all solos
restores those mute settings. Muting suppresses subsequent FM key-on writes;
unmuting allows subsequent key-ons. An already sounding note is not immediately muted
or restored, and release envelopes and queued audio or hardware writes can
delay the audible change. The piano roll continues to show the original notes.
Mute and solo settings reset when playback switches tracks, except for the
selections made in the stopped preview of the track being started. OPL3/OPL4 channels in
4-operator mode are controlled as a pair. In OPLL/OPL rhythm mode, the
percussion channels are marked **Rhythm** and cannot be controlled by these FM
controls. Their solo selections are suspended while rhythm mode is active and
resume if it ends. SSG, DAC, PCM, ADPCM, and rhythm sounds are not muted,
even while FM solo is active.
The existing pitch tracker may still show notes from the secondary register
channel of a 4-operator pair; the mute control applies to the pair as a whole.

The view tracks the three SN76489 tone channels and YM2151, YM2203, YM2413,
YM2608, YM2610B, YM2612, YM3526, Y8950, YM3812, YMF262, and YMF278B state
for both software and NanoDrive routes. SN76489 tone activity is inferred from
attenuation; its channels have visibility controls but no mute or solo. Pitch
changes are shown as semitone steps. PCM, noise, and other sounds without a
tracked pitch are not displayed.

The view follows the estimated audio presentation time; device latency and clock drift can cause
differences from physical NanoDrive output. Incoming notes use command times and
may differ slightly from playback where chip writes are paced. If the bounded
event or note buffer fills, playback continues and the piano roll reports that
notes were skipped.

## NanoDrive Routing

Hardware routing is experimental and host-paced. The cable icon opens the
routing page. Select a serial port in the NanoDrive box and click **Connect**;
use the refresh icon beside the port selector to search again. Connect queries the model name and
advertised chip instances; duplicate model names get a numbered suffix. The
baud rate is fixed at 115,200. Unknown target IDs are shown explicitly.

SN76489 can be routed to hardware when the track does not use a non-default
Game Gear stereo mask. The header's stereo-capable flag alone does not prevent
hardware playback. Tracks requiring per-channel stereo must use emulation.

Connecting the last box adds a new empty box below it. The last extra box can
be removed with its × button after disconnecting it. Use **Add NanoDrive** to
restore an extra box if it was removed. Device and port settings are saved,
but devices are not reconnected automatically. Existing device settings are
retained when upgrading; old routing profiles are no longer used.

Connection status reflects the initial connection and is not a continuous
physical-disconnection monitor. Devices are opened on a background worker,
so connection requests are processed sequentially without blocking the UI.
Playback exclusively leases the required connections and resets them before
returning them to the connection service.

### Connections

Connected NanoDrive targets are automatically assigned to matching sequencer
chip instances, in box order. A target is used at most once; sources without a
matching target use chipstream emulation where available. To change a connection, click a
sequencer chip instance and then a highlighted matching device target or its
chipstream emulation target. Cable editing uses clicks, not dragging. Manual
choices are kept while connecting additional devices, and are reset to
automatic routing after a device is disconnected or the application restarts.
NanoDrive boxes are stacked to the right of chipstream; scroll horizontally
or vertically as needed. The Playlist shows the names of connected NanoDrives.
Device connection and routing edits are disabled during playback.

NanoDrive 7 exposes AY-3-8910 through the SSG of each YM2203. AY playback is
hardware-only and is an approximation of AY-3-8910 output; other AY/YM variants,
discrete or raw output, and per-channel stereo masks are not supported. Without
an AY route, AY writes are ignored as before. If only some instances of a
dual-AY track are routed, playback fails rather than silently dropping one.
The Piano Roll does not currently visualize AY.

NanoDrive 7 shares each AY instance with its corresponding YM2203, and all
AY/YM2203 routes share one clock. Its YM3526, YM3812 and YMF262 routes also
share a physical chip. If the track's active routes conflict, playback is
stopped before the hardware is configured and the reason is shown. Routes on
different devices do not conflict. Equivalent physical clock values are
accepted after the device's documented clock normalization.

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
