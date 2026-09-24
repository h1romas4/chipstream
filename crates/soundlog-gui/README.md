# soundlog-gui

`soundlog-gui` provides a graphical inspector for VGM and MDX data processed
by the `soundlog` library. It contains the AST view, byte viewer, and format
source mapping used by the native GUI.

The package provides the `soundlog-inspector` executable and the reusable
`soundlog_gui::run_gui` library entry point. Additional GUI executables can be
added under `src/bin/`; Cargo uses each source file's name as its binary name.

Run the GUI with no file to open an empty document, or pass a VGM or MDX file:

```bash
cargo run -p soundlog-gui --bin soundlog-inspector -- [FILE]
```

Gzipped input files are decompressed automatically.
