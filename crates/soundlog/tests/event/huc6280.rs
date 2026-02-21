// Huc6280 event test (A4 via wavetable channel).
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Huc6280State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// HuC6280 master clock (typical NTSC value)
const HUC_MASTER_CLOCK: f32 = 3_579_545.0_f32;

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const HUC6280_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_huc6280_keyon_and_tone_freq_matches_a4() {
    // Target pitch (A4)
    let target_hz = 440.0_f32;

    // HuC6280 frequency formula (see state impl):
    //   freq_hz = master_clock / 32 / period
    // Solve for period:
    //   period = round(master_clock / (32 * target_hz))
    let period_f = (HUC_MASTER_CLOCK / (32.0_f32 * target_hz)).round();
    assert!(
        (1.0..4096.0).contains(&period_f),
        "computed period value out of range"
    );
    let period = period_f as u16;

    // split into low and high register values (12-bit: low 8, high 4)
    let low = (period & 0xFF) as u8;
    let high = ((period >> 8) & 0x0F) as u8;

    // Build VGM: register HuC6280 chip, select channel 0, set freq low/high,
    // enable channel via 0x04, and wait to observe KeyOn.
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Huc6280, Instance::Primary, HUC_MASTER_CLOCK as u32);

    // Select channel 0
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x00,
            value: 0x00,
        },
    );

    // Noise Enable, Noise Frequency
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x07,
            value: 0x00,
        },
    );

    // LFO Frequency
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x08,
            value: 0x00,
        },
    );

    // LFO Trigger, Control
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x09,
            value: 0x00,
        },
    );

    // Main Amplitude Level
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x01,
            value: 0xEE,
        },
    );

    // CH Amplitude Level
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x04,
            value: 0x40,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x04,
            value: 0x00,
        },
    );

    // Waveform
    for i in 0..32 {
        builder.add_chip_write(
            Instance::Primary,
            chip::Huc6280Spec {
                register: 0x06,
                value: if i > 15 { 0x00 } else { 0x1F },
            },
        );
    }

    // Frequency low (0x02)
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x02,
            value: low,
        },
    );

    // Frequency high (0x03) - only lower 4 bits used
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x03,
            value: high,
        },
    );

    // Left / Right Amplitude Level
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x05,
            value: 0xFF,
        },
    );

    // Enable channel (0x04) - bit7 = 1
    builder.add_chip_write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x04,
            value: 0x9F,
        },
    );

    // Wait some samples so state tracker can emit events
    builder.add_vgm_command(WaitSamples(22100));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    // Optionally write artifact for debugging; no-op if helper not configured.
    super::maybe_write_vgm("huc6280_a4.vgm", &vgm_bytes);

    // Create callback stream and enable Huc6280 state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Huc6280State>(Instance::Primary, HUC_MASTER_CLOCK);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register callback for Huc6280 writes.
    callback_stream.on_write(move |_inst, _spec: chip::Huc6280Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { tone, .. } = ev {
                    let mut g = captured_cb.lock().unwrap();
                    if g.is_none() {
                        *g = tone.freq_hz;
                    }
                }
            }
        }
    });

    // Iterate the stream (bounded) to process commands and invoke callbacks.
    for _ in (&mut callback_stream).take(200) {
        // no-op; callback captures the freq
    }

    // Assert we observed a KeyOn and the computed freq_hz is close to target.
    let freq_opt = {
        let guard = captured_freq.lock().unwrap();
        *guard
    };
    assert!(
        freq_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for Huc6280 channel, but none was captured"
    );
    let freq_hz = freq_opt.expect("ToneInfo.freq_hz should be Some");
    let diff = (freq_hz - target_hz).abs();
    assert!(
        diff <= HUC6280_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {} Hz, target {} Hz (diff {})",
        freq_hz,
        target_hz,
        diff
    );
}
