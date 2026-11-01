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
entries or clear the playlist. Double-click a playlist entry to play it, or use
the playlist transport to start the first item, pause or resume,
stop, and play the previous or next item. Navigation does not wrap at either
end and starts the chosen item even when playback was paused. The transport
shows the current game and track, scrolling long titles, as well as elapsed
audio time and output volume.
Enable loop before playback to follow VGM or MDX song loop points indefinitely;
files without loop points still end normally. Subsequent playlist entries play in
order after a track ends.
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
externally; the software volume control does not adjust it. Pause is disabled
when a hardware route is active. Stop and track changes cancel the workers and
reset the devices before reuse.

OKIM6258 dynamic clock or divider changes are not yet supported in this
synchronized mode. Device buffer limits, unavailable devices, unsupported clocks, and device errors stop
playback with a notification; explicit hardware routes never silently fall back
to emulation. Cable editing uses clicks, not dragging.

For a track using YM2151 and OKIM6258, one temporary option is to route YM2151
to NanoDrive and explicitly connect OKIM6258 to chipstream emulation. The two
outputs still need external mixing and their synchronization is best-effort.

MDX files that name a PDX sample bank require a matching PDX beside the MDX;
matching is case-insensitive, and ambiguous or missing matches are reported.
The scanner and player limit each input and decompressed VGZ to 64 MiB.

The current implementation uses the default audio device. Some VGM chips or
assets may not be supported by chipstream; playback errors are shown in
notifications.
