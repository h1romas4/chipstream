# Changelog

## [dev] v0.2.0

- Add `VgmDocument::sourcemap` to produce absolute file offsets for each command, so callers can map commands back to their byte ranges in the serialized VGM (respects header/data layout, extra-header placement and GD3 offsets).
- Fix encoding/decoding of `Instance::Secondary` opcodes: implement symmetric writer/parser mapping (PSG special-case 0x50↔0x30/0x3F, YM-family 0x5n↔0xAn, and other chips using the high-bit (0x80) to indicate the second instance), improving round-trip and historical opcode compatibility.
- Add `vgm::detail` module with `parse_data_block()` function to parse VGM data blocks (command 0x67) into detailed types including uncompressed/compressed streams, ROM/RAM dumps, decompression tables, and RAM writes with chip-specific type information.
- Add `BitPackingCompression::decompress()` and `DpcmCompression::decompress()` methods that decompress data in-place (modifying `self.data`) instead of returning a new `Vec<u8>`, improving memory efficiency.
- Add `Ay8910StereoMaskDetail` structure and `parse_ay8910_stereo_mask()` function to parse AY8910 stereo mask bytes into detailed channel/speaker configuration with individual boolean fields for left/right channels 1-3, chip instance, and YM2203/AY8910 selection.

## v0.1.0 — Initial release

- First public release of `soundlog`.
- Provides a VGM builder and parser, GD3 metadata handling, extra-header support, and a typed `vgm::command` API.
- See README and crate documentation for usage examples and migration notes.
