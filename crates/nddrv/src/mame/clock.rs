//! Discover the YM2151 clock using the configured MAME installation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);

pub fn ym2151_clock(home: &Path, machine: &str, tag: &str, stop: &AtomicBool) -> Result<u32> {
    let executable = mame_executable(home)?;
    let mut command = Command::new(&executable);
    command
        .args(["-noreadconfig", "-listxml", machine])
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().context("Could not run MAME -listxml")?;
    let output = child.stdout.take().context("MAME stdout is unavailable")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.take(MAX_XML_BYTES + 1).read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    let start = Instant::now();
    let result = loop {
        if stop.load(Ordering::Acquire) || start.elapsed() >= QUERY_TIMEOUT {
            child.kill().context("Could not stop MAME clock query")?;
            child.wait().context("Could not reap MAME clock query")?;
            break Err(anyhow::anyhow!("MAME clock query cancelled or timed out"));
        }
        match child
            .try_wait()
            .context("Could not wait for MAME clock query")?
        {
            Some(status) if status.success() => break Ok(()),
            Some(status) => break Err(anyhow::anyhow!("MAME -listxml failed: {status}")),
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let output = reader
        .join()
        .map_err(|_| anyhow::anyhow!("MAME XML reader failed"))??;
    result?;
    if output.len() as u64 > MAX_XML_BYTES {
        bail!("MAME XML exceeds the size limit");
    }
    parse_clock(
        std::str::from_utf8(&output).context("MAME XML is not UTF-8")?,
        machine,
        tag,
    )
}

fn parse_clock(xml: &str, machine: &str, tag: &str) -> Result<u32> {
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .context("Invalid MAME XML")?;
    let mut machines = document
        .descendants()
        .filter(|node| node.has_tag_name("machine") && node.attribute("name") == Some(machine));
    let system = machines
        .next()
        .context("MAME XML does not contain the requested machine")?;
    if machines.next().is_some() {
        bail!("MAME XML contains duplicate machines");
    }
    let expected = tag.trim_start_matches(':');
    let matching = |node: &roxmltree::Node<'_, '_>| {
        node.attribute("tag")
            .is_some_and(|value| value.trim_start_matches(':') == expected)
    };
    let refs = system
        .children()
        .filter(|node| {
            node.has_tag_name("device_ref")
                && node.attribute("name") == Some("ym2151")
                && matching(node)
        })
        .count();
    let mut chips = system.children().filter(|node| {
        node.has_tag_name("chip") && node.attribute("type") == Some("audio") && matching(node)
    });
    let chip = chips
        .next()
        .context("MAME XML does not contain the requested audio chip")?;
    if refs != 1 || chips.next().is_some() {
        bail!("MAME XML does not uniquely identify the YM2151");
    }
    let hz: u32 = chip
        .attribute("clock")
        .context("MAME XML has no YM2151 clock")?
        .parse()
        .context("Invalid YM2151 clock")?;
    if hz == 0 {
        bail!("YM2151 clock is zero");
    }
    Ok(hz)
}

pub(super) fn mame_executable(home: &Path) -> Result<PathBuf> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(home).context("Could not read MAME HOME")? {
        let entry = entry.context("Could not inspect MAME HOME")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        #[cfg(windows)]
        let valid = matches!(name.as_str(), "mame.exe" | "mame64.exe");
        #[cfg(not(windows))]
        let valid = matches!(name.as_str(), "mame" | "mame64");
        if valid && path.is_file() {
            let path = path
                .canonicalize()
                .context("Could not resolve MAME executable")?;
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }
    match candidates.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("MAME HOME does not contain a MAME executable"),
        _ => bail!("MAME HOME contains multiple MAME executables"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_exact_machine_and_chip() {
        let xml = r#"<mame><machine name="other"/><machine name="game"><device_ref name="ym2151" tag="sound:ym"/><chip type="audio" tag="sound:ym" clock="3579545"/></machine></mame>"#;
        assert_eq!(parse_clock(xml, "game", ":sound:ym").unwrap(), 3_579_545);
        assert!(parse_clock(xml, "other", ":sound:ym").is_err());
        assert!(parse_clock(xml, "game", ":other").is_err());
        assert!(parse_clock(&xml.replace("3579545", "0"), "game", ":sound:ym").is_err());
    }

    #[test]
    fn finds_one_mame_executable_in_home() {
        let home = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "mame.exe" } else { "mame" };
        let alternate = if cfg!(windows) {
            "mame64.exe"
        } else {
            "mame64"
        };
        assert!(mame_executable(home.path()).is_err());
        std::fs::write(home.path().join(name), b"").unwrap();
        assert_eq!(
            mame_executable(home.path()).unwrap(),
            home.path().join(name).canonicalize().unwrap()
        );
        std::fs::write(home.path().join(alternate), b"").unwrap();
        assert!(mame_executable(home.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn queries_clock_from_home_executable() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let executable = home.path().join("mame");
        std::fs::write(&executable, b"#!/bin/sh\nprintf '%s' '<mame><machine name=\"game\"><device_ref name=\"ym2151\" tag=\"ym\"/><chip type=\"audio\" tag=\"ym\" clock=\"4000000\"/></machine></mame>'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            ym2151_clock(home.path(), "game", ":ym", &AtomicBool::new(false)).unwrap(),
            4_000_000
        );
    }
}
