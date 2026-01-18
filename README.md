# chipstream

![](https://github.com/h1romas4/chipstream/workflows/Build/badge.svg)

`chipstream` is a toolkit suite for running and working with retro sound chips.
This repository is organized as a monorepo containing multiple crates and provides
utilities for building and parsing register-write logs such as the VGM
(Video Game Music) format.

## Contents

- `crates/soundlog` — a builder and parser for VGM documents (included in this repository).
- Additional utilities and experimental crates for working with sound chips.

## Quick start

Build all crates:

```bash
cargo build
```

Run tests for the `soundlog` crate:

```bash
cargo test -p soundlog
```

## License

Each crate in this repository follows its own `LICENSE` file or the `license`
field declared in its `Cargo.toml`.
