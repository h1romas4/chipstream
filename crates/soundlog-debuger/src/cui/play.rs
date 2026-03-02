use std::path::Path;

use anyhow::{Context, Result};
use soundlog::chip::event::StateEvent;
use soundlog::vgm::command::Instance;
use soundlog::vgm::stream::StreamResult;
use soundlog::{VgmCallbackStream, VgmHeader, VgmStream, chip};

/// Play VGM file using VgmCallbackStream and output register logs with events
pub fn play_vgm(
    file_path: &Path,
    data: Vec<u8>,
    dry_run: bool,
    loop_count: Option<u32>,
    loop_modifier: Option<u8>,
    loop_base: Option<i8>,
) -> Result<()> {
    // Parse header only (for chip instance configuration)
    let header = VgmHeader::from_bytes(&data)
        .with_context(|| format!("failed to parse VGM header: {}", file_path.display()))?;
    let instances = header.chip_instances();

    // Create stream and callback stream
    let mut stream = VgmStream::from_vgm(data)
        .with_context(|| format!("failed to create VGM stream: {}", file_path.display()))?;
    if let Some(n) = loop_count {
        stream.set_loop_count(Some(n));
    }
    if let Some(m) = loop_modifier {
        stream.set_loop_modifier(m);
    }
    if let Some(b) = loop_base {
        stream.set_loop_base(b);
    }
    let mut callback_stream = VgmCallbackStream::new(stream);

    if !dry_run {
        println!("{:<12} {:<40} Events", "Sample", "Register Write");
        println!("{}", "-".repeat(100));
    }

    // Track state for all chip types present in the file
    callback_stream.track_chips(&instances);

    // Register callbacks for all chip types
    callback_stream.on_write(
        |inst: Instance, spec: chip::PsgSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!("Sn76489Write({:?}, 0x{:02X})", inst, spec.value);
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2413Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2413Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2612Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2612Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2151Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2151Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2203Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2203Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2608Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym2608Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2610Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            // format_command_brief uses Ym2610bWrite for the Ym2610b command; keep the Write style
            let reg_info = format!(
                "Ym2610bWrite({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym3812Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym3812Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym3526Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ym3526Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Y8950Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Y8950Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf262Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!("Ymf262Write({:?}, {:?})", inst, spec);
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf278bSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!("Ymf278bWrite({:?}, {:?})", inst, spec);
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf271Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!("Ymf271Write({:?}, {:?})", inst, spec);
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymz280bSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ymz280bWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Rf5c68U8Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Rf5c68U8Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Rf5c68U16Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Rf5c68U16Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Rf5c164U16Spec,
         sample: usize,
         event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Rf5c164U16Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::SegaPcmSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "SegaPcmWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::QsoundSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "QsoundWrite({:?}, 0x{:04X}=0x{:04X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::ScspSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "ScspWrite({:?}, 0x{:04X}=0x{:04X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::WonderSwanSpec,
         sample: usize,
         event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "WonderSwanWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::WonderSwanRegSpec,
         sample: usize,
         event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "WonderSwanRegWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::VsuSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "VsuWrite({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Saa1099Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Saa1099Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5503Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Es5503Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5506U8Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Es5506U8Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5506U16Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Es5506U16Write({:?}, 0x{:02X}=0x{:04X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::X1010Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "X1010Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.offset, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::C352Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "C352Write({:?}, 0x{:04X}=0x{:04X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ga20Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ga20Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::MultiPcmSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "MultiPcmWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Upd7759Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Upd7759Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Okim6258Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Okim6258Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Okim6295Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Okim6295Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::K054539Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "K054539Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    // Harmonize write naming with parse output (use XWrite form)
    callback_stream.on_write(
        |inst: Instance, spec: chip::Huc6280Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Huc6280Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::C140Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "C140Write({:?}, 0x{:04X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::K053260Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "K053260Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::PokeySpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "PokeyWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ay8910Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "Ay8910Write({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::GbDmgSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "GbDmgWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::NesApuSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "NesApuWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::MikeySpec, sample: usize, event: Option<Vec<StateEvent>>| {
            let reg_info = format!(
                "MikeyWrite({:?}, 0x{:02X}=0x{:02X})",
                inst, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Scc1Spec, sample: usize, event: Option<Vec<StateEvent>>| {
            // Keep explicit Scc1Write format used by parse for readability
            let reg_info = format!(
                "Scc1Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                inst, spec.port, spec.register, spec.value
            );
            print_register_log(sample, &reg_info, event, dry_run);
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::PwmSpec, sample: usize, event: Option<Vec<StateEvent>>| {
            // Match parse's PwmWrite formatting (show register and 24-bit value)
            let reg_info = format!(
                "PwmWrite({:?}, reg=0x{:02X}=0x{:06X})",
                inst,
                spec.register,
                spec.value & 0x00FF_FFFF
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

    Ok(())
}

/// Helper function to print register log line with events
fn print_register_log(sample: usize, reg_info: &str, events: Option<Vec<StateEvent>>, dry_run: bool) {
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
