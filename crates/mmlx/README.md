# mmlx

`mmlx` parses MML source and compiles it into typed documents supported by
[`soundlog`]. It currently supports MXDRV MML input and MDX output.

The crate keeps parsing and compilation separate, so applications can inspect
or format the parsed document before generating MDX data.

## Features

- Parse MML source, including title and PDX metadata, voice definitions, and
  per-channel command streams.
- Inspect the typed syntax tree through [`MmlDocument`](mdx::MmlDocument) and
  [`MmlCommand`](mdx::MmlCommand), or render it with [`format_tree`](mdx::format_tree).
- Compile supported commands into [`MdxDocument`](soundlog::mdx::document::MdxDocument).
- Retain syntax ranges and output source maps with the optional `source-map` feature.

## Example

```rust
let source = r#"
@1 = {
  /* AR  D1R D2R RR D1L TL  KS MUL DT1 DT2 AME */
      28, 4,  0,  5, 1,  37, 2, 1,  7,  0,  0,
      22, 9,  1,  2, 1,  47, 2, 12, 0,  0,  0,
      29, 4,  3,  6, 1,  37, 1, 3,  3,  0,  0,
      15, 7,  0,  5, 10,  0, 2, 1,  0,  0,  1,
  /* CON FL OP */
      2,  7, 15
}
A t120 @1 l8 [[cdef]2]2
B t120 @1 l8 [[efga]2]2
C t120 @1 l8 [[gab>c<]2]2
"#;

let parsed = mmlx::mdx::parse(source).expect("valid MML source");
println!("{}", mmlx::mdx::format_tree(&parsed));
let document = mmlx::mdx::compile(&parsed).expect("supported MML commands");
let mdx_bytes = document.to_bytes().expect("valid MDX layout");
println!("Generated {} MDX bytes", mdx_bytes.len());
```

`parse` returns a syntax tree and reports malformed syntax or values outside the
supported ranges as `ParseError`. `compile` converts that tree to a typed MDX
document and reports unsupported commands or values that cannot be represented
as `CompileError`. The compiler returns a typed MDX document; call its
`to_bytes` method to serialize it to MDX bytes.

Repeat nesting is limited to 64 levels by [`MAX_REPEAT_DEPTH`](mdx::MAX_REPEAT_DEPTH)
in both parsing and compilation. Deeper sources and edited ASTs return errors.

The accepted syntax and command coverage follow the MXDRV MML dialect
implemented by this crate. Parsing successfully does not guarantee that every
command can be compiled to MDX.

## Streaming Example

The following example parses and compiles MML, then uses [`soundlog`] to convert
the resulting [`MdxDocument`](soundlog::mdx::document::MdxDocument) into a
streaming [`VgmStream`](soundlog::vgm::VgmStream) through callbacks. The same
example is available as [`mml_to_vgm.rs`](examples/mml_to_vgm.rs).

```rust,no_run
//! Parse and compile an MML string, then print YM2151 writes and VGM waits.

use std::error::Error;

use soundlog::VgmCommandGenerator;
use soundlog::chip::state::Ym2151State;
use soundlog::mdx::convert::MdxToVgmOptions;
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VgmCallbackStream;
use soundlog::vgm::command::Instance;

fn main() -> Result<(), Box<dyn Error>> {
    let source = r#"
#title "Callback stream example"
@1 = {
  /* AR  D1R D2R RR D1L TL  KS MUL DT1 DT2 AME */
      28, 4,  0,  5, 1,  37, 2, 1,  7,  0,  0,
      22, 9,  1,  2, 1,  47, 2, 12, 0,  0,  0,
      29, 4,  3,  6, 1,  37, 1, 3,  3,  0,  0,
      15, 7,  0,  5, 10,  0, 2, 1,  0,  0,  1,
  /* CON FL OP */
      2,  7, 15
}
A t120 @1 l8 [[cdef]2]2
B t120 @1 l8 [[efga]2]2
C t120 @1 l8 [[gab>c<]2]2
"#;
    let parsed = mmlx::mdx::parse(source)?;
    let mdx = mmlx::mdx::compile(&parsed)?;
    let package = MdxPackage { mdx, pdx: None };
    let options = MdxToVgmOptions {
        loop_count: Some(1),
        ..MdxToVgmOptions::default()
    };
    let ym2151_clock = options.ym2151_clock as f32;
    let generator: Box<dyn VgmCommandGenerator> = (package, options).into();

    let mut stream = VgmCallbackStream::from_generator(generator);
    stream.track_state::<Ym2151State>(Instance::Primary, ym2151_clock);

    println!("{:<12} {:<40} Events", "Samples", "Register Write");

    stream.on_wait(|wait, sample, _events| {
        let start_sample = sample.saturating_sub(wait.0 as usize);
        println!("{start_sample:<12} WaitSamples({})", wait.0);
    });
    stream.on_write(
        |instance, spec: soundlog::chip::Ym2151Spec, sample, events| {
            println!(
                "{:<12} Ym2151Write({instance:?}, 0x{:02X}=0x{:02X}) {:?}",
                sample,
                spec.register,
                spec.value,
                events.unwrap_or_default()
            );
        },
    );

    for result in stream {
        result?;
    }

    Ok(())
}
```

## Source Maps

Enable `source-map` for position-aware parsing and compilation:

```toml
mmlx = { version = "0.2.0", features = ["source-map"] }
```

Locate MML commands, metadata, and voice definitions in the source, report
parse and compile errors with source ranges when available, and trace generated
MDX commands back to their originating MML. Source positions can be expressed
as line and column numbers or UTF-16 coordinates for editor integration.

```rust
# #[cfg(feature = "source-map")]
# {
use mmlx::mdx::frontend::{self, MdxLocation};

let source = "A c4 d4";
let parsed = frontend::parse(source).expect("valid MML source");
let compiled = frontend::compile(&parsed).expect("supported MML commands");

let location = MdxLocation::TrackCommand { track: 0, index: 0 };
let span = compiled.source_map().get(&location).expect("source range");
assert_eq!(compiled.source().slice(span), Some("c4"));

let position = compiled.source().position(span.start()).unwrap();
assert_eq!((position.line_number, position.column), (1, 3));
# }
```
