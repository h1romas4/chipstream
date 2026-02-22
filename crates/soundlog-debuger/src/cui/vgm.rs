use std::convert::TryInto;
use std::path::Path;

use anyhow::{Context, Result};

use soundlog::VgmDocument;
use soundlog::vgm::detail::{DataBlockType, parse_data_block};

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
        for (inst, chip, clock_hz) in &instances {
            let instance_number = usize::from(*inst) + 1;
            // `chip_instances()` provides the clock as an `f32` in Hz.
            // Display it with 3 fractional digits to show sub-hertz precision
            // while keeping the column reasonably compact.
            let clock = *clock_hz;
            lines.push(format!(
                "{:<12} {:>10.3} Hz ({})",
                format!("{:?}", chip),
                clock,
                instance_number
            ));
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
    const BASE_SR: f32 = 44100.0f32;
    let wait_seconds = (total_wait_samples as f32) / BASE_SR;
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

    // data block types
    use std::collections::HashMap;
    let mut db_type_counts: HashMap<String, usize> = HashMap::new();
    for c in &doc.commands {
        if let soundlog::vgm::command::VgmCommand::DataBlock(db) = c {
            match parse_data_block(db.clone()) {
                Ok(data_type) => {
                    let type_name = format_data_block_type(&data_type);
                    *db_type_counts.entry(type_name).or_insert(0) += 1;
                }
                Err(_) => {
                    *db_type_counts.entry("ParseError".to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    let data_block_types = if db_type_counts.is_empty() {
        "(none)".to_string()
    } else {
        let mut type_parts: Vec<String> = db_type_counts
            .iter()
            .map(|(type_name, count)| format!("{}:{}", type_name, count))
            .collect();
        type_parts.sort();
        type_parts.join(", ")
    };

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
        ("data_block_types".into(), data_block_types),
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
/// This is used by `test_roundtrip` to show diagnostics when the roundtrip
/// succeeds and output is enabled (i.e. when the caller did not request --dry-run).
pub(crate) fn print_diag_table(orig: &VgmDocument, rebuilt: &VgmDocument) {
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
/// This is used by `test_roundtrip` on mismatch to print a compact textual
/// comparison and report the first differing byte offset when diagnostics are
/// enabled (i.e. when the caller did not request --dry-run).
pub(crate) fn print_diag_compact(
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
pub(crate) fn docs_equal_allow_gd3_offset(a: &VgmDocument, b: &VgmDocument) -> bool {
    let mut ha = a.header.clone();
    let mut hb = b.header.clone();
    // Ignore placement-only differences: GD3 offset.
    ha.gd3_offset = 0;
    hb.gd3_offset = 0;
    // Header must match exactly.
    if ha != hb {
        return false;
    }

    // Extra header: only compare data content, not placement details (header_size, offsets).
    if !extra_headers_semantically_equal(&a.extra_header, &b.extra_header) {
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

/// Compare two extra headers semantically, ignoring placement details.
///
/// This function considers two extra headers equal if they contain the same
/// chip_clocks and chip_volumes data, regardless of header_size, chip_clock_offset,
/// or chip_vol_offset values (which are just placement details).
fn extra_headers_semantically_equal(
    a: &Option<soundlog::vgm::VgmExtraHeader>,
    b: &Option<soundlog::vgm::VgmExtraHeader>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a_extra), Some(b_extra)) => {
            // Compare data content only, not placement fields
            a_extra.chip_clocks == b_extra.chip_clocks
                && a_extra.chip_volumes == b_extra.chip_volumes
        }
        _ => false, // One has extra_header, the other doesn't
    }
}

pub use crate::cui::test::test_roundtrip;

pub use crate::cui::redump::redump_vgm;

/// Parse and display VGM file commands with offsets and lengths
pub fn parse_vgm(file_path: &Path, data: Vec<u8>) -> Result<()> {
    use soundlog::VgmDocument;

    // Parse VGM document
    let doc: VgmDocument = (&data[..])
        .try_into()
        .with_context(|| format!("failed to parse VGM file: {}", file_path.display()))?;

    // Get command offsets and lengths
    let offsets_and_lengths = doc.command_offsets_and_lengths();

    // Print commands with offsets and lengths
    println!("{:<8} {:<8} {:<80} Length", "Index", "Offset", "Command");
    println!("{}", "-".repeat(120));

    for (index, (cmd, (offset, length))) in doc
        .commands
        .iter()
        .zip(offsets_and_lengths.iter())
        .enumerate()
    {
        let cmd_str = format_command_brief(cmd);
        println!("{:<8} 0x{:06X} {:<80} {}", index, offset, cmd_str, length);
    }

    Ok(())
}

/// Format a command for brief display
fn format_command_brief(cmd: &soundlog::VgmCommand) -> String {
    use soundlog::VgmCommand;

    match cmd {
        VgmCommand::AY8910StereoMask(m) => format!("AY8910StereoMask({:?})", m),
        VgmCommand::WaitSamples(w) => format!("WaitSamples({})", w.0),
        VgmCommand::Wait735Samples(_) => "Wait735Samples".to_string(),
        VgmCommand::Wait882Samples(_) => "Wait882Samples".to_string(),
        VgmCommand::WaitNSample(w) => format!("WaitNSample({})", w.0),
        VgmCommand::EndOfData(_) => "EndOfData".to_string(),
        VgmCommand::DataBlock(db) => match parse_data_block(db.clone()) {
            Ok(data_type) => format!(
                "DataBlock({}, size={})",
                format_data_block_type(&data_type),
                db.size
            ),
            Err((_, err)) => format!("DataBlock(parse_error={}, size={})", err, db.size),
        },
        VgmCommand::PcmRamWrite(p) => format!("PcmRamWrite({:?})", p),
        VgmCommand::YM2612Port0Address2AWriteAndWaitN(s) => {
            format!("YM2612Port0Address2AWriteAndWaitN({:?})", s)
        }
        VgmCommand::SetupStreamControl(s) => format!(
            "SetupStreamControl(id={}, chip={:?})",
            s.stream_id, s.chip_type
        ),
        VgmCommand::SetStreamData(s) => format!(
            "SetStreamData(id={}, bank=0x{:02X})",
            s.stream_id, s.data_bank_id
        ),
        VgmCommand::SetStreamFrequency(s) => format!(
            "SetStreamFrequency(id={}, freq={})",
            s.stream_id, s.frequency
        ),
        VgmCommand::StartStream(s) => format!(
            "StartStream(id={}, offset=0x{:X})",
            s.stream_id, s.data_start_offset
        ),
        VgmCommand::StopStream(s) => format!("StopStream(id={})", s.stream_id),
        VgmCommand::StartStreamFastCall(s) => format!("StartStreamFastCall({:?})", s),
        VgmCommand::SeekOffset(s) => format!("SeekOffset({:?})", s),
        VgmCommand::Sn76489Write(inst, spec) => {
            format!("Sn76489Write({:?}, 0x{:02X})", inst, spec.value)
        }
        VgmCommand::Ym2413Write(inst, spec) => {
            format!(
                "Ym2413Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Ym2612Write(inst, spec) => {
            format!(
                "Ym2612Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Ym2151Write(inst, spec) => {
            format!(
                "Ym2151Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::SegaPcmWrite(inst, spec) => {
            // offset is u16
            format!(
                "SegaPcmWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::Rf5c68U8Write(inst, spec) => {
            // offset is u8
            format!(
                "Rf5c68U8Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::Rf5c68U16Write(inst, spec) => {
            format!(
                "Rf5c68U16Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::Ym2203Write(inst, spec) => {
            format!(
                "Ym2203Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Ym2608Write(inst, spec) => {
            format!(
                "Ym2608Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            )
        }
        VgmCommand::Ym2610bWrite(inst, spec) => {
            format!(
                "Ym2610bWrite({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            )
        }
        VgmCommand::Ym3812Write(inst, spec) => {
            format!(
                "Ym3812Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Ym3526Write(inst, spec) => {
            format!(
                "Ym3526Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Y8950Write(inst, spec) => {
            format!(
                "Y8950Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Ymf262Write(inst, spec) => format!("Ymf262Write({:?}, {:?})", inst, spec),
        VgmCommand::Ymf278bWrite(inst, spec) => format!("Ymf278bWrite({:?}, {:?})", inst, spec),
        VgmCommand::Ymf271Write(inst, spec) => format!("Ymf271Write({:?}, {:?})", inst, spec),
        VgmCommand::Scc1Write(inst, spec) => {
            // Keep Scc1 (VGM) spec debug but show port/register/value explicitly for readability
            format!(
                "Scc1Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            )
        }
        VgmCommand::Ymz280bWrite(inst, spec) => {
            format!(
                "Ymz280bWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Rf5c164U8Write(inst, spec) => {
            format!(
                "Rf5c164U8Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::Rf5c164U16Write(inst, spec) => {
            format!(
                "Rf5c164U16Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::PwmWrite(inst, spec) => {
            // register is low 4 bits; value uses lower 24 bits
            format!(
                "PwmWrite({:?}, reg=0x{:02X}=0x{:06X})",
                inst,
                spec.register,
                spec.value & 0x00FF_FFFF
            )
        }
        VgmCommand::Ay8910Write(inst, spec) => {
            format!(
                "Ay8910Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::GbDmgWrite(inst, spec) => {
            format!(
                "GbDmgWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::NesApuWrite(inst, spec) => {
            format!(
                "NesApuWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::MultiPcmWrite(inst, spec) => {
            format!(
                "MultiPcmWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::MultiPcmBankWrite(inst, spec) => {
            format!("MultiPcmBankWrite({:?}, {:?})", inst, spec)
        }
        VgmCommand::Upd7759Write(inst, spec) => {
            format!(
                "Upd7759Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Okim6258Write(inst, spec) => {
            format!(
                "Okim6258Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Okim6295Write(inst, spec) => {
            format!(
                "Okim6295Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::K054539Write(inst, spec) => {
            format!(
                "K054539Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Huc6280Write(inst, spec) => {
            format!(
                "Huc6280Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::C140Write(inst, spec) => {
            format!(
                "C140Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::K053260Write(inst, spec) => {
            format!(
                "K053260Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::PokeyWrite(inst, spec) => {
            format!(
                "PokeyWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::QsoundWrite(inst, spec) => {
            // register/value combined as u16
            format!(
                "QsoundWrite({:?}, 0x{:04X}=0x{:04X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::ScspWrite(inst, spec) => {
            format!(
                "ScspWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::WonderSwanWrite(inst, spec) => {
            format!(
                "WonderSwanWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::WonderSwanRegWrite(inst, spec) => {
            format!(
                "WonderSwanRegWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::VsuWrite(inst, spec) => {
            format!(
                "VsuWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::Saa1099Write(inst, spec) => {
            format!(
                "Saa1099Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Es5503Write(inst, spec) => {
            format!(
                "Es5503Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Es5506BEWrite(inst, spec) => {
            format!(
                "Es5506BEWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Es5506D6Write(inst, spec) => {
            format!(
                "Es5506D6Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::X1010Write(inst, spec) => {
            format!(
                "X1010Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            )
        }
        VgmCommand::C352Write(inst, spec) => {
            format!(
                "C352Write({:?}, 0x{:04X}=0x{:04X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::Ga20Write(inst, spec) => {
            format!(
                "Ga20Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::MikeyWrite(inst, spec) => {
            format!(
                "MikeyWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            )
        }
        VgmCommand::GameGearPsgWrite(inst, spec) => {
            format!("GameGearPsgWrite({:?}, 0x{:02X})", inst, spec.value)
        }
        VgmCommand::ReservedU8Write(r) => format!("ReservedU8Write({:?})", r),
        VgmCommand::ReservedU16Write(r) => format!("ReservedU16Write({:?})", r),
        VgmCommand::ReservedU24Write(r) => format!("ReservedU24Write({:?})", r),
        VgmCommand::ReservedU32Write(r) => format!("ReservedU32Write({:?})", r),
        VgmCommand::UnknownCommand(u) => format!("UnknownCommand({:?})", u),
    }
}

fn format_data_block_type(data_type: &DataBlockType) -> String {
    use soundlog::vgm::detail::DataBlockType;
    match data_type {
        DataBlockType::UncompressedStream(us) => format!("UncompressedStream({:?})", us.chip_type),
        DataBlockType::CompressedStream(cs) => format!(
            "CompressedStream({:?}, {:?})",
            cs.chip_type, cs.compression_type
        ),
        DataBlockType::DecompressionTable(dt) => {
            format!("DecompressionTable({:?})", dt.compression_type)
        }
        DataBlockType::RomRamDump(rr) => format!("RomRamDump({:?})", rr.chip_type),
        DataBlockType::RamWrite16(rw) => format!("RamWrite16({:?})", rw.chip_type),
        DataBlockType::RamWrite32(rw) => format!("RamWrite32({:?})", rw.chip_type),
    }
}
