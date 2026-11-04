# Changelog

## v0.7.0 (unreleased)

- Add a General setting to choose English or original VGM metadata.
- Add Playback settings for play count and fade-out duration.
- Add Playback settings to select the emulated audio output device and sample rate.
- Update gpui-kit to 0.9.0 and refresh other dependencies.
- Refine Routing labels and NanoDrive boxes, and update Playlist routing tags.
- Fix volume LFO reset handling during MDX playback when an LFO delay is configured.
- Prevent a console window from opening when launching the Windows MSVC build.
- Stop writing application and NanoDrive cleanup errors to standard error.

## v0.6.0

- Normalize NanoDrive 6/6.1 VGM startup writes before starting the timed DAC,
  reducing event FIFO pressure for dense initial register dumps.
- Preserve DAC buffering when dense FM/PSG writes exceed the timed-event FIFO,
  with ordered host-paced fallback and asynchronous DAC status polling.
- Restore normal STATUS polling after a successful retry so a lost response
  cannot throttle DAC transfers until the buffer underruns.

## v0.5.0

- Add a copy button to notifications, including full error details.
- Match the update notification button's blue to the reference color.
- Fix NanoDrive 8 startup prebuffer gaps caused by ADPCM render-ahead.
- Fix NanoDrive 8 ADPCM source playback resets while keeping the device decoder continuous.
- Add a Music context menu action to copy a track's full path.
- Fix timing discontinuities when VGM DAC stream frequency changes during playback.

## v0.4.0

- Publish an icon-bearing portable Windows executable and macOS DMG bundles,
  alongside the existing archives.
- Prepare transparent multi-size Windows and macOS application icons.
- Ignore OKIM6258 writes to `0x10`–`0x17` emitted by VGMMAME.
- Increase the Playlist playback title and elapsed time text sizes.
- Show a concise playback status without the file path.
- Show chip tags in Track Info.
- Simplify the Piano Roll Chip Gain panel and spell out NanoDrive in chip labels.
- Show an indeterminate progress meter in library scan notifications.
- Update scan progress in place instead of repeatedly showing the notification.
- Widen NanoDrive boxes to give serial port selectors more room.
- Add a NanoDrive Music search tag for chips advertised by connected devices.
- Number only connected NanoDrives, keep the last box available, and place the
  sequencer between emulation and NanoDrive routes.
- Accept fixed NanoDrive clocks within 10 ppm of the track's requested clock.
- Place NanoDrive connect and disconnect icons beside the port selector.
- Suppress scan error notifications on startup and directory removal; show them
  for Add directory and Rescan.
- Automatically route connected NanoDrive chips and replace routing profiles
  with connection boxes in the routing view.
- Show connected NanoDrive names in the Playlist transport.
- Refresh serial ports from the icon beside each port selector.
- Recognize NanoDrive 7 AY8910 and chip aliases, route AY to hardware, and
  reject conflicting shared chips or clocks before playback.
- Account for connected hardware routes in Music and Playlist support indicators.
- Group NanoDrive 6/6.1 YM2612 page 1 with its primary chip in Routing.
- Allow hardware SN76489 playback when the header enables Game Gear stereo
  but the track does not use non-default stereo masks.
- Adapt the timed DAC startup lead to the device's record and event capacities.
- Bound unanswered timed DAC traffic and collapse consecutive identical DAC
  writes at tick zero to avoid overloading NanoDrive during initialization.
- Pack DAC sample waits into data records to reduce streaming bandwidth.
- Retry delayed read-only timed DAC status queries to recover from incomplete
  NanoDrive serial responses without replaying writes.

## v0.3.0

- Show an Add directory hint when the library has no registered folders.
- Show SN76489 tone channels in the Piano Roll.
- Increase Track Info artwork to up to 192 x 192 pixels.
- Improve the quality of Track Info artwork downscaling.
- Preview the first playlist track and its Piano Roll controls before playback.
- Add per-instance FM channel mute/solo controls and distinct Piano Roll voice colors.
- Add per-chip and FM/SSG software gain controls (0-100%) to the Piano Roll.
- Rebalance emulated SN76489 and YM2203/YM2608/YM2610 SSG output against FM.
- Prepare Piano Roll previews with soundlog without rendering audio.
- Add keyboard navigation to Music and Playlist, with Enter to add tracks and Delete to remove entries.

## v0.2.0

- Filter Music by MDX or VGM format using search tags.
- Show format tags and right-align chip tags in Music.
- Improve Music and Playlist title hierarchy and show playlist positions.
- Show a warning on Playlist entries with chips not emulated by chipstream.
- Dim Music entries that declare chips unsupported by chipstream.
- Disable Add device until a serial port is selected.
- Show the number of selected Music or Playlist entries in the status bar.
- Add playlist playback controls to the Piano Roll view.
- Hide folders without tracks from the library tree.

## v0.1.0

- Initial release.
