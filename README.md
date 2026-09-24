# chipstream

![](https://github.com/h1romas4/chipstream/workflows/Build/badge.svg) [![crates.io](https://img.shields.io/crates/v/soundlog.svg)](https://crates.io/crates/soundlog) [![docs.rs](https://docs.rs/soundlog/badge.svg)](https://docs.rs/soundlog)

`chipstream` is a toolkit suite for running and working with retro sound chips.
This repository is organized as a monorepo containing multiple crates and provides
utilities for building and parsing register-write logs such as the VGM
(Video Game Music) format.

## Crates

| Crate | Description |
| --- | --- |
| `mmlx` | MML parser and compiler; currently supports MXDRV MML input and MDX output. |
| `soundlog` | Builds and parses VGM data and supports sound-chip log processing. |
| `soundlog-cli` | Command-line tools for working with soundlog-supported formats. |
| `soundlog-gui` | GUI for inspecting soundlog-supported audio files. |

## Quick Start

## Installation

Install the `soundlog` binary from this repository with Cargo:

Rust 1.97.1 or later is required.

```bash
cargo install --git https://github.com/h1romas4/chipstream --bin soundlog --locked --force soundlog-cli
```

### soundlog

`soundlog --help`

```bash
Command-line tools for soundlog.

Usage: soundlog <COMMAND>

Commands:
  test    Test a VGM file with a parse/build round trip and display its header
  redump  Re-dump a VGM file, expanding DAC streams to chip writes
  parse   Parse a VGM file and display its commands, offsets, and lengths
  play    Play a VGM file and display its register writes and detected events
  mdx     MDX file operations
  pdx     PDX file operations
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

For detailed usage instructions, please refer to [crates/soundlog-cli](https://github.com/h1romas4/chipstream/blob/main/crates/soundlog-cli/README.md).

The documentation for the `soundlog` crate library is available at [crates/soundlog](https://github.com/h1romas4/chipstream/blob/main/crates/soundlog/README.md).

## License

Each crate in this repository follows its own `LICENSE` file or the `license`
field declared in its `Cargo.toml`.
