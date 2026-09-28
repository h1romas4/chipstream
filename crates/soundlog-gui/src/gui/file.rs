use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use eframe::egui;

pub(crate) struct FileLoadMessage {
    pub(crate) generation: u64,
    pub(crate) path: PathBuf,
    pub(crate) result: Result<(Vec<u8>, Vec<u8>), String>,
}

pub(crate) fn spawn_file_load(
    path: PathBuf,
    generation: u64,
    tx: mpsc::Sender<FileLoadMessage>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        let result = fs::read(&path)
            .map(|bytes| {
                let parse_bytes = bytes.clone();
                (bytes, parse_bytes)
            })
            .map_err(|error| error.to_string());
        let _ = tx.send(FileLoadMessage {
            generation,
            path,
            result,
        });
        ctx.request_repaint();
    });
}
