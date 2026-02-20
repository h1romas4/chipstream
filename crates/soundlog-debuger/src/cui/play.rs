use std::path::Path;

use anyhow::{Context, Result};
use soundlog::chip::event::StateEvent;
use soundlog::vgm::command::Instance;
use soundlog::vgm::stream::StreamResult;
use soundlog::{VgmCallbackStream, VgmDocument, VgmStream, chip};

/// Play VGM file using VgmCallbackStream and output register logs with events
pub fn play_vgm(file_path: &Path, data: Vec<u8>, dry_run: bool) -> Result<()> {
    // Parse VGM document
    let doc: VgmDocument = (&data[..])
        .try_into()
        .with_context(|| format!("failed to parse VGM file: {}", file_path.display()))?;

    // Print header information
    if !dry_run {
        println!("=== VGM File: {} ===", file_path.display());
        println!("Version: 0x{:08X}", doc.header.version);
        println!("Total Samples: {}", doc.header.total_samples);
        println!("Loop Offset: 0x{:08X}", doc.header.loop_offset);
        println!("Loop Samples: {}", doc.header.loop_samples);

        // Show chip instances and clocks
        let instances = doc.header.chip_instances();
        if !instances.is_empty() {
            println!("Chips:");
            for (inst, chip, _clock_hz) in &instances {
                let raw_clock = doc.header.get_chip_clock(chip);
                let clock = raw_clock & 0x7FFF_FFFF;
                println!("  {:?} (instance {:?}): {} Hz", chip, inst, clock);
            }
        }

        println!();
        println!("Register Write Log:");
        println!("{:<12} {:<40} Events", "Sample", "Register Write");
        println!("{}", "-".repeat(100));
    }

    let instances = doc.header.chip_instances();

    // Create stream and callback stream
    let stream = VgmStream::from_document(doc);
    let mut callback_stream = VgmCallbackStream::new(stream);

    // Track state for all chip types present in the file
    callback_stream.track_chips(&instances);

    // Register callbacks for all chip types
    callback_stream.on_write(
        |inst: Instance, spec: chip::PsgSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!("Sn76489[{:?}] 0x{:02X}", inst, spec.value);
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2413Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2413[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2612Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2612[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2151Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2151[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2203Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2203[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2608Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2608[{:?}] P0x{:02X}:0x{:02X}=0x{:02X}",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2610Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2610[{:?}] P0x{:02X}:0x{:02X}=0x{:02X}",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym3812Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym3812[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym3526Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym3526[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Y8950Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Y8950[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf262Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ymf262[{:?}] P0x{:02X}:0x{:02X}=0x{:02X}",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf278bSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ymf278b[{:?}] P0x{:02X}:0x{:02X}=0x{:02X}",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf271Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ymf271[{:?}] P0x{:02X}:0x{:02X}=0x{:02X}",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymz280bSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ymz280b[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Rf5c68U8Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Rf5c68[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Rf5c68U16Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Rf5c68[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Rf5c164U16Spec,
         sample: u64,
         event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Rf5c164[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::SegaPcmSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "SegaPcm[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::QsoundSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "QSound[{:?}] 0x{:02X}=0x{:04X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::ScspSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Scsp[{:?}] 0x{:04X}=0x{:04X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::WonderSwanSpec,
         sample: u64,
         event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "WonderSwan[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::VsuSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!("Vsu[{:?}] 0x{:04X}=0x{:02X}", inst, spec.offset, spec.value);
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Saa1099Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Saa1099[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5503Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Es5503[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5506U8Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Es5506[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5506U16Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Es5506[{:?}] 0x{:02X}=0x{:04X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::X1010Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "X1010[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::C352Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "C352[{:?}] 0x{:04X}=0x{:04X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ga20Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ga20[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::MultiPcmSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "MultiPcm[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Upd7759Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Upd7759[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Okim6258Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Okim6258[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Okim6295Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Okim6295[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::K054539Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "K054539[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Huc6280Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Huc6280[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::C140Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "C140[{:?}] 0x{:04X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::K053260Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "K053260[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::PokeySpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Pokey[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ay8910Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ay8910[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::GbDmgSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "GbDmg[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::NesApuSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "NesApu[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::MikeySpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Mikey[{:?}] 0x{:02X}=0x{:02X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Scc1Spec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Scc1[{:?}] P0x{:02X}:0x{:02X}=0x{:02X}",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::PwmSpec, sample: u64, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Pwm[{:?}] 0x{:02X}=0x{:06X}",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    // Process the stream
    for result in callback_stream {
        match result {
            Ok(StreamResult::Command(_)) => {
                // Callbacks have already been invoked
            }
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => {
                // Should not happen when using VgmStream::from_document
                eprintln!(
                    "{}: Unexpected NeedsMoreData in stream",
                    file_path.display()
                );
                break;
            }
            Err(e) => {
                eprintln!("{}: Stream error: {:?}", file_path.display(), e);
                break;
            }
        }
    }

    if !dry_run {
        println!();
        println!("=== Playback Complete ===");
    }

    Ok(())
}

/// Helper function to print register log line with events
fn print_register_log(sample: u64, reg_info: &str, events: Option<Vec<StateEvent>>, dry_run: bool) {
    let event_str = if let Some(evs) = events {
        if evs.is_empty() {
            String::new()
        } else {
            evs.iter().map(format_event).collect::<Vec<_>>().join(", ")
        }
    } else {
        String::new()
    };

    if !dry_run {
        println!("{:<12} {:<40} {}", sample, reg_info, event_str);
    }
}

/// Format a StateEvent for display
fn format_event(event: &StateEvent) -> String {
    match event {
        StateEvent::KeyOn { channel, tone } => {
            if let Some(freq) = tone.freq_hz {
                // Show both the chip f-number and the calculated Hz to avoid confusion.
                format!(
                    "KeyOn(ch={}, fnum=0x{:03X}({}), freq={:.2}Hz)",
                    channel, tone.fnum, tone.fnum, freq
                )
            } else {
                format!(
                    "KeyOn(ch={}, fnum={}, block={})",
                    channel, tone.fnum, tone.block
                )
            }
        }
        StateEvent::KeyOff { channel } => {
            format!("KeyOff(ch={})", channel)
        }
        StateEvent::ToneChange { channel, tone } => {
            if let Some(freq) = tone.freq_hz {
                // Include f-number as well for clarity
                format!(
                    "ToneChange(ch={}, fnum=0x{:03X}({}), freq={:.2}Hz)",
                    channel, tone.fnum, tone.fnum, freq
                )
            } else {
                format!(
                    "ToneChange(ch={}, fnum={}, block={})",
                    channel, tone.fnum, tone.block
                )
            }
        }
    }
}
