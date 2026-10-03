# Changelog

## Unreleased

## v0.2.0

### Breaking API Changes

- [x] Remove: Legacy post-failure source lookup APIs and `mdx::SourcePosition`; use `mdx::frontend` with the `source-map` feature instead.
- [x] Change: `CompileError::InvalidValue.value` is now `i32` instead of `i64`.
- [x] Change: Add `RepeatDepthExceeded` variants to `ParseError` and `CompileError`; update exhaustive matches.

### Added

- [x] Add: Opt-in `source-map` frontend APIs with borrowed source snapshots, structured diagnostics, syntax ranges and finalized MDX source maps.
- [x] Add: UTF-8 source spans and optional line/UTF-16 position indexes.
- [x] Add: `mdx::MAX_REPEAT_DEPTH` exposes the shared repeat nesting limit.

### Fixed

- [x] Fix: Reject repeat nesting beyond 64 levels during parsing and compilation, including edited ASTs, instead of overflowing the stack; mapped diagnostics identify the rejected bracket.

## v0.1.0

- [x] Add: Initial MXDRV MML support, including parsing and MDX compilation.
