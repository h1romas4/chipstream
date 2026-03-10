use std::fmt;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use soundlog::chip::event::StateEvent;
use soundlog::vgm::command::Instance;
use soundlog::vgm::stream::StreamResult;
use soundlog::{VgmCallbackStream, VgmHeader, VgmStream, chip};

use crate::logger::Logger;

/// Play VGM file using VgmCallbackStream and output register logs with events
///
/// This version accepts an `Arc<Logger>` so callers can create and configure
/// the logger (e.g. a Noop logger for dry-run) and pass it in. Event and
/// register formatting is deferred via `format_args!` and custom `Display`
/// wrappers so that when the logger is a Noop no formatting/allocation occurs.
pub fn play_vgm(
    file_path: &Path,
    data: Vec<u8>,
    logger: Arc<Logger>,
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

    // Header output: print a small header using the logger (noop in dry-run).
    // Use a constant dashed line to avoid allocating at runtime.
    let _ = logger.info(format_args!(
        "{:<12} {:<40} Events",
        "Samples", "Register Write"
    ));

    // Track state for all chip types present in the file
    callback_stream.track_chips(&instances);

    // Display wrapper for a single StateEvent that formats without allocating.
    struct StateEventDisplay<'a>(&'a StateEvent);

    impl<'a> fmt::Display for StateEventDisplay<'a> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.0 {
                StateEvent::KeyOn { channel, tone } => {
                    if let Some(freq) = tone.freq_hz {
                        write!(
                            f,
                            "KeyOn(ch={}, fnum=0x{:03X}({}), freq={:.2}Hz)",
                            channel, tone.fnum, tone.fnum, freq
                        )
                    } else {
                        write!(
                            f,
                            "KeyOn(ch={}, fnum={}, block={})",
                            channel, tone.fnum, tone.block
                        )
                    }
                }
                StateEvent::KeyOff { channel } => write!(f, "KeyOff(ch={})", channel),
                StateEvent::ToneChange { channel, tone } => {
                    if let Some(freq) = tone.freq_hz {
                        write!(
                            f,
                            "ToneChange(ch={}, fnum=0x{:03X}({}), freq={:.2}Hz)",
                            channel, tone.fnum, tone.fnum, freq
                        )
                    } else {
                        write!(
                            f,
                            "ToneChange(ch={}, fnum={}, block={})",
                            channel, tone.fnum, tone.block
                        )
                    }
                }
            }
        }
    }

    // Display wrapper for an optional slice of StateEvent that formats as CSV without allocating.
    struct EventList<'a>(Option<&'a [StateEvent]>);

    impl<'a> fmt::Display for EventList<'a> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.0 {
                None => Ok(()),
                Some(slice) => {
                    let mut first = true;
                    for ev in slice {
                        if !first {
                            f.write_str(", ")?;
                        }
                        first = false;
                        write!(f, "{}", StateEventDisplay(ev))?;
                    }
                    Ok(())
                }
            }
        }
    }

    /// Print a single register log line using deferred formatting.
    /// - `reg_info` is passed as `fmt::Arguments` via `format_args!` at the call site,
    ///   avoiding allocation unless the logger actually writes.
    /// - `events` is passed as an optional slice reference so the `EventList` can borrow it.
    /// - `total_samples` is the running cumulative sample counter maintained by the caller.
    fn print_register_log(
        logger: &Arc<Logger>,
        total_samples: u64,
        reg_info: fmt::Arguments<'_>,
        events: Option<&[StateEvent]>,
    ) {
        // Avoid applying width specifiers to `fmt::Arguments` (they don't take effect).
        // Ensure each log line ends with a newline so outputs don't run together.
        let _ = logger.info(format_args!(
            "{:<12} {} {}",
            total_samples,
            reg_info,
            EventList(events)
        ));
    }

    // Cumulative sample counter shared across all callbacks.
    // Wrapped in a Cell so it can be mutated from within multiple closures
    // without needing a RefCell borrow guard at every call site.
    use std::cell::Cell;
    let total_samples = Cell::new(0u64);

    // Register the on_wait callback first so the counter is updated before
    // any chip-write callbacks that may fire in the same tick.
    callback_stream.on_wait(
        |spec: soundlog::vgm::command::WaitSamples,
         _sample: usize,
         _event: Option<Vec<StateEvent>>| {
            let current = total_samples.get();
            total_samples.set(current + spec.0 as u64);
            let _ = logger.info(format_args!("{:<12} WaitSamples({})", current, spec.0,));
        },
    );

    // Register callbacks for all chip types. Use `format_args!` to defer formatting.
    callback_stream.on_write(
        |inst: Instance, spec: chip::PsgSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!("Sn76489Write({:?}, 0x{:02X})", inst, spec.value),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2413Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym2413Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2612Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym2612Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2151Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym2151Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2203Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym2203Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2608Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym2608Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                    inst, spec.port, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym2610Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            // format_command_brief uses Ym2610bWrite for the Ym2610b command; keep the Write style
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym2610bWrite({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                    inst, spec.port, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym3812Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym3812Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ym3526Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ym3526Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Y8950Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Y8950Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf262Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!("Ymf262Write({:?}, {:?})", inst, spec),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Ymf278bSpec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!("Ymf278bWrite({:?}, {:?})", inst, spec),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ymf271Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!("Ymf271Write({:?}, {:?})", inst, spec),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Ymz280bSpec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ymz280bWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Rf5c68U8Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Rf5c68U8Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Rf5c68U16Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Rf5c68U16Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Rf5c164U16Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Rf5c164U16Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::SegaPcmSpec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "SegaPcmWrite({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::QsoundSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "QsoundWrite({:?}, 0x{:04X}=0x{:04X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::ScspSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "ScspWrite({:?}, 0x{:04X}=0x{:04X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::WonderSwanSpec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "WonderSwanWrite({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::WonderSwanRegSpec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "WonderSwanRegWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::VsuSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "VsuWrite({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Saa1099Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Saa1099Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Es5503Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Es5503Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Es5506U8Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Es5506U8Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Es5506U16Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Es5506U16Write({:?}, 0x{:02X}=0x{:04X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::X1010Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "X1010Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.offset, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::C352Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "C352Write({:?}, 0x{:04X}=0x{:04X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ga20Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ga20Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::MultiPcmSpec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "MultiPcmWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Upd7759Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Upd7759Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Okim6258Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Okim6258Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Okim6295Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Okim6295Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::K054539Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "K054539Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    // Harmonize write naming with parse output (use XWrite form)
    callback_stream.on_write(
        |inst: Instance,
         spec: chip::Huc6280Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Huc6280Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::C140Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "C140Write({:?}, 0x{:04X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance,
         spec: chip::K053260Spec,
         _sample: usize,
         event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "K053260Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::PokeySpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "PokeyWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Ay8910Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Ay8910Write({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::GbDmgSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "GbDmgWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::NesApuSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "NesApuWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::MikeySpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "MikeyWrite({:?}, 0x{:02X}=0x{:02X})",
                    inst, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::Scc1Spec, _sample: usize, event: Option<Vec<StateEvent>>| {
            // Keep explicit Scc1Write format used by parse for readability
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "Scc1Write({:?}, P0x{:02X}:0x{:02X}=0x{:02X})",
                    inst, spec.port, spec.register, spec.value
                ),
                event.as_deref(),
            );
        },
    );

    callback_stream.on_write(
        |inst: Instance, spec: chip::PwmSpec, _sample: usize, event: Option<Vec<StateEvent>>| {
            // Match parse's PwmWrite formatting (show register and 24-bit value)
            print_register_log(
                &logger,
                total_samples.get(),
                format_args!(
                    "PwmWrite({:?}, reg=0x{:02X}=0x{:06X})",
                    inst,
                    spec.register,
                    spec.value & 0x00FF_FFFF
                ),
                event.as_deref(),
            );
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
                let _ = logger.error(format_args!(
                    "{}: Unexpected NeedsMoreData in stream",
                    file_path.display()
                ));
                break;
            }
            Err(e) => {
                let _ = logger.error(format_args!(
                    "{}: Stream error: {:?}",
                    file_path.display(),
                    e
                ));
                break;
            }
        }
    }

    Ok(())
}
