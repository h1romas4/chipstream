// VSU (Virtual Boy) event test (A4 via VSU channel 0).
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::VsuState;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// VSU master clock (adjusted for test so A4 fits within VSU range)
const VSU_MASTER_CLOCK: f32 = 5000000.000f32;

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const VSU_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_vsu_channel_keyon_and_tone_freq_matches_a4() {
    // Target pitch (A4)
    let target_hz = 440.0_f32;

    // VSU formula from state implementation:
    // freq = master_clock / (32 * (2048 - frequency_value))
    let period_f = (VSU_MASTER_CLOCK / (32.0 * target_hz)).round();
    assert!(
        (1.0..2048.0).contains(&period_f),
        "computed period out of range"
    );
    let period = period_f as u16;

    // freq is 11-bit
    let freq_val = 2048u16.saturating_sub(period);
    assert!(freq_val <= 0x07FF, "computed freq out of 11-bit range");

    // split into low and high register values (11-bit: low 8, high low-3-bits)
    let low = (freq_val & 0xFF) as u8;
    let high = ((freq_val >> 8) & 0x07) as u8;

    // Build VGM: register VSU chip, write freq low/high (relative offsets),
    // set volume, enable channel, and wait to observe KeyOn.
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Vsu, Instance::Primary, VSU_MASTER_CLOCK as u32);

    // Waveform
    for i in 0..32 {
        builder.add_chip_write(
            Instance::Primary,
            chip::VsuSpec {
                offset: i as u16,
                value: if i > 15 { 0x00 } else { 0x3F },
            },
        );
    }

    // Write frequency low (target VSU address 0x0408)
    builder.add_chip_write(
        Instance::Primary,
        chip::VsuSpec {
            offset: 0x0102,
            value: low,
        },
    );

    // Write frequency high (target VSU address 0x040C) - lower 3 bits used
    builder.add_chip_write(
        Instance::Primary,
        chip::VsuSpec {
            offset: 0x0103,
            value: high,
        },
    );

    // Set volume for channel 0 (target VSU address 0x0404). Use non-zero -> key-on possible.
    builder.add_chip_write(
        Instance::Primary,
        chip::VsuSpec {
            offset: 0x0101,
            value: 0x88,
        },
    );

    // Enable interval for channel 0 (target VSU address 0x0400 bit7)
    builder.add_chip_write(
        Instance::Primary,
        chip::VsuSpec {
            offset: 0x0100,
            value: 0x80,
        },
    );

    // Wait enough samples so the callback stream can detect KeyOn
    builder.add_vgm_command(WaitSamples(22100));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("vsu_a4_no_sound.vgm", &vgm_bytes); // TODO: NO SOUND

    // Create VgmCallbackStream and enable VSU state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<VsuState>(Instance::Primary, VSU_MASTER_CLOCK);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register callback for VSU writes.
    callback_stream.on_write(move |_inst, _spec: chip::VsuSpec, _sample, event_opt| {
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

    // Assert we observed a KeyOn and compute the expected frequency from the actual registers written.
    let freq_opt = {
        let guard = captured_freq.lock().unwrap();
        *guard
    };
    assert!(
        freq_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for VSU channel, but none was captured"
    );
    let freq_hz = freq_opt.expect("ToneInfo.freq_hz should be Some");

    // Recompute expected frequency from the registers we wrote (low, high).
    let freq_val: u16 = (low as u16) | (((high & 0x07) as u16) << 8);
    let expected_hz = if freq_val > 0 {
        VSU_MASTER_CLOCK / (32.0_f32 * (2048.0_f32 - freq_val as f32))
    } else {
        0.0_f32
    };

    let diff = (freq_hz - expected_hz).abs();
    assert!(
        diff <= VSU_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from expected: got {} Hz, expected {} Hz (diff {})",
        freq_hz,
        expected_hz,
        diff
    );
}
