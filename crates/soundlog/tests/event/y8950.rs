// Y8950 (MSX-Audio) event test (OPL-compatible).
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::fnumber::{Opl2Spec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Y8950State;

const FM_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

#[inline]
fn y8950_write(builder: &mut VgmBuilder, instance: Instance, register: u8, value: u8) {
    builder.add_chip_write(instance, chip::Y8950Spec { register, value });
}

/// Configure a Y8950 FM channel to produce a sine wave using additive synthesis.
///
/// OPL-style voice parameters for a basic sine wave:
/// - Algorithm: Additive (both operators output to mixer)
/// - Operator 1 (Modulator): Full volume, sine wave
/// - Operator 2 (Carrier): Muted
/// - Attack Rate: 15 (instant attack)
/// - Decay/Sustain/Release: Minimal
///
/// # Arguments
/// * `channel` - FM channel index (0-8)
pub fn write_y8950_sine_voice(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 9, "Y8950 has 9 FM channels (0-8)");

    // Operator mapping (OPL style):
    let op1 = channel; // Modulator
    let op2 = channel + 3; // Carrier

    // 0x20-0x35: AM/VIB/EGT/KSR/MULT
    y8950_write(builder, instance, 0x20 + op1, 0x21); // Modulator: EGT=1, MULT=1
    y8950_write(builder, instance, 0x20 + op2, 0x2F); // Carrier: EGT=1, MULT=15 (muted via TL)

    // 0x40-0x55: KSL/TL
    y8950_write(builder, instance, 0x40 + op1, 0x00); // Modulator: full volume
    y8950_write(builder, instance, 0x40 + op2, 0x3F); // Carrier: silent

    // 0x60-0x75: AR/DR
    y8950_write(builder, instance, 0x60 + op1, 0xFF); // Modulator: AR=15, DR=15
    y8950_write(builder, instance, 0x60 + op2, 0xFF); // Carrier: AR=15, DR=15

    // 0x80-0x95: SL/RR
    y8950_write(builder, instance, 0x80 + op1, 0x0F); // Modulator: SL=0, RR=15
    y8950_write(builder, instance, 0x80 + op2, 0x0F); // Carrier: SL=0, RR=15

    // 0xC0-0xC8: Feedback/Algorithm
    // CNT=1 => both operators to output (additive), FB=0
    y8950_write(builder, instance, 0xC0 + channel, 0x01);
}

/// Write the 10-bit F-number and 3-bit Block to a Y8950 FM channel.
///
/// # Arguments
/// * `channel`   – 0-8
/// * `fnum`      – 10-bit F-number (0-1023)
/// * `block`     – 3-bit block (0-7)
pub fn write_y8950_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 9, "Y8950 has 9 FM channels (0-8)");
    assert!(fnum <= 0x3FF, "F-number must be 10-bit (0-1023)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    // F-number low 8 bits: 0xA0-0xA8
    let fnum_low = (fnum & 0xFF) as u8;
    y8950_write(builder, instance, 0xA0 + channel, fnum_low);

    // Block + F-number high 2 bits: 0xB0-0xB8
    let block_fnum_high = ((block & 0x07) << 2) | ((fnum >> 8) as u8 & 0x03);
    y8950_write(builder, instance, 0xB0 + channel, block_fnum_high);
}

/// Emit a key-on command for a Y8950 FM channel.
///
/// Register 0xB0 encoding:
/// bit 5: Key On (1=on)
/// bits 4-2: Block
/// bits 1-0: F-number [9:8]
pub fn write_y8950_keyon(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 9, "Y8950 has 9 FM channels (0-8)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    let block_fnum_high = 0x20 | ((block & 0x07) << 2) | ((fnum >> 8) as u8 & 0x03);
    y8950_write(builder, instance, 0xB0 + channel, block_fnum_high);
}

/// Emit a key-off command for a Y8950 FM channel.
pub fn write_y8950_keyoff(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 9, "Y8950 has 9 FM channels (0-8)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    let block_fnum_high = ((block & 0x07) << 2) | ((fnum >> 8) as u8 & 0x03);
    y8950_write(builder, instance, 0xB0 + channel, block_fnum_high);
}

#[test]
fn test_y8950_fm_keyon_and_tone_freq_matches_a4() {
    // Target pitch and chip configuration
    let target_hz = 440.0_f32;

    // Y8950 typical master clock (NTSC)
    let master_clock = 3_579_545.0_f32;

    // Use FM channel 0
    let channel: u8 = 0;

    // F-number calculation using Opl2Spec (10-bit F-number)
    let table = generate_12edo_fnum_table::<Opl2Spec>(master_clock).expect("generate fnum table");
    let tuned = find_and_tune_fnumber::<Opl2Spec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber");

    let fnum_u32 = tuned.f_num;
    let block_u8 = tuned.block;

    // Y8950 F-number is 10 bits
    let fnum = fnum_u32 as u16;
    let block = block_u8;

    // Build VGM
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Y8950, Instance::Primary, master_clock as u32);

    // Initialize voice and write frequency/key
    write_y8950_sine_voice(&mut builder, Instance::Primary, channel);
    write_y8950_frequency(&mut builder, Instance::Primary, channel, fnum, block);
    write_y8950_keyon(&mut builder, Instance::Primary, channel, fnum, block);

    // Hold note for ~0.5 s
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // Key off
    write_y8950_keyoff(&mut builder, Instance::Primary, channel, fnum, block);

    let doc = builder.finalize();

    // Optionally write VGM artifact
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("y8950_fm_a4.vgm", &vgm_bytes);

    // State-tracking assertion
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Y8950State>(Instance::Primary, master_clock);

    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Y8950Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { tone, .. } = ev {
                    let mut guard = captured_freq_hz_cb.lock().unwrap();
                    if guard.is_none() {
                        *guard = tone.freq_hz;
                    }
                }
            }
        }
    });

    // Iterate bounded to process commands and trigger callbacks.
    for _ in (&mut callback_stream).take(200) {}

    let got_guard = captured_freq_hz.lock().unwrap();
    let got_opt = *got_guard;
    assert!(
        got_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz, but none was captured"
    );
    let freq = got_opt.unwrap();
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= FM_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {freq} Hz, target {target_hz} Hz (diff {diff})"
    );
}
