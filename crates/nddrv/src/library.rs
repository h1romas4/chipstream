use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chipstream::metadata::{TrackMetadata, probe_mdx_bytes, probe_vgm_bytes};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use soundlog::chip::Chip;
use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::header::MdxHeader;
use soundlog::vgm::header::VgmHeader;
use walkdir::WalkDir;

const MAX_BYTES: u64 = 64 * 1024 * 1024;
const YMFM_CHIPS: [Chip; 11] = [
    Chip::Ym2413,
    Chip::Ym2151,
    Chip::Ym2203,
    Chip::Ym2608,
    Chip::Ym2610b,
    Chip::Ym2612,
    Chip::Ym3526,
    Chip::Y8950,
    Chip::Ym3812,
    Chip::Ymf262,
    Chip::Ymf278b,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChipTag {
    pub name: &'static str,
    pub instances: usize,
    pub ymfm: bool,
}

impl ChipTag {
    fn new(chip: &Chip) -> Self {
        Self {
            name: Track::chip_name(chip),
            instances: 1,
            ymfm: YMFM_CHIPS.contains(chip),
        }
    }

    pub fn is_ymfm_name(name: &str) -> bool {
        YMFM_CHIPS.iter().any(|chip| Track::chip_name(chip) == name)
    }

    pub fn software_supported(&self) -> bool {
        self.ymfm
            || matches!(
                self.name,
                "SN76489" | "Sega PCM" | "C140" | "PWM" | "OKIM6258" | "OKIM6295"
            )
    }

    pub fn label(&self) -> String {
        if self.instances > 1 {
            format!("{}x{}", self.name, self.instances)
        } else {
            self.name.to_string()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub metadata: TrackMetadata,
    pub chips: Vec<ChipTag>,
    search_fields: [String; 5],
}

impl Track {
    fn new(path: PathBuf, metadata: TrackMetadata, chips: Vec<ChipTag>) -> Self {
        let title = metadata.title.clone().unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        let search_fields = [
            title.to_lowercase(),
            metadata
                .artist
                .as_deref()
                .unwrap_or_default()
                .to_lowercase(),
            metadata.album.as_deref().unwrap_or_default().to_lowercase(),
            metadata
                .comment
                .as_deref()
                .unwrap_or_default()
                .to_lowercase(),
            path.to_string_lossy().to_lowercase(),
        ];
        Self {
            path,
            metadata,
            chips,
            search_fields,
        }
    }

    fn chip_name(chip: &Chip) -> &'static str {
        match chip {
            Chip::Sn76489 => "SN76489",
            Chip::Ym2413 => "YM2413",
            Chip::Ym2612 => "YM2612",
            Chip::Ym2151 => "YM2151",
            Chip::SegaPcm => "Sega PCM",
            Chip::Rf5c68 => "RF5C68",
            Chip::Ym2203 => "YM2203",
            Chip::Ym2608 => "YM2608",
            Chip::Ym2610b => "YM2610/B",
            Chip::Ym3812 => "YM3812",
            Chip::Ym3526 => "YM3526",
            Chip::Y8950 => "Y8950",
            Chip::Ymf262 => "YMF262",
            Chip::Ymf278b => "YMF278B",
            Chip::Ymf271 => "YMF271",
            Chip::Ymz280b => "YMZ280B",
            Chip::Rf5c164 => "RF5C164",
            Chip::Pwm => "PWM",
            Chip::Ay8910 => "AY8910",
            Chip::GbDmg => "Game Boy DMG",
            Chip::NesApu => "NES APU",
            Chip::MultiPcm => "MultiPCM",
            Chip::Upd7759 => "uPD7759",
            Chip::Okim6258 => "OKIM6258",
            Chip::Okim6295 => "OKIM6295",
            Chip::K051649 => "K051649",
            Chip::K054539 => "K054539",
            Chip::Huc6280 => "HuC6280",
            Chip::C140 => "C140",
            Chip::K053260 => "K053260",
            Chip::Pokey => "POKEY",
            Chip::Qsound => "QSound",
            Chip::Scsp => "SCSP",
            Chip::WonderSwan => "WonderSwan",
            Chip::Vsu => "VSU",
            Chip::Saa1099 => "SAA1099",
            Chip::Es5503 => "ES5503",
            Chip::Es5506U8 | Chip::Es5506U16 => "ES5505/ES5506",
            Chip::X1010 => "X1-010",
            Chip::C352 => "C352",
            Chip::Ga20 => "GA20",
            Chip::Mikey => "Mikey",
        }
    }

    fn track_chips(bytes: &[u8], is_mdx: bool) -> Result<Vec<ChipTag>> {
        if is_mdx {
            let normalized = normalize_mdx_for_playback(bytes).map_err(anyhow::Error::msg)?;
            let (header, _) =
                MdxHeader::parse(normalized.as_ref()).context("invalid MDX header")?;
            let mut chips = vec![ChipTag::new(&Chip::Ym2151)];
            if header.pdx_name.is_some() {
                chips.push(ChipTag::new(&Chip::Okim6258));
            }
            Ok(chips)
        } else {
            let header = VgmHeader::from_bytes(bytes).context("invalid VGM header")?;
            let mut chips: Vec<ChipTag> = Vec::new();
            for (_, chip, _) in header.chip_instances() {
                let tag = ChipTag::new(&chip);
                if let Some(existing) = chips.iter_mut().find(|existing| existing.name == tag.name)
                {
                    existing.instances += 1;
                } else {
                    chips.push(tag);
                }
            }
            Ok(chips)
        }
    }

    pub fn display_title(&self) -> String {
        Self::format_display_title(&self.path, &self.metadata)
    }

    pub fn format_display_title(path: &Path, metadata: &TrackMetadata) -> String {
        let title = metadata.title.clone().unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        match metadata
            .album
            .as_deref()
            .map(str::trim)
            .filter(|game| !game.is_empty())
        {
            Some(game) => format!("{game} - {title}"),
            None => title,
        }
    }

    fn matches_normalized(&self, query: &str) -> bool {
        query.is_empty() || self.search_fields.iter().any(|value| value.contains(query))
    }
}

pub fn filter_indices(tracks: &[Track], query: &str, directory: Option<&Path>) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            (directory.is_none_or(|directory| track.path.starts_with(directory))
                && track.matches_normalized(&query))
            .then_some(index)
        })
        .collect()
}

pub fn filter_indices_with_chips(
    tracks: &[Track],
    query: &str,
    directory: Option<&Path>,
    chips: &[String],
) -> Vec<usize> {
    let mut indices = filter_indices(tracks, query, directory);
    if !chips.is_empty() {
        indices.retain(|&index| {
            chips
                .iter()
                .all(|chip| tracks[index].chips.iter().any(|tag| tag.name == chip))
        });
    }
    indices
}

#[derive(Debug)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub root: PathBuf,
    pub depth: usize,
    pub has_children: bool,
}

pub struct ScanResult {
    pub tracks: Vec<Track>,
    pub directories: Vec<DirectoryEntry>,
    pub errors: Vec<String>,
}

pub fn visible_directories(
    directories: &[DirectoryEntry],
    expanded: &HashMap<PathBuf, HashSet<PathBuf>>,
) -> Vec<usize> {
    let mut hidden_below = None;
    let mut visible = Vec::new();
    for (index, directory) in directories.iter().enumerate() {
        if hidden_below.is_some_and(|depth| directory.depth > depth) {
            continue;
        }
        hidden_below = None;
        visible.push(index);
        if directory.has_children
            && !expanded
                .get(&directory.root)
                .is_some_and(|paths| paths.contains(&directory.path))
        {
            hidden_below = Some(directory.depth);
        }
    }
    visible
}

pub const DEFAULT_LIBRARY_WIDTH: f32 = 312.;
pub const MIN_PANEL_WIDTH: f32 = 180.;
pub const MAX_PANEL_WIDTH: f32 = 1600.;

fn default_library_width() -> f32 {
    DEFAULT_LIBRARY_WIDTH
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub directories: Vec<PathBuf>,
    pub playlist: Vec<PathBuf>,
    #[serde(default = "default_library_width")]
    pub library_width: f32,
    #[serde(default)]
    pub playlist_width: Option<f32>,
    #[serde(default)]
    pub routing: crate::routing::RoutingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            directories: Vec::new(),
            playlist: Vec::new(),
            library_width: DEFAULT_LIBRARY_WIDTH,
            playlist_width: None,
            routing: crate::routing::RoutingConfig::default(),
        }
    }
}

pub fn config_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "nddrv")
        .context("could not determine the application configuration directory")?;
    Ok(dirs.config_dir().join("library.json"))
}

pub fn load_config(path: &Path) -> Result<Config> {
    match File::open(path) {
        Ok(file) => {
            let bytes = read_bounded(file).context("failed to read library configuration")?;
            let config: Config =
                serde_json::from_slice(&bytes).context("invalid library configuration")?;
            config
                .routing
                .validate()
                .context("invalid routing configuration")?;
            for (name, width) in [
                ("library_width", Some(config.library_width)),
                ("playlist_width", config.playlist_width),
            ] {
                let Some(width) = width else { continue };
                if !width.is_finite() || !(MIN_PANEL_WIDTH..=MAX_PANEL_WIDTH).contains(&width) {
                    bail!(
                        "invalid library configuration: {name} must be between {MIN_PANEL_WIDTH} and {MAX_PANEL_WIDTH}"
                    );
                }
            }
            Ok(config)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error).context("failed to read library configuration"),
    }
}

pub fn save_config(path: &Path, config: &Config) -> Result<()> {
    config
        .routing
        .validate()
        .context("invalid routing configuration")?;
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent).context("failed to create configuration directory")?;
    let bytes = serde_json::to_vec_pretty(config)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .context("failed to create temporary configuration")?;
    temporary
        .write_all(&bytes)
        .context("failed to write configuration")?;
    temporary
        .as_file()
        .sync_all()
        .context("failed to flush configuration")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("failed to replace configuration")?;
    Ok(())
}

pub fn read_bounded(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("file exceeds {} MiB", MAX_BYTES / 1024 / 1024);
    }
    Ok(bytes)
}

pub fn read_track(path: &Path) -> Result<Vec<u8>> {
    let bytes =
        read_bounded(File::open(path).with_context(|| format!("open {}", path.display()))?)?;
    if extension(path) == "vgz" || bytes.starts_with(&[0x1f, 0x8b]) {
        read_bounded(GzDecoder::new(Cursor::new(bytes))).context("decompress VGZ")
    } else {
        Ok(bytes)
    }
}

pub fn read_track_info(path: &Path) -> Result<Track> {
    let bytes = read_track(path)?;
    let is_mdx = extension(path) == "mdx";
    let metadata = if is_mdx {
        probe_mdx_bytes(&bytes).context("invalid MDX")
    } else {
        probe_vgm_bytes(&bytes).context("invalid VGM")
    }?;
    let chips = Track::track_chips(&bytes, is_mdx)?;
    Ok(Track::new(path.to_path_buf(), metadata, chips))
}

/// Find artwork in the track's directory without scanning subdirectories.
pub fn find_artwork(path: &Path) -> Result<Option<PathBuf>> {
    let directory = path
        .parent()
        .context("track file has no parent directory")?;
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    let mut images = Vec::new();
    for entry in fs::read_dir(directory).context("read artwork directory")? {
        let entry = entry.context("read artwork directory entry")?;
        if !entry
            .file_type()
            .context("read artwork file type")?
            .is_file()
        {
            continue;
        }
        let path = entry.path();
        if !matches!(
            extension(&path).as_str(),
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "bmp"
                | "tif"
                | "tiff"
                | "ico"
                | "tga"
                | "dds"
                | "exr"
                | "ff"
                | "hdr"
                | "pnm"
                | "pbm"
                | "pgm"
                | "ppm"
                | "pam"
                | "qoi"
        ) {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let priority = match stem.to_ascii_lowercase().as_str() {
            "cover" => 0,
            "folder" => 1,
            "front" => 2,
            _ => 3,
        };
        let name = entry.file_name().to_string_lossy().to_lowercase();
        images.push((priority, name, path));
    }
    images.sort();
    Ok(images.into_iter().next().map(|(_, _, path)| path))
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanProgress {
    pub files: usize,
    pub errors: usize,
}

#[cfg(test)]
fn scan(directories: &[PathBuf]) -> ScanResult {
    scan_with_progress(directories, |_| {})
}

pub fn scan_with_progress(
    directories: &[PathBuf],
    mut report: impl FnMut(ScanProgress),
) -> ScanResult {
    let mut tracks = Vec::new();
    let mut tree = Vec::<DirectoryEntry>::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut files = 0;
    for directory in directories {
        let canonical_root = match directory.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                tree.push(DirectoryEntry {
                    path: directory.clone(),
                    root: directory.clone(),
                    depth: 0,
                    has_children: false,
                });
                errors.push(format!("{}: {error}", directory.display()));
                report(ScanProgress {
                    files,
                    errors: errors.len(),
                });
                continue;
            }
        };
        let root_error = match fs::metadata(&canonical_root) {
            Ok(metadata) if metadata.is_dir() => None,
            Ok(_) => Some("not a directory".to_string()),
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = root_error {
            tree.push(DirectoryEntry {
                path: canonical_root,
                root: directory.clone(),
                depth: 0,
                has_children: false,
            });
            errors.push(format!("{}: {error}", directory.display()));
            report(ScanProgress {
                files,
                errors: errors.len(),
            });
            continue;
        }
        let mut parents: Vec<usize> = Vec::new();
        for item in WalkDir::new(&canonical_root)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry = match item {
                Ok(entry) => entry,
                Err(error) => {
                    errors.push(error.to_string());
                    report(ScanProgress {
                        files,
                        errors: errors.len(),
                    });
                    continue;
                }
            };
            if entry.file_type().is_dir() {
                let depth = entry.depth();
                parents.truncate(depth);
                if let Some(&parent) = parents.last() {
                    tree[parent].has_children = true;
                }
                parents.push(tree.len());
                tree.push(DirectoryEntry {
                    path: entry.path().to_path_buf(),
                    root: directory.clone(),
                    depth,
                    has_children: false,
                });
                continue;
            }
            if !entry.file_type().is_file()
                || !matches!(extension(entry.path()).as_str(), "vgm" | "vgz" | "mdx")
            {
                continue;
            }
            let path = match entry.path().canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    files += 1;
                    errors.push(format!("{}: {error}", entry.path().display()));
                    report(ScanProgress {
                        files,
                        errors: errors.len(),
                    });
                    continue;
                }
            };
            if !seen.insert(path.clone()) {
                continue;
            }
            files += 1;
            let result = read_track_info(&path);
            match result {
                Ok(track) => tracks.push(track),
                Err(error) => errors.push(format!("{}: {error:#}", path.display())),
            }
            report(ScanProgress {
                files,
                errors: errors.len(),
            });
        }
    }
    tracks.sort_unstable_by(|a, b| {
        a.path
            .parent()
            .cmp(&b.path.parent())
            .then_with(|| a.path.file_name().cmp(&b.path.file_name()))
    });
    let track_directories: HashSet<_> = tracks
        .iter()
        .flat_map(|track| track.path.ancestors().skip(1))
        .collect();
    tree.retain(|entry| track_directories.contains(entry.path.as_path()));
    for index in 0..tree.len() {
        tree[index].has_children = tree
            .get(index + 1)
            .is_some_and(|next| next.depth > tree[index].depth);
    }
    ScanResult {
        tracks,
        directories: tree,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_title_uses_game_and_falls_back_to_filename() {
        let path = PathBuf::from("music/song.vgm");
        let track = Track::new(
            path.clone(),
            TrackMetadata {
                title: Some("Metal Zone".into()),
                album: Some("Sonic".into()),
                ..Default::default()
            },
            vec![],
        );
        assert_eq!(track.display_title(), "Sonic - Metal Zone");
        let missing_title = Track::new(
            path.clone(),
            TrackMetadata {
                album: Some("Sonic".into()),
                ..Default::default()
            },
            vec![],
        );
        assert_eq!(missing_title.display_title(), "Sonic - song.vgm");
        let missing_game = Track::new(
            path,
            TrackMetadata {
                title: Some("Metal Zone".into()),
                album: Some("   ".into()),
                ..Default::default()
            },
            vec![],
        );
        assert_eq!(missing_game.display_title(), "Metal Zone");
    }

    #[test]
    fn scan_orders_by_directory_then_filename_not_metadata_title() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        for (path, title) in [
            (root.join("b.vgm"), "Alpha"),
            (root.join("a.vgm"), "Zulu"),
            (sub.join("a.vgm"), "Earlier"),
        ] {
            let mut builder = soundlog::vgm::VgmBuilder::new();
            builder.set_gd3(soundlog::meta::Gd3 {
                track_name_en: Some(title.into()),
                ..Default::default()
            });
            fs::write(path, Vec::<u8>::from(builder.finalize())).unwrap();
        }
        let result = scan(&[root.to_path_buf()]);
        assert!(result.errors.is_empty());
        let paths: Vec<_> = result
            .tracks
            .iter()
            .map(|track| track.path.strip_prefix(root).unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                Path::new("a.vgm"),
                Path::new("b.vgm"),
                Path::new("sub/a.vgm")
            ]
        );
        assert_eq!(
            filter_indices(&result.tracks, "", Some(root)),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn legacy_config_defaults_widths_and_invalid_width_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.json");
        fs::write(&path, r#"{"directories":[],"playlist":[]}"#).unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.library_width, 312.);
        assert_eq!(config.playlist_width, None);
        fs::write(
            &path,
            r#"{"directories":[],"playlist":[],"library_width":-1}"#,
        )
        .unwrap();
        assert!(
            load_config(&path)
                .unwrap_err()
                .to_string()
                .contains("library_width")
        );
    }

    #[test]
    fn directory_tree_and_filter_include_descendants_without_prefix_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let album = root.join("album");
        let nested = album.join("disc");
        let sibling = root.join("album-extra");
        let empty = root.join("empty");
        for path in [&nested, &sibling, &empty] {
            fs::create_dir_all(path).unwrap();
        }
        fs::create_dir(sibling.join("empty-child")).unwrap();
        let bytes = Vec::<u8>::from(soundlog::vgm::VgmBuilder::new().finalize());
        for path in [
            root.join("root.vgm"),
            album.join("a.vgm"),
            nested.join("b.vgm"),
            sibling.join("c.vgm"),
        ] {
            fs::write(path, &bytes).unwrap();
        }
        let result = scan(&[root.clone(), nested.clone()]);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.tracks.len(), 4);
        assert_eq!(result.directories.len(), 5);
        assert!(!result.directories.iter().any(|entry| entry.path == empty));
        assert!(
            result
                .directories
                .iter()
                .any(|entry| { entry.path == sibling && !entry.has_children })
        );
        let selected: Vec<_> = filter_indices(&result.tracks, "", Some(&album))
            .into_iter()
            .map(|index| result.tracks[index].path.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(selected, ["a.vgm", "b.vgm"]);
        assert_eq!(
            filter_indices(&result.tracks, "b.vgm", Some(&album)).len(),
            1
        );
        assert!(filter_indices(&result.tracks, "c.vgm", Some(&album)).is_empty());
        assert!(filter_indices(&result.tracks, "", Some(&empty)).is_empty());

        let mut expanded = HashMap::from([
            (root.clone(), HashSet::from([root.clone()])),
            (nested.clone(), HashSet::from([nested.clone()])),
        ]);
        let visible = visible_directories(&result.directories, &expanded);
        let paths: Vec<_> = visible
            .iter()
            .map(|&index| &result.directories[index].path)
            .collect();
        assert_eq!(paths, [&root, &album, &sibling, &nested]);
        expanded.get_mut(&root).unwrap().insert(album.clone());
        let visible = visible_directories(&result.directories, &expanded);
        let paths: Vec<_> = visible
            .iter()
            .map(|&index| &result.directories[index].path)
            .collect();
        assert_eq!(paths, [&root, &album, &nested, &sibling, &nested]);
    }

    #[test]
    fn empty_root_and_invalid_tracks_are_not_shown() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        let invalid = dir.path().join("invalid");
        fs::create_dir_all(empty.join("nested")).unwrap();
        fs::create_dir(&invalid).unwrap();
        fs::write(invalid.join("broken.vgm"), b"not a vgm").unwrap();

        let result = scan(&[empty, invalid]);
        assert!(result.tracks.is_empty());
        assert!(result.directories.is_empty());
        assert_eq!(result.errors.len(), 1);
    }

    #[test]
    fn missing_or_non_directory_roots_report_errors_without_tree_entries() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let file = dir.path().join("file");
        fs::write(&file, b"data").unwrap();
        let result = scan(&[missing.clone(), file.clone()]);
        assert_eq!(result.errors.len(), 2);
        assert!(result.directories.is_empty());
        assert!(result.tracks.is_empty());
    }

    #[test]
    fn artwork_prefers_named_images_then_sorts_and_stays_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("song.vgm");
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("cover.png"), b"image").unwrap();
        fs::write(dir.path().join("notes.txt"), b"not an image").unwrap();
        assert_eq!(find_artwork(&track).unwrap(), None);
        fs::write(dir.path().join("z.JPG"), b"image").unwrap();
        fs::write(dir.path().join("A.png"), b"image").unwrap();
        assert_eq!(
            find_artwork(&track).unwrap(),
            Some(dir.path().join("A.png"))
        );
        fs::write(dir.path().join("Front.webp"), b"image").unwrap();
        assert_eq!(
            find_artwork(&track).unwrap(),
            Some(dir.path().join("Front.webp"))
        );
        fs::write(dir.path().join("FOLDER.gif"), b"image").unwrap();
        assert_eq!(
            find_artwork(&track).unwrap(),
            Some(dir.path().join("FOLDER.gif"))
        );
        fs::write(dir.path().join("cover.PNG"), b"image").unwrap();
        fs::write(dir.path().join("cover.jpg"), b"image").unwrap();
        assert_eq!(
            find_artwork(&track).unwrap(),
            Some(dir.path().join("cover.jpg"))
        );
        assert!(find_artwork(&nested.join("missing").join("song.vgm")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_directory_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("music");
        let outside = dir.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let bytes = Vec::<u8>::from(soundlog::vgm::VgmBuilder::new().finalize());
        fs::write(outside.join("song.vgm"), bytes).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let result = scan(&[root]);
        assert!(result.errors.is_empty());
        assert!(result.tracks.is_empty());
        assert!(result.directories.is_empty());
    }

    #[test]
    fn filtering_preserves_order_and_searches_each_field() {
        let tracks = vec![
            Track::new(
                PathBuf::from("/music/first.mdx"),
                TrackMetadata {
                    title: Some("Opening".into()),
                    artist: Some("Composer".into()),
                    album: Some("Album".into()),
                    comment: Some("Loop notes".into()),
                },
                vec![],
            ),
            Track::new(
                PathBuf::from("/music/second.mdx"),
                TrackMetadata {
                    artist: Some("COMPOSER".into()),
                    ..Default::default()
                },
                vec![],
            ),
        ];
        assert_eq!(filter_indices(&tracks, " ", None), vec![0, 1]);
        assert_eq!(filter_indices(&tracks, " cOmPoSeR ", None), vec![0, 1]);
        assert_eq!(filter_indices(&tracks, "opening", None), vec![0]);
        assert_eq!(filter_indices(&tracks, "album", None), vec![0]);
        assert_eq!(filter_indices(&tracks, "notes", None), vec![0]);
        assert_eq!(filter_indices(&tracks, "SECOND.MDX", None), vec![1]);
        assert!(filter_indices(&tracks, "openingcomposer", None).is_empty());
        assert!(filter_indices(&tracks, "no match", None).is_empty());
    }

    #[test]
    fn chip_filter_requires_every_chip_and_combines_with_text_and_directory() {
        let tracks = vec![
            Track::new(
                PathBuf::from("/music/one/a.vgm"),
                TrackMetadata {
                    title: Some("Opening".into()),
                    ..Default::default()
                },
                vec![ChipTag::new(&Chip::Ym2151), ChipTag::new(&Chip::Okim6258)],
            ),
            Track::new(
                PathBuf::from("/music/one/b.vgm"),
                TrackMetadata {
                    title: Some("Opening".into()),
                    ..Default::default()
                },
                vec![ChipTag::new(&Chip::Ym2151)],
            ),
            Track::new(
                PathBuf::from("/music/two/c.vgm"),
                TrackMetadata {
                    title: Some("Ending".into()),
                    ..Default::default()
                },
                vec![ChipTag::new(&Chip::Ym2151), ChipTag::new(&Chip::Okim6258)],
            ),
        ];
        let chips = vec!["YM2151".into(), "OKIM6258".into()];
        assert_eq!(
            filter_indices_with_chips(&tracks, "", None, &chips),
            vec![0, 2]
        );
        assert_eq!(
            filter_indices_with_chips(&tracks, "opening", None, &chips),
            vec![0]
        );
        assert_eq!(
            filter_indices_with_chips(&tracks, "", Some(Path::new("/music/two")), &chips),
            vec![2]
        );
        assert!(filter_indices_with_chips(&tracks, "", None, &["YM2612".into()]).is_empty());
    }

    #[test]
    fn filtering_large_library_returns_original_track_indices() {
        let tracks: Vec<_> = (0..10_000)
            .map(|index| {
                Track::new(
                    PathBuf::from(format!("/music/song-{index:05}.mdx")),
                    TrackMetadata::default(),
                    vec![],
                )
            })
            .collect();
        let all = filter_indices(&tracks, "", None);
        assert_eq!(all.len(), 10_000);
        assert_eq!(all[0], 0);
        assert_eq!(all[9_999], 9_999);
        assert_eq!(filter_indices(&tracks, "song-09999.mdx", None), vec![9_999]);
        assert!(filter_indices(&tracks, "not present", None).is_empty());
    }

    #[test]
    fn ymfm_names_match_chip_tag_classification() {
        for chip in YMFM_CHIPS {
            let tag = ChipTag::new(&chip);
            assert!(tag.ymfm);
            assert!(ChipTag::is_ymfm_name(tag.name), "{}", tag.name);
        }
        for chip in [Chip::Sn76489, Chip::Okim6258, Chip::Ymf271, Chip::Ymz280b] {
            let tag = ChipTag::new(&chip);
            assert!(!tag.ymfm);
            assert!(!ChipTag::is_ymfm_name(tag.name), "{}", tag.name);
        }
        assert!(!ChipTag::is_ymfm_name("unknown"));
    }

    #[test]
    fn chip_tags_identify_software_backend_support() {
        for chip in YMFM_CHIPS.into_iter().chain([
            Chip::Sn76489,
            Chip::SegaPcm,
            Chip::C140,
            Chip::Pwm,
            Chip::Okim6258,
            Chip::Okim6295,
        ]) {
            assert!(ChipTag::new(&chip).software_supported(), "{chip:?}");
        }
        for chip in [Chip::Pokey, Chip::Ay8910, Chip::Ymf271, Chip::Ymz280b] {
            assert!(!ChipTag::new(&chip).software_supported(), "{chip:?}");
        }
    }

    #[test]
    fn vgm_header_marks_unsupported_chips_in_mixed_tracks() {
        use soundlog::vgm::command::Instance;

        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Pokey, Instance::Primary, 1_789_790);
        let tags = Track::track_chips(&Vec::<u8>::from(builder.finalize()), false).unwrap();
        let unsupported: Vec<_> = tags
            .iter()
            .filter(|tag| !tag.software_supported())
            .map(|tag| tag.name)
            .collect();
        assert_eq!(unsupported, ["POKEY"]);
    }

    #[test]
    fn playlist_track_info_loads_chip_support_from_vgm_and_vgz() {
        use flate2::{Compression, write::GzEncoder};
        use soundlog::vgm::command::Instance;

        let directory = tempfile::tempdir().unwrap();
        for (extension, unsupported) in [("vgm", false), ("vgz", true)] {
            let mut builder = soundlog::vgm::VgmBuilder::new();
            builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
            if unsupported {
                builder.register_chip(Chip::Pokey, Instance::Primary, 1_789_790);
            }
            let bytes = Vec::<u8>::from(builder.finalize());
            let path = directory.path().join(format!("track.{extension}"));
            if extension == "vgz" {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&bytes).unwrap();
                fs::write(&path, encoder.finish().unwrap()).unwrap();
            } else {
                fs::write(&path, bytes).unwrap();
            }
            let track = read_track_info(&path).unwrap();
            let labels: Vec<_> = track
                .chips
                .iter()
                .filter(|chip| !chip.software_supported())
                .map(ChipTag::label)
                .collect();
            assert_eq!(labels, if unsupported { vec!["POKEY"] } else { vec![] });
        }
    }

    #[test]
    fn chip_labels_cover_vgm_instances_and_mdx_pdx_declarations() {
        use soundlog::vgm::command::Instance;

        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
        builder.register_chip(Chip::Ym2612, Instance::Secondary, 7_670_454);
        builder.register_chip(Chip::Ym2203, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Ym2203, Instance::Secondary, 4_000_000);
        builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
        let vgm = Vec::<u8>::from(builder.finalize());
        let tags = Track::track_chips(&vgm, false).unwrap();
        assert_eq!(
            tags.iter().map(ChipTag::label).collect::<Vec<_>>(),
            ["SN76489", "YM2612x2", "YM2203x2"]
        );
        assert!(!tags[0].ymfm);
        assert!(tags[1].ymfm);
        assert!(tags[2].ymfm);
        let track = Track::new(PathBuf::from("music.vgm"), TrackMetadata::default(), tags);
        assert_eq!(
            filter_indices_with_chips(&[track], "", None, &["YM2203".into()]),
            [0]
        );
        let empty_vgm = Vec::<u8>::from(soundlog::vgm::VgmBuilder::new().finalize());
        assert!(Track::track_chips(&empty_vgm, false).unwrap().is_empty());

        let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
        assert_eq!(
            Track::track_chips(mdx, true).unwrap(),
            vec![ChipTag::new(&Chip::Ym2151), ChipTag::new(&Chip::Okim6258)]
        );
        let (header, _) = MdxHeader::parse(mdx).unwrap();
        let mut no_pdx = mdx.to_vec();
        let title_end = header.title_byte_len() + 3;
        no_pdx.drain(title_end..title_end + header.pdx_name_byte_len());
        assert_eq!(
            Track::track_chips(&no_pdx, true).unwrap(),
            vec![ChipTag::new(&Chip::Ym2151)]
        );
        assert!(Track::track_chips(b"broken", true).is_err());
        assert!(Track::track_chips(b"broken", false).is_err());
    }

    #[test]
    fn scan_progress_counts_files_and_reports_all_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = Vec::<u8>::from(soundlog::vgm::VgmBuilder::new().finalize());
        fs::write(dir.path().join("good.vgm"), good).unwrap();
        fs::write(dir.path().join("bad.vgm"), b"broken").unwrap();
        let mut updates = Vec::new();
        let result = scan_with_progress(
            &[dir.path().to_path_buf(), dir.path().join("missing")],
            |progress| updates.push(progress),
        );
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(result.errors.len(), 2);
        assert_eq!(
            updates.last(),
            Some(&ScanProgress {
                files: 2,
                errors: 2
            })
        );
        assert!(
            updates
                .windows(2)
                .all(|pair| { pair[0].files <= pair[1].files && pair[0].errors <= pair[1].errors })
        );
    }

    #[test]
    fn scan_nested_tracks_and_persist_paths() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.set_gd3(soundlog::meta::Gd3 {
            track_name_en: Some("Test song".into()),
            author_name_en: Some("Composer".into()),
            game_name_en: Some("Test game".into()),
            ..Default::default()
        });
        fs::write(nested.join("song.vgm"), Vec::<u8>::from(builder.finalize())).unwrap();
        let ScanResult { tracks, errors, .. } = scan(&[dir.path().to_path_buf(), nested]);
        assert!(errors.is_empty());
        assert_eq!(tracks.len(), 1);
        assert_eq!(
            read_track_info(&tracks[0].path).unwrap().metadata,
            tracks[0].metadata
        );
        assert_eq!(tracks[0].display_title(), "Test game - Test song");
        assert_eq!(filter_indices(&tracks, "composer", None), vec![0]);
        assert!(filter_indices(&tracks, "not found", None).is_empty());
        let config = Config {
            directories: vec![dir.path().to_path_buf()],
            playlist: vec![tracks[0].path.clone()],
            library_width: 410.,
            playlist_width: Some(380.),
            routing: crate::routing::RoutingConfig::default(),
        };
        let path = dir.path().join("config/config.json");
        save_config(&path, &config).unwrap();
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.directories, config.directories);
        assert_eq!(loaded.playlist, config.playlist);
        assert_eq!(loaded.library_width, 410.);
        assert_eq!(loaded.playlist_width, Some(380.));
    }

    #[test]
    fn malformed_input_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.vgz"), b"broken").unwrap();
        let ScanResult { tracks, errors, .. } = scan(&[dir.path().to_path_buf()]);
        assert!(tracks.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(read_track_info(&dir.path().join("bad.vgz")).is_err());
    }

    #[test]
    fn routing_configuration_is_saved_and_invalid_routes_are_reported() {
        use crate::routing::{ChipInstance, Connection, Device, HardwareChip, Profile};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.json");
        let mut config = Config::default();
        config.routing.devices.push(Device {
            id: 1,
            name: "NanoDrive".into(),
            port: "serial-port".into(),
            baud_rate: 115_200,
        });
        config.routing.profiles.push(Profile {
            id: 2,
            name: "Hardware".into(),
            connections: vec![Connection {
                chip: HardwareChip::Ym2151,
                source: ChipInstance::Secondary,
                device_id: 1,
                target: ChipInstance::Primary,
            }],
        });
        config.routing.selected_profile = Some(2);
        save_config(&path, &config).unwrap();
        assert_eq!(load_config(&path).unwrap().routing, config.routing);
        config.routing.devices.clear();
        assert!(
            format!("{:#}", save_config(&path, &config).unwrap_err()).contains("unknown device")
        );
        assert_eq!(load_config(&path).unwrap().routing.devices.len(), 1);
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        assert!(format!("{:#}", load_config(&path).unwrap_err()).contains("unknown device"));
    }

    #[test]
    fn scan_supports_vgz_and_mdx() {
        use flate2::{Compression, write::GzEncoder};

        let dir = tempfile::tempdir().unwrap();
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(
            Chip::Ym2151,
            soundlog::vgm::command::Instance::Primary,
            4_000_000,
        );
        let bytes = Vec::<u8>::from(builder.finalize());
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes).unwrap();
        fs::write(dir.path().join("song.VGZ"), encoder.finish().unwrap()).unwrap();
        fs::write(
            dir.path().join("song.MDX"),
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
        )
        .unwrap();
        let ScanResult { tracks, errors, .. } = scan(&[dir.path().to_path_buf()]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(tracks.len(), 2);
        for track in &tracks {
            assert_eq!(
                read_track_info(&track.path).unwrap().metadata,
                track.metadata
            );
        }
        assert_eq!(
            tracks
                .iter()
                .find(|track| extension(&track.path) == "vgz")
                .unwrap()
                .chips,
            vec![ChipTag::new(&Chip::Ym2151)]
        );
        assert_eq!(
            tracks
                .iter()
                .find(|track| extension(&track.path) == "mdx")
                .unwrap()
                .chips,
            vec![ChipTag::new(&Chip::Ym2151), ChipTag::new(&Chip::Okim6258)]
        );
    }
}
