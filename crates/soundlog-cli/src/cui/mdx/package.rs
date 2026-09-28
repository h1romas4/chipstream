use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::compat::normalize_mxdrv16y_tracks;
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;

use super::mml::parse_source;

pub(super) fn find_pdx_path(input: &Path, name: &str) -> Option<PathBuf> {
    let parent = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let path = parent.join(name);
    if path.is_file() {
        return Some(path);
    }
    if Path::new(name).extension().is_none() {
        for extension in ["pdx", "PDX"] {
            let candidate = parent.join(format!("{name}.{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let requested_name = Path::new(name).file_name()?.to_str()?;
    fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.eq_ignore_ascii_case(requested_name))
        })
}

/// Reads an MDX package, resolving its PDX sidecar when no explicit path was supplied.
pub(super) fn read_mdx_package(input: &Path, pdx: Option<&Path>) -> Result<MdxPackage> {
    let mdx_bytes = fs::read(input)
        .with_context(|| format!("failed to read MDX input: {}", input.display()))?;
    let mdx_bytes = normalize_mxdrv16y_tracks(&mdx_bytes)
        .map_err(|error| anyhow!("failed to normalize MDX input: {error}"))?;
    let mdx = MdxDocument::parse(mdx_bytes.as_ref())
        .map_err(|error| anyhow!("failed to parse MDX input: {error}"))?;

    let pdx_bytes = read_pdx_bytes(input, pdx, mdx.header.pdx_name.as_deref())?;
    MdxPackage::parse(mdx_bytes.as_ref(), pdx_bytes.as_deref())
        .map_err(|error| anyhow!("failed to parse MDX package: {error}"))
}

fn read_mml_package(input: &Path, pdx: Option<&Path>) -> Result<MdxPackage> {
    let source = fs::read_to_string(input)
        .with_context(|| format!("failed to read MML input: {}", input.display()))?;
    let parsed = parse_source(input, &source)?;
    let mdx = mmlx::mdx::compile(&parsed)
        .map_err(|error| anyhow!("{}: error: compile error: {error}", input.display()))?;
    let mdx_bytes = mdx.to_bytes();
    let pdx_bytes = read_pdx_bytes(input, pdx, mdx.header.pdx_name.as_deref())?;
    MdxPackage::parse(&mdx_bytes, pdx_bytes.as_deref())
        .map_err(|error| anyhow!("failed to parse compiled MML package: {error}"))
}

pub(super) fn read_mdx_or_mml_package(input: &Path, pdx: Option<&Path>) -> Result<MdxPackage> {
    if input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mml"))
    {
        read_mml_package(input, pdx)
    } else {
        read_mdx_package(input, pdx)
    }
}

fn read_pdx_bytes(
    input: &Path,
    pdx: Option<&Path>,
    pdx_name: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    let pdx_path = resolve_pdx_path(input, pdx, pdx_name);
    pdx_path
        .as_deref()
        .filter(|path| path.is_file())
        .map(|path| {
            fs::read(path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()
}

pub(super) fn resolve_pdx_path(
    input: &Path,
    pdx: Option<&Path>,
    pdx_name: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = pdx {
        return Some(path.to_path_buf());
    }
    let name = pdx_name?;
    let directory = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let candidates = [
        directory.join(name),
        directory.join(format!("{name}.PDX")),
        directory.join(format!("{name}.pdx")),
    ];
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .or_else(|| find_case_insensitive_file(directory, &candidates))
        .or_else(|| candidates.into_iter().next())
}

fn find_case_insensitive_file(directory: &Path, candidates: &[PathBuf]) -> Option<PathBuf> {
    let entries = fs::read_dir(directory).ok()?;
    let candidate_names = candidates
        .iter()
        .filter_map(|candidate| candidate.file_name())
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .collect::<Vec<_>>();
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .map(|name| {
                        let name = name.to_string_lossy().to_ascii_lowercase();
                        candidate_names.iter().any(|candidate| candidate == &name)
                    })
                    .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mml_as_an_mdx_package() {
        let input =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../mmlx/assets/mdx/compact.mml");

        let package = read_mdx_or_mml_package(&input, None).expect("parse MML package");

        assert_eq!(package.mdx.header.title, "Compact MXDRV parser fixture");
        assert_eq!(package.mdx.tracks.len(), 16);
    }

    #[test]
    fn resolves_pdx_stem_with_uppercase_extension() {
        let stem = format!("soundlog-cli-pdx-path-{}", std::process::id());
        let input_path = std::env::temp_dir().join(format!("{stem}.mml"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(find_pdx_path(&input_path, &stem), Some(pdx_path.clone()));

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_name_case_insensitively() {
        let stem = format!("soundlog-cli-pdx-case-{}", std::process::id());
        let input_path = std::env::temp_dir().join(format!("{stem}.mml"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(
            find_pdx_path(&input_path, &format!("{stem}.pdx")),
            Some(pdx_path.clone())
        );

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_for_input_without_parent_directory() {
        let stem = format!("soundlog-cli-pdx-relative-{}", std::process::id());
        let input_path = PathBuf::from(format!("{stem}.mml"));
        let pdx_path = PathBuf::from(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(
            find_pdx_path(&input_path, &format!("{stem}.pdx")),
            Some(PathBuf::from(format!("./{stem}.PDX")))
        );

        fs::remove_file(pdx_path).unwrap();
    }
}
