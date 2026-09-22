# mmlx-cli

`mmlx-cli` provides the `mmlx` command-line tool for parsing MXDRV MML,
compiling it to MDX or VGM, and building PDX sample banks.

> [!IMPORTANT]
> `mmlx-cli` is part of the chipstream workspace and currently targets the
> MXDRV MML dialect implemented by the `mmlx` crate. Command-line options and
> generated output details may change while MML coverage is expanded.

Contents:

- Building and running
- CLI overview and help example
- MML checking and compilation
- PDX building
- Profiling
- Troubleshooting and caveats

## Building and running

Build the workspace or just this package from the repository root:

```bash
cargo build --release -p mmlx-cli
```

The binary is located at `target/release/mmlx`.

```bash
target/release/mmlx --help
```

## CLI overview

```text
Parse MML source files

Usage: mmlx <COMMAND>

Commands:
  mdx   Parse or compile MML/MDX data
  pdx   Build PDX sample data
  help  Print this message or the help of the given command(s)
```

Use `--help` after any subcommand for the complete command-specific usage.

## MML commands

### `mdx check`

Parse and validate an MML source file. Add `--verbose` to print the typed MML
syntax tree.

```bash
mmlx mdx check <INPUT>
mmlx mdx check <INPUT> --verbose
```

Example:

```bash
target/release/mmlx mdx check crates/mmlx/assets/mdx/readable.mml --verbose
```

### `mdx compile`

Compile an MML source file to MDX. MDX is the default output format.

```bash
mmlx mdx compile <INPUT_MML> <OUTPUT_MDX>
```

To produce VGM, pass `--output-format vgm`. The VGM path uses the `soundlog`
MDX-to-VGM conversion implementation.

```bash
mmlx mdx compile <INPUT_MML> <OUTPUT_VGM> --output-format vgm
```

When the MML declares a PDX file with `#pcmfile`, the compiler looks for that
file relative to the input MML path when producing VGM output.

Example:

```bash
target/release/mmlx mdx compile \
  crates/mmlx/assets/mdx/readable.mml \
  /tmp/readable.vgm \
  --output-format vgm
```

Available output formats:

- `mdx`: compile to an MDX binary; this is the default.
- `vgm`: compile to MDX internally, then convert the result to VGM.

## PDX commands

### `pdx build`

Convert one or more mono WAV files into a PDX sample bank. Input WAV files
must be followed by the output PDX path.

```bash
mmlx pdx build <INPUT_WAV>... <OUTPUT_PDX>
```

Example:

```bash
target/release/mmlx pdx build \
  samples/kick.wav samples/snare.wav \
  /tmp/drums.pdx
```

Supported WAV input includes integer 8-, 16-, 24-, and 32-bit samples, as well
as floating-point samples. Stereo WAV files are rejected.

## Profiling

`mmlx_prof` is a reference profiler for the complete MML-to-VGM streaming
path. It embeds `crates/mmlx/assets/mdx/readable.mml`, performs MML parsing and
MDX compilation, consumes VGM commands from the lazy generator, and does not
write output files.

Build it with release optimizations and debug symbols:

```bash
RUSTFLAGS="-C debuginfo=2" \
  cargo build --release -p mmlx-cli --bin mmlx_prof
```

Run a heap profile with Massif:

```bash
valgrind --tool=massif \
  --massif-out-file=massif-mmlx-prof.out \
  target/release/mmlx_prof
massif-visualizer massif-mmlx-prof.out
```

Run allocation profiling with heaptrack:

```bash
heaptrack target/release/mmlx_prof
heaptrack_gui heaptrack.mmlx_prof.<pid>.zst
```

The fixture contains an intentional infinite loop and the profiler limits the
number of consumed VGM commands so the run terminates.

## Troubleshooting and caveats

- Run commands from the repository root when using the paths shown above.
- `mdx check` validates syntax and values but does not produce an output file.
- VGM output is generated through `soundlog` and may require a matching PDX
  file when the MML uses PCM samples.
- For Rust source-level profiling, keep debug information enabled in release
  builds.
