// YMF278B (OPL4) event test.
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::fnumber::{Opl3Spec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Ymf278bState;

const FM_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

#[inline]
fn ymf278b_write(builder: &mut VgmBuilder, instance: Instance, port: u8, register: u8, value: u8) {
    builder.add_chip_write(
        instance,
        chip::Ymf278bSpec {
            port,
            register,
            value,
        },
    );
}

/// Configure a YMF278B FM channel to produce a sine wave using additive synthesis.
///
/// OPL4 FM part is compatible with OPL3 (YMF262).
/// Voice parameters for a basic sine wave:
/// - Algorithm: Additive (both operators output to mixer)
/// - Operator 1 (Modulator): Full volume, sine wave
/// - Operator 2 (Carrier): Muted
/// - Attack Rate: 15 (instant attack)
/// - Decay/Sustain/Release: Minimal
///
/// # Arguments
/// * `channel` - FM channel index (0-17, where 0-8 are on port 0, 9-17 are on port 1)
pub fn write_ymf278b_sine_voice(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 18, "YMF278B has 18 FM channels (0-17)");

    // Determine port and local channel
    let port = channel / 9;
    let local_ch = channel % 9;

    // OPL4 FM register layout (same as OPL3 per port):
    // Operator registers are organized as:
    // - Modulator (OP1): base + channel (0x00-0x08, 0x0B-0x12, etc.)
    // - Carrier (OP2): base + channel + 3 (0x03-0x0B, 0x0E-0x15, etc.)

    let op1 = local_ch; // Modulator
    let op2 = local_ch + 3; // Carrier

    // --- 0x20-0x35: AM/VIB/EGT/KSR/MULT ---
    // Bits: [AM-VIB-EGT-KSR-MULT[3:0]]
    // AM=0, VIB=0, EGT=1 (sustaining), KSR=0, MULT=1 (×1 frequency)
    ymf278b_write(builder, instance, port, 0x20 + op1, 0x21); // Modulator: EGT=1, MULT=1
    ymf278b_write(builder, instance, port, 0x20 + op2, 0x2F); // Carrier: EGT=1, MULT=15 (muted via TL)

    // --- 0x40-0x55: KSL/TL (Key Scale Level / Total Level) ---
    // Bits: [KSL[1:0]-TL[5:0]]
    // TL: 0 = loudest, 63 = silent
    ymf278b_write(builder, instance, port, 0x40 + op1, 0x00); // Modulator: full volume
    ymf278b_write(builder, instance, port, 0x40 + op2, 0x3F); // Carrier: silent

    // --- 0x60-0x75: AR/DR (Attack Rate / Decay Rate) ---
    // Bits: [AR[3:0]-DR[3:0]]
    ymf278b_write(builder, instance, port, 0x60 + op1, 0xFF); // Modulator: AR=15, DR=15 (fast)
    ymf278b_write(builder, instance, port, 0x60 + op2, 0xFF); // Carrier: AR=15, DR=15

    // --- 0x80-0x95: SL/RR (Sustain Level / Release Rate) ---
    // Bits: [SL[3:0]-RR[3:0]]
    // SL=0 (sustain at full level), RR=15 (fast release)
    ymf278b_write(builder, instance, port, 0x80 + op1, 0x0F); // Modulator: SL=0, RR=15
    ymf278b_write(builder, instance, port, 0x80 + op2, 0x0F); // Carrier: SL=0, RR=15

    // --- 0xE0-0xF5: WS (Waveform Select) ---
    // 0=sine, 1=half-sine, 2=abs-sine, 3=quarter-sine
    ymf278b_write(builder, instance, port, 0xE0 + op1, 0x00); // Modulator: sine wave
    ymf278b_write(builder, instance, port, 0xE0 + op2, 0x00); // Carrier: sine wave

    // --- 0xC0-0xC8: Feedback/Algorithm/Stereo ---
    // Bits: [L-R-CNT-FB[2:0]-unused[2:0]]
    // L=1, R=1 (stereo), CNT=1 (both operators to output = additive synthesis)
    // FB=0 (no feedback)
    ymf278b_write(builder, instance, port, 0xC0 + local_ch, 0xF1); // Stereo, Additive, no feedback
}

/// Write the 11-bit F-number and 3-bit Block to a YMF278B FM channel.
///
/// # Arguments
/// * `channel`   – 0-17 (0-8 on port 0, 9-17 on port 1)
/// * `fnum`      – 11-bit F-number (0-2047)
/// * `block`     – 3-bit block (0-7)
pub fn write_ymf278b_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 18, "YMF278B has 18 FM channels (0-17)");
    assert!(fnum <= 0x7FF, "F-number must be 11-bit (0-2047)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    let port = channel / 9;
    let local_ch = channel % 9;

    // F-number low 8 bits: register 0xA0-0xA8
    let fnum_low = (fnum & 0xFF) as u8;
    ymf278b_write(builder, instance, port, 0xA0 + local_ch, fnum_low);

    // Block + F-number high 2 bits: register 0xB0-0xB8
    // Bits: [--Key-Block[2:0]-FNum[9:8]]
    // We don't set Key here, that's done separately
    let block_fnum_high = ((block & 0x07) << 2) | ((fnum >> 8) as u8 & 0x03);
    ymf278b_write(builder, instance, port, 0xB0 + local_ch, block_fnum_high);
}

/// Emit a key-on command for a YMF278B FM channel.
///
/// Register 0xB0-0xB8 encoding:
/// ```
/// bit 5: Key On (1=on, 0=off)
/// bits 4-2: Block (octave)
/// bits 1-0: F-Number bits 9-8
/// ```
pub fn write_ymf278b_keyon(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 18, "YMF278B has 18 FM channels (0-17)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    let port = channel / 9;
    let local_ch = channel % 9;

    // Set bit 5 (Key On) along with block and fnum high bits (2 bits only)
    let block_fnum_high = 0x20 | ((block & 0x07) << 2) | ((fnum >> 8) as u8 & 0x03);
    ymf278b_write(builder, instance, port, 0xB0 + local_ch, block_fnum_high);
}

/// Emit a key-off command for a YMF278B FM channel.
///
/// Same as keyon but with bit 5 cleared.
pub fn write_ymf278b_keyoff(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    fnum: u16,
    block: u8,
) {
    assert!(channel < 18, "YMF278B has 18 FM channels (0-17)");
    assert!(block <= 7, "Block must be 3-bit (0-7)");

    let port = channel / 9;
    let local_ch = channel % 9;

    // Clear bit 5 (Key Off)
    let block_fnum_high = ((block & 0x07) << 2) | ((fnum >> 8) as u8 & 0x03);
    ymf278b_write(builder, instance, port, 0xB0 + local_ch, block_fnum_high);
}

#[test]
fn test_ymf278b_fm_keyon_and_tone_freq_matches_a4() {
    // ---------------------------------------------------------------
    // Target pitch and chip configuration
    // ---------------------------------------------------------------
    let target_hz = 440.0_f32;

    // YMF278B typical master clock (4x the base clock)
    // 14.31818 MHz × 4 = 57.27272 MHz, but typically specified as 33.8688 MHz
    let master_clock = 33_868_800.0_f32;

    // We'll use FM channel 0 (port 0, local channel 0)
    let channel: u8 = 0;

    // ---------------------------------------------------------------
    // F-number calculation using Opl3Spec (11-bit F-number)
    // YMF278B FM part uses the same formula as OPL3
    // ---------------------------------------------------------------
    let table = generate_12edo_fnum_table::<Opl3Spec>(master_clock).expect("generate fnum table");
    let tuned = find_and_tune_fnumber::<Opl3Spec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber");

    let fnum_u32 = tuned.f_num;
    let block_u8 = tuned.block;

    // YMF278B F-number is 11 bits (same as YMF262)
    let fnum = fnum_u32 as u16;
    let block = block_u8;

    // ---------------------------------------------------------------
    // Build VGM
    // ---------------------------------------------------------------
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ymf278b, Instance::Primary, master_clock as u32);

    // 0. Enable OPL4 mode (register 0x105 on port 1, bit 0 = 1)
    ymf278b_write(&mut builder, Instance::Primary, 1, 0x05, 0x01);

    // 1. Enable waveform selection (register 0x01, bit 5)
    ymf278b_write(&mut builder, Instance::Primary, 0, 0x01, 0x20);

    // 2. Initialize channel with a sine wave voice
    write_ymf278b_sine_voice(&mut builder, Instance::Primary, channel);

    // 3. Write the frequency (F-number and Block)
    write_ymf278b_frequency(&mut builder, Instance::Primary, channel, fnum, block);

    // 4. Key on
    write_ymf278b_keyon(&mut builder, Instance::Primary, channel, fnum, block);

    // 5. Hold note for ~0.5 s at 44 100 Hz sample rate
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // 6. Key off
    write_ymf278b_keyoff(&mut builder, Instance::Primary, channel, fnum, block);

    let doc = builder.finalize();

    // ---------------------------------------------------------------
    // Optionally write the VGM artifact for manual verification
    // ---------------------------------------------------------------
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ymf278b_fm_a4.vgm", &vgm_bytes);

    // ---------------------------------------------------------------
    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz
    // ---------------------------------------------------------------
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ymf278bState>(Instance::Primary, master_clock);

    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ymf278bSpec, _sample, event_opt| {
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
