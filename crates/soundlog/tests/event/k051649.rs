// K051649 (Konami SCC / SCC1) event test.
//
// This mirrors the style of other event tests in this directory:
// - build a small VGM document with SCC1 register writes (Scc1Spec)
// - register the K051649 chip in the VGM builder
// - write frequency low/high and enable (key-on) the channel
// - run the VGM through a VgmCallbackStream with K051649State tracking
// - assert the KeyOn StateEvent contains ToneInfo.freq_hz ≈ A4 (440 Hz)

use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::state::K051649State;

const TARGET_A4_HZ: f32 = 440.0_f32;
const SCC_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

#[test]
fn test_k051649_keyon_and_tone_freq_matches_a4() {
    // Choose a master clock that is often used in tests and for which the
    // SCC period/frequency formula is known to produce values near A4.
    let master_clock = 3_579_545.0_f32;

    // Compute an ideal 12-bit SCC period value for A4 using the chip formula:
    // freq = master_clock / (32 * (period + 1))
    // Rearranged: period ≈ master_clock / (32 * freq)
    // We'll round the result to a nearest integer (tests allow a small tolerance).
    let period = (master_clock / (32.0_f32 * TARGET_A4_HZ)).round() as u16;
    assert!(period <= 0x0FFF, "computed period out of 12-bit range");

    let fnum_low = (period & 0xFF) as u8;
    let fnum_high = ((period >> 8) & 0x0F) as u8;

    // Build the VGM document containing SCC1 writes:
    // - frequency low (0x80)
    // - frequency high (0x81)
    // - enable channel 0 via 0x8F (bit 0)
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::K051649, Instance::Primary, master_clock as u32);

    let waveform: [u8; 32] = [
        0x00, 0xF8, 0xF0, 0xE8, 0xE0, 0xD8, 0xD0, 0xC8, 0xC0, 0xB8, 0xB0, 0xA8, 0xA0, 0x98, 0x90,
        0x88, 0x80, 0x78, 0x70, 0x68, 0x60, 0x58, 0x50, 0x48, 0x40, 0x38, 0x30, 0x28, 0x20, 0x18,
        0x10, 0x08,
    ];

    // Write the 32-byte waveform into port 0 registers 0x00..=0x1F (wave RAM for channel 0)
    for (i, &v) in waveform.iter().enumerate() {
        let reg = 0x00u8.wrapping_add(i as u8);
        builder.add_chip_write(
            Instance::Primary,
            chip::Scc1Spec {
                port: 0,
                register: reg,
                value: v,
            },
        );
    }

    // Frequency low / high (SCC1 port 0)
    builder.add_chip_write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 1,
            register: 0,
            value: fnum_low,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 1,
            register: 1,
            value: fnum_high,
        },
    );

    // Set volume channel 0
    builder.add_chip_write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 2,
            register: 0,
            value: 0x0F,
        },
    );

    // Enable (key-on) channel 0
    builder.add_chip_write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 3,
            register: 0,
            value: 0x01,
        },
    );

    // Hold for a short duration then key off
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));
    builder.add_chip_write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 3,
            register: 0,
            value: 0,
        },
    );

    let doc = builder.finalize();

    // Optionally write VGM artifact for manual verification (no-op unless env set).
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("k051649_a4.vgm", &vgm_bytes);

    // Create a callback stream and enable K051649 state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<K051649State>(Instance::Primary, master_clock);

    // Capture the first KeyOn ToneInfo.freq_hz observed.
    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Scc1Spec, _sample, event_opt| {
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

    // Process the stream (bounded to avoid infinite iteration).
    for _ in (&mut callback_stream).take(200) {}

    let guard = captured_freq_hz.lock().unwrap();
    let got = *guard;
    assert!(
        got.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz, but none was captured"
    );
    let freq = got.unwrap();
    let diff = (freq - TARGET_A4_HZ).abs();
    assert!(
        diff <= SCC_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {freq} Hz, target {TARGET_A4_HZ} Hz (diff {diff})"
    );
}
