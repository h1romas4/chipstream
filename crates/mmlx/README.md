# mmlx

`mmlx` parses MML source and compiles it into typed documents supported by
[`soundlog`]. It currently supports MXDRV MML input and MDX output.

The crate keeps parsing and compilation separate, so applications can inspect
or format the parsed document before generating MDX data.

## Features

- Parse MML source, including title and PDX metadata, voice definitions, and
  per-channel command streams.
- Inspect the typed syntax tree through `MmlDocument` and `MmlCommand`, or
  render it with `format_tree`.
- Compile supported commands into `soundlog::mdx::document::MdxDocument`.

## Installation

```toml
[dependencies]
mmlx = "0.14"
```

## Usage

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = r#"
#title "Example"
A c4 d4 e4
"#;

    let parsed = mmlx::mdx::parse(source)?;
    println!("{}", mmlx::mdx::format_tree(&parsed));

    let document = mmlx::mdx::compile(&parsed)?;
    let mdx_bytes = document.to_bytes();
    println!("Generated {} MDX bytes", mdx_bytes.len());
    Ok(())
}
```

`parse` returns a syntax tree and reports malformed syntax or values outside the
supported ranges as `ParseError`. `compile` converts that tree to a typed MDX
document and reports unsupported commands or values that cannot be represented
as `CompileError`. The compiler returns a typed MDX document; call its
`to_bytes` method to serialize it to MDX bytes.

The accepted syntax and command coverage follow the MXDRV MML dialect
implemented by this crate. Parsing successfully does not guarantee that every
command can be compiled to MDX.

## See also

[`soundlog`] can convert an `MdxDocument` (with optional PDX data) into a
`VgmDocument` or a streaming `VgmStream`.
