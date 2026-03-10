use std::convert::TryInto;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use soundlog::VgmDocument;
use soundlog::vgm::detail::{DataBlockType, parse_data_block};

use crate::logger::Logger;

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
    let eof_offset = format!("0x{:08X}", header.eof_offset);
    let gd3_offset = format!("0x{:08X}", header.gd3_offset);
    let extra_header_offset = format!("0x{:08X}", header.extra_header_offset);
    let loop_offset = format!("0x{:08X}", header.loop_offset);
    let loop_samples = format!("{}", header.loop_samples);
    let loop_base = format!("{}", header.loop_base);
    let loop_modifier = format!("{}", header.loop_modifier);
    let sn76489_feedback = format!("{:?}", header.sn76489_feedback);
    let data_offset = format!("0x{:08X}", header.data_offset);
    let volume_modifier = format!("{}", header.volume_modifier);
    let sn76489_shift_register_width = format!(
        "{:?}({})",
        header.sn76489_shift_register_width,
        u8::from(header.sn76489_shift_register_width)
    );
    let sn76489_flags = format!("{:?}", header.sn76489_flags);
    let ay_chip_type = format!("{:?}", header.ay_chip_type);
    let ay8910_flags = format!("{:?}", header.ay8910_flags);
    let ym2203_ay8910_flags = format!("{:?}", header.ym2203_ay8910_flags);
    let ym2608_ay8910_flags = format!("{:?}", header.ym2608_ay8910_flags);
    let okim6258_flags = format!("{:?}", header.okim6258_flags);
    let k054539_flags = format!("{:?}", header.k054539_flags);
    let c140_chip_type = format!("{:?}", header.c140_chip_type);
    let total_samples = format!("{}", header.total_samples);

    // waits total
    let total_wait_samples: u64 = doc
        .commands
        .iter()
        .map(|c| match c {
            soundlog::vgm::command::VgmCommand::WaitSamples(ws) => ws.0 as u64,
            soundlog::vgm::command::VgmCommand::WaitNSample(n) => n.0 as u64 + 1,
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
            match parse_data_block(*db.clone()) {
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
        if let Some(s) = &g.track_name_origin {
            gd3_fields.push(("gd3.track_name_origin".into(), s.clone()));
        }
        if let Some(s) = &g.game_name_en {
            gd3_fields.push(("gd3.game_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.game_name_origin {
            gd3_fields.push(("gd3.game_name_origin".into(), s.clone()));
        }
        if let Some(s) = &g.author_name_en {
            gd3_fields.push(("gd3.author_name_en".into(), s.clone()));
        }
        if let Some(s) = &g.author_name_origin {
            gd3_fields.push(("gd3.author_name_origin".into(), s.clone()));
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
        ("eof_offset".into(), eof_offset),
        ("gd3_offset".into(), gd3_offset),
        ("extra_header_offset".into(), extra_header_offset),
        ("data_offset".into(), data_offset),
        ("loop_offset".into(), loop_offset),
        ("loop_samples".into(), loop_samples),
        ("loop_base".into(), loop_base),
        ("loop_modifier".into(), loop_modifier),
        ("volume_modifier".into(), volume_modifier),
        ("total_samples".into(), total_samples),
        ("waits_total (calc)".into(), waits_total),
        ("data_blocks".into(), data_blocks),
        ("data_block_types".into(), data_block_types),
    ];

    if u16::from(header.sn76489_feedback) != 0 {
        rows.push(("sn76489_feedback".into(), sn76489_feedback));
    }
    if u8::from(header.sn76489_shift_register_width) != 0 {
        rows.push((
            "sn76489_shift_register_width".into(),
            sn76489_shift_register_width,
        ));
    }
    if u8::from(header.sn76489_flags) != 0 {
        rows.push(("sn76489_flags".into(), sn76489_flags));
    }
    if u8::from(header.ay_chip_type) != 0 {
        rows.push(("ay_chip_type".into(), ay_chip_type));
    }
    if u8::from(header.ay8910_flags) != 0 {
        rows.push(("ay8910_flags".into(), ay8910_flags));
    }
    if u8::from(header.ym2203_ay8910_flags) != 0 {
        rows.push(("ym2203_ay8910_flags".into(), ym2203_ay8910_flags));
    }
    if u8::from(header.ym2608_ay8910_flags) != 0 {
        rows.push(("ym2608_ay8910_flags".into(), ym2608_ay8910_flags));
    }
    if u8::from(header.okim6258_flags) != 0 {
        rows.push(("okim6258_flags".into(), okim6258_flags));
    }
    if u8::from(header.k054539_flags) != 0 {
        rows.push(("k054539_flags".into(), k054539_flags));
    }
    if u8::from(header.c140_chip_type) != 0 {
        rows.push(("c140_chip_type".into(), c140_chip_type));
    }

    // Insert vgmheader_misc row: if any derived misc field is Some(...) display the debug
    // representation of the struct; otherwise display "(none)".
    let vgm_misc = header.misc();
    let vgm_misc_has_some = vgm_misc.t6w28_detected.is_some()
        || vgm_misc.use_ym2413_clock_for_ym2612.is_some()
        || vgm_misc.use_ym2413_clock_for_ym2151.is_some()
        || vgm_misc.ym2610b_detected.is_some()
        || vgm_misc.fds_detected.is_some()
        || vgm_misc.is_es5506.is_some();

    if vgm_misc_has_some {
        rows.push(("vgmheader_misc".into(), format!("{:?}", vgm_misc)));
    } else {
        rows.push(("vgmheader_misc".into(), "(none)".to_string()));
    }

    // If an extra-header was parsed, include a compact summary of its contents.
    // We emit two rows when present: `extra_header.chip_clocks` and
    // `extra_header.chip_volumes`. Each row's value may be multi-line (one entry
    // per line) so the diagnostics table keeps alignment.
    if let Some(eh) = &doc.extra_header {
        // chip_clocks: show chip id, instance and clock value (Hz)
        if !eh.chip_clocks.is_empty() {
            let mut lines: Vec<String> = Vec::new();
            for cc in &eh.chip_clocks {
                lines.push(format!(
                    "{:?} {:?} {} Hz",
                    cc.chip_id, cc.instance, cc.clock
                ));
            }
            rows.push(("extra_header.chip_clocks".into(), lines.join("\n")));
        }

        // chip_volumes: show chip id, instance, volume and whether it's paired
        if !eh.chip_volumes.is_empty() {
            let mut lines: Vec<String> = Vec::new();
            for cv in &eh.chip_volumes {
                lines.push(format!(
                    "{:?} {:?} relative={} vol={} paired={}",
                    cv.chip_id, cv.instance, cv.relative, cv.volume, cv.paired_chip
                ));
            }
            rows.push(("extra_header.chip_volumes".into(), lines.join("\n")));
        }
    }

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
pub fn parse_vgm(file_path: &Path, data: Vec<u8>, logger: Arc<Logger>) -> Result<()> {
    use soundlog::VgmDocument;

    // Parse VGM document
    let doc: VgmDocument = (&data[..])
        .try_into()
        .with_context(|| format!("failed to parse VGM file: {}", file_path.display()))?;

    // Get command offsets and lengths
    let offsets_and_lengths = doc.command_offsets_and_lengths();

    // Print commands with offsets and lengths
    let _ = logger.info(format_args!(
        "{:<12} {:<8} {:<8} {:<8} {:}",
        "Samples", "Index", "Offset", "Length", "Command"
    ));
    let mut total_samples: u64 = 0;

    for (index, (cmd, (offset, length))) in doc
        .commands
        .iter()
        .zip(offsets_and_lengths.iter())
        .enumerate()
    {
        // Accumulate samples from Wait-family commands.
        // WaitNSample stores raw n (0..=15); actual wait is n+1 samples.
        // YM2612Port0Address2AWriteAndWaitN stores n directly (no +1).
        let delta: u64 = match cmd {
            soundlog::VgmCommand::WaitSamples(s) => s.0 as u64,
            soundlog::VgmCommand::Wait735Samples(_) => 735,
            soundlog::VgmCommand::Wait882Samples(_) => 882,
            soundlog::VgmCommand::WaitNSample(s) => s.0 as u64 + 1,
            soundlog::VgmCommand::YM2612Port0Address2AWriteAndWaitN(s) => s.0 as u64,
            _ => 0,
        };
        let samples_at_issue = total_samples;
        total_samples += delta;

        let _ = logger.info(format_args!(
            "{:<12} {:<8} 0x{:06X} {:<8} {:<80}",
            samples_at_issue,
            index,
            offset,
            length,
            CommandBrief(cmd)
        ));
    }

    Ok(())
}

/// Lightweight Display wrapper that formats a `VgmCommand` on-demand without allocating.
///
/// Use `CommandBrief(&cmd)` in `format_args!` to delay formatting until the logger
/// actually performs `write_fmt`. This avoids creating intermediate `String`s
/// when the logger is a Noop (dry-run).
struct CommandBrief<'a>(&'a soundlog::VgmCommand);

impl<'a> std::fmt::Display for CommandBrief<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use soundlog::VgmCommand;

        match self.0 {
            VgmCommand::AY8910StereoMask(m) => write!(f, "AY8910StereoMask({:?})", m),
            VgmCommand::WaitSamples(w) => write!(f, "WaitSamples({})", w.0),
            VgmCommand::Wait735Samples(_) => write!(f, "Wait735Samples"),
            VgmCommand::Wait882Samples(_) => write!(f, "Wait882Samples"),
            // w.0 is the raw n (0..=15); actual wait is n+1 samples.
            VgmCommand::WaitNSample(w) => write!(f, "WaitNSample(n={}, wait={})", w.0, w.0 + 1),
            VgmCommand::EndOfData(_) => write!(f, "EndOfData"),
            VgmCommand::DataBlock(db) => match parse_data_block(*db.clone()) {
                Ok(data_type) => write!(
                    f,
                    "DataBlock({}, size={})",
                    DataBlockTypeDisplay(&data_type),
                    db.size
                ),
                Err((_, err)) => write!(f, "DataBlock(parse_error={}, size={})", err, db.size),
            },
            VgmCommand::PcmRamWrite(p) => write!(f, "PcmRamWrite({:?})", p),
            VgmCommand::YM2612Port0Address2AWriteAndWaitN(s) => {
                write!(f, "YM2612Port0Address2AWriteAndWaitN({:?})", s)
            }
            VgmCommand::SetupStreamControl(s) => write!(
                f,
                "SetupStreamControl(id={}, chip={:?})",
                s.stream_id, s.chip_type
            ),
            VgmCommand::SetStreamData(s) => {
                write!(
                    f,
                    "SetStreamData(id={}, bank=0x{:02X})",
                    s.stream_id, s.data_bank_id
                )
            }
            VgmCommand::SetStreamFrequency(s) => {
                write!(
                    f,
                    "SetStreamFrequency(id={}, freq={})",
                    s.stream_id, s.frequency
                )
            }
            VgmCommand::StartStream(s) => {
                write!(
                    f,
                    "StartStream(id={}, offset=0x{:X})",
                    s.stream_id, s.data_start_offset
                )
            }
            VgmCommand::StopStream(s) => write!(f, "StopStream(id={})", s.stream_id),
            VgmCommand::StartStreamFastCall(s) => write!(f, "StartStreamFastCall({:?})", s),
            VgmCommand::SeekOffset(s) => write!(f, "SeekOffset({:?})", s),
            VgmCommand::Sn76489Write(inst, spec) => {
                write!(f, "Sn76489Write({:?}, 0x{:02X})", inst, spec.value)
            }
            VgmCommand::Ym2413Write(inst, spec) => write!(
                f,
                "Ym2413Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Ym2612Write(inst, spec) => write!(
                f,
                "Ym2612Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Ym2151Write(inst, spec) => write!(
                f,
                "Ym2151Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::SegaPcmWrite(inst, spec) => {
                // offset is u16
                write!(
                    f,
                    "SegaPcmWrite({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                )
            }
            VgmCommand::Rf5c68U8Write(inst, spec) => {
                // offset is u8
                write!(
                    f,
                    "Rf5c68U8Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.offset, spec.value
                )
            }
            VgmCommand::Rf5c68U16Write(inst, spec) => {
                write!(
                    f,
                    "Rf5c68U16Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                )
            }
            VgmCommand::Ym2203Write(inst, spec) => write!(
                f,
                "Ym2203Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Ym2608Write(inst, spec) => write!(
                f,
                "Ym2608Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            ),
            VgmCommand::Ym2610bWrite(inst, spec) => write!(
                f,
                "Ym2610bWrite({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            ),
            VgmCommand::Ym3812Write(inst, spec) => write!(
                f,
                "Ym3812Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Ym3526Write(inst, spec) => write!(
                f,
                "Ym3526Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Y8950Write(inst, spec) => write!(
                f,
                "Y8950Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Ymf262Write(inst, spec) => write!(f, "Ymf262Write({:?}, {:?})", inst, spec),
            VgmCommand::Ymf278bWrite(inst, spec) => {
                write!(f, "Ymf278bWrite({:?}, {:?})", inst, spec)
            }
            VgmCommand::Ymf271Write(inst, spec) => write!(f, "Ymf271Write({:?}, {:?})", inst, spec),
            VgmCommand::Scc1Write(inst, spec) => {
                // Keep Scc1 (VGM) spec debug but show port/register/value explicitly for readability
                write!(
                    f,
                    "Scc1Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                    inst, spec.port, spec.register, spec.value
                )
            }
            VgmCommand::Ymz280bWrite(inst, spec) => write!(
                f,
                "Ymz280bWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Rf5c164U8Write(inst, spec) => write!(
                f,
                "Rf5c164U8Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.offset, spec.value
            ),
            VgmCommand::Rf5c164U16Write(inst, spec) => write!(
                f,
                "Rf5c164U16Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            ),
            VgmCommand::PwmWrite(inst, spec) => {
                // register is low 4 bits; value uses lower 24 bits
                write!(
                    f,
                    "PwmWrite({:?}, reg=0x{:02X}=0x{:06X})",
                    inst,
                    spec.register,
                    spec.value & 0x00FF_FFFF
                )
            }
            VgmCommand::Ay8910Write(inst, spec) => write!(
                f,
                "Ay8910Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::GbDmgWrite(inst, spec) => write!(
                f,
                "GbDmgWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::NesApuWrite(inst, spec) => write!(
                f,
                "NesApuWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::MultiPcmWrite(inst, spec) => write!(
                f,
                "MultiPcmWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::MultiPcmBankWrite(inst, spec) => {
                write!(f, "MultiPcmBankWrite({:?}, {:?})", inst, spec)
            }
            VgmCommand::Upd7759Write(inst, spec) => write!(
                f,
                "Upd7759Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Okim6258Write(inst, spec) => write!(
                f,
                "Okim6258Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Okim6295Write(inst, spec) => write!(
                f,
                "Okim6295Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::K054539Write(inst, spec) => write!(
                f,
                "K054539Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Huc6280Write(inst, spec) => write!(
                f,
                "Huc6280Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::C140Write(inst, spec) => write!(
                f,
                "C140Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::K053260Write(inst, spec) => write!(
                f,
                "K053260Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::PokeyWrite(inst, spec) => write!(
                f,
                "PokeyWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::QsoundWrite(inst, spec) => {
                // register/value combined as u16
                write!(
                    f,
                    "QsoundWrite({:?}, 0x{:04X}=0x{:04X})",
                    inst, spec.register, spec.value
                )
            }
            VgmCommand::ScspWrite(inst, spec) => write!(
                f,
                "ScspWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            ),
            VgmCommand::WonderSwanWrite(inst, spec) => write!(
                f,
                "WonderSwanWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            ),
            VgmCommand::WonderSwanRegWrite(inst, spec) => write!(
                f,
                "WonderSwanRegWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::VsuWrite(inst, spec) => write!(
                f,
                "VsuWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            ),
            VgmCommand::Saa1099Write(inst, spec) => write!(
                f,
                "Saa1099Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Es5503Write(inst, spec) => write!(
                f,
                "Es5503Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Es5506BEWrite(inst, spec) => write!(
                f,
                "Es5506BEWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Es5506D6Write(inst, spec) => write!(
                f,
                "Es5506D6Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::X1010Write(inst, spec) => write!(
                f,
                "X1010Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            ),
            VgmCommand::C352Write(inst, spec) => write!(
                f,
                "C352Write({:?}, 0x{:04X}=0x{:04X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::Ga20Write(inst, spec) => write!(
                f,
                "Ga20Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::MikeyWrite(inst, spec) => write!(
                f,
                "MikeyWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            ),
            VgmCommand::GameGearPsgWrite(inst, spec) => {
                write!(f, "GameGearPsgWrite({:?}, 0x{:02X})", inst, spec.value)
            }
            VgmCommand::ReservedU8Write(r) => write!(f, "ReservedU8Write({:?})", r),
            VgmCommand::ReservedU16Write(r) => write!(f, "ReservedU16Write({:?})", r),
            VgmCommand::ReservedU24Write(r) => write!(f, "ReservedU24Write({:?})", r),
            VgmCommand::ReservedU32Write(r) => write!(f, "ReservedU32Write({:?})", r),
            VgmCommand::UnknownCommand(u) => write!(f, "UnknownCommand({:?})", u),
        }
    }
} // <-- Fixed: close impl block for CommandBrief

/// Display wrapper for `DataBlockType` that formats on-demand without allocating.
///
/// Use `DataBlockTypeDisplay(&data_type)` inside `format_args!` to defer the
/// formatting until `write_fmt` is invoked by the Logger.
struct DataBlockTypeDisplay<'a>(&'a DataBlockType);

impl<'a> std::fmt::Display for DataBlockTypeDisplay<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use soundlog::vgm::detail::DataBlockType;
        match self.0 {
            DataBlockType::UncompressedStream(us) => {
                write!(f, "UncompressedStream({:?})", us.chip_type)
            }
            DataBlockType::CompressedStream(cs) => write!(
                f,
                "CompressedStream({:?}, {:?})",
                cs.chip_type, cs.compression_type
            ),
            DataBlockType::DecompressionTable(dt) => {
                write!(f, "DecompressionTable({:?})", dt.compression_type)
            }
            DataBlockType::RomRamDump(rr) => write!(f, "RomRamDump({:?})", rr.chip_type),
            DataBlockType::RamWrite16(rw) => write!(f, "RamWrite16({:?})", rw.chip_type),
            DataBlockType::RamWrite32(rw) => write!(f, "RamWrite32({:?})", rw.chip_type),
        }
    }
}

/// Backwards-compatible helper: keep existing `format_data_block_type` returning a `String`.
/// Internally it uses the `DataBlockTypeDisplay` wrapper so callers that still need a String
/// will get one, but new call sites can use the display wrapper to avoid allocation.
fn format_data_block_type(data_type: &DataBlockType) -> String {
    DataBlockTypeDisplay(data_type).to_string()
}
