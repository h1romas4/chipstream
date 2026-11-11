# nddrv

NDDRV is a desktop music library and player for VGM, VGZ, and MDX files. It
supports software emulation with chipstream and experimental NanoDrive hardware
routing.

## Installation

### Prebuilt binaries

Download an `nddrv-` release from [GitHub Releases](https://github.com/h1romas4/chipstream/releases).

| Platform | Download | Notes |
| --- | --- | --- |
| Linux x86-64 | Archive | Requires compatible ALSA, PipeWire, and GPUI platform libraries. |
| Windows x86-64 | Archive or standalone executable | Not signed; Windows may show a security warning. |
| macOS, Apple Silicon or Intel | App bundle | Not signed or notarized; macOS may show a security warning. |

### Build from source

Install Rust and the prerequisites in the
[GPUI Kit installation guide](https://gpui-kit.com/docs/installation/).
On Linux, also install the PipeWire and ALSA development packages and libclang
(for example, `libpipewire-0.3-dev`, `libasound2-dev`, and `libclang-dev` on
Debian/Ubuntu).

```sh
cargo install --git https://github.com/h1romas4/chipstream --bin nddrv --locked --force nddrv
```

## Quick Start

1. In Library, choose **Add directory** and select a folder containing music.
2. Select a folder or **All tracks** and filter the Music list.
3. Double-click a track, or select tracks and press Enter, to add them to Playlist.
4. Use the Playlist transport to play, pause, stop, or change tracks.
5. Use **Rescan** to refresh a registered folder after its contents change.

The window size and maximized state are restored on startup. When the primary
display's available area is known, the window is centered and fitted to it;
otherwise, placement is left to the window system.

## Library

### Search

| Search | Behavior |
| --- | --- |
| Text | Matches title, artist, album, notes, or path. |
| Chip tags | Multiple chip tags require all listed chips. |
| Format tags | `MDX` matches MDX files; `VGM` matches VGM and VGZ files. |
| `NanoDrive` tag | Matches chips advertised by a connected device; does not guarantee hardware routing. |
| Clear button | Removes all search text and tags. |

Search text and tags are saved on exit and restored on startup. An invalid saved
search is cleared with a notification. A restored NanoDrive tag remains active
while disconnected and matches no tracks until a NanoDrive is connected.

### Selection and actions

| Action | Control |
| --- | --- |
| Select a row | Click |
| Add or remove a row from the selection | Ctrl-click (Command-click on macOS) |
| Select a range | Shift-click |
| Select all visible rows | Focus Music or Playlist and press Ctrl+A (Command+A on macOS) |
| Add selected Music tracks to Playlist | Focus Music and press Enter |
| Remove selected Playlist entries | Focus Playlist and press Delete |
| Music context menu | Right-click a row to add selected tracks or copy its path. |
| Playlist context menu | Right-click selected rows to remove them or clear the playlist. |

Track Info shows available metadata and loads artwork from the track's folder.

## Settings

Settings are organized into General, Playback, Recording, and VGM pages.

### General

| Setting | Behavior |
| --- | --- |
| VGM metadata language | Choose English (default) or original-language VGM/VGZ metadata. Missing fields fall back to the other language; search matches both. MDX metadata is unaffected. |
| YM2608 rhythm ROM | Select an optional ROM file for software YM2608 playback. See [YM2608 rhythm ROM](#ym2608-rhythm-rom). |

### Playback

| Setting | Behavior |
| --- | --- |
| Play count | 1–99 plays; default: 2. |
| Fade out (seconds) | 0–600 seconds; default: 2. |
| Start delay (seconds) | 0–600 seconds; default: 0. |
| Emulation audio device | Select the software-emulation output, or **System default**. NanoDrive audio is separate. |
| Emulation sample rate | Select a supported 44.1, 48, 96, or 192 kHz rate, or **Device default**. Changing the output device resets this to **Device default**. |
| Show all audio devices | Linux only. See [Linux audio devices](#linux-audio-devices). |

Settings are saved and apply to the next track. Playlist tracks play in order;
previous and next do not wrap. With loop off, playback uses the configured play
count and fade. Enable loop to repeat indefinitely.

### Recording

Recording is experimental.

| Setting | Behavior |
| --- | --- |
| Enable recording (experimental) | Enables recording for the playback controls. |
| Input audio device | Select the source to record, or **System default**. |
| Input sample rate | Select an input rate or **Device default**. |
| WAV output folder | Select or clear the folder using the icons beside the path. A folder is required for recording. |
| Show all audio devices | Linux only. See [Linux audio devices](#linux-audio-devices). |

Use the record button in the playback controls after configuring these settings.
The selected input must receive the audio to be recorded; NanoDrive and
software output may need external mixing or system loopback.

| Recording behavior | Details |
| --- | --- |
| Output | One 16-bit WAV per playlist track, named with the source filename and local timestamp. Existing files are preserved with a numeric suffix. |
| Timing | Recording starts before playback and includes the start delay. Pausing playback does not pause input recording. |
| Stopping or changing tracks | Saves the current file. |
| Notifications | Each saved file is reported. Recoverable input interruptions may cause gaps or timing shifts and are reported as warnings. Other recording errors stop playback and are reported separately. |

### VGM

| Setting | Behavior |
| --- | --- |
| Assign YM2149 to AY8910 | Off by default. Uses software YM2149 to approximate AY8910-family VGM audio; changes apply to the next track. |

NanoDrive routes take priority per instance; unrouted instances use YM2149.
AY8910 and YM2149 sound different. AY8930, AY8914, raw/discrete output, and
reserved AY flags are not supported by this emulation. YM2149 clock division
and per-channel stereo masks are supported.

For a VGM with two AY instances and only one hardware route, enable emulation or
route both instances. A partial hardware route without emulation is not
playable. Music and Playlist support indicators reflect the setting and the
track's AY variant and output flags.

### Linux audio devices

| Situation | Behavior |
| --- | --- |
| PipeWire available | Playback and recording use PipeWire. Lists contain output and input devices, not individual application streams. Recording also offers **System output (monitor)**. |
| PipeWire unavailable | Playback and recording fall back to ALSA. The default list shows common audio servers and representative hardware devices. |
| Show all audio devices | Includes ALSA devices and plugins. The display choice lasts until Settings is closed; existing selections remain available when filtered. |
| Direct ALSA device is busy | Select **System default** or a PipeWire device and choose the desired device in system sound settings. PipeWire or another application may already be using the hardware. |

## Piano Roll

Open the full-window Piano Roll with the piano icon.

| Control or display | Behavior |
| --- | --- |
| Incoming / Trail | Show upcoming / recent notes. |
| Channels | Select which tracked voices are visible. |
| Chip Gain | Adjusts software-emulated output. |
| FM mute / solo | Controls FM channels; does not affect NanoDrive output. |

The Piano Roll shows selected pitched voices, not every sound such as PCM and
noise. AY8910 and YM2149 tone channels appear in both modes, including when
routed to NanoDrive. Without an AY hardware route or enabled YM2149 emulation
they are visual-only. Envelope-enabled tones are shown as active without
simulating envelope amplitude. Chip Gain does not affect NanoDrive output.

## NanoDrive Routing

NanoDrive routing is experimental.

1. Open the routing page with the cable icon, select a serial port, and connect.
2. Matching chip instances are routed automatically where possible.
3. To change a route, select a sequencer chip and then a highlighted device or
   software-emulation target.

| Topic | Behavior |
| --- | --- |
| Hardware audio | Comes from the device and must be mixed externally with software output. |
| Timing | Best-effort; playback may not be sample-accurate. |
| Pause | Unavailable while a hardware route is active. |
| Invalid routes | Unsupported or incompatible explicit routes stop playback rather than silently switching to emulation. |
| YM2149 | Tracks appear as AY8910 and can use a NanoDrive 7 AY route. The device uses YM2203 SSG rather than a physical YM2149, so the sound may differ. |
| AY and YM2203 | Routes share physical chips and clocks; incompatible combinations are rejected. YM2149 clock-divider, raw, and discrete-output flags are not supported. |

Internally, each sequencer source resolves to a hardware, software, or silent
target and an adapter. Hardware routes become NDSIF routes; software assignments
use the same plan for playback, preview, and support indicators. This separation
does not yet add conversions between different physical chip types.

## Supported Files and Notes

| Format | Notes |
| --- | --- |
| VGM / VGZ | VGZ files are decompressed for playback. Input files and decompressed VGZ data are limited to 64 MiB. |
| MDX | A matching PDX sample bank is required when the MDX names one. |

Chip support depends on the file and available software emulation or connected
hardware. Playback errors are shown in the application.

### YM2608 rhythm ROM

The optional rhythm ROM is not included with the application and is used only
for software YM2608 instances.

1. A file selected in **Settings > General > YM2608 rhythm ROM** takes priority.
   Changes apply to the next track.
2. Clear the selection to search for `ym2608_adpcm_rom.bin` beside the track,
   then in the working directory.
3. A selected missing, unreadable, or empty file is reported as an error rather
   than falling back.

## License

BSD-3-Clause License
