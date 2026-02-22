// SAA1099 event test (A4 via SAA1099 channel 0).
//
// This test constructs a minimal VGM that programs SAA1099 registers to
// produce a tone near A4 = 440 Hz on channel 0 and asserts that the
// state-tracker emits a KeyOn with a ToneInfo.freq_hz close to the target.
//
// The SAA1099 frequency formula used by the state implementation is:
//   freq_hz = master_clock / ((511 - frequency) * 2^(8 - octave))
//
// We search for an octave in 0..7 that yields a valid 8-bit `frequency`
// register (0..=255) when targeting 440 Hz and pick that for the test.
//
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Saa1099State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

/// Master clock used for the test (common SAA1099 value for SAM Coupé)
const SAA1099_MASTER_CLOCK: f32 = 8_000_000.0_f32;

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const SAA1099_TOLERANCE_HZ: f32 = 3.0;

#[test]
fn test_saa1099_channel_keyon_and_tone_freq_matches_a4() {
    // Target pitch (A4)
    let target_hz = 440.0_f32;

    // Build VGM: register SAA1099 chip, write frequency, octave, enable flags,
    // amplitude and then wait so the callback stream can detect KeyOn.
    let mut builder = VgmBuilder::new();
    builder.register_chip(
        Chip::Saa1099,
        Instance::Primary,
        SAA1099_MASTER_CLOCK as u32,
    );

    // Write frequency register for channel 0: target register 0x08
    builder.add_chip_write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x08,
            value: 0xe3,
        },
    );

    // Write octave register (0x10 controls channels 0 and 1) - low 3 bits for ch0
    builder.add_chip_write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x10,
            value: 0x2,
        },
    );

    // Enable all-channels (0x1C bit0 = 1)
    builder.add_chip_write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x1C,
            value: 0x01,
        },
    );

    // Enable frequency for channel 0 (0x14 bit 0)
    builder.add_chip_write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x14,
            value: 0x01,
        },
    );

    // Set amplitude for channel 0 (0x00) to a non-zero value to allow KeyOn
    builder.add_chip_write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x00,
            value: 0x88,
        },
    );

    // Wait enough samples so the callback stream can detect KeyOn
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(22050));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("saa1099_a4.vgm", &vgm_bytes);

    // Create VgmCallbackStream and enable SAA1099 state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Saa1099State>(Instance::Primary, SAA1099_MASTER_CLOCK);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register callback for SAA1099 writes.
    callback_stream.on_write(move |_inst, _spec: chip::Saa1099Spec, _sample, event_opt| {
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
    for _ in (&mut callback_stream).take(300) {
        // no-op; callback captures the freq
    }

    // Assert we observed a KeyOn and compute the expected frequency from the actual registers written.
    let freq_opt = {
        let guard = captured_freq.lock().unwrap();
        *guard
    };
    assert!(
        freq_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for SAA1099 channel, but none was captured"
    );
    let freq_hz = freq_opt.expect("ToneInfo.freq_hz should be Some");
    let diff = (freq_hz - target_hz).abs();
    assert!(
        diff <= SAA1099_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from expected: got {} Hz, expected {} Hz (diff {})",
        freq_hz,
        target_hz,
        diff
    );
}
