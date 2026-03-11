# chipstream

![](https://github.com/h1romas4/chipstream/workflows/Build/badge.svg)

`chipstream` is a toolkit suite for running and working with retro sound chips.
This repository is organized as a monorepo containing multiple crates and provides
utilities for building and parsing register-write logs such as the VGM
(Video Game Music) format.

## Quick Start

[![crates.io](https://img.shields.io/crates/v/soundlog.svg)](https://crates.io/crates/soundlog) [![docs.rs](https://docs.rs/soundlog/badge.svg)](https://docs.rs/soundlog)

A debug frontend is provided for easily trying out the `soundlog` crate.

Build all crates:

```bash
cargo build --release
```

`target/release/soundlog --help`:

```bash
GUI/CLI frontend for soundlog for debug

Usage: soundlog [FILE] [COMMAND]

Commands:
  test    Execute parse and build round-trip tests. Also output header details
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

For detailed usage instructions, please refer to [crates/soundlog-debugger](https://github.com/h1romas4/chipstream/blob/main/crates/soundlog-debugger/README.md).

The documentation for the `soundlog` crate library is available at [crates/soundlog](https://github.com/h1romas4/chipstream/blob/main/crates/soundlog/README.md).

## License

Each crate in this repository follows its own `LICENSE` file or the `license`
field declared in its `Cargo.toml`.
