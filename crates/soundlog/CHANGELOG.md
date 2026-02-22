# Changelog

## v0.6.0

- Enable runtime ChipState tracking for a variety of sound chips and add unit/integration tests to validate state and event behavior (e.g. frequency/key events, VGM parsing).
  - ay8910.rs
  - gamegear.rs
  - gb_dmg.rs
  - huc6280.rs
  - k051649.rs
  - nes_apu.rs
  - pokey.rs
  - saa1099.rs
  - sn76489.rs
  - vsu.rs
  - wonderswan.rs
  - y8950.rs
  - ym2151.rs
  - ym2203.rs
  - ym2413.rs
  - ym2608.rs
  - ym2610b.rs
  - ym2612.rs
  - ym3526.rs
  - ym3812.rs
  - ymf262.rs
  - ymf271.rs (NOT WORKING)
  - ymf278b.rs

## v0.5.0

- **Change (Builder)**: `VgmBuilder::finalize()` now ensures a finalized document contains an explicit end marker. If the assembled command stream does not already include an `EndOfData` command, `finalize()` appends one to the end of `commands`. This makes documents constructed via the builder safe to serialize and stream without requiring callers to remember to add an explicit terminator.
  - Note: `VgmDocument::to_bytes()` still intentionally does not append `EndOfData` automatically. The builder's behavior is a convenience for programmatic construction flows; manually-assembled `VgmDocument` values continue to require explicit termination by the caller.
  - Added unit tests covering finalize behavior: `test_finalize_appends_end_of_data_when_missing` and `test_finalize_does_not_duplicate_end_of_data`.
- **Docs**: README updated to document the builder's EndOfData append behavior and to clarify differences between `VgmBuilder::finalize()` and `VgmDocument::to_bytes()` semantics.
- **Breaking change (streaming)**: `VgmStream::push_chunk()` no longer parses or strips the VGM header from provided bytes and treats pushed input as command/data bytes only. When using `push_chunk()` you must supply only the serialized command/data region starting at the header's `data_offset` (i.e. begin at offset `0x34 + header.data_offset`). Supplying the full file including the header will not be interpreted as expected. If you already have a parsed `VgmDocument`, prefer `VgmStream::from_document()` which handles header/commands automatically. The README includes guidance about this usage.
- Internal change (chip state API): `ChipState::read_register` no longer has a default implementation that returned `None`. Implementors must now provide an explicit `read_register` implementation. This avoids accidental always-None semantics and makes register-read behavior explicit for each chip state implementation. Update any custom `ChipState` implementations accordingly.
- **Breaking change (floating-point types / MCU compatibility)**: Many internal and public APIs that previously used `f64` have been migrated to `f32` to improve performance and reduce binary size on MCU targets (for example, ESP32-S3) which typically lack a hardware `f64` FPU. This is a breaking change for some public types and signatures — for example, the clock field in `VgmHeader::ChipInstances` is now `f32`, and some `track_state` / callback signatures that accepted `f64` now use `f32`. Update downstream code that relied on `f64` parameters or tuple element types accordingly.
- **Fix (VgmCallbackStream)**: Treat Game Gear PSG writes as SN76489/PSG hardware by initializing the SN76489 state tracker for `GameGearPsg` instances. This ensures Game Gear PSG register writes are tracked and reported consistently with SN76489 behavior.
- **Fix (Pwm state tracking)**: Add `PwmState` to track PWM (Sega 32X) register writes. `PwmState` stores register values using `ArrayStorage<u32, 256>` and masks written values to the lower 24 bits to match the PWM spec. PWM writes are exposed via callbacks (`on_pwm_write`) and tracked by `VgmCallbackStream`.
- **Fix (extra header parsing/serialization)**: Make parsing of VGM `extra_header` more tolerant of real-world files that contain invalid or non-canonical offsets. The parser now safely falls back when `chip_clock_offset` or `chip_vol_offset` point inside the header area, reads the actual data where present, and normalizes `header_size` and the offset fields after parsing so subsequent serialization does not corrupt header bytes.
- **Docs (storage)**: Expanded rustdoc for storage implementations to clarify `SparseStorage<R,V>` memory behavior and capacity bounds. In particular, the effective maximum number of distinct entries is bounded by the register address type `R` (for example, `u8` → 256 entries, `u16` → 65,536 entries). `ArrayStorage` and `CompactStorage` continue to provide fixed bounds; `SparseStorage` uses a `HashMap` and thus its potential memory usage should be considered when using wider address types.
- **Tests**: Added unit tests for the new finalize behavior and ensured existing test suite passes. This release includes several additional test improvements around VGM parsing/serialization.

## v0.4.0

- **New**: Add `VgmCallbackStream`, a wrapper around `VgmStream` that provides callback support for chip register writes with automatic state tracking and event detection. Register callbacks using `on_write()` with chip-specific spec types (e.g., `Ym2612Spec`, `Sn76489Spec`) to receive notifications with optional `StateEvent` information for key-on/off, tone changes, and other notable events.
  - Add `track_state<S: ChipState>()` method to enable state tracking for specific chip instances with clock frequency information.
  - Add `track_chips()` method to automatically enable state tracking for all chips defined in `ChipInstances`.
  - Callbacks receive `Instance`, chip-specific spec, sample count, and optional `Vec<StateEvent>` when notable state changes occur.
- **New**: Add `chip::event` module (moved from `chip::state::event`) with `StateEvent` enum that represents notable chip state changes including `KeyOn`, `KeyOff`, `ToneChange`, `VolumeChange`, `TimbreChange`, and chip-specific events.
- **Breaking change**: `VgmHeader::chip_instances()` now returns `ChipInstances` (a newtype wrapping `Vec<(Instance, Chip, f64)>`) instead of `Vec<(Instance, Chip)>`. The new type includes clock frequency information for each chip instance. Use `ChipInstances::iter()` or iterate over the inner `Vec` to access the tuples.

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
