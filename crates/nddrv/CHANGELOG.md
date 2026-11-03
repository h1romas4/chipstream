# Changelog

## Unreleased

- Automatically route connected NanoDrive chips and replace routing profiles
  with connection boxes in the routing view.
- Show connected NanoDrive names in the Playlist transport.
- Refresh serial ports from the icon beside each port selector.
- Recognize NanoDrive 7 AY8910 and chip aliases, route AY to hardware, and
  reject conflicting shared chips or clocks before playback.
- Account for connected hardware routes in Music and Playlist support indicators.

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
