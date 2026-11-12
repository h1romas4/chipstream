# chipstream

![](https://github.com/h1romas4/chipstream/workflows/Build/badge.svg)

`chipstream` is a toolkit suite for running and working with retro sound chips.
This repository is organized as a monorepo containing multiple crates and provides
utilities for building and parsing register-write logs such as the VGM
(Video Game Music) format.

## Crates

| Crate | Type | Description | Documentation | crates.io |
| --- | --- | --- | --- | --- |
| `chipstream` | `lib` | Sound-chip emulation and device-independent VGM/MDX playback. | [README](crates/chipstream/README.md) | - |
| `chipstream-ndsif` | `lib` | NDSIF hardware adapters and routing for chipstream. | [README](crates/chipstream-ndsif/README.md) | - |
| `chipstream-gstreamer` | `plugin` | GStreamer source for VGM/MDX playback through chipstream. | [README](crates/chipstream-gstreamer/README.md) | - |
| `cstools` | `bin` | VGM/VGZ, MDX, and MML player using chipstream. | [README](crates/cstools/README.md) | - |
| `nddrv` | `bin` | Desktop VGM/VGZ and MDX library and player using GPUI Kit. | [README](crates/nddrv/README.md) | - |
| `mmlx` | `lib` | MML parser and compiler; currently supports MXDRV MML input and MDX output. | [README](crates/mmlx/README.md) | [0.3.0](https://crates.io/crates/mmlx/0.3.0) |
| `ndsif` | `lib` | I/O-independent, allocation-free NDSIF protocol codec. | [README](crates/ndsif/README.md) | - |
| `ndsif-transport` | `lib` | Serial NDSIF transport and scheduled playback. | [README](crates/ndsif-transport/README.md) | - |
| `soundlog` | `lib` | Parses and builds sound logs and converts MDX playback to VGM commands. | [README](crates/soundlog/README.md) | [0.16.0](https://crates.io/crates/soundlog/0.16.0) |
| `soundlog-cli` | `bin` | Command-line tools for working with soundlog-supported formats. | [README](crates/soundlog-cli/README.md) | - |
| `soundlog-gui` | `bin` | `soundlog-inspector` GUI for inspecting soundlog-supported audio files. | [README](crates/soundlog-gui/README.md) | - |

## Quick Start

## Installation

Rust 1.97.1 or later is required.

### soundlog

```bash
cargo install --git https://github.com/h1romas4/chipstream --bin soundlog --locked --force soundlog-cli
soundlog --help
```

```bash
Command-line tools for soundlog.

Usage: soundlog <COMMAND>

Commands:
  test         Test a VGM or VGZ file with a parse/build round trip and display its header
  redump       Re-dump a VGM or VGZ file, expanding DAC streams to chip writes
  parse        Parse a VGM or VGZ file and display its commands, offsets, and lengths
  completions  Generate shell completion scripts
  stream       Stream a VGM or VGZ file and display its register writes and detected events
  mdx          MDX file operations
  pdx          PDX file operations
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### NDDRV

Install the NDDRV desktop player:

```bash
cargo install --git https://github.com/h1romas4/chipstream --bin nddrv --locked --force nddrv
nddrv
```

## License

Each crate in this repository follows its own `LICENSE` file or the `license`
field declared in its `Cargo.toml`.
