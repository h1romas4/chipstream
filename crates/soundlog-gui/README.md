# soundlog-gui

`soundlog-gui` provides a graphical inspector for VGM and MDX data processed
by the `soundlog` library. It contains the AST view, byte viewer, and format
source mapping used by the native GUI.

The package includes a `soundlog-gui` executable and a library entry point,
`soundlog_gui::run_gui`.

Run the GUI with no file to open an empty document, or pass a VGM or MDX file:

```bash
cargo run -p soundlog-gui -- [FILE]
```

Gzipped input files are decompressed automatically.
