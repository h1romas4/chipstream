//! Discover sound-chip clocks using the configured MAME installation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);

pub fn ym2151_clock(home: &Path, machine: &str, tag: &str, stop: &AtomicBool) -> Result<u32> {
    let xml = query_xml(home, machine, stop)?;
    parse_clock(&xml, machine, tag)
}

pub fn nd7_clocks(
    home: &Path,
    machine: &str,
    routes: &[super::protocol::Nd7Route],
    stop: &AtomicBool,
) -> Result<Vec<Result<u32>>> {
    let xml = query_xml(home, machine, stop)?;
    let document = parse_document(&xml)?;
    let system = find_machine(&document, machine)?;
    Ok(routes
        .iter()
        .map(|route| {
            let types: &[&str] = match route.write_id {
                0x51 => &["ym2413"],
                0x55 => &["ym2203"],
                0x5a => &["ym3812"],
                0x5b => &["ym3526"],
                0x5e => &["ymf262"],
                0xa0 => &["ay8910", "ay8912", "ay8913", "ym2149"],
                _ => bail!("Unsupported ND7 chip"),
            };
            let (hz, chip_type) = find_chip(system, &route.tag, types)?;
            if route.divide_clock && chip_type != "ym2149" {
                bail!("Only YM2149 supports the SEL clock divider");
            }
            let hz = if route.divide_clock { hz / 2 } else { hz };
            if hz == 0 {
                bail!("MAME chip clock is zero");
            }
            Ok(hz)
        })
        .collect())
}

fn query_xml(home: &Path, machine: &str, stop: &AtomicBool) -> Result<String> {
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
    String::from_utf8(output).context("MAME XML is not UTF-8")
}

fn parse_clock(xml: &str, machine: &str, tag: &str) -> Result<u32> {
    let document = parse_document(xml)?;
    let system = find_machine(&document, machine)?;
    Ok(find_chip(system, tag, &["ym2151"])?.0)
}

fn parse_document(xml: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .context("Invalid MAME XML")
}

fn find_machine<'a, 'input>(
    document: &'a roxmltree::Document<'input>,
    machine: &str,
) -> Result<roxmltree::Node<'a, 'input>> {
    let mut machines = document
        .descendants()
        .filter(|node| node.has_tag_name("machine") && node.attribute("name") == Some(machine));
    let system = machines
        .next()
        .context("MAME XML does not contain the requested machine")?;
    if machines.next().is_some() {
        bail!("MAME XML contains duplicate machines");
    }
    Ok(system)
}

fn find_chip<'a, 'input>(
    system: roxmltree::Node<'a, 'input>,
    tag: &str,
    types: &[&str],
) -> Result<(u32, &'a str)> {
    let expected = tag.trim_start_matches(':');
    let matching = |node: &roxmltree::Node<'_, '_>| {
        node.attribute("tag")
            .is_some_and(|value| value.trim_start_matches(':') == expected)
    };
    let mut refs = system.children().filter(|node| {
        node.has_tag_name("device_ref")
            && node
                .attribute("name")
                .is_some_and(|name| types.contains(&name))
            && matching(node)
    });
    let chip_type = refs
        .next()
        .and_then(|node| node.attribute("name"))
        .context("MAME XML does not contain the requested chip type")?;
    let mut chips = system.children().filter(|node| {
        node.has_tag_name("chip") && node.attribute("type") == Some("audio") && matching(node)
    });
    let chip = chips
        .next()
        .context("MAME XML does not contain the requested audio chip")?;
    if refs.next().is_some() || chips.next().is_some() {
        bail!("MAME XML does not uniquely identify the sound chip");
    }
    let hz: u32 = chip
        .attribute("clock")
        .context("MAME XML has no sound-chip clock")?
        .parse()
        .context("Invalid sound-chip clock")?;
    if hz == 0 {
        bail!("Sound-chip clock is zero");
    }
    Ok((hz, chip_type))
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

    #[cfg(unix)]
    #[test]
    fn queries_nd7_clocks_and_checks_ym2149_divider() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let executable = home.path().join("mame");
        std::fs::write(&executable, b"#!/bin/sh\nprintf '%s' '<mame><machine name=\"game\"><device_ref name=\"ym2149\" tag=\"psg\"/><chip type=\"audio\" tag=\"psg\" clock=\"4000000\"/><device_ref name=\"ay8910\" tag=\"ay\"/><chip type=\"audio\" tag=\"ay\" clock=\"2000000\"/></machine></mame>'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let route = |tag: &str, divide_clock| super::super::protocol::Nd7Route {
            write_id: 0xa0,
            instance: 0,
            tag: tag.into(),
            divide_clock,
        };
        let results = nd7_clocks(
            home.path(),
            "game",
            &[route(":psg", true), route(":ay", false), route(":ay", true)],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(*results[0].as_ref().unwrap(), 2_000_000);
        assert_eq!(*results[1].as_ref().unwrap(), 2_000_000);
        assert!(results[2].is_err());
    }
}
