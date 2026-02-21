// YM2612 (OPN2) event test.
//
// # Voice design (pure sine from OP1 only)
//
//   Algorithm 7  – all four operators are output "slots" (summed to output).
//   Feedback  0  – no OP1 self-feedback → OP1 generates a pure sine wave.
//   OP1 TL = 0   – full volume.
//   OP2/3/4 TL = 127 – fully attenuated → they contribute nothing audible.
//   AR = 31 for all ops – instant attack.
//   D1R = D2R = 0 – no decay while key is held.
//   D1L = 0, RR = 15 – sustain at full level, fast release after key-off.
//   Stereo: both L and R enabled (B4H = 0xC0).
//
// # Common helpers
//
//   write_ym2612_global_init()  – one-time chip reset (LFO, Ch3 mode, DAC, key-off all).
//   write_ym2612_sine_voice()   – operator + channel registers for a pure sine voice.
//   write_ym2612_frequency()    – write A4H (MSB first) then A0H.
//   write_ym2612_keyon()        – 0x28 with all-operator-on mask.
//   write_ym2612_keyoff()       – 0x28 with zero operator mask.
//
//   All helpers are parameterised on (port: u8, ch_in_port: u8) so they work for
//   any of the six YM2612 FM channels:
//     Port 0: ch_in_port 0/1/2  → channels 1/2/3
//     Port 1: ch_in_port 0/1/2  → channels 4/5/6

use soundlog::chip::event::StateEvent;
use soundlog::chip::fnumber::{OpnaSpec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Ym2612State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const YM2612_TOLERANCE_HZ: f32 = 2.0;

#[inline]
fn ym2612_write(builder: &mut VgmBuilder, instance: Instance, port: u8, register: u8, value: u8) {
    builder.add_chip_write(
        instance,
        chip::Ym2612Spec {
            port,
            register,
            value,
        },
    );
}

/// Emit the one-time global initialisation sequence for a YM2612.
///
/// Writes:
/// - `0x22 = 0x00`  LFO disabled
/// - `0x27 = 0x00`  Channel 3 normal mode (not special)
/// - `0x28 = 0x00..0x06`  Key-off all six channels
/// - `0x2B = 0x00`  DAC disabled
///
/// Call once before any per-channel configuration.
pub fn write_ym2612_global_init(builder: &mut VgmBuilder, instance: Instance) {
    // All global registers live on port 0.
    ym2612_write(builder, instance, 0, 0x22, 0x00); // LFO off
    ym2612_write(builder, instance, 0, 0x27, 0x00); // Ch3 mode: normal
    // Key-off all channels:
    //   port-0 channels → 0x28 value 0x00, 0x01, 0x02
    //   port-1 channels → 0x28 value 0x04, 0x05, 0x06
    for ch_bits in [0x00u8, 0x01, 0x02, 0x04, 0x05, 0x06] {
        ym2612_write(builder, instance, 0, 0x28, ch_bits);
    }
    ym2612_write(builder, instance, 0, 0x2B, 0x00); // DAC off
}

/// Configure one YM2612 FM channel to produce a **pure sine wave** using only
/// Operator 1.
///
/// Voice parameters
/// ----------------
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
/// * `port`        – 0 (channels 1-3) or 1 (channels 4-6).
/// * `ch_in_port`  – Channel within the port: 0, 1, or 2.
pub fn write_ym2612_sine_voice(
    builder: &mut VgmBuilder,
    instance: Instance,
    port: u8,
    ch_in_port: u8,
) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    let ch = ch_in_port; // register offset within the port

    // DT1/MUL  [--DT1[2:0]-MUL[3:0]]
    // DT1=0 (no detune), MUL=1 (×1 frequency).  Encoded value = 0x01.
    ym2612_write(builder, instance, port, 0x30 + ch, 0x01); // op1
    ym2612_write(builder, instance, port, 0x34 + ch, 0x01); // op2
    ym2612_write(builder, instance, port, 0x38 + ch, 0x01); // op3
    ym2612_write(builder, instance, port, 0x3C + ch, 0x01); // op4

    // TL  [---TL[6:0]]
    // OP1 at full volume (TL=0); OP2/3/4 fully attenuated (TL=127 = 0x7F).
    ym2612_write(builder, instance, port, 0x40 + ch, 0x00); // op1 – loudest
    ym2612_write(builder, instance, port, 0x44 + ch, 0x7F); // op2 – muted
    ym2612_write(builder, instance, port, 0x48 + ch, 0x7F); // op3 – muted
    ym2612_write(builder, instance, port, 0x4C + ch, 0x7F); // op4 – muted

    // RS/AR  [RS[1:0]-0-AR[4:0]]
    // RS=0, AR=31 (0x1F) → instant attack for all operators.
    ym2612_write(builder, instance, port, 0x50 + ch, 0x1F); // op1
    ym2612_write(builder, instance, port, 0x54 + ch, 0x1F); // op2
    ym2612_write(builder, instance, port, 0x58 + ch, 0x1F); // op3
    ym2612_write(builder, instance, port, 0x5C + ch, 0x1F); // op4

    // AM/D1R  [AM-0-D1R[4:0]]
    // AM=0, D1R=0 → no first-decay while key is held.
    ym2612_write(builder, instance, port, 0x60 + ch, 0x00); // op1
    ym2612_write(builder, instance, port, 0x64 + ch, 0x00); // op2
    ym2612_write(builder, instance, port, 0x68 + ch, 0x00); // op3
    ym2612_write(builder, instance, port, 0x6C + ch, 0x00); // op4

    // D2R  [---D2R[4:0]]
    // D2R=0 → no secondary decay.
    ym2612_write(builder, instance, port, 0x70 + ch, 0x00); // op1
    ym2612_write(builder, instance, port, 0x74 + ch, 0x00); // op2
    ym2612_write(builder, instance, port, 0x78 + ch, 0x00); // op3
    ym2612_write(builder, instance, port, 0x7C + ch, 0x00); // op4

    // D1L/RR  [D1L[3:0]-RR[3:0]]
    // D1L=0 (sustain at full level), RR=15 (fast release after key-off).
    // Encoded: (0 << 4) | 15 = 0x0F.
    ym2612_write(builder, instance, port, 0x80 + ch, 0x0F); // op1
    ym2612_write(builder, instance, port, 0x84 + ch, 0x0F); // op2
    ym2612_write(builder, instance, port, 0x88 + ch, 0x0F); // op3
    ym2612_write(builder, instance, port, 0x8C + ch, 0x0F); // op4

    // SSG-EG  [---SSG-EG[3:0]]
    // Proprietary; always 0.
    ym2612_write(builder, instance, port, 0x90 + ch, 0x00); // op1
    ym2612_write(builder, instance, port, 0x94 + ch, 0x00); // op2
    ym2612_write(builder, instance, port, 0x98 + ch, 0x00); // op3
    ym2612_write(builder, instance, port, 0x9C + ch, 0x00); // op4

    // Per-channel registers (B0H+, B4H+)

    // B0H+  [--FB[2:0]-ALG[2:0]]
    // Feedback=0 (no OP1 self-feedback → pure sine), Algorithm=7 (all ops are
    // output slots). Encoded: 0b000_000_111 = 0x07.
    ym2612_write(builder, instance, port, 0xB0 + ch, 0x07);

    // B4H+  [L-R-AMS[1:0]-0-FMS[2:0]]
    // L=1, R=1 (both speakers), AMS=0, FMS=0.  Encoded: 0b11_00_0_000 = 0xC0.
    ym2612_write(builder, instance, port, 0xB4 + ch, 0xC0);
}

/// Write the 14-bit F-number and Block to a YM2612 FM channel.
///
/// Per the YM2612 manual, the high byte (A4H+) **must** be written before
/// the low byte (A0H+).
///
/// # Arguments
/// * `port`        – 0 or 1 (selects the register bank).
/// * `ch_in_port`  – 0, 1, or 2.
/// * `a4_value`    – `Block[2:0]` in bits 5-3 and `F-num[10:8]` in bits 2-0.
/// * `fnum_low`    – `F-num[7:0]` (low byte).
pub fn write_ym2612_frequency(
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
    ym2612_write(builder, instance, port, 0xA4 + ch, a4_value);
    ym2612_write(builder, instance, port, 0xA0 + ch, fnum_low);
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
pub fn write_ym2612_keyon(builder: &mut VgmBuilder, instance: Instance, port: u8, ch_in_port: u8) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    // Bit 2 of the channel selector encodes the port.
    let ch_bits = if port == 0 {
        ch_in_port
    } else {
        0x04 | ch_in_port
    };
    // All four operator slots on (bits 7-4 = 0xF).
    ym2612_write(builder, instance, 0, 0x28, 0xF0 | ch_bits);
}

/// Emit a key-off command that silences all four operators on one channel.
///
/// Same encoding as [`write_ym2612_keyon`] but with operator mask = 0.
pub fn write_ym2612_keyoff(builder: &mut VgmBuilder, instance: Instance, port: u8, ch_in_port: u8) {
    assert!(ch_in_port < 3, "ch_in_port must be 0, 1, or 2");
    let ch_bits = if port == 0 {
        ch_in_port
    } else {
        0x04 | ch_in_port
    };
    // Operator mask = 0 → key off.
    ym2612_write(builder, instance, 0, 0x28, ch_bits);
}

#[test]
fn test_ym2612_keyon_and_tone_freq_matches_a4() {
    // Target pitch and chip configuration
    let target_hz = 440.0_f32;

    // YM2612 master clock for Genesis (NTSC)
    let master_clock = 7_670_454.0_f32;

    // We'll use port 0, channel-in-port 0  →  YM2612 channel 1 (the first FM channel).
    let port: u8 = 0;
    let ch_in_port: u8 = 0;

    // F-number calculation (unchanged from original test)
    let table = generate_12edo_fnum_table::<OpnaSpec>(master_clock).expect("generate fnum table");
    let tuned = find_and_tune_fnumber::<OpnaSpec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber");

    let fnum_u32 = tuned.f_num;
    let block_u8 = tuned.block;

    // YM2612 F-number is 11 bits: low 8 → A0H, high 3 → A4H bits 2-0.
    // Block occupies A4H bits 5-3.
    let fnum_low = (fnum_u32 & 0xFF) as u8;
    let fnum_high = ((fnum_u32 >> 8) & 0x07) as u8;
    let a4_value = ((block_u8 & 0x07) << 3) | (fnum_high & 0x07);

    // Build VGM
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2612, Instance::Primary, master_clock as u32);

    // One-time global initialisation (LFO off, DAC off, all channels key-off …)
    write_ym2612_global_init(&mut builder, Instance::Primary);

    // Initialise channel 1 with a pure-sine voice (algorithm 7, feedback 0,
    // OP1 full volume, OP2/3/4 muted, instant attack, no decay, fast release).
    write_ym2612_sine_voice(&mut builder, Instance::Primary, port, ch_in_port);

    // Write the frequency (A4H high byte first, then A0H low byte).
    write_ym2612_frequency(
        &mut builder,
        Instance::Primary,
        port,
        ch_in_port,
        a4_value,
        fnum_low,
    );

    // Key on – all four operators.
    write_ym2612_keyon(&mut builder, Instance::Primary, port, ch_in_port);

    // Hold note for ~0.5 s at 44 100 Hz sample rate.
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(22100));

    // Key off.
    write_ym2612_keyoff(&mut builder, Instance::Primary, port, ch_in_port);

    let doc = builder.finalize();

    // Optionally write the VGM artefact for manual verification
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2612_a4.vgm", &vgm_bytes);

    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2612State>(Instance::Primary, master_clock);

    let captured_freq_hz = std::sync::Arc::new(std::sync::Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2612Spec, _sample, event_opt| {
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
        diff <= YM2612_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {freq} Hz, target {target_hz} Hz (diff {diff})"
    );
}
