// chipstream/crates/soundlog-debuger/src/cui/redump.rs
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use soundlog::VgmBuilder;
use soundlog::VgmDocument;
use soundlog::vgm::stream::{StreamResult, VgmStream};

/// Redump VGM file with DAC streams expanded to chip writes.
///
/// This function parses the input VGM, processes it through VgmStream (which expands
/// DAC Stream Control commands into actual chip writes), and writes the result to
/// a new VGM file. This is useful for verifying that stream expansion works correctly.
pub fn redump_vgm(
    input_path: &Path,
    output_path: &Path,
    data: Vec<u8>,
    loop_count: Option<u32>,
    fadeout_samples: Option<u64>,
    diag: bool,
) -> Result<()> {
    // Parse original VGM document
    let doc_orig: VgmDocument = (&data[..])
        .try_into()
        .with_context(|| format!("failed to parse input VGM: {}", input_path.display()))?;

    // Calculate the original loop command index from the header's loop_offset
    let original_loop_index = if loop_count.is_none() {
        doc_orig.loop_command_index()
    } else {
        None
    };

    // Determine loop offset in expanded output by processing intro commands
    let output_loop_index = if let Some(orig_loop_idx) = original_loop_index {
        // Create a document with only the intro commands (before the loop point)
        let mut intro_builder = VgmBuilder::new();

        // Copy chip setup from original
        for (instance, chip, _clock_hz) in doc_orig.header.chip_instances() {
            let raw_clock = doc_orig.header.get_chip_clock(&chip);
            let clock = raw_clock & 0x7FFF_FFFF;
            if clock > 0 {
                intro_builder.register_chip(chip, instance, clock);
            }
        }

        // Add only intro commands (commands before the loop point)
        for (idx, cmd) in doc_orig.commands.iter().enumerate() {
            if idx >= orig_loop_idx {
                break;
            }
            intro_builder.add_vgm_command(cmd.clone());
        }

        // Expand the intro commands through VgmStream
        let intro_doc = intro_builder.finalize();
        let mut intro_stream = VgmStream::from_document(intro_doc);
        // Don't set loop_count - we want to process all intro commands exactly once
        // (The intro_doc doesn't have a loop point set, so it will process all commands)

        let mut intro_expanded_count = 0;
        loop {
            match intro_stream.next() {
                Some(Ok(StreamResult::Command(_))) => {
                    intro_expanded_count += 1;
                }
                Some(Ok(StreamResult::NeedsMoreData)) => break,
                Some(Ok(StreamResult::EndOfStream)) => break,
                Some(Err(e)) => {
                    return Err(anyhow::anyhow!(
                        "stream processing error in intro expansion: {}",
                        e
                    ));
                }
                None => break,
            }
        }

        Some(intro_expanded_count)
    } else {
        None
    };

    // Create VgmStream from document for full expansion
    let mut stream = VgmStream::from_document(doc_orig.clone());

    // Configure stream settings
    // If loop_count is specified, use it to expand loops
    // If not specified, set to 1 to preserve original loop structure (intro + one loop iteration)
    if let Some(count) = loop_count {
        stream.set_loop_count(Some(count));
    } else {
        stream.set_loop_count(Some(1));
    }
    if let Some(samples) = fadeout_samples {
        stream.set_fadeout_samples(Some(samples));
    }

    // Collect all commands from stream
    let mut commands = Vec::new();
    loop {
        match stream.next() {
            Some(Ok(StreamResult::Command(cmd))) => {
                commands.push(cmd);
            }
            Some(Ok(StreamResult::NeedsMoreData)) => {
                break;
            }
            Some(Ok(StreamResult::EndOfStream)) => {
                break;
            }
            Some(Err(e)) => {
                return Err(anyhow::anyhow!("stream processing error: {}", e));
            }
            None => {
                break;
            }
        }
    }

    // Ensure the redumped command stream terminates with EndOfData
    commands.push(soundlog::vgm::command::VgmCommand::EndOfData(
        soundlog::vgm::command::EndOfData,
    ));

    // Build new VGM document with expanded commands
    let mut builder = VgmBuilder::new();

    // Copy chip clocks from original header
    // We need to extract the actual clock value (masking the high bit for secondary instances)
    for (instance, chip, _clock_hz) in doc_orig.header.chip_instances() {
        let raw_clock = doc_orig.header.get_chip_clock(&chip);
        let clock = raw_clock & 0x7FFF_FFFF;
        if clock > 0 {
            builder.register_chip(chip, instance, clock);
        }
    }

    // Copy GD3 metadata if present
    if let Some(gd3) = &doc_orig.gd3 {
        builder.set_gd3(gd3.clone());
    }

    // Copy extra header if present
    if let Some(extra) = &doc_orig.extra_header {
        builder.set_extra_header(extra.clone());
    }

    // Add all expanded commands
    for cmd in commands {
        builder.add_vgm_command(cmd);
    }

    // Set loop offset if we're preserving the original loop structure
    if let Some(index) = output_loop_index {
        builder.set_loop_offset(index);
    }

    // Set version and sample_rate from original header BEFORE finalize()
    // This is critical because finalize() uses the version to calculate data_offset
    builder.set_version(doc_orig.header.version);
    builder.set_sample_rate(doc_orig.header.sample_rate);

    // Finalize and serialize
    let mut doc_rebuilt = builder.finalize();

    // If we're preserving loop, copy loop_samples from original
    if loop_count.is_none() && doc_orig.header.loop_offset != 0 {
        doc_rebuilt.header.loop_samples = doc_orig.header.loop_samples;
    }

    // Copy chip-specific configuration fields from original header
    // (these are not copied by register_chip and contain important chip behavior flags)
    doc_rebuilt.header.sn_fb = doc_orig.header.sn_fb;
    doc_rebuilt.header.snw = doc_orig.header.snw;
    doc_rebuilt.header.sf = doc_orig.header.sf;
    doc_rebuilt.header.ay_misc = doc_orig.header.ay_misc;
    doc_rebuilt.header.spcm_interface = doc_orig.header.spcm_interface;
    doc_rebuilt.header.okim6258_flags = doc_orig.header.okim6258_flags;
    doc_rebuilt.header.es5506_channels = doc_orig.header.es5506_channels;
    doc_rebuilt.header.es5506_cd = doc_orig.header.es5506_cd;
    doc_rebuilt.header.es5506_reserved = doc_orig.header.es5506_reserved;

    let rebuilt_bytes: Vec<u8> = (&doc_rebuilt).into();

    // Write to output file or stdout if output_path is "-" (convention)
    if output_path == std::path::Path::new("-") {
        // Write to stdout
        use std::io::Write;
        let mut stdout = std::io::stdout();
        stdout
            .write_all(&rebuilt_bytes)
            .with_context(|| "failed to write output VGM to stdout")?;
    } else {
        fs::write(output_path, &rebuilt_bytes)
            .with_context(|| format!("failed to write output VGM: {}", output_path.display()))?;
    }

    // Re-parse serialized bytes into a VgmDocument
    let doc_reparsed_res: Result<VgmDocument, _> = (&rebuilt_bytes[..]).try_into();
    match doc_reparsed_res {
        Ok(doc_reparsed) => {
            if diag {
                crate::cui::vgm::print_diag_table(&doc_orig, &doc_reparsed);
            }
        }
        Err(e) => {
            eprintln!(
                "\"{}\": roundtrip: serialization produced bytes (len={}), but re-parse failed: {} — run with --diag to see serialized bytes and diagnostics",
                output_path.display(),
                rebuilt_bytes.len(),
                e
            );
        }
    }

    Ok(())
}
