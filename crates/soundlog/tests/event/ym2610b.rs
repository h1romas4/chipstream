// YM2610b event test.(WIP: NO SOUND)
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::fnumber::{OpnaSpec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Ym2610bState;

const TARGET_A4_HZ: f32 = 440.0_f32;
const PSG_TOLERANCE_HZ: f32 = 2.0;
const FM_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

#[inline]
fn ym2610_write(builder: &mut VgmBuilder, instance: Instance, port: u8, register: u8, value: u8) {
    builder.add_chip_write(
        instance,
        chip::Ym2610Spec {
            port,
            register,
            value,
        },
    );
}

/// Configure a YM2610B FM channel to produce a pure sine wave using only Operator 1.
///
/// Voice parameters:
/// | Param      | OP1 | OP2 | OP3 | OP4 |
/// |------------|-----|-----|-----|-----|
/// | DT1 / MUL  | 0/1 | 0/1 | 0/1 | 0/1 |
/// | TL         |   0 | 127 | 127 | 127 |
/// | AR         |  31 |  31 |  31 |  31 |
/// | D1R        |   0 |   0 |   0 |   0 |
/// | D2R        |   0 |   0 |   0 |   0 |
/// | D1L / RR   | 0/15| 0/15| 0/15| 0/15|
/// | SSG-EG     |   0 |   0 |   0 |   0 |
///
/// Channel parameters: Algorithm = 7, Feedback = 0, L = R = 1.
///
/// # Arguments
/// * `port`        – 0 (channels 0-2) or 1 (channels 3-5).
/// * `ch_in_port`  – Channel within the port: 0, 1, or 2.
pub fn write_ym2610b_sine_voice(
    builder: &mut VgmBuilder,
    instance: Instance,
    port: u8,
    ch_in_port: u8,
) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    let ch = ch_in_port;

    // Operator register layout (all four operators, per channel):
    //   base + op_offset + ch_in_port
    //     op_offset : op1=0x00, op2=0x04, op3=0x08, op4=0x0C

    // --- DT1/MUL  [--DT1[2:0]-MUL[3:0]] ---------------------------------
    // DT1=0 (no detune), MUL=1 (×1 frequency).  Encoded value = 0x01.
    ym2610_write(builder, instance, port, 0x30 + ch, 0x01); // op1
    ym2610_write(builder, instance, port, 0x34 + ch, 0x01); // op2
    ym2610_write(builder, instance, port, 0x38 + ch, 0x01); // op3
    ym2610_write(builder, instance, port, 0x3C + ch, 0x01); // op4

    // --- TL  [---TL[6:0]] ------------------------------------------------
    // OP1 at full volume (TL=0); OP2/3/4 fully attenuated (TL=127 = 0x7F).
    ym2610_write(builder, instance, port, 0x40 + ch, 0x00); // op1 – loudest
    ym2610_write(builder, instance, port, 0x44 + ch, 0x7F); // op2 – muted
    ym2610_write(builder, instance, port, 0x48 + ch, 0x7F); // op3 – muted
    ym2610_write(builder, instance, port, 0x4C + ch, 0x7F); // op4 – muted

    // --- RS/AR  [RS[1:0]-0-AR[4:0]] --------------------------------------
    // RS=0, AR=31 (0x1F) → instant attack for all operators.
    ym2610_write(builder, instance, port, 0x50 + ch, 0x1F); // op1
    ym2610_write(builder, instance, port, 0x54 + ch, 0x1F); // op2
    ym2610_write(builder, instance, port, 0x58 + ch, 0x1F); // op3
    ym2610_write(builder, instance, port, 0x5C + ch, 0x1F); // op4

    // --- AM/D1R  [AM-0-D1R[4:0]] -----------------------------------------
    // AM=0, D1R=0 → no first-decay while key is held.
    ym2610_write(builder, instance, port, 0x60 + ch, 0x00); // op1
    ym2610_write(builder, instance, port, 0x64 + ch, 0x00); // op2
    ym2610_write(builder, instance, port, 0x68 + ch, 0x00); // op3
    ym2610_write(builder, instance, port, 0x6C + ch, 0x00); // op4

    // --- D2R  [---D2R[4:0]] -----------------------------------------------
    // D2R=0 → no secondary decay.
    ym2610_write(builder, instance, port, 0x70 + ch, 0x00); // op1
    ym2610_write(builder, instance, port, 0x74 + ch, 0x00); // op2
    ym2610_write(builder, instance, port, 0x78 + ch, 0x00); // op3
    ym2610_write(builder, instance, port, 0x7C + ch, 0x00); // op4

    // --- D1L/RR  [D1L[3:0]-RR[3:0]] --------------------------------------
    // D1L=0 (sustain at full level), RR=15 (fast release after key-off).
    // Encoded: (0 << 4) | 15 = 0x0F.
    ym2610_write(builder, instance, port, 0x80 + ch, 0x0F); // op1
    ym2610_write(builder, instance, port, 0x84 + ch, 0x0F); // op2
    ym2610_write(builder, instance, port, 0x88 + ch, 0x0F); // op3
    ym2610_write(builder, instance, port, 0x8C + ch, 0x0F); // op4

    // --- SSG-EG  [---SSG-EG[3:0]] ----------------------------------------
    // Proprietary; always 0.
    ym2610_write(builder, instance, port, 0x90 + ch, 0x00); // op1
    ym2610_write(builder, instance, port, 0x94 + ch, 0x00); // op2
    ym2610_write(builder, instance, port, 0x98 + ch, 0x00); // op3
    ym2610_write(builder, instance, port, 0x9C + ch, 0x00); // op4

    // ------------------------------------------------------------------
    // Per-channel registers (B0H+, B4H+)
    // ------------------------------------------------------------------

    // B0H+  [--FB[2:0]-ALG[2:0]]
    // Feedback=0 (no OP1 self-feedback → pure sine), Algorithm=7 (all ops are
    // output slots). Encoded: 0b000_000_111 = 0x07.
    ym2610_write(builder, instance, port, 0xB0 + ch, 0x07);

    // B4H+  [L-R-AMS[1:0]-0-FMS[2:0]]
    // L=1, R=1 (both speakers), AMS=0, FMS=0.  Encoded: 0b11_00_0_000 = 0xC0.
    ym2610_write(builder, instance, port, 0xB4 + ch, 0xC0);
}

/// Write the 11-bit F-number and Block to a YM2610B FM channel.
///
/// Per the YM2610B manual, the high byte (A4H+) **must** be written before
/// the low byte (A0H+).
///
/// # Arguments
/// * `port`        – 0 or 1 (selects the register bank).
/// * `ch_in_port`  – 0, 1, or 2.
/// * `a4_value`    – `Block[2:0]` in bits 5-3 and `F-num[10:8]` in bits 2-0.
/// * `fnum_low`    – `F-num[7:0]` (low byte).
pub fn write_ym2610b_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    port: u8,
    ch_in_port: u8,
    a4_value: u8,
    fnum_low: u8,
) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    let ch = ch_in_port;
    // High byte first, then low byte (as required by the manual).
    ym2610_write(builder, instance, port, 0xA4 + ch, a4_value);
    ym2610_write(builder, instance, port, 0xA0 + ch, fnum_low);
}

/// Emit a key-on command that activates all four operators on one channel.
///
/// Register 0x28 encoding:
/// ```
/// bits 7-4: operator slots to key-on (0xF = all four)
/// bits 2-0: channel selector
///   0,1,2 → port-0 channels 0,1,2
///   4,5,6 → port-1 channels 0,1,2  (bit 2 selects port 1)
/// ```
///
/// The key-on register is always written to **port 0** regardless of which
/// channel is being addressed.
pub fn write_ym2610b_keyon(builder: &mut VgmBuilder, instance: Instance, port: u8, ch_in_port: u8) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    // Bit 2 of the channel selector encodes the port.
    let ch_bits = if port == 0 {
        ch_in_port
    } else {
        0x04 | ch_in_port
    };
    // All four operator slots on (bits 7-4 = 0xF).
    ym2610_write(builder, instance, 0, 0x28, 0xF0 | ch_bits);
}

/// Emit a key-off command that silences all four operators on one channel.
///
/// Same encoding as [`write_ym2610b_keyon`] but with operator mask = 0.
pub fn write_ym2610b_keyoff(
    builder: &mut VgmBuilder,
    instance: Instance,
    port: u8,
    ch_in_port: u8,
) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    let ch_bits = if port == 0 {
        ch_in_port
    } else {
        0x04 | ch_in_port
    };
    // Operator mask = 0 → key off.
    ym2610_write(builder, instance, 0, 0x28, ch_bits);
}

#[test]
fn test_ym2610b_fm_keyon_and_tone_freq_matches_a4() {
    // ---------------------------------------------------------------
    // Target pitch and chip configuration
    // ---------------------------------------------------------------
    let target_hz = 440.0_f32;

    // YM2610B typical master clock (Neo Geo: 8_000_000)
    let master_clock = 8_000_000.0_f32;

    // We'll use port 0, channel-in-port 0 → YM2610B channel 0 (the first FM channel).
    let port: u8 = 0;
    let ch_in_port: u8 = 0;

    // ---------------------------------------------------------------
    // F-number calculation using OpnaSpec (prescaler = 1.0 for YM2610B)
    // ---------------------------------------------------------------
    let table = generate_12edo_fnum_table::<OpnaSpec>(master_clock).expect("generate fnum table");
    let tuned = find_and_tune_fnumber::<OpnaSpec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber");

    let fnum_u32 = tuned.f_num;
    let block_u8 = tuned.block;

    // YM2610B F-number is 11 bits: low 8 → A0H, high 3 → A4H bits 2-0.
    // Block occupies A4H bits 5-3.
    let fnum_low = (fnum_u32 & 0xFF) as u8;
    let fnum_high = ((fnum_u32 >> 8) & 0x07) as u8;
    let a4_value = ((block_u8 & 0x07) << 3) | (fnum_high & 0x07);

    // ---------------------------------------------------------------
    // Build VGM
    // ---------------------------------------------------------------
    let mut builder = VgmBuilder::new();
    // Bit 31 is used to set whether it is an YM2610 or an YM2610B chip.
    // If bit 31 is set it is an YM2610B, if bit 31 is clear it is an YM2610.
    builder.register_chip(Chip::Ym2610b, Instance::Secondary, master_clock as u32);

    // 0. Global chip initialization
    // - Disable LFO for stable pure FM tone (0x22 = 0x00)
    // - Ensure mixer / channel enable / pan are set to sensible defaults
    ym2610_write(&mut builder, Instance::Primary, 0, 0x22, 0x00); // LFO off
    ym2610_write(&mut builder, Instance::Primary, 0, 0x27, 0x00);
    ym2610_write(&mut builder, Instance::Primary, 1, 0x01, 0x3F);

    // 1. Initialize channel with a pure-sine voice
    write_ym2610b_sine_voice(&mut builder, Instance::Primary, port, ch_in_port);

    // 2. Write the frequency (A4H high byte first, then A0H low byte)
    write_ym2610b_frequency(
        &mut builder,
        Instance::Primary,
        port,
        ch_in_port,
        a4_value,
        fnum_low,
    );

    // 3. Key on – all four operators
    write_ym2610b_keyon(&mut builder, Instance::Primary, port, ch_in_port);

    // 4. Hold note for ~0.5 s at 44 100 Hz sample rate
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // 5. Key off
    write_ym2610b_keyoff(&mut builder, Instance::Primary, port, ch_in_port);

    let doc = builder.finalize();

    // ---------------------------------------------------------------
    // Optionally write the VGM artifact for manual verification
    // ---------------------------------------------------------------
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2610b_fm_a4.vgm", &vgm_bytes);

    // ---------------------------------------------------------------
    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz
    // ---------------------------------------------------------------
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2610bState>(Instance::Primary, master_clock);

    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2610Spec, _sample, event_opt| {
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

#[test]
fn test_ym2610b_psg_channel_a_matches_a4() {
    // YM2610B typical master clock (Neo Geo: 8_000_000)
    let master_clock = 8_000_000.0_f32;

    // PSG period formula: the YM2610B SSG section uses master_clock / 4 as its effective
    // clock before the AY-compatible tone counters (YM2203 divides by 2; YM2608/YM2610B by 4).
    // period = round(master_clock / 4 / (16 * freq)) = round(master_clock / (64 * freq))
    let ideal_period = (master_clock / (4.0_f32 * 16.0_f32 * TARGET_A4_HZ)).round() as u16;
    assert!(
        ideal_period > 0 && ideal_period <= 0x0FFF,
        "period out of range"
    );

    let fine = (ideal_period & 0xFF) as u8;
    let coarse = ((ideal_period >> 8) & 0x0F) as u8;

    // Build VGM: YM2610B uses port-based writes similar to YM2608.
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2610b, Instance::Primary, master_clock as u32);

    // PSG registers on port 0
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x00,
            value: fine,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x01,
            value: coarse & 0x0F,
        },
    );

    // Channel A volume: bits 3-0 = level (0=silent, 0xF=max), bit4=0 (fixed-level mode).
    // Must be written before enabling the mixer, otherwise the PSG channel is silent.
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x08,
            value: 0x0F, // max volume, fixed-level mode
        },
    );

    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x07,
            value: 0b1111_1110u8, // enable channel A only
        },
    );

    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // Key off: disable channel A and silence volume
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x07,
            value: 0b1111_1111u8, // disable channel A
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x08,
            value: 0x00, // silence channel A volume
        },
    );

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2610b_psg_a4.vgm", &vgm_bytes);

    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2610bState>(Instance::Primary, master_clock);

    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2610Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { tone, .. } = ev {
                    let mut g = captured_cb.lock().unwrap();
                    *g = tone.freq_hz;
                }
            }
        }
    });

    for _ in (&mut callback_stream).take(200) {}

    let got = captured_freq.lock().unwrap();
    assert!(
        got.is_some(),
        "Expected KeyOn StateEvent for YM2610B PSG but none captured"
    );
    // Observed frequency from the state tracker
    let freq = got.unwrap();

    // Expected frequency: master_clock / 4 / (16 * ideal_period) ≈ TARGET_A4_HZ
    let expected_freq = master_clock / (4.0_f32 * 16.0_f32 * ideal_period as f32);
    let diff = (freq - expected_freq).abs();
    assert!(
        diff <= PSG_TOLERANCE_HZ,
        "YM2610B PSG freq differs: got {} Hz, expected {} Hz (target {} Hz, diff {})",
        freq,
        expected_freq,
        TARGET_A4_HZ,
        diff
    );
}
