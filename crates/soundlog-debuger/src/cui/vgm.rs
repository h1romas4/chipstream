use std::convert::TryInto;
use std::path::Path;

use anyhow::Result;

use soundlog::VgmDocument;

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
        soundlog::VgmHeader::fallback_header_size_for_version(header.version)
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
        if let Some(s) = &g.author_name_en {
            gd3_fields.push(("gd3.author_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.author_name_jp {
            gd3_fields.push(("gd3.author_name_jp".into(), s.clone()));
        }
        if let Some(s) = &g.notes {
            gd3_fields.push(("gd3.notes".into(), s.clone()));
        }
    }

    // commands info: count and rough distribution
    let mut cmd_counts: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    for c in &doc.commands {
        let key: &'static str = match c {
            soundlog::vgm::command::VgmCommand::WaitSamples(_) => "wait",
            soundlog::vgm::command::VgmCommand::DataBlock(_) => "data_block",
            _ => "other",
        };
        *cmd_counts.entry(key).or_default() += 1;
    }
    let commands_info = format!(
        "count={} waits={} data_blocks={}",
        doc.commands.len(),
        *cmd_counts.get("wait").unwrap_or(&0usize),
        *cmd_counts.get("data_block").unwrap_or(&0usize)
    );

    // Assemble rows in a stable order
    let mut rows: Vec<(String, String)> = vec![
        ("chips".into(), chips_value),
        ("version".into(), version),
        ("header_size".into(), header_size),
        ("loop_offset".into(), loop_offset),
        ("data_offset".into(), data_offset),
        ("total_samples".into(), total_samples),
        ("waits_total".into(), waits_total),
        ("data_blocks".into(), data_blocks),
    ];

    // Append individual GD3 fields (one row per field) to keep columns aligned.
    for (k, v) in gd3_fields.into_iter() {
        rows.push((k, v));
    }
    rows.push(("commands".into(), commands_info));

    rows
}

/// Print a rich side-by-side table of original vs. rebuilt document fields.
///
/// This is used by `test_roundtrip --diag` when the roundtrip succeeds
/// and the caller requested verbose diagnostics.
fn print_diag_table(orig: &VgmDocument, rebuilt: &VgmDocument) {
    // Build a field-aligned side-by-side table using summarize_doc but
    // split multi-line values into per-line rows so columns stay aligned.
    let orig_rows = summarize_doc(orig);
    let rebuilt_rows = summarize_doc(rebuilt);
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
}

/// Print a compact, fixed-width, unicode-aware diagnostic summary.
///
/// This is used by `test_roundtrip --diag` on mismatch to print a compact
/// textual comparison and report the first differing byte offset.
fn print_diag_compact(
    orig: &VgmDocument,
    rebuilt: &VgmDocument,
    data: &[u8],
    rebuilt_bytes: &[u8],
) {
    let orig_rows = summarize_doc(orig);
    let rebuilt_rows = summarize_doc(rebuilt);

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
                    combined.push((k.clone(), "<missing>".to_string(), rl.to_string()));
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
    let diff_idx = data
        .iter()
        .zip(rebuilt_bytes.iter())
        .position(|(a, b)| a != b);
    if let Some(i) = diff_idx {
        println!(
            "\nfirst difference at offset 0x{:08X}: original=0x{:02X} serialized=0x{:02X}",
            i, data[i], rebuilt_bytes[i]
        );
        println!(
            "(detailed hexdump omitted; enable a build with hexdump support to see side-by-side bytes)"
        );
    } else {
        println!("no byte differences within min length; length differs");
        println!(
            "(detailed hexdump omitted; enable a build with hexdump support to see side-by-side bytes)"
        );
    }
}

/// Compare two parsed documents, allowing placement-only differences for GD3/data_offset.
fn docs_equal_allow_gd3_offset(a: &VgmDocument, b: &VgmDocument) -> bool {
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

/// Test command: parse, serialize, re-parse roundtrip test and compare binary bytes.
/// Prints detailed diagnostics including a compact field-by-field comparison.
///
/// The comparison is semantic: a roundtrip is considered successful if either the
/// serialized bytes match exactly, or the parsed documents match except for
/// placement-only differences (GD3/data offset).
pub fn test_roundtrip(path: &Path, data: Vec<u8>, diag: bool) -> Result<()> {
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
            let semantic_match = docs_equal_allow_gd3_offset(&doc_orig, &doc_reparsed);
            if rebuilt == data || semantic_match {
                if diag {
                    print_diag_table(&doc_orig, &doc_reparsed);
                    if rebuilt == data {
                        println!(
                            "roundtrip: serialized matches original ({} bytes)",
                            rebuilt.len()
                        );
                    }
                }
            } else {
                // One-line error with filename as requested. Exit code remains zero.
                println!(
                    "\"{}\": roundtrip: MISMATCH (original {} bytes, serialized {} bytes) — run with --diag to see detailed diagnostics",
                    file_str,
                    data.len(),
                    rebuilt.len()
                );
                if diag {
                    print_diag_compact(&doc_orig, &doc_reparsed, &data, &rebuilt);
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
        }
    }

    Ok(())
}
