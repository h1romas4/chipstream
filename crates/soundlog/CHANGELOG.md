# Changelog

## v0.3.0

- **New**: Add `VgmStream`, an iterator-based, low-memory VGM stream processor that accepts raw VGM bytes (`push_chunk`) or a pre-parsed `VgmDocument` (`from_document`) and emits `VgmCommand` values incrementally. `VgmStream` handles DAC stream control, DataBlock storage/decompression, and automatically expands stream-generated chip writes during `Wait` periods.
  - `VgmStream` includes configurable memory limits for accumulated data blocks (default 32 MiB). Use `set_max_data_block_size()` to adjust the limit, and `max_data_block_size()` / `total_data_block_size()` to query current limits and usage. When the limit is exceeded, `ParseError::DataBlockSizeExceeded` is returned.
  - **Security**: `push_chunk()` now enforces a configurable maximum buffer size limit (default 64 MiB) to prevent unbounded memory growth from untrusted input. Returns `Result<(), ParseError>` instead of being infallible. Use `set_max_buffer_size()` / `max_buffer_size()` to configure and query the limit.
  - **Security**: Added comprehensive documentation warnings about infinite loop behavior when `loop_count` is not set. Users processing untrusted VGM files should always call `set_loop_count(Some(n))` to prevent infinite loops.
  - Add `set_loop_count()` / `current_loop_count()` to control loop iteration limits.
  - Add `set_fadeout_samples()` / `fadeout_samples()` to configure fadeout grace period after loop end.
  - Add `reset()` to clear parser state and buffers.
  - Add `get_uncompressed_stream()` and `get_decompression_table()` to inspect stored stream data.
- **New**: Add `VgmHeader::from_bytes(&[u8])` helper function to parse VGM headers from byte slices with detailed error reporting (`HeaderTooShort`, `InvalidIdent`, `OffsetOutOfRange`). `TryFrom<&[u8]>` for `VgmHeader` is implemented using this helper.
- **New**: Add `DacStreamChipType` enum to provide type-safe chip type values for DAC stream control commands. Includes conversion methods `from_u8()` / `to_u8()` / `to_u8_with_instance()` and `TryFrom<u8>` / `Into<u8>` implementations.
- **Breaking change**: `VgmStream::push_chunk()` now returns `Result<(), ParseError>` instead of being infallible, to support buffer size limit enforcement and proper error handling.
- **Breaking change**: `Ay8910StereoMask` is now a regular struct with fields (`chip_instance`, `is_ym2203`, `left_ch1`, `right_ch1`, etc.) instead of a tuple struct wrapping `u8`. Access fields directly via `mask.chip_instance`, `mask.left_ch1`, etc. `Ay8910StereoMaskDetail` has been removed (merged into `Ay8910StereoMask`). The `parse_ay8910_stereo_mask()` function has been removed as it's no longer needed—use `Ay8910StereoMask::from_mask()` to parse from a byte.
- **Fix**: VGM header parsing updated to follow the VGM specification. Main-header fields are only read when defined by the file's declared VGM version; `data_offset` is used only for VGM >= 1.50; extra-header (v1.70+) offsets are interpreted relative to the extra-header start. Serialization was adjusted for round-trip compatibility and unit tests were added to cover these cases.

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
