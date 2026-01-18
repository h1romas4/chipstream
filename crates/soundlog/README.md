# soundlog

Builder and parser for retro sound-chip register-write logs (VGM)

`soundlog` is a small Rust crate for constructing and parsing register-write
logs for retro sound chips. It focuses on the VGM (Video Game Music)
file format and provides type-safe APIs for building VGM documents and
representing chip-specific register writes.

Features

- Builder API to construct `VgmDocument` programmatically.
- Parser to read VGM bytes into a structured `VgmDocument`.
- Type-safe chip specifications and command types to reduce invalid writes.

## Quick start

Builder example

```rust
use soundlog::{VgmBuilder, VgmDocument};
use soundlog::chip::{Chip, Ym2612Spec};
use soundlog::vgm::command::{WaitSamples, Instance};
use soundlog::meta::Gd3;

let mut builder = VgmBuilder::new();

// Register the chip master clock in the header (Hz)
builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);

// Append a chip write using a chip-specific spec
builder.add_chip_write(
    Instance::Primary,
    Ym2612Spec { port: 0, register: 0x22, value: 0x91 },
);

builder.add_vgm_command(WaitSamples(44100));

builder.set_gd3(Gd3 {
    track_name_en: Some("Example Track".to_string()),
    game_name_en: Some("soundlog examples".to_string()),
    ..Default::default()
});

let document: VgmDocument = builder.finalize();
let bytes: Vec<u8> = document.into();
```

Parser example

```rust
use soundlog::{VgmBuilder, VgmDocument};
use soundlog::vgm::command::{WaitSamples, VgmCommand};

let mut b = VgmBuilder::new();
b.add_vgm_command(WaitSamples(100));
b.add_vgm_command(WaitSamples(200));
let doc = b.finalize();
let bytes: Vec<u8> = (&doc).into();

let document: VgmDocument = (bytes.as_slice())
    .try_into()
    .expect("failed to parse serialized VGM");

let total_wait: u32 = document
    .iter()
    .map(|cmd| match cmd {
        VgmCommand::WaitSamples(s) => s.0 as u32,
        _ => 0,
    })
    .sum();

assert_eq!(total_wait, 300);
```

## Running tests

```bash
cargo test -p soundlog
```

## License

MIT License
