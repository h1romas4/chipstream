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
- Compile supported commands into [`MdxDocument`](soundlog::mdx::document::MdxDocument).

## Example

```rust
let source = r#"
#title "Example"
@1 = {
  /* AR  D1R D2R RR D1L TL  KS MUL DT1 DT2 AME */
     28, 4,  0,  5, 1,  37, 2, 1,  7,  0,  0,
     22, 9,  1,  2, 1,  47, 2, 12, 0,  0,  0,
     29, 4,  3,  6, 1,  37, 1, 3,  3,  0,  0,
     15, 7,  0,  5, 10,  0, 2, 1,  0,  0,  1,
  /* CON FL OP */
     2,  7, 15
}
A @1 [c8]4 [g8]4 [c8]8
B @1 c2 g2 c1
C @1 e2 b2 e1
"#;

let parsed = mmlx::mdx::parse(source).expect("valid MML source");
println!("{}", mmlx::mdx::format_tree(&parsed));

let document = mmlx::mdx::compile(&parsed).expect("supported MML commands");
let mdx_bytes = document.to_bytes();
println!("Generated {} MDX bytes", mdx_bytes.len());
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

[`soundlog`] can convert an [`MdxDocument`](soundlog::mdx::document::MdxDocument)
(with optional PDX data) into a [`VgmDocument`](soundlog::vgm::VgmDocument) or a
streaming [`VgmStream`](soundlog::vgm::VgmStream).
