use std::{
    env, fs,
    io::{self, ErrorKind},
    path::PathBuf,
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtasks: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let task = env::args().nth(1).ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            "usage: cargo run -p xtasks -- install-gstreamer",
        )
    })?;

    match task.as_str() {
        "install-gstreamer" => install_gstreamer(),
        _ => Err(io::Error::new(ErrorKind::InvalidInput, format!("unknown task: {task}")).into()),
    }
}

fn install_gstreamer() -> Result<(), Box<dyn std::error::Error>> {
    if env::consts::OS != "linux" {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "install-gstreamer is supported only on Linux",
        )
        .into());
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.ancestors().nth(3).ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidData,
            "xtasks must be under crates/chipstream-gstreamer",
        )
    })?;

    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "chipstream-gstreamer"])
        .current_dir(workspace_root)
        .status()?;
    if !status.success() {
        return Err(
            io::Error::other(format!("GStreamer plugin build failed with {status}")).into(),
        );
    }

    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let target_dir = if target_dir.is_absolute() {
        target_dir
    } else {
        workspace_root.join(target_dir)
    };
    let source = target_dir.join("release/libgstchipstream.so");
    let home_dir = env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        io::Error::new(ErrorKind::NotFound, "HOME environment variable is not set")
    })?;
    let destination_dir = home_dir.join(".local/lib/chipstream");
    fs::create_dir_all(&destination_dir)?;
    let destination = destination_dir.join("libgstchipstream.so");
    fs::copy(&source, &destination)?;

    println!(
        "Installed {} to {}",
        source.display(),
        destination.display()
    );
    Ok(())
}
