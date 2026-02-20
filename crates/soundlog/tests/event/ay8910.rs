// AY-3-8910 event test.
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Ay8910State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// AY master clock for ZX Spectrum / MSX typical usage
const AY_MASTER_CLOCK: f32 = 1_789_773.0_f32;

/// Allowed tolerance when checking observed frequency (Hz)
const AY_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_ay8910_keyon_and_tone_freq_matches_a4() {
    // Target frequency (A4)
    let target_hz = 440.0_f32;

    // Compute AY period for target frequency:
    // AY formula: f = master_clock / (16 * period)  => period = master_clock / (16 * f)
    let ideal_period = (AY_MASTER_CLOCK / (16.0_f32 * target_hz)).round() as u16;
    assert!(
        ideal_period > 0 && ideal_period <= 0x0FFF,
        "period out of range"
    );

    let fine = (ideal_period & 0xFF) as u8;
    let coarse = ((ideal_period >> 8) & 0x0F) as u8;

    // Build VGM: register AY chip, write period low/high, then enable tone via mixer,
    // wait 22100 samples, then disable tone (key off).
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ay8910, Instance::Primary, AY_MASTER_CLOCK as u32);

    // Period registers for channel A: 0x00 (fine), 0x01 (coarse low nibble)
    builder.add_chip_write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x00,
            value: fine,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x01,
            // coarse uses low 4 bits
            value: coarse & 0x0F,
        },
    );

    // Channel A volume: bits 3-0 = volume level (0=silent, 0xF=max), bit4=0 (fixed vol mode).
    // Must be set before (or alongside) enabling the mixer, otherwise the channel is silent.
    builder.add_chip_write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x08,
            value: 0x0F, // max volume, fixed-level mode
        },
    );

    // Mixer: bit = 1 disables, 0 enables. Start with enabling channel A (clear bit 0).
    // 0b1111_1110 disables all tone channels except A (enables A).
    builder.add_chip_write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x07,
            value: 0b1111_1110u8,
        },
    );

    // Wait 22100 samples so downstream logic can detect duration, then key off
    builder.add_vgm_command(WaitSamples(22100));

    // Key off: disable channel A via mixer (bit 0 = 1) and silence volume (0x00).
    builder.add_chip_write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x07,
            value: 0b1111_1111u8,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x08,
            value: 0x00, // silence channel A volume
        },
    );

    let doc = builder.finalize();

    // Optionally write the VGM artifact via the shim helper (no-op if OUTPUT_VGM_DIR is None).
    let vgm_bytes: Vec<u8> = (&doc).into();
    // Use `super::maybe_write_vgm` so the shim controls where (and whether) we write.
    super::maybe_write_vgm("ay8910_a4.vgm", &vgm_bytes);

    // Create callback stream and enable AY state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ay8910State>(Instance::Primary, AY_MASTER_CLOCK);

    // Capture first observed KeyOn frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register AY write callback
    callback_stream.on_write(move |_inst, _spec: chip::Ay8910Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { channel: _ch, tone } = ev {
                    let mut g = captured_cb.lock().unwrap();
                    *g = tone.freq_hz;
                }
            }
        }
    });

    // Iterate the stream (bounded) to process commands and invoke callbacks.
    for _ in (&mut callback_stream).take(200) {
        // no-op; the callback captures the freq
    }

    // Assert we observed a key-on and that the frequency is within tolerance.
    let got_guard = captured_freq.lock().unwrap();
    let got_opt = *got_guard; // copy Option<f32> out of the guard
    assert!(
        got_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz, but none was captured"
    );
    let freq = got_opt.unwrap();
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= AY_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {} Hz, target {} Hz (diff {})",
        freq,
        target_hz,
        diff
    );
}
