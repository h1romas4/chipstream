// Pokey event test. (WIP: NOISE)
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::PokeyState;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// Typical POKEY master clock for NTSC Atari machines (Hz)
const POKEY_MASTER_CLOCK: f32 = 1_789_790.0_f32;

/// Allowed tolerance when checking observed frequency (Hz)
const POKEY_TOLERANCE_HZ: f32 = 4.0;

#[test]
fn test_pokey_keyon_and_tone_freq_matches_a4() {
    // Target frequency (A4)
    let target_hz = 440.0_f32;

    // The Pokey state tracker assumes AUDCTL is 0x00 by default, which means
    // channels use the "base clock" (64k mode) that we approximate as
    // master_clock / 28. Compute AUDF from that effective clock so the
    // resulting tone will be near 440 Hz.
    let base_clock = POKEY_MASTER_CLOCK / 28.0_f32; // effective base clock (64 kHz approximation)

    // Compute AUDF value for POKEY using simplified formula with effective base_clock:
    // freq = base_clock / (2 * (AUDF + 1))
    // => AUDF = (base_clock / (2 * freq)) - 1
    let ideal = base_clock / (2.0_f32 * target_hz) - 1.0_f32;
    let audf = ideal.round().clamp(0.0, 255.0) as u8;

    // Build a VGM document:
    //  - register POKEY in header
    //  - write AUDF (register 0x00) with computed audf
    //  - write AUDC (register 0x01) with non-zero volume to enable channel
    //  - wait 22100 samples
    //  - write AUDC = 0 (volume 0) to key off
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pokey, Instance::Primary, POKEY_MASTER_CLOCK as u32);

    // Ensure AUDCTL is explicitly set to 0x00 (default) so the test is deterministic
    builder.add_chip_write(
        Instance::Primary,
        chip::PokeySpec {
            register: 0x08,
            value: 0x00,
        },
    );

    // AUDF1 (channel 0 frequency)
    builder.add_chip_write(
        Instance::Primary,
        chip::PokeySpec {
            register: 0x00,
            value: audf,
        },
    );

    // AUDC1 (channel 0 control) — key on
    builder.add_chip_write(
        Instance::Primary,
        chip::PokeySpec {
            register: 0x01,
            value: 0xEF,
        },
    );

    // Wait 22100 samples so downstream logic can detect duration, then key off
    builder.add_vgm_command(WaitSamples(22100));

    // AUDC1 (channel 0 control) — key off
    builder.add_chip_write(
        Instance::Primary,
        chip::PokeySpec {
            register: 0x01,
            value: 0xE0,
        },
    );

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    // Optionally write artifact via shim helper (no-op if OUTPUT_VGM_DIR is None)
    super::maybe_write_vgm("pokey_a4.vgm", &vgm_bytes);

    // Create callback stream and enable POKEY state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<PokeyState>(Instance::Primary, POKEY_MASTER_CLOCK);

    // Capture first observed KeyOn frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_cb = captured_freq.clone();

    // Register callback for Pokey writes
    callback_stream.on_write(move |_inst, _spec: chip::PokeySpec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { tone, .. } = ev {
                    let mut guard = captured_freq_cb.lock().unwrap();
                    *guard = tone.freq_hz;
                }
            }
        }
    });

    // Iterate the stream (bounded) to process commands and invoke callbacks.
    for _ in (&mut callback_stream).take(200) {
        // no-op; callback captures the freq
    }

    // Assert we observed a key-on and that the frequency is within tolerance.
    let got_guard = captured_freq.lock().unwrap();
    let got_opt = *got_guard; // copy out Option<f32> (Option<f32> is Copy)
    assert!(
        got_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for POKEY, but none was captured"
    );
    let freq = got_opt.unwrap();
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= POKEY_TOLERANCE_HZ,
        "POKEY ToneInfo.freq_hz differs from expected: got {} Hz, expected {} Hz (diff {})",
        freq,
        target_hz,
        diff
    );
}
