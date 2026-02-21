// Game Boy DMG event test (A4 via Pulse channel).
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::GbDmgState;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// Game Boy master clock (DMG)
const GB_MASTER_CLOCK: f32 = 4_194_304.0_f32;

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const GB_DMG_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_gbdmg_pulse_keyon_and_tone_freq_matches_a4() {
    // Target pitch (A4)
    let target_hz = 440.0_f32;

    // For pulse channels (channel 0/1) frequency formula:
    //   hz = hz_game_boy(timer)
    // where hz_game_boy(timer) = 131_072 / (2048 - timer)
    //
    // Solve for timer to produce target_hz on pulse channel:
    //   timer = round(2048 - 131072 / target_hz)
    let timer_f = (2048.0f32 - 131_072.0f32 / target_hz).round();
    assert!(
        (0.0..2048.0).contains(&timer_f),
        "computed timer out of range"
    );
    let timer = timer_f as u16;

    // split into low and high register values (11-bit timer)
    let low = (timer & 0xFF) as u8;
    let high = ((timer >> 8) & 0x07) as u8;

    // Build VGM: register GB DMG chip, initialize master/volumes, set channel registers,
    // write frequency low/high with trigger bit, and wait to observe KeyOn.
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::GbDmg, Instance::Primary, GB_MASTER_CLOCK as u32);

    // Initialization: enable master sound (NR52), set master volumes (NR50) and panning (NR51)
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x16, // compact -> NR52
            value: 0x80,    // master enable (bit7)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x14, // compact -> NR50
            value: 0x77,    // left/right master volumes
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x15, // compact -> NR51
            value: 0xFF,    // enable all channels to both speakers
        },
    );

    // Channel 0 init: duty/length (NR11), envelope (NR12)
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x01, // NR11
            value: 0x00,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x02, // NR12
            value: 0xF9,    // envelope/volume (non-zero so audible)
        },
    );

    // Write frequency low (NR13)
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x03, // NR13
            value: low,
        },
    );

    // Write frequency high (NR14) with trigger bit set to initiate KeyOn
    builder.add_chip_write(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x04,     // NR14
            value: high | 0x80, // set trigger bit (bit7) to key on
        },
    );

    // Wait enough samples so the callback stream can detect KeyOn
    builder.add_vgm_command(WaitSamples(22100));

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    // Optionally write artifact via shim helper; no-op if OUTPUT_VGM_DIR is None.
    super::maybe_write_vgm("gb_dmg_a4.vgm", &vgm_bytes);

    // Create VgmCallbackStream and enable GbDmg state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<GbDmgState>(Instance::Primary, GB_MASTER_CLOCK);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    // Register callback for GbDmg writes.
    callback_stream.on_write(move |_inst, _spec: chip::GbDmgSpec, _sample, event_opt| {
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
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for GbDmg pulse channel, but none was captured"
    );
    let freq = freq_opt.expect("ToneInfo.freq_hz should be Some");
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= GB_DMG_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {} Hz, target {} Hz (diff {})",
        freq,
        target_hz,
        diff
    );
}
