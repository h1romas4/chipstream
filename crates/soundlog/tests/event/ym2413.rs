// YM2413 (OPLL) event test.
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::fnumber::{OpllSpec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Ym2413State;

const FM_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

/// Helper: emit a single YM2413 register write
#[inline]
fn ym2413_write(builder: &mut VgmBuilder, instance: Instance, register: u8, value: u8) {
    builder.add_chip_write(instance, chip::Ym2413Spec { register, value });
}

/// Configure a YM2413 FM channel to use a preset instrument.
///
/// YM2413 has 15 preset instruments (0-14) and 1 custom instrument (15).
/// We'll use preset instrument 0 (Piano) which produces a reasonable tone.
///
/// # Arguments
/// * `channel` - FM channel index (0-8)
pub fn write_ym2413_preset_voice(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 9, "YM2413 has 9 FM channels (0-8)");

    // Register 0x30-0x38: Instrument and volume
    // Bits: [Inst[3:0]-Vol[3:0]]
    // Inst=0 (Piano preset), Vol=0 (loudest)
    ym2413_write(builder, instance, 0x30 + channel, 0x00);
}

/// Write the 9-bit F-number and 3-bit Block to a YM2413 FM channel.
///
/// # Arguments
/// * `channel`   – 0-8
/// * `fnum`      – 9-bit F-number (0-511)
/// * `block`     – 3-bit block (0-7)
pub fn write_ym2413_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 9, "YM2413 has 9 FM channels (0-8)");
    assert!(fnum <= 0x1FF, "F-number must be 9-bit (0-511)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    // F-number low 8 bits: register 0x10-0x18
    let fnum_low = (fnum & 0xFF) as u8;
    ym2413_write(builder, instance, 0x10 + channel, fnum_low);

    // Block + F-number high 1 bit: register 0x20-0x28
    // Bits: [--Key-Block[2:0]-FNum[8]]
    // We don't set Key here, that's done separately
    let block_fnum_high = ((block & 0x07) << 1) | ((fnum >> 8) as u8 & 0x01);
    ym2413_write(builder, instance, 0x20 + channel, block_fnum_high);
}

/// Emit a key-on command for a YM2413 FM channel.
///
/// Register 0x20-0x28 encoding:
/// ```
/// bit 4: Key On (1=on, 0=off)
/// bits 3-1: Block (octave)
/// bit 0: F-Number bit 8
/// ```
pub fn write_ym2413_keyon(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 9, "YM2413 has 9 FM channels (0-8)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    // Set bit 4 (Key On) along with block and fnum high bit
    let block_fnum_high = 0x10 | ((block & 0x07) << 1) | ((fnum >> 8) as u8 & 0x01);
    ym2413_write(builder, instance, 0x20 + channel, block_fnum_high);
}

/// Emit a key-off command for a YM2413 FM channel.
///
/// Same as keyon but with bit 4 cleared.
pub fn write_ym2413_keyoff(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 9, "YM2413 has 9 FM channels (0-8)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    // Clear bit 4 (Key Off)
    let block_fnum_high = ((block & 0x07) << 1) | ((fnum >> 8) as u8 & 0x01);
    ym2413_write(builder, instance, 0x20 + channel, block_fnum_high);
}

#[test]
fn test_ym2413_fm_keyon_and_tone_freq_matches_a4() {
    // Target pitch and chip configuration
    let target_hz = 440.0_f32;

    // YM2413 typical master clock (NTSC)
    let master_clock = 3_579_545.0_f32;

    // We'll use FM channel 0
    let channel: u8 = 0;

    // F-number calculation using OpllSpec (9-bit F-number)
    let table = generate_12edo_fnum_table::<OpllSpec>(master_clock).expect("generate fnum table");
    let tuned = find_and_tune_fnumber::<OpllSpec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber");

    let fnum_u32 = tuned.f_num;
    let block_u8 = tuned.block;

    // YM2413 F-number is 9 bits
    let fnum = fnum_u32 as u16;
    let block = block_u8;

    // Build VGM
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2413, Instance::Primary, master_clock as u32);

    // Global instrument / user instrument registers
    ym2413_write(&mut builder, Instance::Primary, 0x00, 0x31);
    ym2413_write(&mut builder, Instance::Primary, 0x01, 0x31);
    ym2413_write(&mut builder, Instance::Primary, 0x02, 0x17);
    ym2413_write(&mut builder, Instance::Primary, 0x03, 0x06);
    ym2413_write(&mut builder, Instance::Primary, 0x04, 0xD3);
    ym2413_write(&mut builder, Instance::Primary, 0x05, 0xE1);
    ym2413_write(&mut builder, Instance::Primary, 0x06, 0xBB);
    ym2413_write(&mut builder, Instance::Primary, 0x07, 0xEB);

    // Initialize channel with a preset instrument (channel-specific instrument/volume)
    // Keep using the helper which writes 0x30+channel
    write_ym2413_preset_voice(&mut builder, Instance::Primary, channel);

    // Write the frequency (F-number and Block)
    write_ym2413_frequency(&mut builder, Instance::Primary, channel, fnum, block);

    // Ensure channel volume (0 = loudest). This is safe even if the preset
    // wrote the same register earlier; it guarantees an audible level.
    ym2413_write(&mut builder, Instance::Primary, 0x30 + channel, 0x00);

    // Key on
    write_ym2413_keyon(&mut builder, Instance::Primary, channel, fnum, block);

    // Hold note for ~0.5 s at 44 100 Hz sample rate
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // Key off
    write_ym2413_keyoff(&mut builder, Instance::Primary, channel, fnum, block);

    let doc = builder.finalize();

    // Optionally write the VGM artifact for manual verification
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2413_fm_a4.vgm", &vgm_bytes);

    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2413State>(Instance::Primary, master_clock);

    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2413Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { channel: _ch, tone } = ev {
                    let mut guard = captured_freq_hz_cb.lock().unwrap();
                    if guard.is_none() {
                        // Capture only the first KeyOn so the test is deterministic.
                        *guard = tone.freq_hz;
                    }
                }
            }
        }
    });

    // Iterate bounded to process all commands and trigger callbacks.
    for _res in (&mut callback_stream).take(200) {}

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
