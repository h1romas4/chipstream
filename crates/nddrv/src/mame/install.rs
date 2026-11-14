//! Installation of the bundled MAME plugins.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};

const LICENSE: &str = include_str!("../../assets/mame-plugins/LICENSE");
const FILES: [(&str, &str); 6] = [
    (
        "nanodrive6/init.lua",
        include_str!("../../assets/mame-plugins/nanodrive6/init.lua"),
    ),
    (
        "nanodrive6/plugin.json",
        include_str!("../../assets/mame-plugins/nanodrive6/plugin.json"),
    ),
    ("nanodrive6/LICENSE", LICENSE),
    (
        "nanodrive8/init.lua",
        include_str!("../../assets/mame-plugins/nanodrive8/init.lua"),
    ),
    (
        "nanodrive8/plugin.json",
        include_str!("../../assets/mame-plugins/nanodrive8/plugin.json"),
    ),
    ("nanodrive8/LICENSE", LICENSE),
];

pub fn install(home: &Path) -> Result<()> {
    if !home.is_dir() {
        bail!("MAME HOME is not an existing directory: {}", home.display());
    }
    let plugins = home.join("plugins");
    for (name, contents) in FILES {
        let path = plugins.join(name);
        let parent = path
            .parent()
            .context("Plugin file has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Cannot create {}; installation may be incomplete",
                parent.display()
            )
        })?;
        let mut file = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "Cannot create temporary file in {}; installation may be incomplete",
                parent.display()
            )
        })?;
        file.write_all(contents.as_bytes()).with_context(|| {
            format!(
                "Cannot write {}; installation may be incomplete",
                path.display()
            )
        })?;
        file.persist(&path).with_context(|| {
            format!(
                "Cannot replace {}; installation may be incomplete",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_all_files_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        install(home.path()).unwrap();
        for (name, contents) in FILES {
            assert_eq!(
                fs::read(home.path().join("plugins").join(name)).unwrap(),
                contents.as_bytes()
            );
        }
        install(home.path()).unwrap();
    }

    #[test]
    fn overwrites_existing_plugin_files() {
        let home = tempfile::tempdir().unwrap();
        let plugin = home.path().join("plugins/nanodrive8");
        fs::create_dir_all(&plugin).unwrap();
        fs::write(plugin.join("init.lua"), "custom plugin").unwrap();
        fs::write(plugin.join("extra.lua"), "keep").unwrap();
        install(home.path()).unwrap();
        assert_eq!(
            fs::read_to_string(plugin.join("init.lua")).unwrap(),
            include_str!("../../assets/mame-plugins/nanodrive8/init.lua")
        );
        assert_eq!(
            fs::read_to_string(plugin.join("extra.lua")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn rejects_missing_home_and_invalid_plugin_directory() {
        let home = tempfile::tempdir().unwrap();
        assert!(install(&home.path().join("missing")).is_err());
        fs::write(home.path().join("plugins"), "not a directory").unwrap();
        assert!(install(home.path()).is_err());
    }
}
