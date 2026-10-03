// chipstream/crates/soundlog-cli/src/cui/redump.rs
use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Context, Result};

use soundlog::VgmBuilder;
use soundlog::VgmDocument;
use soundlog::vgm::command::{VgmCommand, command_to_vgm_bytes};
use soundlog::vgm::stream::{StreamResult, VgmStream};

#[derive(Clone, Copy)]
struct RedumpLimits {
    max_commands: usize,
    max_output_bytes: usize,
}

const DEFAULT_REDUMP_LIMITS: RedumpLimits = RedumpLimits {
    max_commands: 10_000_000,
    max_output_bytes: 64 * 1024 * 1024,
};

struct RedumpBudget {
    limits: RedumpLimits,
    commands: usize,
    bytes: usize,
}

impl RedumpBudget {
    fn new(limits: RedumpLimits) -> Self {
        Self {
            limits,
            commands: 0,
            bytes: 0,
        }
    }

    fn account(&mut self, command: &VgmCommand) -> Result<()> {
        if self.commands >= self.limits.max_commands {
            anyhow::bail!(
                "redump output command limit exceeded: limit {} commands",
                self.limits.max_commands
            );
        }
        let remaining = self.limits.max_output_bytes - self.bytes;
        if let VgmCommand::DataBlock(block) = command
            && block.data.len() > remaining
        {
            anyhow::bail!(
                "redump output byte limit exceeded: limit {} bytes",
                self.limits.max_output_bytes
            );
        }
        let (_, len) = command_to_vgm_bytes(command);
        if len > remaining {
            anyhow::bail!(
                "redump output byte limit exceeded: limit {} bytes",
                self.limits.max_output_bytes
            );
        }
        self.commands += 1;
        self.bytes += len;
        Ok(())
    }
}

// Redump VGM file with DAC streams expanded to chip writes.
//
// This function parses the input VGM, processes it through VgmStream (which expands
// DAC Stream Control commands into actual chip writes), and writes the result to
// a new VGM file. This is useful for verifying that stream expansion works correctly.
pub fn redump_vgm(input_path: &Path, output_path: &Path, data: Vec<u8>, diag: bool) -> Result<()> {
    redump_vgm_with_limits(input_path, output_path, data, diag, DEFAULT_REDUMP_LIMITS)
}

fn redump_vgm_with_limits(
    input_path: &Path,
    output_path: &Path,
    data: Vec<u8>,
    diag: bool,
    limits: RedumpLimits,
) -> Result<()> {
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

        let mut intro_budget = RedumpBudget::new(limits);
        loop {
            match intro_stream.next() {
                Some(Ok(StreamResult::Command(cmd))) => {
                    intro_budget
                        .account(&cmd)
                        .context("redump intro expansion output limit exceeded")?;
                }
                Some(Ok(StreamResult::NeedsMoreData)) => break,
                Some(Ok(StreamResult::EndOfStream)) => break,
                Some(Err(e)) => {
                    return Err(e).context("stream processing error in intro expansion");
                }
                None => break,
            }
        }

        Some(intro_budget.commands)
    } else {
        None
    };

    // Create VgmStream from document for full expansion
    let mut stream = VgmStream::from_document(doc_orig.clone());

    // Redump after a single playback
    stream.set_loop_count(Some(1));

    // Collect all commands from stream
    let mut commands = Vec::new();
    let mut budget = RedumpBudget::new(limits);
    loop {
        match stream.next() {
            Some(Ok(StreamResult::Command(cmd))) => {
                budget.account(&cmd)?;
                commands.push(cmd);
            }
            Some(Ok(StreamResult::NeedsMoreData)) => {
                break;
            }
            Some(Ok(StreamResult::EndOfStream)) => {
                break;
            }
            Some(Err(e)) => {
                return Err(e).context("stream processing error");
            }
            None => {
                break;
            }
        }
    }

    // Ensure the redumped command stream terminates with EndOfData
    let end = VgmCommand::EndOfData(soundlog::vgm::command::EndOfData);
    budget.account(&end)?;
    commands.push(end);

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
    if rebuilt_bytes.len() > limits.max_output_bytes {
        anyhow::bail!(
            "redump output byte limit exceeded: {} bytes including header and metadata, limit {} bytes",
            rebuilt_bytes.len(),
            limits.max_output_bytes
        );
    }

    // Write to output file or stdout if output_path is "-" (convention)
    if output_path == Path::new("-") {
        // Write to stdout
        use std::io::Write;
        let mut stdout = io::stdout();
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

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::vgm::command::{DataBlock, EndOfData, WaitNSample, WaitSamples};

    struct OutputFile(std::path::PathBuf);

    impl OutputFile {
        fn new() -> Self {
            use std::io::Write;
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "soundlog-redump-{}-{}.vgm",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            file.write_all(b"existing output").unwrap();
            Self(path)
        }

        fn assert_unchanged(&self) {
            assert_eq!(fs::read(&self.0).unwrap(), b"existing output");
        }
    }

    impl Drop for OutputFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn dac_input(looped: bool) -> Vec<u8> {
        use soundlog::vgm::command::{
            DacStreamChipType, Instance, LengthMode, SetStreamData, SetStreamFrequency,
            SetupStreamControl, StartStream,
        };
        use soundlog::vgm::detail::{StreamChipType, UncompressedStream};
        use soundlog::vgm::header::ChipId;
        let mut builder = VgmBuilder::new();
        builder.attach_data_block(UncompressedStream {
            chip_type: StreamChipType::Ym2612Pcm,
            data: vec![0x80],
        });
        builder.add_vgm_command(SetupStreamControl {
            stream_id: 0,
            chip_type: DacStreamChipType {
                chip_id: ChipId::Ym2612,
                instance: Instance::Primary,
            },
            write_port: 0,
            write_command: 0x2a,
        });
        builder.add_vgm_command(SetStreamData {
            stream_id: 0,
            data_bank_id: 0,
            step_size: 1,
            step_base: 0,
        });
        builder.add_vgm_command(SetStreamFrequency {
            stream_id: 0,
            frequency: 88_200,
        });
        builder.add_vgm_command(StartStream {
            stream_id: 0,
            data_start_offset: 0,
            length_mode: LengthMode::CommandCount {
                reverse: false,
                looped: true,
            },
            data_length: 1,
        });
        builder.add_vgm_command(WaitSamples(5));
        if looped {
            builder.set_loop_index(builder.command_count());
        }
        builder.add_vgm_command(WaitSamples(1));
        builder.finalize().into()
    }

    #[test]
    fn redump_dac_and_intro_limits_do_not_overwrite_output() {
        for looped in [false, true] {
            let output = OutputFile::new();
            let error = redump_vgm_with_limits(
                Path::new("input.vgm"),
                &output.0,
                dac_input(looped),
                false,
                RedumpLimits {
                    max_commands: 4,
                    max_output_bytes: 1024,
                },
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("command limit"));
            assert_eq!(error.to_string().contains("intro expansion"), looped);
            output.assert_unchanged();
        }
    }

    #[test]
    fn redump_file_byte_boundary_includes_header_and_terminator() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(2));
        let input: Vec<u8> = builder.finalize().into();
        let limits = RedumpLimits {
            max_commands: 2,
            max_output_bytes: input.len(),
        };
        let output = OutputFile::new();
        redump_vgm_with_limits(
            Path::new("input.vgm"),
            &output.0,
            input.clone(),
            false,
            limits,
        )
        .unwrap();
        let bytes = fs::read(&output.0).unwrap();
        assert_eq!(bytes.len(), input.len());
        let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
        assert_eq!(document.commands.len(), 2);
        assert!(matches!(
            document.commands.last(),
            Some(VgmCommand::EndOfData(_))
        ));

        for limits in [
            RedumpLimits {
                max_commands: 2,
                max_output_bytes: input.len() - 1,
            },
            RedumpLimits {
                max_commands: 1,
                max_output_bytes: input.len(),
            },
            RedumpLimits {
                max_commands: 2,
                max_output_bytes: 3,
            },
        ] {
            let output = OutputFile::new();
            assert!(
                redump_vgm_with_limits(
                    Path::new("input.vgm"),
                    &output.0,
                    input.clone(),
                    false,
                    limits
                )
                .is_err()
            );
            output.assert_unchanged();
        }
    }

    #[test]
    fn redump_budget_exact_boundaries_and_rejection_preserve_counts() {
        let mut budget = RedumpBudget::new(RedumpLimits {
            max_commands: 2,
            max_output_bytes: 4,
        });
        budget.account(&WaitSamples(1).into()).unwrap();
        budget.account(&EndOfData.into()).unwrap();
        assert_eq!((budget.commands, budget.bytes), (2, 4));
        assert!(
            budget
                .account(&WaitNSample(0).into())
                .unwrap_err()
                .to_string()
                .contains("command limit")
        );
        assert_eq!((budget.commands, budget.bytes), (2, 4));

        let mut budget = RedumpBudget::new(RedumpLimits {
            max_commands: 3,
            max_output_bytes: 3,
        });
        budget.account(&WaitSamples(1).into()).unwrap();
        assert!(
            budget
                .account(&EndOfData.into())
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
        assert_eq!((budget.commands, budget.bytes), (1, 3));
    }

    #[test]
    fn redump_budget_zero_and_oversized_data_block() {
        for limits in [
            RedumpLimits {
                max_commands: 0,
                max_output_bytes: 1,
            },
            RedumpLimits {
                max_commands: 1,
                max_output_bytes: 0,
            },
        ] {
            let mut budget = RedumpBudget::new(limits);
            assert!(budget.account(&EndOfData.into()).is_err());
            assert_eq!((budget.commands, budget.bytes), (0, 0));
        }
        let mut budget = RedumpBudget::new(RedumpLimits {
            max_commands: 1,
            max_output_bytes: 4,
        });
        let block = DataBlock {
            marker: 0x66,
            chip_instance: 0,
            data_type: 0xff,
            size: 8,
            data: vec![0; 8],
        };
        assert!(
            budget
                .account(&block.into())
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
        assert_eq!((budget.commands, budget.bytes), (0, 0));
    }
}
