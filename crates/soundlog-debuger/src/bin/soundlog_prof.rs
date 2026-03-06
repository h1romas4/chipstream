//! Minimal profiling binary for soundlog.
//!
//! This binary embeds a VGM file (as static bytes), constructs a `VgmStream`
//! from it and iterates the entire stream. The purpose is purely performance
//! measurement: it does not print or perform any other work aside from a final
//! processed-item counter so the compiler cannot optimize the loop away.

use std::hint::black_box;

use soundlog::VgmStream;
use soundlog::vgm::stream::StreamResult;

/// Embedded VGM asset. This is included at compile time.
///
/// Path is relative to this file. It points to the example VGM shipped in the
/// `soundlog` crate's assets.
static REMDME_VGM: &[u8] = include_bytes!("../../../soundlog/assets/vgm/REMDME.vgm");

fn main() {
    // Obtain raw pointer/length (demonstrate an unsafe access to the static bytes)
    let ptr = REMDME_VGM.as_ptr();
    let len = REMDME_VGM.len();

    // SAFETY: `ptr`/`len` reference a valid `'static` byte slice (from `include_bytes!`).
    // We create a temporary slice via `from_raw_parts` and then clone it into a Vec<u8>.
    // This copies the data into an owned buffer so we avoid any unsafe ownership/UB issues.
    let data_vec: Vec<u8> = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();

    // Build the VgmStream from the owned Vec<u8>.
    let mut stream = match VgmStream::from_vgm(data_vec) {
        Ok(s) => s,
        Err(_e) => {
            // On parse error, exit the binary silently (no printing).
            return;
        }
    };

    // Ensure the stream does not loop infinitely on VGM files with loop points.
    stream.set_loop_count(Some(1));

    // Iterate the stream fully. We intentionally ignore most values (bind to `_`),
    // but update the counter on each item so the optimizer cannot elide the loop.
    for item in &mut stream {
        match item {
            Ok(StreamResult::Command(cmd)) => {
                // Observe the `cmd` value on the stack so the compiler cannot
                // optimize away stack/register traffic. Do not print it.
                let _ = black_box(cmd);
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            Err(_err) => break,
        }
    }
}
