use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use eframe::egui;

pub(crate) struct FileLoadMessage {
    pub(crate) generation: u64,
    pub(crate) path: PathBuf,
    pub(crate) result: Result<(Vec<u8>, Vec<u8>), String>,
}

pub fn load_decompressed_bytes(path: &Path) -> Result<Vec<u8>> {
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

pub(crate) fn spawn_file_load(
    path: PathBuf,
    generation: u64,
    tx: mpsc::Sender<FileLoadMessage>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        let result = load_decompressed_bytes(&path)
            .map(|bytes| (bytes.clone(), bytes))
            .map_err(|error| format!("{error:#}"));
        let _ = tx.send(FileLoadMessage {
            generation,
            path,
            result,
        });
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::load_decompressed_bytes;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use std::path::Path;

    #[test]
    fn decompresses_vgz_bytes_before_inspection() {
        let expected = b"Vgm ".to_vec();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&expected).unwrap();
        let compressed = encoder.finish().unwrap();
        let input =
            std::env::temp_dir().join(format!("soundlog-gui-vgz-{}.vgz", std::process::id()));
        std::fs::write(&input, compressed).unwrap();

        let decompressed = load_decompressed_bytes(&input).unwrap();

        assert_eq!(decompressed, expected);
        std::fs::remove_file(input).unwrap();
    }

    #[test]
    fn detects_gzip_by_magic_without_gzip_extension() {
        let expected = b"Vgm ".to_vec();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&expected).unwrap();
        let compressed = encoder.finish().unwrap();
        let input =
            std::env::temp_dir().join(format!("soundlog-gui-gzip-{}.bin", std::process::id()));
        std::fs::write(&input, compressed).unwrap();

        let decompressed = load_decompressed_bytes(&input).unwrap();

        assert_eq!(decompressed, expected);
        std::fs::remove_file(input).unwrap();
    }

    #[test]
    fn leaves_uncompressed_vgm_bytes_unchanged() {
        let expected = b"Vgm ".to_vec();
        let input =
            std::env::temp_dir().join(format!("soundlog-gui-vgm-{}.vgm", std::process::id()));
        std::fs::write(&input, &expected).unwrap();

        assert_eq!(
            load_decompressed_bytes(Path::new(&input)).unwrap(),
            expected
        );
        std::fs::remove_file(input).unwrap();
    }
}
