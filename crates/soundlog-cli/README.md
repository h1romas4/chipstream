# soundlog-cli

`soundlog-cli` provides the `soundlog` CLI for inspecting, testing, and re-dumping VGM and MDX files processed by the `soundlog` library. The independent `soundlog-gui` crate provides a reusable GUI frontend.

> [!IMPORTANT]
> `soundlog-cli` is a development / debugging frontend for the `soundlog` library and is not a stable public API. Command-line flags, output formats, and internal behavior may change between releases. If you depend on this crate in scripts or CI, verify compatibility when upgrading. Also, please note that since this is primarily intended for debugging the soundlog crate, it may allocate more memory than necessary.

Contents:

- Building and running
- CLI overview and help example
- Subcommand details and usage examples
  - `test`
  - `redump`
  - `parse`
  - `stream`
  - `mdx`
  - `pdx`
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
Command-line debugging tools for soundlog.

Usage: soundlog <COMMAND>

Commands:
  test    Test a VGM or VGZ file with a parse/build round trip and display its header
  redump  Re-dump a VGM or VGZ file, expanding DAC streams to chip writes
  parse   Parse a VGM or VGZ file and display its commands, offsets, and lengths
  stream  Stream a VGM or VGZ file and display its register writes and detected events
  mdx     MDX file operations
  pdx     PDX file operations
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

- Use `--help` after any subcommand to get subcommand-specific usage.

## Subcommands and usage

### `test`

Run a headless test / round-trip check on a VGM or VGZ file. Useful for automated verification and CI.

```bash
soundlog test <VGM_FILE> [--dry-run]
```

- `<VGM_FILE>`: path to input VGM or VGZ. Use `-` to read from stdin.
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

Expand DAC streams from a VGM or VGZ file into explicit chip writes and re-serialize as a VGM file.
This converts synthesized DAC/digital streams into the equivalent sequence of chip register writes,
and is also useful for producing data suitable for playback on memory-constrained microcontrollers.

Please note that the Wait command will not be restructured or optimized.
All `Wait*` and `Ym2612Port0Address2AWriteAndWaitN` commands are converted to `WaitSamples`.

```bash
soundlog redump <INPUT_VGM> <OUTPUT_VGM> [--diag]
```

- `<INPUT_VGM>`: path to input VGM or VGZ. `-` for stdin is supported (useful with pipes).
- `<OUTPUT_VGM>`: path to write the rebuilt VGM. If `<OUTPUT_VGM>` is `-`, the program writes the raw rebuilt VGM bytes to stdout.
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

Parse a VGM or VGZ file and display its command stream with offsets and lengths.

```bash
soundlog parse <VGM_FILE>
```

- `<VGM_FILE>`: path to input VGM or VGZ. Use `-` to read from stdin (gzipped input is detected automatically).
- No additional options are required for basic parsing; use this command to inspect the serialized command stream, command offsets, and lengths within the VGM's data region.

Behavior:

- The `parse` subcommand reads a VGM or VGZ file (or stdin), parses the command/data region into `VgmCommand` values, and prints a human-readable listing.
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

- The output is intended as an inspection aid — it does not expand DAC streams (use `redump` for expansion) and does not perform state tracking (use `stream` for event detection and state tracking).
- When comparing parse output with `redump` or `stream`, note that `redump` may reserialize the document with expanded stream writes and `stream` will expand stream-generated writes on the timeline; use those commands accordingly for deeper inspection.

### `stream`

Process a VGM or VGZ file as a command stream and display register writes with state events.

```bash
soundlog stream <VGM_FILE> [--dry-run]
```

- `<VGM_FILE>`: path to input VGM or VGZ. Use `-` to read from stdin.
- `--dry-run`: parse and track events but suppress console output (useful for CI or scripted checks).

Behavior:

- The `stream` subcommand uses `VgmCallbackStream` to process the VGM document, expand DAC streams where applicable, and perform per-chip state tracking.
- For each register write emitted by the stream, `stream` prints a concise one-line log containing:
  - The sample offset (timeline position),
  - A brief description of the register write (chip, port/register, value),
  - Any detected events such as `KeyOn`, `KeyOff`, or `ToneChange`, including frequency information when available.
- Output is oriented toward debugging and inspection; no audio is produced. It is intended to help verify timing, register sequences, and event detection when developing or validating VGM streams and chip state trackers.

Examples:

- Stream and print register logs to the terminal:

```bash
soundlog stream samples/example.vgz
```

- Parse and track events but suppress printing (dry-run):

```bash
soundlog stream samples/example.vgz --dry-run
```

Notes:

- `stream` will automatically enable state tracking for chip instances recorded in the VGM header. If the VGM lacks master-clock information for a chip, some frequency calculations or event heuristics may be unavailable or reported as `None`.
- The frequency values shown in `stream` reflect the crate's current calculation logic (register-derived values and any crate-specific adjustments). See the library documentation for details about nominal vs. audible frequency semantics.

### `mdx`

The `mdx` command group provides MDX parsing, conversion, and command streaming through
the same VGM command and register-write processing used by the other CLI
commands. It also provides MML source validation and compilation.

```bash
soundlog mdx <COMMAND>
```

```text
MDX file operations

Usage: soundlog mdx <COMMAND>

Commands:
  check    Parse and validate an MML source file
  compile  Compile MML source to MDX or VGM
  parse    Parse an MDX or MML file and display its track commands
  test     Convert an MDX file and verify that the generated VGM parses
  convert  Convert an MDX file to a VGM file
  stream   Convert an MDX or MML file lazily and print the same register write/event log format as `soundlog stream`
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

#### `mdx check`

Parse and validate an MML source file. Add `--verbose` to print its typed
syntax tree.

```bash
soundlog mdx check <MML_FILE> [--verbose]
```

To run the check on the active MML file from VS Code and report diagnostics in
the Problems panel, add the following task to `.vscode/tasks.json`. This
requires `soundlog` to be available on `PATH`.

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "MML: check current file",
      "type": "process",
      "command": "soundlog",
      "args": ["mdx", "check", "${file}"],
      "problemMatcher": {
        "owner": "mmlx",
        "fileLocation": "absolute",
        "pattern": {
          "regexp": "^(.*):(\\d+):(\\d+): error: (?=.*?, columns \\d+-(\\d+)(?:: .*)?$)(.*)$",
          "file": 1,
          "line": 2,
          "column": 3,
          "endColumn": 4,
          "message": 5
        }
      }
    }
  ]
}
```

Run **MML: check current file** with **Tasks: Run Task**. Parser diagnostics
include an end column, which the matcher uses to mark the full source range.

#### `mdx compile`

Compile an MML source file to MDX. Pass `--output-format vgm` to produce VGM
instead; MDX is the default.

```bash
soundlog mdx compile <MML_FILE> <OUTPUT_FILE> [--output-format mdx|vgm]
```

When producing VGM from MML that declares `#pcmfile`, the referenced PDX file
is searched for relative to the input MML file.

#### `mdx convert`

Convert an MDX file to a VGM file. Use `mdx compile --output-format vgm` for
MML input.

```bash
soundlog mdx convert <MDX_FILE> <VGM_FILE> [OPTIONS]
```

The conversion options include `--pdx <PDX_FILE>`, `--ym2151-clock <HZ>`,
`--okim6258-clock <HZ>`, `--loop-count <COUNT>`, and
`--adpcm-mode <through|resample|lpf>`.

#### `mdx parse`

Parse an MDX file and display its track commands with source offsets and
lengths, in the same inspection style as `soundlog parse`. MML files are also
accepted; they are compiled to MDX commands for display. An optional PDX file
can be supplied for MDX packages that reference external PCM data.

```bash
soundlog mdx parse <MDX_OR_MML_FILE> [--pdx <PDX_FILE>]
```

Examples:

```bash
soundlog mdx parse samples/example.mdx
soundlog mdx parse samples/example.mdx --pdx samples/example.pdx
soundlog mdx parse samples/example.mml
```

When `--pdx <PDX_FILE>` is omitted, the PDX filename stored in the MDX header is
used to search the input file's directory. The exact name, `.PDX`, and `.pdx`
variants are checked, followed by a case-insensitive filename search. If no
matching file is found, processing continues without PDX data.

#### `mdx test`

Convert an MDX file to VGM in memory and verify that the generated VGM can be
parsed as a VGM document.

```bash
soundlog mdx test <MDX_FILE> [OPTIONS]
```

The test options include `--pdx <PDX_FILE>`, `--dry-run`, `--ym2151-clock <HZ>`,
`--okim6258-clock <HZ>`, `--sample-rate <HZ>`, `--loop-count <COUNT>`, and

Examples:

```bash
soundlog mdx test samples/example.mdx
soundlog mdx test samples/example.mdx --pdx samples/example.pdx --dry-run
```

#### `mdx stream`

Convert an MDX or MML file lazily and print the same register-write and event
log format as `soundlog stream`. MML input is compiled to MDX first; the
complete VGM command list is not built up front.

```bash
soundlog mdx stream <MDX_OR_MML_FILE> [OPTIONS]
```

The stream options include `--pdx <PDX_FILE>`, `--dry-run`,
`--ym2151-clock <HZ>`, `--okim6258-clock <HZ>`, `--sample-rate <HZ>`,
`--loop-count <COUNT>`, and
`--adpcm-mode <through|resample|lpf>` (default: `through`).

Examples:

```bash
soundlog mdx stream samples/example.mdx
soundlog mdx stream samples/example.mdx --pdx samples/example.pdx --dry-run
soundlog mdx stream samples/example.mml
```

## PDX

Build a PDX sample bank from one or more mono WAV files. The output path is the
final argument.

```bash
soundlog pdx build <INPUT_WAV>... <OUTPUT_PDX>
```

Integer 8-, 16-, 24-, and 32-bit samples and floating-point samples are
supported. Signed 16-bit samples are converted to signed 12-bit values by
shifting right by four bits before ADPCM encoding. Stereo WAV files are
rejected.

## MML profiler

`soundlog-mml-prof` profiles the complete MML-to-VGM streaming path using the
bundled fixture.

```bash
RUSTFLAGS="-C debuginfo=2" cargo build --release -p soundlog-cli --bin soundlog-mml-prof
heaptrack target/release/soundlog-mml-prof
```

---

## Test Heaptrack

Ubuntu:

```
sudo apt install heaptrack heaptrack-gui
RUSTFLAGS="-C debuginfo=2" cargo build --release
heaptrack target/release/soundlog-prof
heaptrack_gui heaptrack.<pid>.gz
```
