# soundlog-debuger

Important: `soundlog-debuger` is a development / debugging frontend for the `soundlog` library and is not a stable public API. Command-line flags, output formats, and internal behavior may change between releases. If you depend on this crate in scripts or CI, verify compatibility when upgrading.

`soundlog-debuger` provides a lightweight GUI and a small CLI to inspect, test, and re-dump VGM files processed by the `soundlog` library. It is intended for debugging and development use.

Contents:

- Building and running
- CLI overview and help example
- Subcommand details and usage examples
  - `test`
  - `redump`
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
  test    Run in test / headless mode
  redump  Re-dump VGM file with DAC streams expanded to chip writes
  parse   Parse and display VGM file commands with offsets and lengths
  play    Play VGM file and display register writes with events
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

### test

Run a headless test / round-trip check on a VGM file. Useful for automated verification and CI.

Synopsis:

```bash
${soundlog} test <FILE> [--dry-run]
```

- `<FILE>`: path to input binary. Use `-` to read from stdin.
- `--dry-run`: process the input and run the checks without printing the usual one-line result or diagnostic output. 

Examples:

- Run a test on a file (prints a one-line result or diagnostics by default):

```bash
${soundlog} test samples/example.vgz
```

- Read gzipped input from a pipe (stdin) and suppress normal output:

```bash
cat samples/example.vgz | ${soundlog} test - --dry-run
```

Behavior:

- The `test` subcommand re-parses the input using `soundlog`'s parser and performs round-trip checks. 
- Input detection supports `.vgz`/`.gz` extensions and will attempt gzip decompression when appropriate.

### redump

Expand DAC streams into explicit chip writes and re-serialize as a VGM file. This is useful when you need to expand synthesized DAC/digital streams into the sequence of chip register writes that represent them.

Synopsis:
```bash
${soundlog} redump <INPUT> <OUTPUT> [--loop-count <N>] [--fadeout-samples <SAMPLES>] [--diag]
```

- `<INPUT>`: path to input VGM. `-` for stdin is supported (useful with pipes).
- `<OUTPUT>`: path to write the rebuilt VGM. If `<OUTPUT>` is `-`, the program writes the raw rebuilt VGM bytes to stdout.
- `--loop-count <N>`: override loop expansion count. If omitted, original behavior is preserved (intro + one loop iteration by default when expanding).
- `--fadeout-samples <SAMPLES>`: specify additional fadeout sample count appended after loop(s).
- `--diag`: after creating the rebuilt VGM, re-parse it and print diagnostics comparing original vs rebuilt output.

Examples:

- Re-dump to a file:

```bash
${soundlog} redump samples/input.vgz samples/output.vgm
```

- Expand loops to exactly 2 iterations and add 44100 samples (1 second @ 44.1kHz) fadeout:

```bash
${soundlog} redump samples/input.vgz rebuilt.vgm --loop-count 2 --fadeout-samples 44100
```

Notes:

- The `redump` implementation copies header chip registration and some chip-specific configuration fields from the original header into the rebuilt document so the expanded output preserves timing and chip configuration where possible.
- If `--diag` is specified the rebuilt bytes are re-parsed with the same parser used for input, and a comparison table or diagnostics are printed. This is helpful to validate that expansion and serialization did not change the command semantics.

## GUI notes

- Launch the GUI by running the binary with no subcommand:

```bash
${soundlog} samples/example.vgz
```

- The GUI is a simple inspector for parsed VGM documents and command streams. It is intended for interactive debugging and visualization, not for production conversion pipelines.
