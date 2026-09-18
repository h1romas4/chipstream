# soundlog-debugger

`soundlog-debugger` provides a GUI and a small CLI to inspect, test, and re-dump VGM files processed by the `soundlog` library. 

> [!IMPORTANT]
> `soundlog-debugger` is a development / debugging frontend for the `soundlog` library and is not a stable public API. Command-line flags, output formats, and internal behavior may change between releases. If you depend on this crate in scripts or CI, verify compatibility when upgrading. Also, please note that since this is primarily intended for debugging the soundlog crate, it may allocate more memory than necessary.

Contents:

- Building and running
- CLI overview and help example
- Subcommand details and usage examples
  - `test`
  - `redump`
  - `parse`
  - `play`
  - `mdx`
- GUI notes
- Diagnostic flags and piping
- Troubleshooting and caveats

## Building and running

From the repository root you can build and run the debug frontend with Cargo. The crate installs a binary named `soundlog`.

```bash
cargo build --release
```

```bash
target/release/soundlog --help
```

## CLI overview

```bash
GUI/CLI frontend for soundlog for debug

Usage: soundlog [FILE] [COMMAND]

Commands:
  test    Execute parse and build round-trip tests. Also output header details
  redump  Re-dump VGM file with DAC streams expanded to chip writes
  parse   Parse and display VGM file commands with offsets and lengths
  play    Play VGM file and display register writes with events
  mdx     MDX file operations
  help    Print this message or the help of the given subcommand(s)

Arguments:
  [FILE]  Path to binary file to display (supports .vgz (gzipped) and raw files)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

- If no subcommand is given the program will launch the GUI. If a single `FILE` argument is passed without a subcommand, the GUI will open with that file loaded.
- Use `--help` after any subcommand to get subcommand-specific usage.

## Subcommands and usage

### `test`

Run a headless test / round-trip check on a VGM file. Useful for automated verification and CI.

```bash
soundlog test <FILE> [--dry-run]
```

- `<FILE>`: path to input binary. Use `-` to read from stdin.
- `--dry-run`: process the input and run the checks without printing the usual one-line result or diagnostic output. 

Examples:

- Run a test on a file (prints a one-line result or diagnostics by default):

```bash
soundlog test samples/example.vgz
```

- Read gzipped input from a pipe (stdin) and suppress normal output:

```bash
cat samples/example.vgz | soundlog test - --dry-run
```

Behavior:

- The `test` subcommand re-parses the input using `soundlog`'s parser and performs round-trip checks. 
- Input detection supports `.vgz`/`.gz` extensions and will attempt gzip decompression when appropriate.

### `redump`

Expand DAC streams into explicit chip writes and re-serialize as a VGM file. 
This converts synthesized DAC/digital streams into the equivalent sequence of chip register writes,
and is also useful for producing data suitable for playback on memory-constrained microcontrollers.

Please note that the Wait command will not be restructured or optimized.
All `Wait*` and `Ym2612Port0Address2AWriteAndWaitN` commands are converted to `WaitSamples`.

```bash
soundlog redump <INPUT> <OUTPUT> [--diag]
```

- `<INPUT>`: path to input VGM. `-` for stdin is supported (useful with pipes).
- `<OUTPUT>`: path to write the rebuilt VGM. If `<OUTPUT>` is `-`, the program writes the raw rebuilt VGM bytes to stdout.
- `--diag`: after creating the rebuilt VGM, re-parse it and print diagnostics comparing original vs rebuilt output.

Examples:

- Re-dump to a file:

```bash
soundlog redump samples/input.vgz samples/output.vgm
```

- Expand loops to exactly 2 iterations and add 44100 samples (1 second @ 44.1kHz) fadeout:

```bash
soundlog redump samples/input.vgz rebuilt.vgm --loop-count 2 --fadeout-samples 44100
```

Notes:

- The `redump` implementation copies header chip registration and some chip-specific configuration fields from the original header into the rebuilt document so the expanded output preserves timing and chip configuration where possible.
- If `--diag` is specified the rebuilt bytes are re-parsed with the same parser used for input, and a comparison table or diagnostics are printed. This is helpful to validate that expansion and serialization did not change the command semantics.

### `parse`

Parse and display the VGM command stream with offsets and lengths.

```bash
soundlog parse <FILE>
```

- `<FILE>`: path to input VGM. Use `-` to read from stdin (gzipped input is detected automatically).
- No additional options are required for basic parsing; use this command to inspect the serialized command stream, command offsets, and lengths within the VGM's data region.

Behavior:

- The `parse` subcommand reads the VGM file (or stdin), parses the command/data region into `VgmCommand` values, and prints a human-readable listing.
- For each parsed command the tool prints:
  - The absolute file offset (or offset relative to the data region),
  - The command kind (e.g. `WaitSamples`, `Ym2612Write`, `DataBlock`),
  - Any compact details (register, value, instance) and the command's serialized length in bytes.
- `parse` is helpful for debugging file layout, verifying serialization round-trips, and locating specific commands or data blocks inside the file.

Examples:

- Parse a file and print the command list:

```bash
soundlog parse samples/example.vgz
```

- Feed gzipped input via stdin and parse:

```bash
cat samples/example.vgz | soundlog parse -
```

Notes:

- The output is intended as an inspection aid — it does not expand DAC streams (use `redump` for expansion) and does not perform state tracking (use `play` for event detection and state tracking).
- When comparing parse output with `redump` or `play`, note that `redump` may reserialize the document with expanded stream writes and `play` will expand stream-generated writes on the timeline; use those commands accordingly for deeper inspection.

### `play`

Play a VGM file and display register writes with state events.

```bash
soundlog play <FILE> [--dry-run]
```

- `<FILE>`: path to input VGM. Use `-` to read from stdin.
- `--dry-run`: parse and track events but suppress console output (useful for CI or scripted checks).

Behavior:

- The `play` subcommand uses `VgmCallbackStream` to process the VGM document, expand DAC streams where applicable, and perform per-chip state tracking.
- For each register write emitted by the stream, `play` prints a concise one-line log containing:
  - The sample offset (timeline position),
  - A brief description of the register write (chip, port/register, value),
  - Any detected events such as `KeyOn`, `KeyOff`, or `ToneChange`, including frequency information when available.
- Output is oriented toward debugging and inspection rather than real-time audio playback; `play` does not produce sound. It is intended to help verify timing, register sequences, and event detection when developing or validating VGM streams and chip state trackers.

Examples:

- Play and print register logs to the terminal:

```bash
soundlog play samples/example.vgz
```

- Parse and track events but suppress printing (dry-run):

```bash
soundlog play samples/example.vgz --dry-run
```

Notes:

- `play` will automatically enable state tracking for chip instances recorded in the VGM header. If the VGM lacks master-clock information for a chip, some frequency calculations or event heuristics may be unavailable or reported as `None`.
- The frequency values shown in `play` reflect the crate's current calculation logic (register-derived values and any crate-specific adjustments). See the library documentation for details about nominal vs. audible frequency semantics.

### `mdx`

The `mdx` command group provides MDX parsing, conversion, and playback through
the same VGM command and register-write processing used by the other CLI
commands.

```bash
soundlog mdx <COMMAND>
```

```text
MDX file operations

Usage: soundlog mdx <COMMAND>

Commands:
  parse    Parse an MDX file and display its track commands
  test     Convert an MDX file and verify that the generated VGM parses
  convert  Convert an MDX file to a VGM file
  play     Convert an MDX file lazily and play it, printing the same register write/event log format as `soundlog play`
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `mdx parse`

Parse an MDX file and display its track commands with source offsets and
lengths, in the same inspection style as `soundlog parse`. An optional PDX
file can be supplied for packages that reference external PCM data.

```bash
soundlog mdx parse <INPUT> [--pdx <FILE>]
```

Examples:

```bash
soundlog mdx parse samples/example.mdx
soundlog mdx parse samples/example.mdx --pdx samples/example.pdx
```

When `--pdx <FILE>` is omitted, the PDX filename stored in the MDX header is
used to search the input file's directory. The exact name, `.PDX`, and `.pdx`
variants are checked, followed by a case-insensitive filename search. If no
matching file is found, processing continues without PDX data.

#### `mdx test`

Convert an MDX file to VGM in memory and verify that the generated VGM can be
parsed as a VGM document.

```bash
soundlog mdx test <INPUT> [OPTIONS]
```

The test options include `--pdx <FILE>`, `--dry-run`, `--ym2151-clock <HZ>`,
`--okim6258-clock <HZ>`, `--sample-rate <HZ>`, `--loop-count <COUNT>`, and
`--mxdrv16y`.

Examples:

```bash
soundlog mdx test samples/example.mdx
soundlog mdx test samples/example.mdx --pdx samples/example.pdx --dry-run
```

#### `mdx convert`

Convert an MDX file to a VGM file. Use `-` as the output path to write the
serialized VGM bytes to stdout.

```bash
soundlog mdx convert <INPUT> <OUTPUT> [OPTIONS]
```

Available options include `--pdx <FILE>`, `--ym2151-clock <HZ>`,
`--okim6258-clock <HZ>`, `--sample-rate <HZ>`, `--loop-count <COUNT>`, and
`--mxdrv16y`, and `--adpcm-mode <through|resample|lpf>` (default:
`through`).

The ADPCM modes follow NanoDriveX naming. For legacy 9-track PCM1, `through`
sends the encoded PDX ADPCM bytes directly, while `resample` decodes and
re-encodes them at the output rate. `lpf` uses the same resampling path and
additionally applies the NanoDriveX-style LPF/HPF before OKIM6258 ADPCM
re-encoding. MDX PCM8/PCM8A output is always mixed and re-encoded into the
single OKIM6258 stream required by VGM, so `through` and `resample` are
equivalent for that format.

Examples:

```bash
soundlog mdx convert samples/example.mdx samples/example.vgm
soundlog mdx convert samples/example.mdx samples/example.vgm \
  --pdx samples/example.pdx --loop-count 2
```

#### `mdx play`

Convert an MDX file lazily and print the same register-write and event log
format as `soundlog play`. The complete VGM command list is not built up
front.

```bash
soundlog mdx play <INPUT> [OPTIONS]
```

The playback options include `--pdx <FILE>`, `--dry-run`,
`--ym2151-clock <HZ>`, `--okim6258-clock <HZ>`, `--sample-rate <HZ>`,
`--loop-count <COUNT>`, `--mxdrv16y`, and
`--adpcm-mode <through|resample|lpf>` (default: `through`).

Examples:

```bash
soundlog mdx play samples/example.mdx
soundlog mdx play samples/example.mdx --pdx samples/example.pdx --dry-run
```

## GUI notes

- Launch the GUI by running the binary with no subcommand:

```bash
soundlog samples/example.vgz
```

- The GUI is a simple inspector for parsed VGM documents and command streams. It is intended for interactive debugging and visualization, not for production conversion pipelines.

---

## Test Heaptrack

Ubuntu:

```
sudo apt install heaptrack heaptrack-gui
RUSTFLAGS="-C debuginfo=2" cargo build --release
heaptrack target/release/soundlog-prof
heaptrack_gui heaptrack.<pid>.gz
```
