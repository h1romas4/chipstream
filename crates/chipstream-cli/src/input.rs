use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use mmlx::diagnostic::Diagnostic;
use mmlx::frontend::SourceFile;
use mmlx::mdx::frontend;
use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pdx::PdxDocument;

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("input exceeds {limit} bytes");
    }
    Ok(bytes)
}

fn decode(bytes: Vec<u8>, gzip_extension: bool, limit: u64) -> Result<Vec<u8>> {
    if gzip_extension || bytes.starts_with(&[0x1f, 0x8b]) {
        read_bounded(GzDecoder::new(Cursor::new(bytes)), limit).context("failed to decompress VGZ")
    } else {
        Ok(bytes)
    }
}

pub enum Input {
    Vgm(Vec<u8>),
    Mdx(Box<MdxPackage>),
}

impl From<Vec<u8>> for Input {
    fn from(data: Vec<u8>) -> Self {
        Self::Vgm(data)
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    read_bounded(file, MAX_FILE_BYTES).with_context(|| format!("failed to read {}", path.display()))
}

fn mml_error(path: &Path, source: &str, phase: &str, error: Diagnostic) -> anyhow::Error {
    let position = error
        .span
        .and_then(|span| SourceFile::new(source)?.position(span.start()));
    let location = match position {
        Some(position) => format!(
            "{}:{}:{}",
            path.display(),
            position.line_number,
            position.column
        ),
        None => path.display().to_string(),
    };
    anyhow::Error::new(error).context(format!("{location}: failed to {phase} MML"))
}

fn compile_mml(path: &Path) -> Result<MdxDocument> {
    let bytes = read_file(path)?;
    let source = String::from_utf8(bytes)
        .with_context(|| format!("MML input is not UTF-8: {}", path.display()))?;
    let parsed =
        frontend::parse(&source).map_err(|error| mml_error(path, &source, "parse", error))?;
    let compiled =
        frontend::compile(&parsed).map_err(|error| mml_error(path, &source, "compile", error))?;
    Ok(compiled.into_document())
}

pub fn load_ym2608_rhythm_rom(path: &Path) -> Result<Vec<u8>> {
    let bytes = read_file(path)
        .with_context(|| format!("failed to load YM2608 rhythm ROM: {}", path.display()))?;
    if bytes.is_empty() {
        bail!("YM2608 rhythm ROM is empty: {}", path.display());
    }
    Ok(bytes)
}

fn is_file(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

pub fn protect_source(source: &Path, output: Option<&Path>) -> Result<()> {
    let Some(output) = output else {
        return Ok(());
    };
    let output_path = match fs::canonicalize(output) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect output {}", output.display()));
        }
    };
    let source_path = fs::canonicalize(source)
        .with_context(|| format!("failed to inspect input {}", source.display()))?;
    if source_path == output_path {
        bail!(
            "output {} refers to input {}",
            output.display(),
            source.display()
        );
    }
    Ok(())
}

pub fn load(path: &Path, pdx: Option<&Path>, output: Option<&Path>) -> Result<Input> {
    let extension = path.extension().and_then(|ext| ext.to_str());
    let is_mdx = extension.is_some_and(|ext| ext.eq_ignore_ascii_case("mdx"));
    let is_mml = extension.is_some_and(|ext| ext.eq_ignore_ascii_case("mml"));
    if is_mdx || is_mml {
        let mdx = if is_mml {
            compile_mml(path)?
        } else {
            let bytes = read_file(path)?;
            let normalized = normalize_mdx_for_playback(&bytes)
                .map_err(|error| anyhow!("failed to normalize MDX input: {error}"))?;
            MdxDocument::parse(normalized.as_ref()).context("failed to parse MDX input")?
        };
        let pdx_path = match (pdx, mdx.header.pdx_name.as_deref()) {
            (Some(path), _) => Some(path.to_path_buf()),
            (None, Some(name)) => Some(resolve_pdx_path(path, name)?),
            (None, None) => None,
        };
        let pdx = pdx_path
            .map(|path| {
                protect_source(&path, output)?;
                let bytes = read_file(&path)
                    .with_context(|| format!("failed to read PDX input: {}", path.display()))?;
                PdxDocument::parse_owned(bytes)
                    .with_context(|| format!("failed to parse PDX input: {}", path.display()))
            })
            .transpose()?;
        return Ok(Input::Mdx(Box::new(MdxPackage { mdx, pdx })));
    }
    if pdx.is_some() {
        bail!("--pdx can only be used with an MDX or MML file");
    }
    let bytes = read_file(path)?;
    let gzip_extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("vgz") || ext.eq_ignore_ascii_case("gz"));
    Ok(Input::Vgm(decode(
        bytes,
        gzip_extension,
        MAX_DECOMPRESSED_BYTES,
    )?))
}

fn resolve_pdx_path(input: &Path, name: &str) -> Result<PathBuf> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        bail!("invalid PDX filename in MDX header");
    }
    let directory = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut names = vec![name.to_owned()];
    if Path::new(name).extension().is_none() {
        names.push(format!("{name}.PDX"));
        names.push(format!("{name}.pdx"));
    }
    for candidate in &names {
        let path = directory.join(candidate);
        if is_file(&path)? {
            return Ok(path);
        }
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to search for PDX in {}", directory.display()))?
    {
        let entry = entry.context("failed to inspect PDX directory entry")?;
        let filename = entry.file_name();
        if names
            .iter()
            .any(|name| filename.to_string_lossy().eq_ignore_ascii_case(name))
            && is_file(&entry.path())?
        {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        0 => bail!("PDX file '{name}' not found alongside {}", input.display()),
        1 => Ok(matches.remove(0)),
        _ => bail!("multiple case-insensitive matches for PDX file '{name}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("chipstream-mdx-{}-{unique}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn write_mdx(path: &Path, pdx_name: Option<&str>) {
        let mut builder = soundlog::mdx::document::MdxBuilder::new();
        builder.set_pdx_name(pdx_name);
        fs::write(path, builder.finalize().unwrap().to_bytes().unwrap()).unwrap();
    }

    #[test]
    fn rhythm_rom_file_is_bounded_and_errors_are_reported() {
        let dir = temp_dir();
        let rom = dir.join("ym2608.rom");
        assert!(load_ym2608_rhythm_rom(&rom).is_err());
        fs::write(&rom, []).unwrap();
        assert!(
            load_ym2608_rhythm_rom(&rom)
                .unwrap_err()
                .to_string()
                .contains("empty")
        );
        fs::write(&rom, [0x77; 8192]).unwrap();
        assert_eq!(load_ym2608_rhythm_rom(&rom).unwrap(), vec![0x77; 8192]);
        let oversized = File::create(&rom).unwrap();
        oversized.set_len(MAX_FILE_BYTES + 1).unwrap();
        let error = load_ym2608_rhythm_rom(&rom).unwrap_err();
        assert!(format!("{error:#}").contains("exceeds"));
        fs::remove_file(rom).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn bounded_input_and_gzip_detection() {
        let raw = b"Vgm example";
        assert_eq!(decode(raw.to_vec(), false, 32).unwrap(), raw);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(decode(compressed.clone(), false, 32).unwrap(), raw);
        assert!(decode(compressed, false, 4).is_err());
        assert!(decode(raw.to_vec(), true, 32).is_err());
        assert!(read_bounded(&raw[..], 4).is_err());
    }

    #[test]
    fn mdx_pdx_resolution_and_errors() {
        let dir = temp_dir();
        let mdx = dir.join("song.MDX");
        let pdx = dir.join("SaMpLe.pDx");
        write_mdx(&mdx, Some("legacy\\SAMPLE"));
        assert!(format!("{:#}", load(&mdx, None, None).err().unwrap()).contains("not found"));
        fs::write(
            &pdx,
            soundlog::mdx::pdx::PdxBuilder::new().finalize().to_bytes(),
        )
        .unwrap();
        let Input::Mdx(package) = load(&mdx, None, None).unwrap() else {
            panic!("expected MDX")
        };
        assert!(package.pdx.is_some());
        let explicit = dir.join("explicit.pdx");
        fs::write(&explicit, b"invalid").unwrap();
        assert!(
            format!("{:#}", load(&mdx, Some(&explicit), None).err().unwrap()).contains("parse PDX")
        );
        fs::remove_file(&explicit).unwrap();
        assert!(load(&mdx, Some(&explicit), None).is_err());
        let other = dir.join("sAMPLE.PdX");
        fs::write(
            &other,
            soundlog::mdx::pdx::PdxBuilder::new().finalize().to_bytes(),
        )
        .unwrap();
        assert!(format!("{:#}", load(&mdx, None, None).err().unwrap()).contains("multiple"));
        let Input::Mdx(package) = load(&mdx, Some(&pdx), None).unwrap() else {
            panic!("expected MDX")
        };
        assert!(package.pdx.is_some());
        write_mdx(&mdx, None);
        let Input::Mdx(package) = load(&mdx, None, None).unwrap() else {
            panic!("expected MDX")
        };
        assert!(package.pdx.is_none());
        let mut builder = soundlog::mdx::document::MdxBuilder::new();
        builder
            .set_lz_compressed(true)
            .add_mdx_command(0, soundlog::mdx::command::MdxRest { ticks: 8 });
        fs::write(&mdx, builder.finalize().unwrap().to_bytes().unwrap()).unwrap();
        assert!(matches!(load(&mdx, None, None).unwrap(), Input::Mdx(_)));
        assert!(load(&dir.join("file.vgm"), Some(&pdx), None).is_err());
        fs::remove_file(mdx).unwrap();
        fs::remove_file(pdx).unwrap();
        fs::remove_file(other).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
