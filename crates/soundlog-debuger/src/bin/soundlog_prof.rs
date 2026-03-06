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

// Main - Switch to parser mode to retrieve the profile.
fn main() {
    push_chunk();
    // from_vgm();
}

#[allow(dead_code)]
fn push_chunk() {
    // Create an owned `Vec<u8>` by copying the embedded bytes.
    // `to_vec()` performs an efficient memcpy.
    let data_vec: Vec<u8> = REMDME_VGM.to_vec();

    // Construct an empty stream and feed it incrementally with push_chunk (2048-byte chunks).
    let mut stream = VgmStream::new();

    // Ensure the stream does not loop infinitely on VGM files with loop points.
    stream.set_loop_count(Some(1));

    // Iterate over the source bytes in 2048-byte chunks and feed them into the stream.
    let chunk_size = 4096;
    let mut finished = false;

    for chunk in data_vec.chunks(chunk_size) {
        if let Err(_e) = stream.push_chunk(chunk) {
            // On parse error while adding chunk, quietly exit the binary (for profiling).
            return;
        }

        // Consume any commands now available after pushing the chunk.
        loop {
            match stream.next() {
                Some(Ok(StreamResult::Command(cmd))) => {
                    // Observe the `cmd` on the stack so the compiler cannot
                    // optimize away the parsing/stack/register traffic. Do not print it.
                    black_box(cmd);
                }
                Some(Ok(StreamResult::NeedsMoreData)) => {
                    // Need more bytes: stop draining and continue feeding next chunk.
                    break;
                }
                Some(Ok(StreamResult::EndOfStream)) => {
                    // Stream ended: finish processing.
                    finished = true;
                    break;
                }
                Some(Err(_)) => {
                    // On parse error while iterating, finish silently.
                    finished = true;
                    break;
                }
                None => {
                    // Iterator returned None unexpectedly; treat as finished.
                    finished = true;
                    break;
                }
            }
        }

        if finished {
            break;
        }
    }

    // If there are remaining commands after feeding all chunks, attempt a final drain.
    if !finished {
        while let Some(Ok(StreamResult::Command(cmd))) = stream.next() {
            black_box(cmd);
        }
    }
}

#[allow(dead_code)]
fn from_vgm() {
    // Create an owned `Vec<u8>` by copying the embedded bytes.
    // `to_vec()` performs an efficient memcpy.
    let data_vec: Vec<u8> = REMDME_VGM.to_vec();

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
