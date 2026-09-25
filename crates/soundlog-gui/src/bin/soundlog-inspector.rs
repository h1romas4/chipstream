use anyhow::{Context, bail};
use flate2::read::GzDecoder;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let path = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        bail!("usage: soundlog-inspector [FILE]");
    }

    let (initial_bytes, initial_file_name) = match path {
        Some(path) => {
            let bytes = load_bytes_from_path(&path)?;
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            (bytes, file_name)
        }
        None => (Vec::new(), None),
    };

    soundlog_gui::run_gui(initial_bytes, initial_file_name);
    Ok(())
}

fn load_bytes_from_path(path: &Path) -> anyhow::Result<Vec<u8>> {
    let data =
        fs::read(path).with_context(|| format!("failed to read file: {}", path.display()))?;
    let is_gzip = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("vgz") || extension.eq_ignore_ascii_case("gz")
        })
        || data.starts_with(&[0x1f, 0x8b]);

    if is_gzip {
        let mut decoder = GzDecoder::new(Cursor::new(data));
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .context("gzip decompression failed")?;
        Ok(decompressed)
    } else {
        Ok(data)
    }
}
