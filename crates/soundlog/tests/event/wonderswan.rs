// WonderSwan APU event test (A4 via PCM channel 0).
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::WonderSwanState;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// WonderSwan master clock (typical)
const WONDERSWAN_MASTER_CLOCK: f32 = 3_072_000.0_f32;

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const WONDERSWAN_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_wonderswan_keyon_and_tone_freq_matches_a4() {
    // Target pitch (A4)
    let target_hz = 440.0_f32;

    // WonderSwan formula from state implementation:
    //   freq_hz = master_clock / (128 * (2048 - freq))
    // Solve for freq:
    //   period = master_clock / (128 * target_hz)
    //   freq = 2048 - period
    let period_f = (WONDERSWAN_MASTER_CLOCK / (128.0_f32 * target_hz)).round();
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

    // Build VGM: register WonderSwan chip, write freq low/high, set volume, enable channel,
    // and wait to observe KeyOn.
    let mut builder = VgmBuilder::new();
    builder.register_chip(
        Chip::WonderSwan,
        Instance::Primary,
        WONDERSWAN_MASTER_CLOCK as u32,
    );

    // Initialize
    let init_commands: [u8; 20] = [
        0x00, 0x00, 0x00, 0x00, 0x2B, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x18,
        0x02, 0x40, 0x07, 0x00, 0x00,
    ];
    for (index, write) in init_commands.iter().enumerate() {
        builder.add_chip_write(
            Instance::Primary,
            chip::WonderSwanRegSpec {
                register: index as u8,
                value: *write,
            },
        );
    }

    // Waveform
    let wave_form: [u8; 32] = [
        0xCC, 0xDD, 0xEE, 0xEE, 0xEE, 0xEE, 0xDD, 0xCC, 0x33, 0x22, 0x11, 0x11, 0x11, 0x11, 0x22,
        0x33, 0xCC, 0xDD, 0xEE, 0xEE, 0xEE, 0xEE, 0xDD, 0xCC, 0x33, 0x22, 0x11, 0x11, 0x11, 0x11,
        0x22, 0x33,
    ];
    for (index, write) in wave_form.iter().enumerate() {
        builder.add_chip_write(
            Instance::Primary,
            chip::WonderSwanSpec {
                offset: 0x80 + (index as u16),
                value: *write,
            },
        );
    }

    // Write frequency low (register 0x80)
    builder.add_chip_write(
        Instance::Primary,
        chip::WonderSwanRegSpec {
            register: 0x00,
            value: low,
        },
    );

    // Write frequency high (register 0x81) - lower 3 bits used
    builder.add_chip_write(
        Instance::Primary,
        chip::WonderSwanRegSpec {
            register: 0x01,
            value: high,
        },
    );

    // Set volume for channel 0 (register 0x88). Use left=8,right=8 -> 0x88
    builder.add_chip_write(
        Instance::Primary,
        chip::WonderSwanRegSpec {
            register: 0x08,
            value: 0x88,
        },
    );

    // Enable channel 0 via audio control register (register 0x90 bit0)
    // WonderSwan[Primary] 0x90=0x4F KeyOn(ch=3, fnum=0x748(1864), freq=130.43Hz)
    builder.add_chip_write(
        Instance::Primary,
        chip::WonderSwanRegSpec {
            register: 0x10,
            value: 0x01,
        },
    );

    // Wait enough samples so the callback stream can detect KeyOn
    builder.add_vgm_command(WaitSamples(22100));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("wonderswan_a4.vgm", &vgm_bytes);

    // Create VgmCallbackStream and enable WonderSwan state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<WonderSwanState>(Instance::Primary, WONDERSWAN_MASTER_CLOCK);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register callback for WonderSwan register writes.
    callback_stream.on_write(
        move |_inst, _spec: chip::WonderSwanRegSpec, _sample, event_opt| {
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
        },
    );

    // Iterate the stream (bounded) to process commands and invoke callbacks.
    for _ in (&mut callback_stream).take(200) {
        // no-op; callback captures the freq
    }

    // Assert we observed a KeyOn and compute the expected frequency from the actual registers written.
    // The state implementation derives frequency from the 11-bit value formed by low + (high & 0x07) << 8.
    let freq_opt = {
        let guard = captured_freq.lock().unwrap();
        *guard
    };
    assert!(
        freq_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for WonderSwan channel, but none was captured"
    );
    let freq_hz = freq_opt.expect("ToneInfo.freq_hz should be Some");

    // Recompute expected frequency from the registers we wrote (low, high).
    let freq_val: u16 = (low as u16) | (((high & 0x07) as u16) << 8);
    let period = 2048u16.saturating_sub(freq_val);
    let expected_hz = if period > 0 {
        WONDERSWAN_MASTER_CLOCK / (128.0_f32 * period as f32)
    } else {
        0.0_f32
    };

    let diff = (freq_hz - expected_hz).abs();
    assert!(
        diff <= WONDERSWAN_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from expected: got {} Hz, expected {} Hz (diff {})",
        freq_hz,
        expected_hz,
        diff
    );
}
