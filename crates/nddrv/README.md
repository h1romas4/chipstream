# nddrv

`nddrv` is a GPUI Kit desktop music library and player for VGM, VGZ, and MDX.
It uses chipstream's software emulation and the system's default audio output,
and supports experimental host-paced routing to NanoDrive hardware.
The interface uses GPUI Kit's dark theme and a custom-styled `TitleBar`.
Windows and Linux client-side decorations provide themed window controls;
macOS uses its native traffic-light controls. On Linux, nddrv requests
client-side decorations, but falls back to the window manager's title bar
when the display server or window manager does not provide them.

## Run

Install the platform prerequisites in the [GPUI Kit installation guide](https://gpui-kit.com/docs/installation/)
and, on Linux, the development libraries for ALSA. Then run:

```sh
cargo run -p nddrv
```

In the library, use **Add directory** to register folders. Files are scanned
recursively without following directory symlinks. **Rescan** refreshes metadata.
Expand folders in the directory tree and select a folder to show tracks in it
and its descendants. Select **All tracks** to clear the directory filter.
Combine this with the title, artist, album, notes, or path filter. Click a Music row to select it,
or Ctrl-click (Command-click on macOS) to toggle multiple selections.
Shift-click selects a range from the last clicked row; Ctrl+Shift-click
(Command+Shift-click on macOS) adds the range to the selection. Right-click
to add the selected tracks, or double-click a track to add it. Changing the
filter clears selections that are no longer visible.
Click the Music list and press Ctrl+A (Command+A on macOS) to select all
tracks in the current filtered results.
Type a chip name in the Music search field and choose a suggestion with the
mouse or arrow keys and Enter to add an inline chip tag. Remove a tag with
Backspace or Delete. Multiple chip tags require all of those chips, combined
with the ordinary text and directory filters. Suggestions use chips found in
the scanned library.
Scanning progress and completion are shown as notifications at the bottom right
of the window. Errors are shown in persistent notifications; scan and playlist
metadata errors include scrollable details.
Playlist rows use the same selection controls, including Ctrl+A (Command+A on
macOS) to select all entries; right-click to remove selected
entries or clear the playlist. Like Music rows, they show the composer below
the title. Double-click a playlist entry to play it, or use
the playlist transport to start the first item, pause or resume,
stop, and play the previous or next item. Navigation does not wrap at either
end and starts the chosen item even when playback was paused. The transport
shows the current game and track, scrolling long titles, as well as elapsed
audio time and output volume.
The Track Info panel below the playlist shows the current playlist track while
playing or paused; otherwise it shows the most recently selected Music or
Playlist track. If that selection is cleared, it falls back to the remaining
selection in the other list. It displays
the track, game, composer and notes when available. Artwork is chosen from
image files in the same directory: `cover`, `folder`, then `front` are preferred
(case-insensitive), followed by filename order. Subdirectories and symbolic
links are not searched. Supported formats are PNG, JPEG, GIF, WebP, BMP, TIFF,
ICO, TGA, DDS, EXR, farbfeld, HDR, PNM and QOI. An unreadable image is indicated
in the panel; directory access errors are reported as notifications.
With loop disabled, VGM and MDX songs play twice through their song loop, then
continue for a two-second software fade. VGM loop-count modifiers in the file
are ignored for this policy. Enable loop before playback to follow song loop
points indefinitely. Files without loop points end normally; MDX files with an
embedded fade or natural ending may end earlier. Subsequent playlist entries
play in order after a track ends.
For VGM/VGZ playback using YM2608 emulation, NDDRV loads the optional
`ym2608_adpcm_rom.bin` from the track's directory, or from the process's current
working directory if it is not beside the track, for its built-in rhythm
instruments. The ROM is not bundled. If it is absent, playback
continues without it; an unreadable, empty, or oversized file is reported as an
error. NanoDrive-routed YM2608 instances do not use this file.
Music results are ordered by directory and then by filename, rather than
track metadata. In Music and Playlist, when game metadata is present, titles are shown as
`Game - Track`; otherwise the track title or filename is shown. Drag the
dividers beside Library and Playlist to resize them;
their widths are saved in the configuration. Before a playlist width is saved,
Music and Playlist share the remaining space equally.
The side panel widths are retained when the window is resized, as space permits.
Library and playlist rows are rendered on demand as you scroll.
The library shows chip tags from VGM/VGZ header clocks. MDX tags show YM2151
and, when the MDX names a PDX sample bank, OKIM6258. These tags describe
declared or inferred hardware, not whether a chip is supported for playback.
Multiple instances are shown as, for example, `YM2203x2`; searching uses the
base chip name. In both the list and the search input, chips implemented by the
ymfm backend use the same teal color as the volume control, while other chip
tags use purple.
Hover over the tags to see the full chip list when it does not fit.
Folders and playlist entries are saved in the platform configuration directory.

The piano icon between Library and Routing opens a full-window piano roll; its
indicator is lit while a playlist track is playing or paused. Select **Incoming**
to see up to six seconds of upcoming notes approach the 88-key keyboard, or
**Trail** to see the previous six seconds recede. A separate, bounded
sequencer prepares incoming events without sending them to NanoDrive or
altering playback. Preparing or catching up may take time for complex tracks;
preview errors are shown in the view without stopping playback. Colors identify
chip voices using eight colors, and a playing key uses the color of its note.
Use the color buttons
on the right to show or hide notes and key highlights of each color in either
view; this affects the visualization only, not sound. Colors are shared by
multiple voices. When several visible voices share a key, the most recently
started note determines its color. The view tracks
YM2151, YM2203, YM2413, YM2608, YM2610B, YM2612,
YM3526, Y8950, YM3812, YMF262, and YMF278B state for both software and
NanoDrive routes. Pitch changes are shown as semitone steps. PCM, noise, and
other sounds without a tracked pitch are not displayed. The view follows the
estimated audio presentation time; device latency and clock drift can cause
differences from physical NanoDrive output. If the bounded event or note buffer
fills, playback continues and the piano roll reports that notes were skipped.

The cable icon opens the routing page. At the top, create or select a named
routing profile. Below it, search for serial ports and select one to add a
NanoDrive device. Connect queries its model name and advertised chip
instances; duplicate model names get a numbered suffix. The baud rate is fixed
at 115,200. Device settings are saved, but connections are not restored
automatically. Unknown
target IDs are shown explicitly. Disconnect a device before removing it.
Connection status reflects the initial connection and is not a continuous
physical-disconnection monitor. Devices are opened on a background worker,
so connection requests are processed sequentially without blocking the UI.
Playback exclusively leases the required connections and resets them before
returning them to the connection service.

In the routing connections section below the devices, click a sequencer chip
instance, then click a highlighted matching device target or its chipstream
emulation target. Cables show each connection; disconnected or unsupported
targets and their cables are orange and dashed without removing the saved route.
NanoDrive targets are to the right of chipstream, with hardware cables passing
behind the chipstream panel. Additional devices are stacked in that column;
scroll horizontally or vertically as needed.
The same profile selector is also available in the Playlist transport; profile
selection and editing are disabled during playback. Devices used by a profile
cannot be removed until the routes or profiles are removed.
Hardware playback sends untimed register writes from separate device workers
against the system audio's estimated presentation time. OKIM6258 streams and
supported YM2612 timed DAC streams are prebuffered before the PCM stream begins
and started against the same estimated time. This is best-effort, not
sample-accurate: serial bandwidth, device
latency, OS scheduling, and independent clock drift can cause timing
differences. Physical output comes from each NanoDrive and must be mixed
externally. The playback volume is a common master when every NanoDrive in
the route advertises output-volume control; it adjusts the software output
and each device's global output. If any routed device lacks that capability,
the panel shows `Software (ND fixed)` and the slider adjusts only software
audio; adjust NanoDrive levels externally. While capabilities are being
checked, the panel shows `Checking ND`. The same slider value carries across
tracks and is applied before supported hardware playback starts. Each capable
device receives an initial volume command before playback; if the route has no
hardware master control, it is initialized to zero attenuation. Device
controls are quantized and best-effort, and external mixer settings are not
changed. For VGM/VGZ and MDX with
loop disabled, devices advertising output-volume control also fade their
global output over the two-second fade interval. Volume updates use immediate
integer-decibel commands, so their timing and smoothness are best-effort.
Devices without that capability do not fade. The master gain and fade multiply
together. After a hardware volume
change, the device is restored to zero attenuation when reset for reuse. Pause is disabled
when a hardware route is active. Stop and track changes cancel the workers and
reset the devices before reuse.

OKIM6258 clock and divider changes are supported at ADPCM byte boundaries.
NDSIF supports 4 MHz and 8 MHz OKIM6258 clocks; other clocks, incompatible
device capabilities, buffer limits, and device errors stop playback with a
notification. Explicit hardware routes never silently fall back to emulation.
Cable editing uses clicks, not dragging.

For a track using YM2151 and OKIM6258, one temporary option is to route YM2151
to NanoDrive and explicitly connect OKIM6258 to chipstream emulation. The two
outputs still need external mixing and their synchronization is best-effort.

MDX files that name a PDX sample bank require a matching PDX beside the MDX;
matching is case-insensitive, and ambiguous or missing matches are reported.
The scanner and player limit each input and decompressed VGZ to 64 MiB.

The current implementation uses the default audio device. Some VGM chips or
assets may not be supported by chipstream; playback errors are shown in
notifications.
