# chipstream

![](https://github.com/h1romas4/chipstream/workflows/Build/badge.svg)

`chipstream` is a toolkit suite for running and working with retro sound chips.
This repository is organized as a monorepo containing multiple crates and provides
utilities for building and parsing register-write logs such as the VGM
(Video Game Music) format.

## Contents

[![crates.io](https://img.shields.io/crates/v/soundlog.svg)](https://crates.io/crates/soundlog) [![docs.rs](https://docs.rs/soundlog/badge.svg)](https://docs.rs/soundlog)

- `crates/soundlog` — a builder and parser for VGM documents (included in this repository).
- Additional utilities and experimental crates for working with sound chips.

## Quick start

Build all crates:

```bash
cargo build --release
```

`target/release/soundlog --help`:

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

Run tests for the `soundlog` crate:

```bash
cargo test -p soundlog
```

Optional: set the `SOUNDLOG_TEST_OUTPUT_VGM` environment variable to a non-empty path (relative to the crate root) to write VGM test artifacts when running tests. Example:

```bash
SOUNDLOG_TEST_OUTPUT_VGM=assets/vgm cargo test -p soundlog
```

When the variable is not set or is empty, no VGM test artifacts will be written.

## License

Each crate in this repository follows its own `LICENSE` file or the `license`
field declared in its `Cargo.toml`.
