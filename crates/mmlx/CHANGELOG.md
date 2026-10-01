# Changelog

## Unreleased

- [x] Add: Opt-in `source-map` frontend APIs with borrowed source snapshots, structured diagnostics, syntax ranges, and finalized MDX output maps.
- [x] Add: Shared compact UTF-8 byte spans and optional line/UTF-16 indexes; keep ordinary ASTs and source-map-free compilation unchanged.
- [ ] Change: Replace `i64`/`u64` with 32-bit integers in MDX compilation; `CompileError::InvalidValue.value` is now `i32`.

## v0.1.0

- [x] Add: Initial MXDRV MML support, including parsing and MDX compilation.
