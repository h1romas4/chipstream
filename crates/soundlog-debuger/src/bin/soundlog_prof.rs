#![allow(clippy::too_many_lines)]
//! Profiling binary for soundlog: feeds a bundled VGM into `VgmStream` incrementally.
//!
//! This binary is intentionally minimal and designed for profiling. It:
//! - primarily intended to verify performance for microcomputers
//! - embeds a VGM file with `include_bytes!`
//! - parses the VGM header and uses the header `eof_offset` and `loop_offset`
//! - feeds the VGM command/data region into `VgmStream` in fixed-size chunks
//! - drains parsed commands and passes them to `std::hint::black_box` so the
//!   compiler cannot elide stack/register traffic
//!
//! # Loop handling (Buffer-based VgmStream)
//!
//! `VgmStream::new()` + `push_chunk()` uses a `Buffer` source. When the stream
//! encounters an `EndOfData` command it calls `jump_to_loop_point()` internally,
//! which clears the buffer so the next `push_chunk` starts from a clean state.
//! The caller (this binary) is responsible for re-sending the data from the loop
//! point on every `NeedsMoreData` response: we rewind `offset` to `loop_pos` and
//! push data again from there until `EndOfStream` is received.
//!
//! Error reporting from `push_chunk` is via `PushError` enum (repr(u8)) so the
//! caller can use compact numeric codes if desired.

use std::hint::black_box;

use soundlog::VgmHeader;
use soundlog::VgmStream;

use soundlog::vgm::VgmHeaderField;
use soundlog::vgm::stream::StreamResult;

/// Embedded VGM asset included at compile time.
static REMDME_VGM: &[u8] = include_bytes!("../../../soundlog/assets/vgm/REMDME.vgm");

/// Error codes returned by `push_chunk`.
/// Use `as u8` to obtain the compact numeric representation for profiling/exit codes.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    /// Header parsing failed / header fields invalid (EOF/command_start etc.)
    HeaderParse = 1,
    /// Error when pushing chunk data into the stream (invalid data).
    PushInvalid = 2,
    /// Parse error encountered while iterating commands.
    IterParse = 3,
    /// Parse error during final drain.
    FinalParse = 4,
}

fn main() {
    // Obtain an owned buffer from the embedded bytes (efficient memcpy for &[u8] -> Vec<u8]).
    let data_vec: Vec<u8> = REMDME_VGM.to_vec();

    // Example: limit loops to 100 iteration so profiling completes.
    // The process returns the error code if push_chunk fails.
    match push_chunk(data_vec, Some(2), 4096usize) {
        Ok(_count) => std::process::exit(0),
        Err(e) => std::process::exit(e as i32),
    }
}

/// Feed the embedded VGM to a `VgmStream` by pushing chunks and draining commands.
/// Returns Ok(push_count) on success (number of chunk pushes performed),
/// or Err(PushError) on various parse/format errors.
#[allow(dead_code)]
fn push_chunk(
    data_vec: Vec<u8>,
    loop_count: Option<u32>,
    chunk_size: usize,
) -> Result<usize, PushError> {
    // Parse header; if header parsing fails, return HeaderParse.
    let header = VgmHeader::from_bytes(&data_vec).map_err(|_| PushError::HeaderParse)?;

    // Compute command start from header fields.
    let command_start = VgmHeader::command_start(header.version, header.data_offset);

    if header.eof_offset == 0 {
        return Err(PushError::HeaderParse);
    }

    let eof_usize = header.eof_offset as usize + VgmHeaderField::EofOffset.offset();
    if eof_usize > data_vec.len() {
        return Err(PushError::HeaderParse);
    }

    if command_start >= eof_usize {
        return Err(PushError::HeaderParse);
    }

    // Build command_region slice bounded by command_start..eof
    let command_region = &data_vec[command_start..eof_usize];

    // Compute loop position relative to command_region, if present.
    let loop_pos_in_commands: Option<usize> = VgmHeader::loop_pos_in_commands(
        header.version,
        header.loop_offset,
        header.data_offset,
        eof_usize,
    );

    // Create stream and configure loop count.
    let mut stream = VgmStream::new();
    stream.set_loop_count(loop_count);

    // Iterate command_region in fixed-size chunks and feed to stream.
    let data_len = command_region.len();
    let mut offset: usize = 0;
    let mut finished = false;
    let mut push_count: usize = 0;

    // The restart position to rewind to when NeedsMoreData is received after an internal loop.
    let restart_pos = loop_pos_in_commands.unwrap_or(0);

    while offset < data_len {
        let end = (offset + chunk_size).min(data_len);
        let chunk = &command_region[offset..end];

        // Push chunk into stream; map push error to PushInvalid.
        if stream.push_chunk(chunk).is_err() {
            return Err(PushError::PushInvalid);
        }
        push_count = push_count.saturating_add(1);

        let loops_before_drain = stream.current_loop_count();
        let mut needs_more = false;
        loop {
            match stream.next() {
                Some(Ok(StreamResult::Command(cmd))) => {
                    // Observe the command to preserve stack/register traffic.
                    black_box(cmd);
                }
                Some(Ok(StreamResult::NeedsMoreData)) => {
                    needs_more = true;
                    break;
                }
                Some(Ok(StreamResult::EndOfStream)) => {
                    // All requested loop iterations completed.
                    finished = true;
                    break;
                }
                Some(Err(_)) => {
                    // Parse error during iteration.
                    return Err(PushError::IterParse);
                }
                None => {
                    // Iterator ended unexpectedly; treat as finished.
                    finished = true;
                    break;
                }
            }
        }

        if finished {
            break;
        }

        if needs_more {
            if stream.current_loop_count() > loops_before_drain {
                offset = restart_pos;
            } else {
                offset = end;
            }
        } else {
            offset = end;
        }
    }

    Ok(push_count)
}
