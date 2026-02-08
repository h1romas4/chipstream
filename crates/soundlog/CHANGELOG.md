# Changelog

## [dev] v0.3.0

- **Breaking change**: `Ay8910StereoMask` is now a regular struct with fields (`chip_instance`, `is_ym2203`, `left_ch1`, `right_ch1`, etc.) instead of a tuple struct wrapping `u8`. Access fields directly via `mask.chip_instance`, `mask.left_ch1`, etc. `Ay8910StereoMaskDetail` has been removed (merged into `Ay8910StereoMask`). The `parse_ay8910_stereo_mask()` function has been removed as it's no longer needed—use `Ay8910StereoMask::from_mask()` to parse from a byte.
- Fix: VGM header parsing updated to follow the VGM specification. Main-header fields are only read when defined by the file's declared VGM version; `data_offset` is used only for VGM >= 1.50; extra-header (v1.70+) offsets are interpreted relative to the extra-header start. Serialization was adjusted for round-trip compatibility and unit tests were added to cover these cases.

## v0.2.0

- Add `VgmDocument::sourcemap` to produce absolute file offsets for each command, so callers can map commands back to their byte ranges in the serialized VGM (respects header/data layout, extra-header placement and GD3 offsets).
- Fix encoding/decoding of `Instance::Secondary` opcodes: implement symmetric writer/parser mapping (PSG special-case 0x50↔0x30/0x3F, YM-family 0x5n↔0xAn, and other chips using the high-bit (0x80) to indicate the second instance), improving round-trip and historical opcode compatibility.
- Add `vgm::detail` module with `parse_data_block()` function to parse VGM data blocks (command 0x67) into detailed types including uncompressed/compressed streams, ROM/RAM dumps, decompression tables, and RAM writes with chip-specific type information.
- Add `BitPackingCompression::decompress()` and `DpcmCompression::decompress()` methods that decompress data in-place (modifying `self.data`) instead of returning a new `Vec<u8>`, improving memory efficiency.
- Add `Ay8910StereoMaskDetail` structure and `parse_ay8910_stereo_mask()` function to parse AY8910 stereo mask bytes into detailed channel/speaker configuration with individual boolean fields for left/right channels 1-3, chip instance, and YM2203/AY8910 selection.

## v0.1.0 — Initial release

- First public release of `soundlog`.
- Provides a VGM builder and parser, GD3 metadata handling, extra-header support, and a typed `vgm::command` API.
- See README and crate documentation for usage examples and migration notes.
