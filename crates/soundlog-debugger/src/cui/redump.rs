// chipstream/crates/soundlog-debugger/src/cui/redump.rs
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use soundlog::VgmBuilder;
use soundlog::VgmDocument;
use soundlog::vgm::stream::{StreamResult, VgmStream};

// Redump VGM file with DAC streams expanded to chip writes.
//
// This function parses the input VGM, processes it through VgmStream (which expands
// DAC Stream Control commands into actual chip writes), and writes the result to
// a new VGM file. This is useful for verifying that stream expansion works correctly.
pub fn redump_vgm(input_path: &Path, output_path: &Path, data: Vec<u8>, diag: bool) -> Result<()> {
    // Parse original VGM document
    let doc_orig: VgmDocument = (&data[..])
        .try_into()
        .with_context(|| format!("failed to parse input VGM: {}", input_path.display()))?;

    // Calculate the original loop command index from the header's loop_offset
    // Always compute the original loop index so we preserve the original loop
    // structure in the redumped output.
    let original_loop_index = doc_orig.loop_command_index();

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

    // Redump after a single playback
    stream.set_loop_count(Some(1));

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
        builder.set_loop_index(index);
    }

    // Set version and sample_rate from original header BEFORE finalize()
    // This is critical because finalize() uses the version to calculate data_offset
    builder.set_version(doc_orig.header.version);
    builder.set_sample_rate(doc_orig.header.sample_rate);

    // Finalize and serialize (calc loop_offser, loop_samples and total_samples)
    let mut doc_rebuilt = builder.finalize();

    // Copy chip-specific configuration fields from original header
    // (these are not copied by register_chip and contain important chip behavior flags,
    // and include typed fields such as `ay_chip_type` and `c140_chip_type`)
    doc_rebuilt.header.sn76489_feedback = doc_orig.header.sn76489_feedback;
    doc_rebuilt.header.sn76489_shift_register_width = doc_orig.header.sn76489_shift_register_width;
    doc_rebuilt.header.sn76489_flags = doc_orig.header.sn76489_flags;
    doc_rebuilt.header.ay_chip_type = doc_orig.header.ay_chip_type;
    doc_rebuilt.header.ay8910_flags = doc_orig.header.ay8910_flags;
    doc_rebuilt.header.ym2203_ay8910_flags = doc_orig.header.ym2203_ay8910_flags;
    doc_rebuilt.header.ym2608_ay8910_flags = doc_orig.header.ym2608_ay8910_flags;
    doc_rebuilt.header.volume_modifier = doc_orig.header.volume_modifier;
    doc_rebuilt.header.reserved_7d = doc_orig.header.reserved_7d;
    doc_rebuilt.header.spcm_interface = doc_orig.header.spcm_interface;
    doc_rebuilt.header.okim6258_flags = doc_orig.header.okim6258_flags;
    doc_rebuilt.header.k054539_flags = doc_orig.header.k054539_flags;
    doc_rebuilt.header.c140_chip_type = doc_orig.header.c140_chip_type;
    doc_rebuilt.header.es5503_output_channels = doc_orig.header.es5503_output_channels;
    doc_rebuilt.header.es5506_output_channels = doc_orig.header.es5506_output_channels;
    doc_rebuilt.header.c352_clock_divider = doc_orig.header.c352_clock_divider;

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
