use anyhow::bail;
use std::path::PathBuf;

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
            let bytes = soundlog_gui::load_decompressed_bytes(&path)?;
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
