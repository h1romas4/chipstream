use std::path::PathBuf;

/// Optional output directory for VGM test artifacts (relative to the crate root).
///
/// Behavior:
/// - If the environment variable `SOUNDLOG_TEST_OUTPUT_VGM` is set to a non-empty
///   path, that path is used (relative paths are interpreted relative to the
///   crate root when tests are run from there).
/// - If the env var is not set or is empty, the function returns `None` (no output).
///
/// Usage:
/// - Enable output only when needed: SOUNDLOG_TEST_OUTPUT_VGM=assets/vgm cargo test
/// - Default (no env var): no files written to the crate tree (safe for `cargo publish`)
pub fn output_vgm_dir() -> Option<PathBuf> {
    match std::env::var("SOUNDLOG_TEST_OUTPUT_VGM") {
        Ok(s) if !s.is_empty() => Some(PathBuf::from(s)),
        _ => None,
    }
}

pub fn maybe_write_vgm(filename: &str, bytes: &[u8]) {
    if let Some(dir) = output_vgm_dir() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let out_dir = std::path::Path::new(manifest).join(dir);
        if let Err(e) = std::fs::create_dir_all(&out_dir) {
            eprintln!("warning: could not create output dir {:?}: {}", out_dir, e);
        } else {
            let out_path = out_dir.join(filename);
            if let Err(e) = std::fs::write(&out_path, bytes) {
                eprintln!("warning: failed to write vgm file {:?}: {}", out_path, e);
            } else {
                eprintln!("Wrote test VGM to {:?}", out_path);
            }
        }
    }
}

#[path = "event/ym2612.rs"]
mod ym2612;

#[path = "event/sn76489.rs"]
mod sn76489;

#[path = "event/ym2203.rs"]
mod ym2203;

#[path = "event/ym2608.rs"]
mod ym2608;

#[path = "event/ym2610b.rs"]
mod ym2610b;

#[path = "event/ay8910.rs"]
mod ay8910;

#[path = "event/gamegear.rs"]
mod gamegear;

#[path = "event/pokey.rs"]
mod pokey;

#[path = "event/ym2151.rs"]
mod ym2151;

#[path = "event/ym2413.rs"]
mod ym2413;

#[path = "event/ym3526.rs"]
mod ym3526;

#[path = "event/ym3812.rs"]
mod ym3812;

#[path = "event/ymf262.rs"]
mod ymf262;

#[path = "event/ymf278b.rs"]
mod ymf278b;

#[path = "event/y8950.rs"]
mod y8950;

#[path = "event/ymf271.rs"]
mod ymf271;

#[path = "event/k051649.rs"]
mod k051649;

#[path = "event/gb_dmg.rs"]
mod gb_dmg;

#[path = "event/nes_apu.rs"]
mod nes_apu;

#[path = "event/huc6280.rs"]
mod huc6280;

#[path = "event/wonderswan.rs"]
mod wonderswan;

#[path = "event/vsu.rs"]
mod vsu;

#[path = "event/saa1099.rs"]
mod saa1099;
