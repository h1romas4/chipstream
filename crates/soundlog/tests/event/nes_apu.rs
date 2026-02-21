// NES APU event test (A4 via Pulse channel).
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::NesApuState;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// NES APU master clock (N2A03)
const NES_MASTER_CLOCK: f32 = 1_789_773.0_f32;

/// Internal NES APU base used by the state implementation (see nes_apu.rs)
const NES_CLK_BASE: f32 = 111_860.78_f32;

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const NES_APU_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_nes_apu_pulse_keyon_and_tone_freq_matches_a4() {
    // Target pitch (A4)
    let target_hz = 440.0_f32;

    // For NES pulse channels frequency formula:
    //   freq = NES_CLK_BASE / (timer + 1)
    // Solve for timer:
    //   timer = round(NES_CLK_BASE / target_hz - 1)
    let timer_f = (NES_CLK_BASE / target_hz - 1.0_f32).round();
    assert!(
        (0.0..2048.0).contains(&timer_f),
        "computed timer out of range"
    );
    let timer = timer_f as u16;

    // split into low and high register values (11-bit timer)
    let low = (timer & 0xFF) as u8;
    let high = ((timer >> 8) & 0x07) as u8;

    // Build VGM: register NES APU chip, set timer low/high, enable channel via status,
    // and wait to observe KeyOn.
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::NesApu, Instance::Primary, NES_MASTER_CLOCK as u32);

    // Set timer low (pulse channel 0: register 0x02)
    builder.add_chip_write(
        Instance::Primary,
        chip::NesApuSpec {
            register: 0x02,
            value: low,
        },
    );

    // Set timer high (pulse channel 0: register 0x03)
    builder.add_chip_write(
        Instance::Primary,
        chip::NesApuSpec {
            register: 0x03,
            value: high,
        },
    );

    // Enable pulse channel 0 via status/enable register (0x15 bit 0)
    // This should transition the channel from Off->On and trigger KeyOn if tone is present.
    builder.add_chip_write(
        Instance::Primary,
        chip::NesApuSpec {
            register: 0x15,
            value: 0x01, // enable pulse channel 0
        },
    );

    // Wait enough samples so the callback stream can detect KeyOn
    builder.add_vgm_command(WaitSamples(22100));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    // Optionally write artifact via shim helper; no-op if OUTPUT_VGM_DIR is None.
    super::maybe_write_vgm("nes_apu_a4.vgm", &vgm_bytes);

    // Create VgmCallbackStream and enable NesApu state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<NesApuState>(Instance::Primary, NES_MASTER_CLOCK);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register callback for NesApu writes.
    callback_stream.on_write(move |_inst, _spec: chip::NesApuSpec, _sample, event_opt| {
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
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for NesApu pulse channel, but none was captured"
    );
    let freq = freq_opt.expect("ToneInfo.freq_hz should be Some");
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= NES_APU_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {} Hz, target {} Hz (diff {})",
        freq,
        target_hz,
        diff
    );
}
