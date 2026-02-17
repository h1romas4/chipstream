# soundlog

soundlog — builder, parser and stream-processor for retro sound-chip register-write logs

`soundlog` is a small crate for building and parsing register-write
logs for retro sound chips. It currently supports the VGM
(Video Game Music) file format.

Key features:
- Builder API to construct VGM documents programmatically.
- Parser support to read VGM data into a structured `VgmDocument`.
- Type-safe APIs: chip specifications and VGM commands are modeled as
  Rust types to help prevent invalid register writes at compile time.
- Stream processing: `VgmStream` provides a low-memory, iterator-based
  processor that can accept either chunked binary input (via `push_chunk`)
  or a pre-parsed `VgmDocument` (via `from_document`) and yields parsed
  `VgmCommand` values as they become available.
- Callback-based processing: `VgmCallbackStream` wraps `VgmStream` to provide
  callback registration for chip register writes with automatic state tracking
  and event detection (KeyOn, KeyOff, ToneChange).
- Memory limits: Configurable limits for data block accumulation (default 32 MiB)
  and parsing buffer size (default 64 MiB) prevent unbounded memory growth from
  untrusted input.
- Chip state tracking: Monitor register writes to track key on/off events and
  extract tone information (frequency, pitch) from sound chip registers in real-time.

### VgmStream overview

`VgmStream` is designed for streaming/real-time consumption of VGM data:
- It yields `VgmCommand` values wrapped in stream results as it parses
  input and as it generates writes from DAC streams.
- It understands DAC stream control commands (e.g. `SetupStreamControl`,
  `SetStreamData`, `SetStreamFrequency`, `StartStream`, `StartStreamFastCall`,
  `StopStream`) and will expand stream-generated writes into the output
  timeline at the correct sample positions.
- It also supports YM2612 direct DAC writes and expands them into corresponding
  `Ym2612Port0Address2AWriteAndWaitN` commands on the stream timeline.
- During `Wait` commands, the internal scheduler finds upcoming stream-
  generated writes and splits waits as necessary so that generated chip
  writes are interleaved with parsed commands. This avoids emitting large
  bursts and preserves per-sample timing when multiple DAC streams are
  active concurrently.
- DataBlock compression (e.g. bit-packed and DPCM streams) is automatically decompressed
  and expanded by the crate so compressed streams and their associated
  decompression tables are applied transparently.
- Memory limits are enforced to protect against malicious or malformed files:
  - Data block size limit (default 32 MiB, configurable via `set_max_data_block_size()`)
  - Parsing buffer size limit (default 64 MiB, configurable via `set_max_buffer_size()`)

## Examples

### `VgmBuilder` as builder

```rust
use soundlog::{VgmBuilder, VgmCommand, VgmDocument};
use soundlog::chip::{Chip, Ym2612Spec};
use soundlog::vgm::command::{WaitSamples, Instance};
use soundlog::meta::Gd3;

let mut builder = VgmBuilder::new();

// Register the chip's master clock in the VGM header (in Hz)
builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
// Append chip register writes using a chip-specific spec
builder.add_chip_write(
    Instance::Primary,
    Ym2612Spec {
        port: 0,
        register: 0x22,
        value: 0x91,
    },
);
// Append a VGM command (example: wait)
builder.add_vgm_command(WaitSamples(44100));
// ... add more commands

// Set GD3 metadata for the document
builder.set_gd3(Gd3 {
    track_name_en: Some("Example Track".to_string()),
    game_name_en: Some("soundlog examples".to_string()),
    ..Default::default()
});

// Finalize the document
let document: VgmDocument = builder.finalize();
// `into()` converts the finalized `VgmDocument` into VGM-format binary bytes
let bytes: Vec<u8> = document.into();
```

### `VgmDocument` as parser

```rust
use soundlog::{VgmBuilder, VgmDocument};
use soundlog::vgm::command::{Instance, VgmCommand, WaitSamples};

// Read VGM bytes from somewhere
let bytes: Vec<u8> = /* read a .vgm file */ Vec::new();

// For this example we construct a VGM byte sequence using the builder
// and then parse it back.
let mut b = VgmBuilder::new();
b.add_vgm_command(WaitSamples(100));
b.add_vgm_command(WaitSamples(200));
let doc = b.finalize();
let bytes: Vec<u8> = (&doc).into();

// Parse the bytes into a `VgmDocument`
let document: VgmDocument = (bytes.as_slice())
    .try_into()
    .expect("failed to parse serialized VGM");

// Example: map commands to their sample counts and sum them.
let total_wait: u32 = document
    .iter()
    .map(|cmd| match cmd {
        VgmCommand::WaitSamples(s) => s.0 as u32,
        _ => 0,
    })
    .sum();

assert_eq!(total_wait, 300);
```

### `VgmStream::from_document`

The `from_document` constructor is convenient when you already have a
parsed `VgmDocument` (for example: constructed programmatically via the
`VgmBuilder`). The stream will expand DAC-stream-generated writes into
the emitted command sequence and split waits so emitted writes are
interleaved at the correct sample positions. All wait commands
(WaitSamples, WaitNSample, Wait735Samples, Wait882Samples) are converted
to WaitSamples for consistent processing.

```rust
use soundlog::{VgmBuilder, VgmStream, VgmDocument};
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::command::{VgmCommand, WaitSamples, WaitNSample, Wait735Samples, Wait882Samples, SetupStreamControl, StartStream, Instance};
use soundlog::chip::Ym2612Spec;
use soundlog::vgm::detail::{parse_data_block, DataBlockType};

// Build a minimal document that contains a data block and stream control
// commands. (Builder helpers for data blocks / stream setup exist on the
// `VgmBuilder` type; see the vgm module docs for details.)
let mut b = VgmBuilder::new();
// Example: append a YM2612 chip register write using the chip-specific spec
b.add_chip_write(
    Instance::Primary,
    Ym2612Spec {
        port: 0,
        register: 0x22,
        value: 0x91,
    },
);
// (pseudo-code) append data block, configure stream and start it
// b.add_data_block(...);
// b.add_vgm_command(SetupStreamControl { /* ... */ });
// b.add_vgm_command(StartStream { /* ... */ });
b.add_vgm_command(WaitSamples(8));
b.add_vgm_command(WaitNSample(5));  // 6 samples (5+1)
b.add_vgm_command(Wait735Samples);  // 735 samples
b.add_vgm_command(Wait882Samples);  // 882 samples

let doc: VgmDocument = b.finalize();

// Create a stream from the parsed document. The iterator will yield
// parsed commands as well as any stream-generated writes expanded into
// the timeline.
let mut stream = VgmStream::from_document(doc);
stream.set_loop_count(Some(2)); // Prevent infinite loops
while let Some(result) = stream.next() {
    match result {
        Ok(StreamResult::Command(cmd)) => match cmd {
            VgmCommand::WaitSamples(s) => {
                // All wait commands (WaitSamples, WaitNSample, Wait735Samples, Wait882Samples)
                // are converted to WaitSamples by VgmStream. Waits may also have been split
                // to accommodate stream-generated writes.
                println!("wait {} samples", s.0);
            }
            VgmCommand::Ym2612Write(inst, spec) => {
                // Handle YM2612 writes here. For example, forward to a device API.
                println!("YM2612 write: {:?} {:?}", inst, spec);
            }
            VgmCommand::DataBlock(block) => {
                // Note: the stream may return certain DataBlock types back to the
                // caller instead of storing them; check the source for details.
                // These returned DataBlock types include:
                // - RomRamDump   (data_type 0x80..=0xBF)
                // - RamWrite16   (data_type 0xC0..=0xDF)
                // - RamWrite32   (data_type 0xE0..=0xFF)
                // Additionally, `PcmRamWrite` commands may be returned to the caller instead of being stored; these appear as the VGM
                // command `PcmRamWrite` (opcode 0x68).                
                match parse_data_block(block) {
                    Ok(DataBlockType::RomRamDump(dump)) => {
                        println!(
                            "ROM/RAM dump: {:?}, size {}, start 0x{:08X}",
                            dump.chip_type, dump.rom_size, dump.start_address
                        );
                        // Handle ROM/RAM dump here (e.g. save to file or load into emulated memory)
                    }
                    Ok(_) => {
                        println!("DataBlock parsed (non-ROM/RAM)");
                    }
                    Err((orig_block, err)) => {
                        eprintln!("Failed to parse DataBlock {:?}: {:?}", orig_block, err);
                    }
                }
            }
            other => {
                // Write to the target chips here (e.g. SN76489).
                // Implement actual playback / device I/O in this branch.                
            },
        },
        Ok(StreamResult::NeedsMoreData) => break,
        Ok(StreamResult::EndOfStream) => break,
        Err(e) => eprintln!("stream error: {:?}", e),
    }
}
```

### `VgmStream` — feeding raw byte chunks

Note: apart from providing input via `push_chunk`, handling the stream is the same as the `from_document` example above — iterate over the stream and handle `StreamResult` variants (`Command`, `NeedsMoreData`, `EndOfStream`, `Err`) in the same way.

Note: When using `push_chunk()`, provide chunks that start at the VGM header's `data_offset` — i.e. the serialized command/data region beginning at offset `0x34 + header.data_offset`.

**Important**: Always set a loop count limit for untrusted input to prevent infinite loops.

```rust
use soundlog::vgm::VgmStream;
use soundlog::vgm::stream::StreamResult;

let mut parser = VgmStream::new();
parser.set_loop_count(Some(2)); // Prevent infinite loops
let chunks = vec![vec![0x61, 0x44], vec![0x01], vec![0x62, 0x63]];

for chunk in chunks {
    parser.push_chunk(&chunk).expect("push chunk");
    for result in &mut parser {
        match result {
            Ok(StreamResult::Command(_)) => {},
            Ok(StreamResult::NeedsMoreData) => break,
            Ok(StreamResult::EndOfStream) => {
                // EndOfStream reached — the stream has no further data.
                // To loop playback, reset your chunk source to the loop
                // offset and call `push_chunk` again so the parser receives
                // the bytes from the loop point onward.
                break
            },
            Err(_) => break,
        }
    }
}
```

### Looping and EndOfData overview

- VgmDocument is a data representation only: it encodes a loop offset and an EndOfData command, but it does not perform playback or looping itself. The document leaves loop behavior to the player. Note: when you construct a document using `VgmBuilder` and call `finalize()`, the builder will ensure the command stream contains an explicit `EndOfData` — if none is present it appends one.
- VgmStream is intended to act as a player-like processor. When created from a parsed `VgmDocument` with `VgmStream::from_document()`, it can automatically loop at the documented loop point. Its `set_loop_count()` API controls how many playthroughs are performed:
  - `set_loop_count(Some(n))` — limit playback to `n` playthroughs (for example, `Some(1)` means play once and stop; `Some(2)` means play one full run and then one loop iteration).
  - `set_loop_count(None)` — infinite looping (the stream will jump back to the loop point on EndOfData and continue indefinitely).
- When the stream reaches an `EndOfData` command it handles looping and end-of-stream semantics internally. If a finite loop count is configured and the limit is reached the stream will stop; if an infinite loop is configured it will jump to the loop point and continue.
- Bytes-mode (raw chunk input via `push_chunk`) does not implicitly rewind or re-feed earlier bytes. The parser maintains a small internal buffer and parses incrementally; it does not replay previously-consumed bytes for you. If you feed the stream using `push_chunk()` and want to loop playback, you must re-supply the bytes starting at the loop point yourself (reset your chunk source to that offset and call `push_chunk` again). See the byte-feeding example above for a usage pattern.
- `VgmCallbackStream` wraps `VgmStream` and invokes callbacks for register writes and other commands as they are emitted. Note that `VgmStream` consumes the `EndOfData` command internally while implementing loop behavior; as a result the `on_end_of_data` callback registered on `VgmCallbackStream` will not be invoked in normal operation. To detect playback termination observe the iterator reaching `EndOfStream` (or the iterator returning `None` in the callback wrapper).
- Fadeout support: configure `set_fadeout_samples(Some(n))` on the stream to allow the stream to continue emitting commands for `n` samples after the final loop end, which can be used to implement graceful fadeouts. When fadeout is active the stream records the loop end sample and will keep yielding commands (or generated waits) until the fadeout period elapses, after which `EndOfStream` is returned.

### VgmCallbackStream overview (WIP)

**Note: This feature is still under testing.**

`VgmCallbackStream` wraps `VgmStream` to provide automatic chip state tracking
and event-driven callbacks for real-time VGM processing:

- **Automatic State Tracking**: Enables per-chip state management for 35+ supported
  sound chips, automatically detecting register writes and maintaining internal state.
- **Event Detection**: Emits `StateEvent` notifications for key musical events:
  - `KeyOn`: Channel starts playing with tone/frequency information
  - `KeyOff`: Channel stops playing
  - `ToneChange`: Frequency changes while channel is active
- **Flexible Callbacks**: Register chip-specific callbacks using type-safe spec types
  (e.g., `Ym2612Spec`, `Sn76489Spec`) to handle register writes with sample timing
  and associated events.
- **Real-time Processing**: Low-overhead design suitable for streaming playback,
  with callbacks invoked automatically as commands are processed.
- **Comprehensive Chip Support**: Works with all major sound chips including FM
  synthesizers (YM2612, YM2151, OPL series), PSG chips (SN76489, AY-8910),
  PCM chips, and more.

This enables building advanced VGM analysis tools, real-time visualizers,
and custom playback engines with minimal boilerplate code.

```rust
use soundlog::{VgmBuilder, VgmCallbackStream};
use soundlog::vgm::command::Instance;
use soundlog::chip::{event::StateEvent, Ym2612Spec};

// Build a simple VGM document with YM2612 commands
let mut b = VgmBuilder::new();
b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
// YM2612 initialization: LFO off
b.add_chip_write(Instance::Primary, Ym2612Spec { port: 0, register: 0x22, value: 0x00 });
// Key on channel 1
b.add_chip_write(Instance::Primary, Ym2612Spec { port: 0, register: 0x28, value: 0xF0 });
b.add_vgm_command(soundlog::vgm::command::WaitSamples(100));
b.add_vgm_command(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData {}));
let doc = b.finalize();

// Grab the header's chip instances.
let instances = doc.header.chip_instances();

// Create VgmCallbackStream from VgmDocument
let mut callback_stream = VgmCallbackStream::from_document(doc);

// Automatically enable state tracking for every chip registered in the VGM header.
// This initializes per-instance state trackers using the clock values recorded in
// the document header; it is equivalent to calling `track_state` for each instance.
// Useful when a VGM contains multiple chip instances.
callback_stream.track_chips(&instances);

// If you prefer to enable tracking for a single chip instance manually,
// Enable state tracking for YM2612 at NTSC Genesis clock
// callback_stream.track_state::<soundlog::chip::state::Ym2612State>(
//     Instance::Primary, 7_670_454.0
// );

// Register callback for YM2612 writes
callback_stream.on_write(|inst, spec: Ym2612Spec, sample, events| {
    println!("YM2612[{:?}] @ sample {}: reg={:02X} val={:02X}",
             inst, sample, spec.register, spec.value);

    if let Some(events) = events {
        for event in events {
            match event {
                StateEvent::KeyOn { channel, tone } => {
                    println!("  → KeyOn ch={} freq={:.1}Hz",
                             channel, tone.freq_hz.unwrap_or(0.0));
                }
                StateEvent::KeyOff { channel } => {
                    println!("  → KeyOff ch={}", channel);
                }
                StateEvent::ToneChange { channel, tone } => {
                    println!("  → ToneChange ch={} freq={:.1}Hz",
                             channel, tone.freq_hz.unwrap_or(0.0));
                }
            }
        }
    }
});

// Process stream - callbacks fire automatically
for result in callback_stream {
    match result {
        Ok(_) => { /* callbacks already invoked */ }
        Err(e) => eprintln!("Error: {:?}", e),
    }
}
```

## Chip State Tracking (WIP)

**Note: This feature is still under testing.**

The `chip::state` module provides real-time state tracking for sound chips,
detecting key on/off events and extracting tone information from register writes.

### Implemented Chips

| Chip | Channels | Key On/Off | Tone Extract | System | Test |
|------|----------|------------|--------------|--------|------|
| **SN76489 (PSG)** | 3 tone + 1 noise | ✅ | ✅ | Master System, Game Gear, etc. | ⬜ |
| **YM2413 (OPLL)** | 9 FM | ✅ | ✅ | MSX, SMS FM Unit | ⬜ |
| **YM2612 (OPN2)** | 6 FM | ✅ | ✅ | Sega Genesis/Mega Drive | ⬜ |
| **YM2151 (OPM)** | 8 FM | ✅ | ✅ | Arcade systems, etc. | ⬜ |
| **SegaPcm** | N/A | N/A | N/A | Sega PCM chip | ⬜ |
| **Rf5c68** | N/A | N/A | N/A | RF5C68 PCM chip | ⬜ |
| **YM2203 (OPN)** | 3 FM + 3 PSG | ✅ | ✅ | NEC PC-8801, etc. | ⬜ |
| **YM2608 (OPNA)** | 6 FM + 3 PSG | ✅ | ✅ | NEC PC-8801, etc. | ⬜ |
| **YM2610b (OPNB)** | 6 FM + 3 PSG + ADPCM | ✅ | ✅ | Neo Geo, etc. | ⬜ |
| **YM3812 (OPL2)** | 9 FM | ✅ | ✅ | AdLib, Sound Blaster | ⬜ |
| **YM3526 (OPL)** | 9 FM | ✅ | ✅ | C64 Sound Expander, etc. | ⬜ |
| **Y8950** | 9 FM + ADPCM | ✅ | ✅ | MSX | ⬜ |
| **YMf262 (OPL3)** | 18 FM | ✅ | ✅ | Sound Blaster 16, etc. | ⬜ |
| **YMF278b (OPL4)** | 18 FM + PCM | ✅ | ✅ | YMF278B | ⬜ |
| **YMF271 (OPX)** | 12 FM + PCM | ✅ | ✅ | YMF271 | ⬜ |
| **SCC1** | 5 | ✅ | ✅ | Konami SCC (same as K051649) | ⬜ |
| **YMZ280b** | N/A | N/A | N/A | YMZ280B PCM | ⬜ |
| **RF5C164** | N/A | N/A | N/A | RF5C164 PCM | ⬜ |
| **PWM** | N/A | N/A | N/A | Sega PWM | ⬜ |
| **AY8910** | 3 tone + noise | ✅ | ✅ | ZX Spectrum, MSX, etc. | ⬜ |
| **GbDmg** | 4 | ✅ | ✅ | Game Boy | ⬜ |
| **NesApu** | 5 | ✅ | ✅ | NES | ⬜ |
| **MultiPcm** | N/A | N/A | N/A | Sega MultiPCM | ⬜ |
| **UPD7759** | N/A | N/A | N/A | uPD7759 ADPCM | ⬜ |
| **OKIM6258** | N/A | N/A | N/A | OKIM6258 ADPCM | ⬜ |
| **OKIM6295** | N/A | N/A | N/A | OKIM6295 ADPCM | ⬜ |
| **K051649** | 5 | ✅ | ✅ | Konami SCC | ⬜ |
| **K054539** | N/A | N/A | N/A | Konami K054539 PCM | ⬜ |
| **HUC6280** | 6 | ✅ | ✅ | PC Engine/TurboGrafx-16 | ⬜ |
| **C140** | N/A | N/A | N/A | Namco C140 PCM | ⬜ |
| **K053260** | N/A | N/A | N/A | Konami K053260 PCM | ⬜ |
| **Pokey** | 4 | ✅ | ✅ | Atari 8-bit computers | ⬜ |
| **QSound** | N/A | N/A | N/A | Capcom QSound | ⬜ |
| **SCSP** | N/A | N/A | N/A | Sega Saturn SCSP | ⬜ |
| **WonderSwan** | 4 | ✅ | ✅ | WonderSwan APU | ⬜ |
| **VSU** | 6 | ✅ | ✅ | Virtual Boy VSU | ⬜ |
| **SAA1099** | 6 | ✅ | ✅ | SAM Coupé, etc. | ⬜ |
| **ES5503** | N/A | N/A | N/A | Ensoniq ES5503 | ⬜ |
| **ES5506U8** | N/A | N/A | N/A | Ensoniq ES5506 (8-bit) | ⬜ |
| **ES5506U16** | N/A | N/A | N/A | Ensoniq ES5506 (16-bit) | ⬜ |
| **X1010** | N/A | N/A | N/A | Setia X1-010 | ⬜ |
| **C352** | N/A | N/A | N/A | Namco C352 | ⬜ |
| **GA20** | N/A | N/A | N/A | Irem GA20 | ⬜ |
| **Mikey** | N/A | N/A | N/A | Atari Lynx | ⬜ |
| **GameGearPsg** | 3 tone + 1 noise | ✅ | ✅ | Game Gear PSG (same as SN76489) | ⬜ |

## License

MIT License
