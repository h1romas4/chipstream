# soundlog-cli

`soundlog` is a command-line tool for inspecting and validating VGM, MDX, and PDX files, compiling MML, and converting MDX to VGM. For a graphical interface, use `soundlog-gui`.

> [!IMPORTANT]
> This CLI is intended for debugging and validation. Commands, options, and output formats may change between releases; check compatibility when upgrading scripts or CI. Memory usage may be higher than necessary.

Contents:

- Building and running
- CLI overview and help example
- Shell completions
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

Build with Cargo from the repository root, then run the `soundlog` binary:

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
  completions  Generate shell completion scripts
  stream  Stream a VGM or VGZ file and display its register writes and detected events
  mdx     MDX file operations
  pdx     PDX file operations
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

- Use `--help` after any subcommand to get subcommand-specific usage.

VGM/VGZ input files and gzip output are each limited to 64 MiB for `test`,
`redump`, `parse`, and `stream`.

`redump` output is limited to 10,000,000 commands and 64 MiB, including headers
and metadata. Intro expansion is also budgeted.

## Shell completions

```text
Generate shell completion scripts

Usage: soundlog completions <SHELL>

Arguments:
  <SHELL>  [possible values: bash, elvish, fish, powershell, zsh]

Options:
  -h, --help  Print help
```

Generate a completion script for your shell:

```bash
soundlog completions bash
```

To enable Bash completion in the current shell session:

```bash
source <(soundlog completions bash)
```

You can also save the generated script and source it later:

```bash
soundlog completions bash > soundlog.bash
source ./soundlog.bash
```

For PowerShell, load completions into the current session with:

```powershell
soundlog completions powershell | Out-String | Invoke-Expression
```

## Subcommands and usage

### `test`: Validate VGM/VGZ round trips

```text
Test a VGM or VGZ file with a parse/build round trip and display its header

Usage: soundlog test [OPTIONS] <VGM_FILE>

Arguments:
  <VGM_FILE>  VGM or VGZ input file path (use '-' for stdin)

Options:
      --dry-run  Dry-run: do not print standard one-line outputs; only emit errors/panics
  -h, --help     Print help
```

Check that a VGM or VGZ file can be parsed and rebuilt. Useful for automated verification and CI.

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

- `test` parses the input, rebuilds it, and checks the result.
- Input detection supports `.vgz`/`.gz` extensions and will attempt gzip decompression when appropriate.

### `redump`: Expand VGM DAC streams

```text
Re-dump a VGM or VGZ file, expanding DAC streams to chip writes

Usage: soundlog redump [OPTIONS] <INPUT_VGM> <OUTPUT_VGM>

Arguments:
  <INPUT_VGM>   Input VGM or VGZ file path
  <OUTPUT_VGM>  Output VGM file path (use '-' for stdout)

Options:
      --diag  Print diagnostic output after redump (re-parse output and show diagnostics)
  -h, --help  Print help
```

Expand DAC streams from a VGM or VGZ file into explicit chip writes and re-serialize as a VGM file.
This converts synthesized DAC/digital streams into the equivalent sequence of chip register writes,
and is also useful for producing data suitable for playback on memory-constrained microcontrollers.

Please note that the Wait command will not be restructured or optimized.
All `Wait*` and `Ym2612Port0Address2AWriteAndWaitN` commands are converted to `WaitSamples`.

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

- The output retains the original chip clocks and configuration where possible.
- Use `--diag` to compare the original and rebuilt files and check for changes in command behavior.


### `parse`: Inspect VGM commands

```text
Parse a VGM or VGZ file and display its commands, offsets, and lengths

Usage: soundlog parse <VGM_FILE>

Arguments:
  <VGM_FILE>  VGM or VGZ file path to parse

Options:
  -h, --help  Print help
```

Parse a VGM or VGZ file and display its command stream with offsets and lengths.

- `<VGM_FILE>`: path to input VGM or VGZ. Use `-` to read from stdin (gzipped input is detected automatically).
- Use this command to locate commands and data blocks in the file.

Behavior:

- Each command is listed with its offset, kind, details (such as register, value, and chip instance), and length in bytes.

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

- `parse` shows stored commands without expanding DAC streams or detecting chip events. Use `redump` to save expanded register writes, or `stream` to inspect them with events and timing.


### `stream`: Inspect VGM register writes and events

```text
Stream a VGM or VGZ file and display its register writes and detected events

Usage: soundlog stream [OPTIONS] <VGM_FILE>

Arguments:
  <VGM_FILE>  VGM or VGZ file to stream

Options:
      --dry-run                        Dry-run mode: process the file without printing output (only errors/panics)
      --loop-count <LOOP_COUNT>        Loop count limit (default: 1 when unspecified — play once). Pass an explicit value to override (e.g. `--loop-count 2` to play twice)
      --loop-modifier <LOOP_MODIFIER>  VGM loop_modifier override (0 = use file default; see VGM spec §loop_modifier)
      --loop-base <LOOP_BASE>          VGM loop_base override (see VGM spec §loop_base)
  -h, --help                           Print help
```

Process a VGM or VGZ file as a command stream and display register writes with state events.

- `<VGM_FILE>`: path to input VGM or VGZ. Use `-` to read from stdin.
- `--dry-run`: parse and track events but suppress console output (useful for CI or scripted checks).

Behavior:

- DAC streams are expanded into register writes where supported.
- Each log line shows the sample position, chip, port/register, value, and detected events such as `KeyOn`, `KeyOff`, or `ToneChange`. Frequencies are included when available.
- No audio is produced; use the log to inspect timing, register writes, and chip events.

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
- Frequencies are calculated from chip registers and may differ from audible pitch. See the library documentation for details.


### `mdx`: MDX and MML tools

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

Use `mdx` to inspect MDX files, validate or compile MML, convert MDX to VGM,
and inspect register writes and events.


### `mdx check`: Validate MML syntax and playback

```text
Parse, compile, and check finite lazy playback of MML without loading PDX

Usage: soundlog mdx check [OPTIONS] <MML_FILE>

Arguments:
  <MML_FILE>  MML source file to parse, or '-' to read from stdin

Options:
      --stdin    Read source from stdin while using MML_FILE for diagnostic locations
      --parse-only  Only parse MML, skipping compilation and playback
      --loop-count <LOOP_COUNT>  Number of whole-song playthroughs to check [default: 1]
      --max-ticks <MAX_TICKS>  Maximum playback ticks [default: 100000]
      --max-commands <MAX_COMMANDS>  Maximum executed MDX commands [default: 1000000]
  -v, --verbose  Print the parsed MML syntax tree
  -h, --help     Print help
```

Check MML syntax, compilation, and playback without producing a VGM file.
Pass `-` as the file to read from stdin;
use `--stdin` to read stdin while keeping the supplied filename in diagnostics
(useful for editor integrations). Add `--verbose` to print the parsed syntax
tree.

The default checks one whole-song playthrough; `--loop-count N` checks additional
passes through song-level loops (the intro runs once). Counts and limits must be
positive. Playback limits prevent stalled synchronization or zero-duration
repeats from running indefinitely. Reaching either limit reports an **incomplete
check** and exits with status 1, not a successful validation. Increase the limits
for long songs.

`check` uses synthetic PCM samples and never loads PDX files. It checks only
reached playback paths, not real sample validity, audio, or sample-length-dependent
behavior.

Diagnostics use `file:line:column: error: ...` when a source position is available,
or `file: error: ...` otherwise. Lines and character columns are one-based; MDX
track/command indices in messages are zero-based. Missing tones point to the note
that triggers key-on. Compilation errors are reported even when playback checks
are skipped.

Use `--parse-only` for syntax-only editor linting, for example while voice
definitions are incomplete. It cannot be combined with explicit loop-count or
playback-budget options.

```bash
cat song.mml | soundlog mdx check -
# For efm-langserver and similar LSP lint integrations:
cat unsaved.mml | soundlog mdx check songs/song.mml --stdin
# Check three passes without reading the referenced PDX:
soundlog mdx check song.mml --loop-count 3
# Lightweight editor linting:
cat unsaved.mml | soundlog mdx check songs/song.mml --stdin --parse-only
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
          "regexp": "^(.*):(\\d+):(\\d+): error: (?=(.*)$)(?:.*?, columns \\d+-(\\d+)(?:: .*)?|.*)$",
          "file": 1,
          "line": 2,
          "column": 3,
          "endColumn": 5,
          "message": 4
        }
      }
    }
  ]
}
```

Run **MML: check current file** with **Tasks: Run Task**. Parser diagnostics
include an end column, which the matcher uses to mark the full source range;
positioned playback diagnostics mark the originating command's start position.

#### Helix with `efm-langserver`

Install `efm-langserver` and make both `efm-langserver` and `soundlog`
available on `PATH`. Add the following to
`~/.config/helix/languages.toml`:

```toml
[language-server.efm]
command = "efm-langserver"

[[language]]
name = "mml"
scope = "source.mml"
file-types = ["mml"]
language-servers = [{ name = "efm", only-features = ["diagnostics"] }]
```

Add the lint definition to `~/.config/efm-langserver/config.yaml`:

```yaml
version: 2

tools:
  soundlog-mml: &soundlog-mml
    lint-command: "soundlog mdx check ${INPUT} --stdin"
    lint-stdin: true
    lint-ignore-exit-code: true
    lint-formats:
      - "%f:%l:%c: %m"
      - "%f: %m"

languages:
  mml:
    - <<: *soundlog-mml
```

The linter reads the current buffer from stdin while `${INPUT}` preserves its
filename in diagnostics. The second lint format accepts compilation and playback
errors without MML line/column coordinates; these are shown at the start of the
file. Positioned playback errors use the first format and point to the originating
MML command. In Helix, use `]d` and `[d` to move
between diagnostics or `Space d` to open the diagnostic picker. After changing
the efm configuration, run `:lsp-restart` and edit the buffer to trigger linting.

### `mdx compile`: Compile MML to MDX or VGM

```text
Compile an MML source file into an MDX or VGM binary file

Usage: soundlog mdx compile [OPTIONS] <MML_FILE> <OUTPUT_FILE>

Arguments:
  <MML_FILE>     MML source file to parse
  <OUTPUT_FILE>  Output MDX or VGM file path

Options:
      --output-format <OUTPUT_FORMAT>  Output format. VGM output uses default settings with native looping (same as `mdx convert --native-loop`) [default: mdx] [possible values: mdx, vgm]
      --adpcm-mode <ADPCM_MODE>        ADPCM processing mode for VGM output [default: through] [possible values: through, resample, lpf]
      --no-playback-check              Skip bounded playback validation for MDX output (VGM conversion still reports playback errors)
  -h, --help                           Print help
```

Compile MML to MDX by default, or use `--output-format vgm` for VGM with native
looping. For VGM output, `--adpcm-mode` defaults to `through`, and PDX files
referenced by `#pcmfile` are searched for relative to the input MML.

MDX output runs the same playback check as `mdx check` before writing: one
playthrough, up to 100,000 ticks and 1,000,000 commands, using synthetic PCM and
no PDX loading. Failures or exhausted limits exit with status 1 without writing
or overwriting output. `--no-playback-check` skips playback validation only;
use `mdx check` separately for custom limits or loop counts.

VGM conversion has no playback-check budgets and still reports playback errors
with `--no-playback-check`. Both formats report available MML source positions
and MDX track/command indices.
VGM output uses the converter's default 100,000-tick runtime limit.

### `mdx convert`: Convert MDX to VGM

```text
Convert an MDX file to a VGM file

Usage: soundlog mdx convert [OPTIONS] <MDX_FILE> <VGM_FILE>

Arguments:
  <MDX_FILE>  MDX input file path
  <VGM_FILE>  VGM output file path

Options:
    --pdx <PDX_FILE>
      Optional PDX file used for PCM references
    --ym2151-clock <YM2151_CLOCK>
      YM2151 clock in Hz [default: 4000000]
    --okim6258-clock <OKIM6258_CLOCK>
      OKIM6258 clock in Hz [default: 8000000]
    --loop-count <COUNT>
      Number of whole-song playthroughs to write (default: 1; 0 means 1; no VGM loop point)
    --native-loop
      Use a native VGM loop point instead of a finite loop count. Sets VGM header loop_offset and loop_samples when a loop is detected. The estimated loop point may be inaccurate for per-track MDX F1 loops
    --adpcm-mode <ADPCM_MODE>
      ADPCM mode: through, resample, or lpf [default: through] [possible values: through, resample, lpf]
    --max-ticks <TICKS>
      Maximum elapsed playback ticks across all loops (0 disables the limit) [default: 100000]
  -h, --help
      Print help
```

Convert an MDX file to a VGM file. Use `mdx compile --output-format vgm` for
MML input.

Playback errors are reported by default during conversion, with available
zero-based MDX track/command coordinates, using the same diagnostic format as
`mdx check`. Conversion stops with an error when a playback or output limit
is exceeded.
A playback failure does not write or overwrite the output VGM file.

The conversion options include `--pdx <PDX_FILE>`, `--ym2151-clock <HZ>`,
`--okim6258-clock <HZ>`, `--loop-count <COUNT>`, `--native-loop`, and
`--adpcm-mode <through|resample|lpf>`.

`--max-ticks <TICKS>` limits elapsed playback ticks (default: 100,000; `0`
disables this limit). A separate fixed limit of 100,000 MDX commands without
advancing playback time remains enabled even with `--max-ticks 0`.
Eager conversion also caps output at 14,000,000 VGM commands.

By default, conversion writes one loop iteration as a finite VGM with no loop
point. `--loop-count <COUNT>` uses the same count convention as VGM playback:
with L markers, the intro plays once and the loop section is emitted COUNT
times. A count of `0` is treated as `1`. `--native-loop` instead sets the VGM header's `loop_offset` and
`loop_samples` when a loop point can be detected, allowing the VGM player to
repeat it. Since MDX F1 markers are per-track, the estimated global loop point
may be inaccurate when tracks have different L positions or loop lengths.

The explicit `--loop-count <COUNT>` option uses the same whole-song loop
iteration count in `mdx test` and `mdx stream`. For example, `--loop-count 2`
plays the intro once and emits two passes through the L region; `--loop-count 0`
is equivalent to `--loop-count 1`.

### `mdx parse`: Inspect MDX/MML track commands

```text
Parse an MDX or MML file and display its track commands

Usage: soundlog mdx parse [OPTIONS] <MDX_OR_MML_FILE>

Arguments:
  <MDX_OR_MML_FILE>  MDX or MML input file path

Options:
      --pdx <PDX_FILE>  Optional PDX file to parse alongside an MDX input
  -h, --help            Print help
```

Parse an MDX file and display its track commands with source offsets and
lengths, in the same inspection style as `soundlog parse`. MML files are also
accepted; they are compiled to MDX commands for display. An optional PDX file
can be supplied for MDX packages that reference external PCM data.

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

### `mdx test`: Verify MDX-to-VGM conversion

```text
Convert an MDX file and verify that the generated VGM parses

Usage: soundlog mdx test [OPTIONS] <MDX_FILE>

Arguments:
  <MDX_FILE>  MDX input file path

Options:
    --pdx <PDX_FILE>
      Optional PDX file used for PCM references
    --dry-run
      Dry-run mode: process the file without printing diagnostics
    --ym2151-clock <YM2151_CLOCK>
      YM2151 clock in Hz [default: 4000000]
    --okim6258-clock <OKIM6258_CLOCK>
      OKIM6258 clock in Hz (only used for files with PCM8/PCM8A tracks) [default: 8000000]
    --loop-count <COUNT>
      Number of whole-song playthroughs; COUNT follows VGM semantics (0 means 1)
    --adpcm-mode <ADPCM_MODE>
      ADPCM mode: through, resample, or lpf (default: through) [default: through] [possible values: through, resample, lpf]
    --max-ticks <TICKS>
      Maximum elapsed playback ticks across all loops (0 disables the limit) [default: 100000]
  -h, --help
      Print help
```

Convert an MDX file to VGM in memory and verify that the generated VGM can be
parsed as a VGM document.

The test options include `--pdx <PDX_FILE>`, `--dry-run`, `--ym2151-clock <HZ>`,
`--okim6258-clock <HZ>`, `--sample-rate <HZ>`, and `--loop-count <COUNT>`.
An explicit `--loop-count` uses the same iteration count as VGM playback: with
L markers, `--loop-count 2` plays the intro once and traverses the loop region
twice. If omitted, the test uses native loop handling.
`--max-ticks` uses the same runtime limit as `mdx convert` (default: 100,000;
0 disables the limit).

Examples:

```bash
soundlog mdx test samples/example.mdx
soundlog mdx test samples/example.mdx --pdx samples/example.pdx --dry-run
```

### `mdx stream`: Inspect MDX/MML register writes and events

```text
Convert an MDX or MML file lazily to a command stream and print register writes and events in the same format as `soundlog stream`

Usage: soundlog mdx stream [OPTIONS] <MDX_OR_MML_FILE>

Arguments:
  <MDX_OR_MML_FILE>  MDX or MML input file path

Options:
    --pdx <PDX_FILE>
      Optional PDX file used for PCM references
    --dry-run
      Dry-run mode: process the file without printing output (only errors/panics)
    --ym2151-clock <YM2151_CLOCK>
      YM2151 clock in Hz [default: 4000000]
    --okim6258-clock <OKIM6258_CLOCK>
      OKIM6258 clock in Hz (only used for files with PCM8/PCM8A tracks; see `MdxCommands::Test`'s `okim6258_clock`) [default: 8000000]
    --loop-count <COUNT>
      Number of whole-song playthroughs (default: 1; COUNT follows VGM semantics, 0 means 1)
    --adpcm-mode <ADPCM_MODE>
      ADPCM mode: through, resample, or lpf (default: through) [default: through] [possible values: through, resample, lpf]
    --max-ticks <TICKS>
      Maximum elapsed playback ticks across all loops (0 disables the limit) [default: 100000]
  -h, --help
      Print help
```

Inspect register writes and events from an MDX or MML file without writing a
VGM file. The log format matches `soundlog stream`. MML input is compiled
before playback processing.

The stream options include `--pdx <PDX_FILE>`, `--dry-run`,
`--ym2151-clock <HZ>`, `--okim6258-clock <HZ>`, `--sample-rate <HZ>`,
`--loop-count <COUNT>`, and
`--adpcm-mode <through|resample|lpf>` (default: `through`).
The default loop count is 1. Explicit counts follow VGM playback semantics;
for L markers, `--loop-count 2` plays the intro once and traverses the loop
region twice.
`--max-ticks` limits the actual lazy playback in the same way as `mdx convert`
(default: 100,000; 0 disables the limit). Register writes already printed before
the error are not rolled back.

Examples:

```bash
soundlog mdx stream samples/example.mdx
soundlog mdx stream samples/example.mdx --pdx samples/example.pdx --dry-run
soundlog mdx stream samples/example.mml
```

### `pdx`: PDX sample bank tools

```text
PDX file operations

Usage: soundlog pdx <COMMAND>

Commands:
  test    Validate a PDX file and display its banks and sample allocation
  build   Convert WAV/raw samples to ADPCM as needed and write a PDX file
  export  Export PDX samples as raw ADPCM bytes or mono WAV files
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```

### `pdx test`: Validate PDX sample banks

```text
Validate a PDX file and display its banks and sample allocation

Usage: soundlog pdx test [OPTIONS] <PDX_FILE>

Arguments:
  <PDX_FILE>  PDX input file

Options:
      --dry-run  Dry-run: validate the file without printing its summary
  -h, --help     Print help
```

Inspect a PDX file, validate its sample ranges, and display compression,
bank/sample counts, per-bank allocation totals, and each populated note slot.

`--dry-run` performs validation while suppressing the summary output.

### `pdx build`: Build PDX from WAV or raw ADPCM

```text
Convert WAV/raw samples to ADPCM as needed and write a PDX file

Usage: soundlog pdx build [OPTIONS] <INPUT_WAV_OR_RAW>... <OUTPUT_PDX>

Arguments:
  [INPUT_WAV_OR_RAW_OR_OUTPUT_PDX] [INPUT_WAV_OR_RAW_OR_OUTPUT_PDX]...
          Input WAV/raw samples followed by the output PDX path

Options:
  -v, --verbose                   Print detailed input and output information
      --disable-12bit-conversion  Skip 16-bit WAV PCM scaling; the ADPCM encoder still clamps to signed 12-bit
      --enable-lz                 Store the PDX payload using LZ compression
  -h, --help                      Print help
```

Build a PDX sample bank from one or more mono WAV or raw ADPCM samples. The
output path is the final argument. Builds are quiet by default; use `--verbose`
to show per-input conversion details and output totals. Use `--enable-lz` to
compress the PDX payload; it is uncompressed by default.

Verbose output reports WAV bit depth and PCM conversion to 12-bit samples; raw
ADPCM is copied without PCM conversion. For 16-bit WAV input,
`--disable-12bit-conversion` skips the default 16-to-12-bit scaling. Values
outside the signed 12-bit range are still clipped during ADPCM encoding.

Samples exported by `pdx export` retain their bank/note positions when passed
back to `pdx build`; ordinary input names continue to use sequential positions.

```bash
soundlog pdx build exported-samples/bank-00-note-00.raw exported-samples/bank-01-note-03.raw rebuilt.pdx
```


To restore exported samples to their original slots, keep the
`bank-<BANK>-note-<NOTE>` filename format. For example, `bank-01-note-03.wav`
or `bank-01-note-03.raw` targets bank 1, note 3 (both indices are zero-based).
WAV input is ADPCM-encoded during build; `.raw` input is copied as-is. Do not
mix slot-named inputs with ordinary names in one build command.

Integer 8-, 16-, 24-, and 32-bit samples and floating-point samples are
supported. Stereo WAV files are rejected.

### `pdx export`: Export PDX samples as raw ADPCM or WAV

```text
Export PDX samples as raw ADPCM bytes or mono WAV files

LZ-compressed PDX input is decompressed first. Raw output contains the original
ADPCM sample bytes, not the PDX-level LZ stream.

Usage: soundlog pdx export [OPTIONS] <INPUT_PDX> <OUTPUT_DIR>

Arguments:
  <INPUT_PDX>
    Input PDX file

  <OUTPUT_DIR>
    Directory for exported sample files

Options:
      --sample-rate <HZ>
    Sample rate for WAV output (ignored for raw output)
    [default: 15625]
      --output-format <OUTPUT_FORMAT>
    Export samples as raw ADPCM bytes or decoded WAV audio
    [default: raw]
    [possible values: raw, wav]
  -v, --verbose
    Print detailed information about exported samples
  -h, --help
    Print help (see a summary with '-h')
```

By default (`--output-format raw`), export each populated ADPCM sample as its
original raw bytes in a `bank-00-note-00.raw` file. Pass
`--output-format wav` to decode samples into mono 16-bit PCM WAV files instead.
PDX files do not contain a sample rate, so exported WAVs default to 15,625 Hz;
override it with `--sample-rate` when appropriate. Export is quiet by default;
use `--verbose` to list each output file, its bank/note slot, and byte/frame
counts.

```bash
soundlog pdx export samples.pdx exported-samples [--verbose]
soundlog pdx export samples.pdx exported-wavs --output-format wav --sample-rate 15625 --verbose
```


WAV export contains decoded ADPCM audio, not the original WAV samples used to
build the PDX. Rebuilding either raw ADPCM exports or WAV exports preserves the
PDX's encoded sample data and populated bank/note slots.

---

## MML profiler

Use `soundlog-mml-prof` with a profiler to measure MML-to-VGM processing of
the bundled MML sample.

```bash
RUSTFLAGS="-C debuginfo=2" cargo build --release -p soundlog-cli --bin soundlog-mml-prof
heaptrack target/release/soundlog-mml-prof
```

## Memory profiling

To inspect memory usage on Ubuntu, install Heaptrack and run:

```
sudo apt install heaptrack heaptrack-gui
RUSTFLAGS="-C debuginfo=2" cargo build --release
heaptrack target/release/soundlog-prof
heaptrack_gui heaptrack.<pid>.gz
```
