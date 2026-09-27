# soundlog-gui

`soundlog-gui` provides a graphical inspector for VGM and MDX data processed
by the `soundlog` library. It contains the AST view, byte viewer, and format
source mapping used by the native GUI.

Inspect parsed VGM and MDX structure, browse the raw bytes, and navigate between
the displayed data and its source locations.

## Executables

### `soundlog-inspector`

Open an empty document, or pass a VGM or MDX file to inspect:

```bash
soundlog-inspector [FILE]
```

Gzipped input files are decompressed automatically.
