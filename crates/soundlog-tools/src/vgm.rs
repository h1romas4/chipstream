use std::fs::File;
use std::io::{Read, stdin};
use std::path::{Path, PathBuf};

use anyhow::Context;
use flate2::read::GzDecoder;
use soundlog::VgmDocument;
use std::convert::TryInto;

use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use unicode_width::UnicodeWidthStr;

/// Pad a &str to a target display width (columns) using unicode-width to
/// account for fullwidth characters (e.g. Japanese). This pads with spaces
/// on the right so strings appear left-aligned in terminal output.
fn pad_to_width(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - w))
    }
}

/// Read VGM bytes from a path or stdin ('-') into a Vec<u8>.
///
/// For regular files, if the extension is `.vgz` it will be decompressed.
/// For stdin, gzipped content is detected via gzip magic bytes (0x1F 0x8B)
/// and decompressed automatically.
pub fn read_vgm_as_vec(path: &PathBuf) -> anyhow::Result<Vec<u8>> {
    // If path is literal '-' treat it as stdin
    if path == Path::new("-") {
        let mut inbuf = Vec::new();
        let mut handle = stdin();
        handle
            .read_to_end(&mut inbuf)
            .context("failed to read from stdin")?;
        // Detect gzip magic
        if inbuf.len() >= 2 && inbuf[0] == 0x1F && inbuf[1] == 0x8B {
            let mut decoder = GzDecoder::new(&inbuf[..]);
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .context("failed to decompress gzip data from stdin")?;
            Ok(out)
        } else {
            Ok(inbuf)
        }
    } else {
        // Open file
        let mut f = File::open(path)
            .with_context(|| format!("failed to open input file: {}", path.display()))?;

        // If extension is .vgz then decompress
        let is_vgz = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("vgz"))
            .unwrap_or(false);

        if is_vgz {
            let mut decoder = GzDecoder::new(f);
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .context("failed to decompress .vgz input")?;
            Ok(out)
        } else {
            let mut out = Vec::new();
            f.read_to_end(&mut out)
                .context("failed to read input file")?;
            Ok(out)
        }
    }
}

/// Produce a stable set of key/value summary fields for a `VgmDocument`.
/// This is used by the test command to compare documents field-by-field
/// rather than requiring byte-for-byte equality.
fn summarize_doc(doc: &VgmDocument) -> Vec<(String, String)> {
    let header = &doc.header;

    // chips
    let instances = header.chip_instances();
    let chips_value = if instances.is_empty() {
        "(none)".to_string()
    } else {
        let mut lines = Vec::new();
        for (inst, chip) in &instances {
            let instance_number = usize::from(*inst) + 1;
            lines.push(format!("{:<12} {}", format!("{:?}", chip), instance_number));
        }
        lines.join("\n")
    };

    // basic header fields
    let version = format!("0x{:08X}", header.version);
    let header_size_val: usize = if header.data_offset == 0 {
        match header.version {
            0x00000100 => 0x20 + 4,
            0x00000101 => 0x24 + 4,
            0x00000110 => 0x30 + 4,
            0x00000150 => 0x34 + 4,
            0x00000151 | 0x00000160 => 0x7F + 4,
            0x00000170 => 0xBC + 4,
            0x00000171 => 0xE0 + 4,
            0x00000172 => 0xE4 + 4,
            _ => 0x100,
        }
    } else {
        0x34usize + header.data_offset as usize
    };
    let header_size = format!("{} (0x{:08X})", header_size_val, header_size_val);
    let loop_offset = format!("0x{:08X}", header.loop_offset);
    let data_offset = format!("0x{:08X}", header.data_offset);
    let total_samples = format!("{}", header.total_samples);

    // waits total
    let total_wait_samples: u64 = doc
        .commands
        .iter()
        .map(|c| match c {
            soundlog::vgm::command::VgmCommand::WaitSamples(ws) => ws.0 as u64,
            soundlog::vgm::command::VgmCommand::WaitNSample(n) => n.0 as u64,
            soundlog::vgm::command::VgmCommand::Wait735Samples(_) => 735u64,
            soundlog::vgm::command::VgmCommand::Wait882Samples(_) => 882u64,
            soundlog::vgm::command::VgmCommand::YM2612Port0Address2AWriteAndWaitN(n) => n.0 as u64,
            _ => 0u64,
        })
        .sum();
    const BASE_SR: f64 = 44100.0;
    let wait_seconds = (total_wait_samples as f64) / BASE_SR;
    let waits_total = format!("{} ({:.3} s @ 44100Hz)", total_wait_samples, wait_seconds);

    // extra header
    let extra_header = if let Some(eh) = &doc.extra_header {
        let eh_ref = eh;
        let abs_start = (header.extra_header_offset.wrapping_add(0xBC)) as usize;
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "header_size = {} (0x{:08X})",
            eh_ref.header_size, eh_ref.header_size
        ));
        lines.push(format!(
            "chip_clock_offset = {} (relative)  absolute = 0x{:08X}",
            eh_ref.chip_clock_offset,
            abs_start.wrapping_add(eh_ref.chip_clock_offset as usize)
        ));
        lines.push(format!(
            "chip_vol_offset   = {} (relative)  absolute = 0x{:08X}",
            eh_ref.chip_vol_offset,
            abs_start.wrapping_add(eh_ref.chip_vol_offset as usize)
        ));
        if eh_ref.chip_clocks.is_empty() {
            lines.push("chip_clocks: (none)".to_string());
        } else {
            lines.push("chip_clocks:".to_string());
            for (idx, (chip_id, clock)) in eh_ref.chip_clocks.iter().enumerate() {
                let id = *chip_id as usize;
                let mapping = if id < instances.len() {
                    format!("{:?}", instances[id].1)
                } else if id >= 1 && id - 1 < instances.len() {
                    format!("{:?} (assuming 1-based)", instances[id - 1].1)
                } else {
                    "<unknown>".to_string()
                };
                lines.push(format!(
                    "  [{}] id={} -> {}  clock={} (0x{:08X})",
                    idx, id, mapping, clock, clock
                ));
            }
        }
        if eh_ref.chip_volumes.is_empty() {
            lines.push("chip_volumes: (none)".to_string());
        } else {
            lines.push("chip_volumes:".to_string());
            for (idx, (chip_id, flags, volume)) in eh_ref.chip_volumes.iter().enumerate() {
                let id = *chip_id as usize;
                let mapping = if id < instances.len() {
                    format!("{:?}", instances[id].1)
                } else if id >= 1 && id - 1 < instances.len() {
                    format!("{:?} (assuming 1-based)", instances[id - 1].1)
                } else {
                    "<unknown>".to_string()
                };
                lines.push(format!(
                    "  [{}] id={} -> {}  flags=0x{:02X}  volume={}",
                    idx, id, mapping, flags, volume
                ));
            }
        }
        lines.join("\n")
    } else {
        "(none)".to_string()
    };

    // data blocks
    let (db_count, db_total_bytes) =
        doc.commands
            .iter()
            .fold((0usize, 0u64), |(cnt, sum), c| match c {
                soundlog::vgm::command::VgmCommand::DataBlock(db) => {
                    (cnt + 1, sum + (db.size as u64))
                }
                _ => (cnt, sum),
            });
    let data_blocks = format!("count={} total_bytes={}", db_count, db_total_bytes);

    // gd3: produce per-field entries to avoid multiline cells in the summary table
    // we'll collect individual gd3 field rows into `gd3_fields` and append them
    // to the main returned `rows` later.
    let mut gd3_fields: Vec<(String, String)> = Vec::new();
    if let Some(g) = &doc.gd3 {
        if let Some(s) = &g.track_name_en {
            gd3_fields.push(("gd3.track_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.track_name_jp {
            gd3_fields.push(("gd3.track_name_jp".into(), s.clone()));
        }
        if let Some(s) = &g.game_name_en {
            gd3_fields.push(("gd3.game_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.game_name_jp {
            gd3_fields.push(("gd3.game_name_jp".into(), s.clone()));
        }
        if let Some(s) = &g.system_name_en {
            gd3_fields.push(("gd3.system_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.system_name_jp {
            gd3_fields.push(("gd3.system_name_jp".into(), s.clone()));
        }
        if let Some(s) = &g.author_name_en {
            gd3_fields.push(("gd3.author_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.author_name_jp {
            gd3_fields.push(("gd3.author_name_jp".into(), s.clone()));
        }
        if let Some(s) = &g.release_date {
            gd3_fields.push(("gd3.release_date".into(), s.clone()));
        }
        if let Some(s) = &g.creator {
            gd3_fields.push(("gd3.creator".into(), s.clone()));
        }
        if let Some(s) = &g.notes {
            let n: String = s
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();
            if !n.is_empty() {
                gd3_fields.push(("gd3.notes".into(), n));
            }
        }
        if gd3_fields.is_empty() {
            gd3_fields.push(("gd3".into(), "(empty)".into()));
        }
    } else {
        gd3_fields.push(("gd3".into(), "(none)".into()));
    }

    // commands count
    let total = doc.commands.len();
    let unknown = doc
        .commands
        .iter()
        .filter(|c| matches!(c, soundlog::vgm::command::VgmCommand::UnknownCommand(_)))
        .count();
    let commands_info = format!("total={} unknown={}", total, unknown);

    let gd3_offset = format!("0x{:08X}", header.gd3_offset);
    let mut rows: Vec<(String, String)> = vec![
        ("VGM version".into(), version),
        ("chips".into(), chips_value),
        ("header_size".into(), header_size),
        ("loop_offset".into(), loop_offset),
        ("data_offset".into(), data_offset),
        ("gd3_offset".into(), gd3_offset),
        ("total_samples".into(), total_samples),
        ("waits_total_samples".into(), waits_total),
        ("extra_header".into(), extra_header),
        ("data_blocks".into(), data_blocks),
    ];
    // Append individual GD3 fields (one row per field) to keep columns aligned.
    for (k, v) in gd3_fields.into_iter() {
        rows.push((k, v));
    }
    rows.push(("commands".into(), commands_info));

    rows
}

/// Helper to display summary information for a parsed `VgmDocument`.
///
/// This implementation prints a fixed-width, left-aligned two-column summary
/// using unicode-aware width calculation so fullwidth (e.g. Japanese)
/// characters are accounted for correctly.
fn display_doc(_title: &str, doc: &VgmDocument) {
    let rows = summarize_doc(doc);

    // Compute column widths using display width (unicode-aware)
    let mut col0 = UnicodeWidthStr::width("Field");
    let mut col1 = UnicodeWidthStr::width("Value");
    for (k, v) in &rows {
        let wk = UnicodeWidthStr::width(k.as_str());
        if wk > col0 {
            col0 = wk;
        }
        for line in v.split('\n') {
            let w = UnicodeWidthStr::width(line);
            if w > col1 {
                col1 = w;
            }
        }
    }

    // Header (left-aligned via pad_to_width)
    println!(
        "{}  {}",
        pad_to_width("Field", col0),
        pad_to_width("Value", col1)
    );

    // Rows: expand multi-line values into per-line rows to keep alignment stable.
    for (k, v) in rows {
        let v_lines: Vec<&str> = v.split('\n').collect();
        for (i, line) in v_lines.iter().enumerate() {
            if i == 0 {
                println!("{}  {}", pad_to_width(&k, col0), pad_to_width(line, col1));
            } else {
                println!("{}  {}", pad_to_width("", col0), pad_to_width(line, col1));
            }
        }
    }
}

/// Print a side-by-side hexdump of original vs serialized bytes.
///
/// If `context_lines` is zero, print the full range (legacy behavior).
/// Otherwise print only a window around `first_diff` with `context_lines`
/// lines of context before and after.
fn print_hexdiff(
    orig: &[u8],
    ser: &[u8],
    first_diff: usize,
    context_lines: usize,
    bytes_per_line: usize,
) {
    let maxlen = std::cmp::max(orig.len(), ser.len());
    if maxlen == 0 {
        println!("(empty data)");
        return;
    }

    let bytes_per_line = if bytes_per_line == 0 {
        16
    } else {
        bytes_per_line
    };

    // Compute line indices
    let first_line = 0usize;
    // compute number of lines without using the (a + b - 1) / b pattern
    let last_line = (maxlen - 1) / bytes_per_line + 1;

    let target_line = first_diff / bytes_per_line;

    let (print_start_line, print_end_line) = if context_lines == 0 {
        (first_line, last_line)
    } else {
        let start = target_line.saturating_sub(context_lines);
        let end = std::cmp::min(last_line, target_line + context_lines + 1);
        (start, end)
    };

    // header
    println!(
        "\nHexdump (original | serialized) (first diff: 0x{:08X}) lines {}..{}:\n",
        first_diff,
        print_start_line,
        print_end_line - 1
    );

    for line in print_start_line..print_end_line {
        let off = line * bytes_per_line;
        print!("0x{:08X}: ", off);

        // original hex
        for i in 0..bytes_per_line {
            let idx = off + i;
            if idx < orig.len() {
                print!("{:02X} ", orig[idx]);
            } else {
                print!("   ");
            }
            if i == bytes_per_line / 2 - 1 {
                print!(" ");
            }
        }

        print!(" | ");

        // serialized hex
        for i in 0..bytes_per_line {
            let idx = off + i;
            if idx < ser.len() {
                print!("{:02X} ", ser[idx]);
            } else {
                print!("   ");
            }
            if i == bytes_per_line / 2 - 1 {
                print!(" ");
            }
        }

        // ASCII representations for original
        print!(" | ");
        for i in 0..bytes_per_line {
            let idx = off + i;
            if idx < orig.len() {
                let b = orig[idx];
                let ch = if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                };
                print!("{}", ch);
            } else {
                print!(" ");
            }
        }

        // ASCII representations for serialized
        print!(" | ");
        for i in 0..bytes_per_line {
            let idx = off + i;
            if idx < ser.len() {
                let b = ser[idx];
                let ch = if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                };
                print!("{}", ch);
            } else {
                print!(" ");
            }
        }

        println!();
    }
    println!();
}

/// Compare two parsed documents, allowing placement-only differences for GD3/data_offset.
fn docs_equal_allow_gd3_offset(a: &VgmDocument, b: &VgmDocument) -> bool {
    // Compare headers but ignore gd3_offset and header/data-offset-related differences
    // (the stored data_offset influences the effective header size on-disk).
    let mut ha = a.header.clone();
    let mut hb = b.header.clone();
    // Ignore placement-only differences: GD3 offset and data_offset/header size.
    ha.gd3_offset = 0;
    hb.gd3_offset = 0;
    ha.data_offset = 0;
    hb.data_offset = 0;
    if ha != hb {
        return false;
    }
    // Extra header must match exactly.
    if a.extra_header != b.extra_header {
        return false;
    }
    // Commands must match exactly.
    if a.commands != b.commands {
        return false;
    }
    // GD3 metadata must match exactly.
    if a.gd3 != b.gd3 {
        return false;
    }
    true
}

/// Info command: parse and display parsed information, without doing roundtrip.
///
/// On parse error this prints a one-line message with the canonicalized path to stderr
/// and returns Ok(()) so callers can continue processing other files.
pub fn info(path: &Path, data: Vec<u8>) -> anyhow::Result<()> {
    // Prepare quoted full-path string for consistent diagnostics.
    let file_str = match path.canonicalize() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    };

    // Parse original bytes, but on error print filename + parse error instead
    // of propagating an anyhow error.
    let doc_orig_res: Result<VgmDocument, _> = (&data[..]).try_into();
    let doc_orig = match doc_orig_res {
        Ok(d) => d,
        Err(e) => {
            eprintln!("\"{}\": parse error: {}", file_str, e);
            return Ok(());
        }
    };

    // Display original parsed information
    display_doc("Parsed document", &doc_orig);

    Ok(())
}

/// Test command: parse, serialize, re-parse roundtrip test and compare binary bytes.
/// Prints detailed diagnostics including hexdiff around the first difference.
///
/// The comparison is semantic: a roundtrip is considered successful if either the
/// serialized bytes match exactly, or the parsed documents match except for
/// placement-only differences (GD3/data offset).
pub fn test_roundtrip(path: &Path, data: Vec<u8>, diag: bool) -> anyhow::Result<()> {
    // Prepare quoted full-path string for one-line outputs. Try to canonicalize to get absolute path,
    // but fall back to the provided path if canonicalize fails.
    let file_str = match path.canonicalize() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    };

    // Parse original bytes, but on parse error print filename + parse error and continue.
    let doc_orig_res: Result<VgmDocument, _> = (&data[..]).try_into();
    let doc_orig = match doc_orig_res {
        Ok(d) => d,
        Err(e) => {
            eprintln!("\"{}\": parse error: {}", file_str, e);
            return Ok(());
        }
    };

    // Round-trip: serialize parsed doc back to bytes and re-parse
    let rebuilt: Vec<u8> = (&doc_orig).into();
    let doc_reparsed_res: Result<VgmDocument, _> = (&rebuilt[..]).try_into();

    match doc_reparsed_res {
        Ok(doc_reparsed) => {
            // Treat as success if raw bytes match, or if documents match except for gd3_offset.
            let semantic_match = docs_equal_allow_gd3_offset(&doc_orig, &doc_reparsed);

            if rebuilt == data || semantic_match {
                if diag {
                    // Provide full diagnostics when requested.
                    // Build a field-aligned side-by-side table using summarize_doc but
                    // split multi-line values into per-line rows so columns stay aligned.
                    let orig_rows = summarize_doc(&doc_orig);
                    let rebuilt_rows = summarize_doc(&doc_reparsed);
                    let mut side = Table::new();
                    side.load_preset(NOTHING);
                    side.set_content_arrangement(ContentArrangement::Dynamic);
                    side.set_header(vec![
                        Cell::new("Field"),
                        Cell::new("Original"),
                        Cell::new("Rebuilt"),
                    ]);
                    for (k, ov) in &orig_rows {
                        let rv = rebuilt_rows
                            .iter()
                            .find(|(rk, _)| rk == k)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_else(|| "<missing>".to_string());
                        let ov_lines: Vec<&str> = ov.split('\n').collect();
                        let rv_lines: Vec<&str> = rv.split('\n').collect();
                        let max_lines = std::cmp::max(ov_lines.len(), rv_lines.len());
                        for i in 0..max_lines {
                            let key_cell = if i == 0 {
                                Cell::new(k.clone())
                            } else {
                                Cell::new("")
                            };
                            let ocell = Cell::new(ov_lines.get(i).unwrap_or(&"").to_string());
                            let rcell = Cell::new(rv_lines.get(i).unwrap_or(&"").to_string());
                            side.add_row(vec![key_cell, ocell, rcell]);
                        }
                    }
                    // Also include any keys present only in rebuilt
                    for (k, rv) in &rebuilt_rows {
                        if !orig_rows.iter().any(|(ok, _)| ok == k) {
                            let rv_lines: Vec<&str> = rv.split('\n').collect();
                            for (i, rl) in rv_lines.iter().enumerate() {
                                if i == 0 {
                                    side.add_row(vec![
                                        Cell::new(k.clone()),
                                        Cell::new("<missing>"),
                                        Cell::new(rl.to_string()),
                                    ]);
                                } else {
                                    side.add_row(vec![
                                        Cell::new(""),
                                        Cell::new(""),
                                        Cell::new(rl.to_string()),
                                    ]);
                                }
                            }
                        }
                    }
                    println!("{}", side);

                    if rebuilt == data {
                        println!(
                            "roundtrip: serialized matches original ({} bytes)",
                            rebuilt.len()
                        );
                    }
                }
                // otherwise silent on success
            } else {
                // One-line error with filename as requested. Exit code remains zero.
                println!(
                    "\"{}\": roundtrip: MISMATCH (original {} bytes, serialized {} bytes) — run with --diag to see detailed diagnostics",
                    file_str,
                    data.len(),
                    rebuilt.len()
                );

                if diag {
                    // When --diag is provided, print detailed diagnostics like before.
                    // Print a compact, fixed-width aligned side-by-side summary without using comfy_table.
                    // This avoids multi-line cell wrapping that caused GD3 alignment issues.
                    let orig_rows = summarize_doc(&doc_orig);
                    let rebuilt_rows = summarize_doc(&doc_reparsed);

                    // Expand multiline values into per-line tuples (field, orig_line, rebuilt_line)
                    let mut combined: Vec<(String, String, String)> = Vec::new();
                    for (k, ov) in &orig_rows {
                        let rv = rebuilt_rows
                            .iter()
                            .find(|(rk, _)| rk == k)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_else(|| "<missing>".to_string());

                        let olines: Vec<&str> = ov.split('\n').collect();
                        let rlines: Vec<&str> = rv.split('\n').collect();
                        let maxl = std::cmp::max(olines.len(), rlines.len());
                        for i in 0..maxl {
                            if i == 0 {
                                combined.push((
                                    k.clone(),
                                    olines.get(i).unwrap_or(&"").to_string(),
                                    rlines.get(i).unwrap_or(&"").to_string(),
                                ));
                            } else {
                                combined.push((
                                    "".to_string(),
                                    olines.get(i).unwrap_or(&"").to_string(),
                                    rlines.get(i).unwrap_or(&"").to_string(),
                                ));
                            }
                        }
                    }
                    for (k, rv) in &rebuilt_rows {
                        if !orig_rows.iter().any(|(ok, _)| ok == k) {
                            let rlines: Vec<&str> = rv.split('\n').collect();
                            for (i, rl) in rlines.iter().enumerate() {
                                if i == 0 {
                                    combined.push((
                                        k.clone(),
                                        "<missing>".to_string(),
                                        rl.to_string(),
                                    ));
                                } else {
                                    combined.push(("".to_string(), "".to_string(), rl.to_string()));
                                }
                            }
                        }
                    }

                    // Compute column widths using display width (unicode-aware)
                    let mut col0 = UnicodeWidthStr::width("Field");
                    let mut col1 = UnicodeWidthStr::width("Original");
                    let mut col2 = UnicodeWidthStr::width("Rebuilt");
                    for (a, b, c) in &combined {
                        let wa = UnicodeWidthStr::width(a.as_str());
                        if wa > col0 {
                            col0 = wa;
                        }
                        let wb = UnicodeWidthStr::width(b.as_str());
                        if wb > col1 {
                            col1 = wb;
                        }
                        let wc = UnicodeWidthStr::width(c.as_str());
                        if wc > col2 {
                            col2 = wc;
                        }
                    }

                    // Print header and rows with padding using unicode-aware padding
                    println!(
                        "{}  {}  {}",
                        pad_to_width("Field", col0),
                        pad_to_width("Original", col1),
                        pad_to_width("Rebuilt", col2)
                    );
                    for (a, b, c) in combined {
                        println!(
                            "{}  {}  {}",
                            pad_to_width(&a, col0),
                            pad_to_width(&b, col1),
                            pad_to_width(&c, col2)
                        );
                    }

                    // report first differing offset if any
                    let minlen = std::cmp::min(data.len(), rebuilt.len());
                    let diff_idx = data.iter().zip(rebuilt.iter()).position(|(a, b)| a != b);
                    if let Some(i) = diff_idx {
                        println!(
                            "\nfirst difference at offset 0x{:08X}: original=0x{:02X} serialized=0x{:02X}",
                            i, data[i], rebuilt[i]
                        );
                        // Print a side-by-side hexdump around the first difference.
                        print_hexdiff(&data, &rebuilt, i, 4, 16);
                    } else {
                        println!("no byte differences within min length; length differs");
                        // show trailing bytes context at the end of the shorter buffer
                        let first_diff = minlen;
                        print_hexdiff(&data, &rebuilt, first_diff, 8, 16);
                    }
                }
            }
        }
        Err(e) => {
            // One-line error with filename; re-parse failed after serialization.
            eprintln!(
                "\"{}\": roundtrip: serialization produced bytes (len={}), but re-parse failed: {} — run with --diag to see serialized bytes and diagnostics",
                file_str,
                rebuilt.len(),
                e
            );

            if diag {
                // show hexdump of serialized head for inspection when requested
                let shown = std::cmp::min(rebuilt.len(), 256);
                println!("serialized bytes (first {} bytes):", shown);
                for (i, b) in rebuilt.iter().take(shown).enumerate() {
                    if i % 16 == 0 {
                        print!("\n0x{:08X}: ", i);
                    }
                    print!("{:02X} ", b);
                }
                println!();
            }
        }
    }

    Ok(())
}
