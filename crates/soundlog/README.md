# soundlog

[![crates.io](https://img.shields.io/crates/v/soundlog.svg)](https://crates.io/crates/soundlog) [![docs.rs](https://docs.rs/soundlog/badge.svg)](https://docs.rs/soundlog)

soundlog — builder, parser and stream-processor for retro sound-chip register-write logs

`soundlog` is a small crate for building and parsing register-write
logs for retro sound chips. It supports the VGM (Video Game Music) file format,
MDX-to-VGM conversion, and XGM v1.

Use [`chipstream`](https://crates.io/crates/chipstream) to render these formats
to stereo PCM. `soundlog` processes commands and tracks chip state; it does
not synthesize audio.

Key features:
- [`VgmBuilder`] constructs VGM documents programmatically.
- [`VgmDocument::parse`] reads VGM data into a structured [`VgmDocument`].
- Type-safe APIs: chip specifications and VGM commands are modeled as
  Rust types to help prevent invalid register writes at compile time.
- Stream processing: [`VgmStream`] accepts chunked commands (via [`push_chunk`](VgmStream::push_chunk)),
  raw VGM bytes (via [`from_vgm`](VgmStream::from_vgm)), or a pre-parsed document
  (via [`from_document`](VgmStream::from_document)), and yields [`VgmCommand`] values.
- Custom streams: [`VgmStream::from_generator`] accepts user-defined command generators.
- Callback-based processing: [`VgmCallbackStream`] wraps [`VgmStream`] to provide
  callback registration for chip register writes with automatic state tracking
  and event detection (KeyOn, KeyOff, ToneChange).
- Memory limits: 32 MiB of data blocks (including decoded data), 256 blocks,
  and a 64 MiB parsing buffer by default.
- Processing limits: Configurable budgets for commands without time progress
  and for DAC stream expansion per wait.
- Chip state tracking: Monitor register writes to track key on/off events and
  extract tone information (frequency, pitch) as writes are processed.
- Format conversion: MDX and XGM v1 can use the same VGM stream-processing
  interface for playback.

## Quick Start — processing a VGM file

Put an uncompressed VGM file containing YM2612 writes at `song.vgm`. This
example prints register writes and waits without opening an audio device.

```rust,no_run
use soundlog::{VgmCallbackStream, VgmHeader, VgmStreamResult};
use soundlog::chip::Ym2612Spec;
use soundlog::vgm::command::VgmCommand;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("song.vgm")?;
    let header = VgmHeader::from_bytes(&bytes)?;
    let mut stream = VgmCallbackStream::from_vgm(bytes)?;
    stream.track_chips(&header.chip_instances());
    stream.on_write(|instance, spec: Ym2612Spec, sample, events| {
        println!(
            "{instance:?} YM2612 P{}:0x{:02X}=0x{:02X} at sample {sample}: {events:?}",
            spec.port, spec.register, spec.value,
        );
    });

    for result in &mut stream {
        if let VgmStreamResult::Command(VgmCommand::WaitSamples(wait)) = result? {
            println!("Wait {} samples at 44.1 kHz", wait.0);
        }
    }
    Ok(())
}
```

## VgmStream overview

`VgmStream` processes commands incrementally on the VGM timeline.
[`current_sample()`](VgmStream::current_sample) and callback timestamps use
44.1 kHz samples and reset when a loop begins. They track command processing,
not audio device playback.
Parsing and DAC expansion can allocate or decompress data. Neither
`VgmStream` nor `VgmCallbackStream` guarantees real-time safety; process
commands outside the audio device callback.

- It yields `VgmCommand` values wrapped in stream results as it parses
  input and as it generates writes from DAC streams.
- It understands DAC stream control commands (e.g. `SetupStreamControl`,
  `SetStreamData`, `SetStreamFrequency`, `StartStream`, `StartStreamFastCall`,
  `StopStream`) and will expand stream-generated writes into the output
  timeline at the correct sample positions.
- Call [`set_emit_stream_controls(true)`](VgmStream::set_emit_stream_controls) to receive start and stop commands
  after processing. Use
  [`dac_stream_targets(DacStreamSelector::Stream(id))`](VgmStream::dac_stream_targets)
  for a start or `dac_stream_targets((&stop_command).into())` for a stop,
  including all-stream stops. Targets remain available after stopping.
- [`stop_dac_streams_for_target`](VgmStream::stop_dac_streams_for_target)
  stops all streams matching a [`DacStreamTarget`](vgm::DacStreamTarget)
  without emitting a command. A later start can resume them. Both
  `DacStreamTarget` and [`DacStreamSelector`](vgm::DacStreamSelector) are in `soundlog::vgm`.
- It also supports YM2612 direct DAC writes and expands them into corresponding
  `Ym2612Port0Address2AWriteAndWaitN`, `SeekOffset` commands on the stream timeline.
- While processing `Wait*` commands, the internal scheduler computes the outputs of the DAC stream
  (including YM2612 `Ym2612Port0Address2AWriteAndWaitN`) and normalises the timeline
  into register-write commands and `WaitSamples`.
  This process converts DAC stream events into explicit write commands and normalised waits,
  thereby preserving per-sample timing when multiple streams are active concurrently.
- DataBlock compression (e.g. bit-packed and DPCM streams) is automatically decompressed
  and expanded by the crate so compressed streams and their associated
  decompression tables are applied transparently.
- Configure memory limits with [`set_max_data_block_size()`](VgmStream::set_max_data_block_size),
  [`set_max_data_block_count()`](VgmStream::set_max_data_block_count), and
  [`set_max_buffer_size()`](VgmStream::set_max_buffer_size). The buffer limit
  applies to chunked input. Pass `None` to disable the block count limit.
- The default limit is 100,000 raw commands without sample-time progress.
  Use [`set_max_commands_without_wait()`](VgmStream::set_max_commands_without_wait)
  to change it or pass `None` to disable it.

## Examples

### `VgmBuilder` as builder

```rust
use soundlog::chip::{Chip, Ym2612Spec};
use soundlog::meta::Gd3;
use soundlog::vgm::command::{Instance, StreamChipType, WaitSamples};
use soundlog::vgm::detail::UncompressedStream;
use soundlog::{VgmBuilder, VgmDocument};

let mut builder = VgmBuilder::new();

// Register the chip's master clock in the VGM header (in Hz)
builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
// Attach a DataBlock detail
builder.attach_data_block(UncompressedStream {
    chip_type: StreamChipType::Ym2612Pcm,
    data: vec![0x01, 0x02],
});
// Add chip register writes using a chip-specific spec
builder.add_chip_write(
    Instance::Primary,
    Ym2612Spec {
        port: 0,
        register: 0x22,
        value: 0x91,
    },
);
builder.add_vgm_command(WaitSamples(44100));
// ... add more commands

// Set loop offset (1: WaitSamples(44100))
builder.set_loop_offset(1);

// Set GD3 metadata for the document
builder.set_gd3(Gd3 {
    track_name_en: Some("Example Track".to_string()),
    game_name_en: Some("soundlog examples".to_string()),
    ..Default::default()
});

// Finalize the document
// An `EndOfData` tag is automatically added.
// Values such as `total_samples` in the header are calculated automatically.
let document: VgmDocument = builder.finalize();

// `into()` converts the finalized `VgmDocument` into VGM-format binary bytes
let _bytes: Vec<u8> = document.into();
```

### `VgmDocument` as parser

```rust
use soundlog::{VgmBuilder, VgmDocument};
use soundlog::vgm::command::{VgmCommand, WaitSamples};

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
(`WaitSamples`, `WaitNSample`, `Wait735Samples`, `Wait882Samples`, `Ym2612Port0Address2AWriteAndWaitN`)
are converted to `WaitSamples` for consistent processing.

```rust
use soundlog::chip::Ym2612Spec;
use soundlog::vgm::command::{
    Instance, SetupStreamControl, StartStream, VgmCommand, Wait735Samples, Wait882Samples,
    WaitNSample, WaitSamples,
};
use soundlog::vgm::detail::{DataBlockType, parse_data_block};
use soundlog::{VgmBuilder, VgmDocument, VgmStream, VgmStreamResult};

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
// b.attach_data_block(...);
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
// Drive the stream until EndOfStream.
while let Some(result) = stream.next() {
    match result {
        Ok(VgmStreamResult::Command(cmd)) => match cmd {
            VgmCommand::WaitSamples(s) => {
                // All wait commands
                // (WaitSamples, WaitNSample, Wait735Samples,
                //  Wait882Samples, Ym2612Port0Address2AWriteAndWaitN)
                // are converted to WaitSamples by VgmStream.
                // Waits may also have been split to accommodate stream-generated writes.
                println!("wait {} samples", s.0);
            }
            VgmCommand::Ym2612Write(inst, spec) => {
                // We simply wait for WaitSamples and write to the register
                // without needing to concern ourselves with DAC Stream.
                println!("YM2612 write: {:?} {:?}", inst, spec);
            }
            VgmCommand::DataBlock(block) => {
                // Note: the stream may return certain DataBlock types back to the
                // caller instead of storing them; check the source for details.
                // These returned DataBlock types include:
                // - RomRamDump   (data_type 0x80..=0xBF)
                // - RamWrite16   (data_type 0xC0..=0xDF)
                // - RamWrite32   (data_type 0xE0..=0xFF)
                // The parse_data_block can parse the details of a DataBlock.
                match parse_data_block(*block) {
                    Ok(DataBlockType::RomRamDump(dump)) => {
                        // Handle ROM/RAM dump here 
                        println!(
                            "ROM/RAM dump: {:?}, size {}, start 0x{:08X}",
                            dump.chip_type, dump.rom_size, dump.start_address
                        );
                    }
                    Ok(_) => {
                        println!("DataBlock parsed (non-ROM/RAM)");
                    }
                    Err((orig_block, err)) => {
                        eprintln!("Failed to parse DataBlock {:?}: {:?}", orig_block, err);
                    }
                }
            }
            VgmCommand::PcmRamWrite(write) => {
                // And also `PcmRamWrite` commands (opcode 0x68) may be emitted by the stream
                // instead of being stored as a DataBlock. Handle them here if you
                // need to process PCM RAM writes directly.
                println!("PCM RAM write: chip_type={:?} size={}", write.chip_type, write.size);
                // Process or store the PCM RAM write as appropriate.
            }
            other => {
                // Write to the target chips here (e.g. SN76489).
                // Implement actual playback / device I/O in this branch.                
            },
        },
        Ok(VgmStreamResult::NeedsMoreData) => break,
        Ok(VgmStreamResult::EndOfStream) => break,
        Err(e) => eprintln!("stream error: {:?}", e),
    }
}
```

### `VgmStream::from_vgm`

[`VgmStream::from_vgm`] takes a complete VGM byte buffer and parses commands on
demand. Unlike `from_document`, it does not need a pre-parsed command list.
`push_chunk` is only supported on streams created for chunked input.

### `VgmStream` — feeding raw byte chunks

Use [`VgmStream::new()`](VgmStream::new) and [`push_chunk`](VgmStream::push_chunk)
to process VGM commands without holding the entire file in memory. Feed only the
command region, not the
header or trailing metadata. For a VGM file, parse the header separately
and find the first command with
[`VgmHeader::command_start(header.version, header.data_offset)`](VgmHeader::command_start).
Data blocks can still consume memory inside the stream.

[`NeedsMoreData`](VgmStreamResult::NeedsMoreData) means the parser needs another
chunk, possibly to finish a split command. If the input has ended, this is not
successful completion. [`EndOfStream`](VgmStreamResult::EndOfStream) means
playback has finished, not that the current chunk ran out.

The example uses the default single traversal and includes `EndOfData`.
If you configure additional loops, the parser cannot rewind the input:
when it requests more data after a loop boundary, resume feeding at the loop
offset from the header. Do not feed another loop after `EndOfStream`.

```rust
use soundlog::{ParseError, VgmStream, VgmStreamResult};

fn main() -> Result<(), ParseError> {
    let mut stream = VgmStream::new();
    // Wait 324 samples (split across two chunks), then EndOfData (0x66).
    let chunks: [&[u8]; 3] = [&[0x61, 0x44], &[0x01], &[0x66]];

    for chunk in chunks {
        stream.push_chunk(chunk)?;
        for result in &mut stream {
            match result? {
                VgmStreamResult::Command(command) => println!("{command:?}"),
                VgmStreamResult::NeedsMoreData => break,
                VgmStreamResult::EndOfStream => return Ok(()),
            }
        }
    }
    Err(ParseError::UnexpectedEof)
}
```

### `VgmStream::from_generator` — streaming playback for other formats

[`VgmCommandGenerator`] is the extension point for producing VGM commands from
your own source or another file format. Implement [`next_command()`](VgmCommandGenerator::next_command)
and pass your boxed generator to [`VgmStream::from_generator`].

The trait definition is:

```rust
use soundlog::vgm::command::VgmCommand;
use soundlog::ParseError;

pub trait VgmCommandGenerator: std::fmt::Debug {
    fn next_command(&mut self) -> Result<Option<VgmCommand>, ParseError>;
}
```

Return `Ok(Some(command))` to yield a command, `Ok(None)` when the generator
has permanently finished, or `Err(error)` if command generation fails.
`Ok(None)` ends the stream even if the generator did not emit `EndOfData`.
`VgmStream` then returns `EndOfStream` (including on repeated reads), while
`VgmCallbackStream` returns `None`. This is not a request for more input.
Implementations must also derive or implement `Debug`.

MDX conversion uses the same interface: its lazy generator is one
`VgmCommandGenerator` implementation. The example below shows how to use it;
the `mdx` feature is only required when using the MDX implementation.

Commands are converted as they are consumed, so the complete VGM command list
does not need to be kept in memory.
`MdxToVgmOptions` provides configurable playback-tick and per-tick command
limits and a decoded-PCM cache limit, plus a retained-output limit for eager
conversion; see the API rustdoc.
MDX support is disabled by default; enable it in your dependency declaration
with `soundlog = { version = "0.19", features = ["mdx"] }`.

```rust
# #[cfg(feature = "mdx")]
# {
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator};
use soundlog::mdx::document::MdxBuilder;
use soundlog::mdx::package::MdxPackage;
use soundlog::{VgmStream, VgmStreamResult};

// Build or parse MDX/PDX data in memory.
let package = MdxPackage {
    mdx: MdxBuilder::new().finalize().unwrap(),
    pdx: None,
};
let generator = to_vgm_stream_generator(package, MdxToVgmOptions::default())
    .expect("create a lazy VGM command generator");
let mut stream = VgmStream::from_generator(generator);

for result in &mut stream {
    match result {
        Ok(VgmStreamResult::Command(command)) => {
            // Send command to the target sound chips or audio renderer.
            println!("{command:?}");
        }
        Ok(VgmStreamResult::EndOfStream) => break,
        Ok(VgmStreamResult::NeedsMoreData) => break,
        Err(error) => panic!("stream error: {error}"),
    }
}
# }
```

### XGM v1

Enable the optional `xgm` feature to parse or build XGM v1 documents and
play a track through `VgmStream`.

```rust
# #[cfg(feature = "xgm")]
# {
use soundlog::vgm::VgmStream;
use soundlog::xgm::{XgmBuilder, XgmCommand, XgmDocument, XgmPlaybackOptions,
                    XgmTiming, XgmTrack, XgmVgmGenerator};

let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
builder.set_sample(1, &[0x20; 256])?;
builder.add_track(XgmTrack {
    commands: vec![
        XgmCommand::Pcm { channel: 0, priority: 0, sample_id: 1 },
        XgmCommand::Frame,
        XgmCommand::End,
    ],
    gd3: None,
});
let bytes = builder.finalize()?.to_bytes()?;
let document = XgmDocument::parse(&bytes)?;
let header = document.header.timing.vgm_header();
let generator = XgmVgmGenerator::new(document, XgmPlaybackOptions::default())?;
let mut stream = VgmStream::from_generator(Box::new(generator));
# let _ = (&header, &mut stream);
# }
# Ok::<(), soundlog::ParseError>(())
```

## VgmCallbackStream overview

[`VgmCallbackStream`] wraps [`VgmStream`] to provide automatic chip state tracking
and event-driven callbacks during incremental VGM processing:

- **Automatic State Tracking**: [`track_chips`](VgmCallbackStream::track_chips)
  enables state tracking for the chip instances in a VGM header.
- **Event Detection**: Emits [`StateEvent`](chip::event::StateEvent) notifications for key musical events:
  - `KeyOn`: Channel starts playing with tone/frequency information
  - `KeyOff`: Channel stops playing
  - `ToneChange`: Frequency changes while channel is active
- **Flexible Callbacks**: Register chip-specific callbacks using type-safe spec types
  (e.g., `Ym2612Spec`, `Sn76489Spec`) with [`on_write`](VgmCallbackStream::on_write)
  to handle register writes with sample timing
  and associated events.
- **Synchronous Callbacks**: Callbacks run as the iterator processes each
  command, not when an audio device plays the corresponding samples.
- **Comprehensive Chip Support**: (**WIP**) Works with all major sound chips including FM
  synthesizers (YM2612, YM2151, OPL series), PSG chips (SN76489, AY-8910),
  PCM chips, and more.

This enables building advanced VGM analysis tools, real-time visualizers,
and custom playback engines with minimal boilerplate code.

See also: the **Chip State Tracking** section below for current implementation status and a list of supported chips.

```rust
use soundlog::chip::{Ym2612Spec, event::StateEvent};
use soundlog::vgm::command::{EndOfData, Instance, VgmCommand, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

// Build a simple VGM document with YM2612 commands
let mut b = VgmBuilder::new();
b.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);
// Sound chip initialisation
// ..snip..
// Key on channel 1
b.add_chip_write(Instance::Primary, Ym2612Spec {
    port: 0,
    register: 0x28,
    value: 0xF0
});
b.add_vgm_command(VgmCommand::WaitSamples(WaitSamples(100)));
b.add_vgm_command(VgmCommand::EndOfData(EndOfData));
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

// For chip-specific settings, track_state_with accepts a configured state.
// For example, after track_chips, override an SN76489 instance with its
// effective clock and the header's clock-divider and zero-period flags:
// callback_stream.track_state_with(
//     Instance::Primary,
//     soundlog::chip::state::Sn76489State::with_config(3_579_545.0, true, true),
// );
// track_chips alone uses the default SN76489 settings.

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

## Seeking

[`VgmStream::seek_to_sample`] and [`VgmCallbackStream::seek_to_sample`] rewind to
the loop point (or the command stream start if there is no loop). The target
is in 44.1 kHz samples from that point. [`Reached { sample }`](VgmSeekOutcome::Reached)
reports the actual position, which may be later than requested because waits
are consumed whole. If the first `EndOfData` comes before the target, the
stream stops and returns [`EndOfRange { sample }`](VgmSeekOutcome::EndOfRange) rather than
entering another loop or starting a fade. You can seek again from there.

Chunk-fed and generator-backed streams cannot seek. On supported inputs, the
callback stream rebuilds its chip trackers during the seek without notifying
user callbacks. It starts from the registered initial tracker states and
does not replay the intro before the loop point. If the loop relies on
register writes from the intro, tracked state can differ from uninterrupted
playback. For PCM playback seeking on the output-frame timeline, use
[chipstream's `seek_to_frame`](https://docs.rs/chipstream/latest/chipstream/sequencer/struct.VgmSequencer.html#method.seek_to_frame) instead.

## Looping and EndOfData overview

- [`VgmBuilder::finalize()`](VgmBuilder::finalize) adds `EndOfData` if the command list has none.
- File- and document-backed streams can rewind to the loop point. They
  handle loop boundaries at `EndOfData`, regardless of `VgmHeader::loop_samples`.
  - [`set_loop_count(Some(n))`](VgmStream::set_loop_count) requests `n` traversals. `Some(0)` is treated
    as `Some(1)`, the default. VGM header loop settings can adjust the count.
  - `set_loop_count(None)` repeats indefinitely if there is a loop point.
- `VgmStream` consumes `EndOfData` internally, so [`on_end_of_data`](VgmCallbackStream::on_end_of_data) does not
  fire during normal iteration. Detect completion with `EndOfStream` on
  `VgmStream` or `None` on `VgmCallbackStream`.
- [`set_fadeout_samples(Some(n))`](VgmStream::set_fadeout_samples) extends a looping stream by `n` samples
  after the final loop boundary. [`is_fading_out()`](VgmStream::is_fading_out) reports stream progress,
  which can run ahead of rendered audio. The stream does not attenuate audio;
  apply the fade to the output level or through chip writes.

## Error Handling

Parsing, streaming, decompression, F-number utilities, and serialization for
MDX and XGM return `Result` with structured errors. See [`ParseError`] for
error variants and examples.

User-defined and MDX generators use [`ParseError::GeneratorError`] to preserve
format-specific error types and their `Error::source()` chains. [`Other`](ParseError::Other) is
reserved for caller-defined message-only errors.

Stream iterators yield library and generator errors as `Err`.
Callbacks return `()`, so errors inside callbacks are not automatically
propagated through the iterator.

Missing or undecodable MDX PCM is treated as silence; use the package's decoding
API for strict validation. PCM cache limit violations still return errors.
Allocation failures and panics in user code are not converted into library errors.

## Chip State Tracking (WIP)

The [`chip::state`] module tracks sound-chip state as writes arrive,
detecting key on/off events and extracting tone information from register writes.

- A check in the `Test` column indicates state-tracking tests.
- The specifications for key on/off detection — including total level extraction — are not yet stabilized.
- `StateEvent` timing and values may change between versions.
- The channel counts below are those reported by each tracker. PCM trackers
  retain register writes but do not emit key or tone events; ES5505/ES5506
  additionally track the selected page and host-bus writes.

### Implemented Chips

| Chip | Channels | Key On/Off | Tone Extract | System | Test |
|------|----------|------------|--------------|--------|------|
| **SN76489 (PSG)** | 3 tone + 1 noise | ✅ | ✅ | Master System, Game Gear, etc. | ✅ |
| **YM2413 (OPLL)** | 9 FM | ✅ | ✅ | MSX, SMS FM Unit | ✅ |
| **YM2612 (OPN2)** | 6 FM | ✅ | ✅ | Sega Genesis/Mega Drive | ✅ |
| **YM2151 (OPM)** | 8 FM | ✅ | ✅ | Arcade systems, etc. | ✅ |
| **Sega PCM** | 16 | — | — | Sega PCM chip | ✅ |
| **RF5C68** | 8 | — | — | RF5C68 PCM chip | ✅ |
| **YM2203 (OPN)** | 3 FM + 3 PSG | ✅ | ✅ | NEC PC-8801, etc. | ✅ |
| **YM2608 (OPNA)** | 6 FM + 3 PSG | ✅ | ✅ | NEC PC-8801, etc. | ✅ |
| **YM2610B (OPNB)** | 6 FM + 3 PSG | ✅ | ✅ | Neo Geo, etc. | ✅ |
| **YM3812 (OPL2)** | 9 FM | ✅ | ✅ | AdLib, Sound Blaster | ✅ |
| **YM3526 (OPL)** | 9 FM | ✅ | ✅ | C64 Sound Expander, etc. | ✅ |
| **Y8950** | 9 FM | ✅ | ✅ | MSX | ✅ |
| **YMF262 (OPL3)** | 18 FM | ✅ | ✅ | Sound Blaster 16, etc. | ✅ |
| **YMF278B (OPL4)** | 18 FM | ✅ | ✅ | YMF278B | ✅ |
| **YMF271 (OPX)** | 12 groups / 48 slots | ✅ | ✅ | YMF271 | ✅ |
| **K051649** | 5 | ✅ | ✅ | Konami SCC | ✅ |
| **SCC1 (K052539)** | 5 | ✅ | ✅ | Konami SCC (same as K051649) | ✅ |
| **YMZ280B** | 8 | — | — | YMZ280B PCM | ✅ |
| **RF5C164** | 8 | — | — | RF5C164 PCM | ✅ |
| **PWM** | 1 | — | — | Sega PWM | ✅ |
| **AY8910** | 3 tone + noise | ✅ | ✅ | ZX Spectrum, MSX, etc. | ✅ |
| **Game Boy DMG** | 4 | ✅ | ✅ | Game Boy | ✅ |
| **NES APU** | 5 + 1 FDS | ✅ | ✅ | NES | ✅ |
| **MultiPCM** | 28 | — | — | Sega MultiPCM | ✅ |
| **uPD7759** | 1 | — | — | uPD7759 ADPCM | ✅ |
| **OKIM6258** | 1 | — | — | OKIM6258 ADPCM | ✅ |
| **OKIM6295** | 4 | — | — | OKIM6295 ADPCM | ✅ |
| **K054539** | 8 | — | — | Konami K054539 PCM | ✅ |
| **HuC6280** | 6 | ✅ | ✅ | PC Engine/TurboGrafx-16 | ✅ |
| **C140** | 24 | — | — | Namco C140 PCM | ✅ |
| **K053260** | 4 | — | — | Konami K053260 PCM | ✅ |
| **POKEY** | 4 | ✅ | ✅ | Atari 8-bit computers | ✅ |
| **QSound** | 16 | — | — | Capcom QSound | ✅ |
| **SCSP** | 32 | — | — | Sega Saturn SCSP | ✅ |
| **WonderSwan** | 4 | ✅ | ✅ | WonderSwan APU | ✅ |
| **VSU** | 6 | ✅ | ✅ | Virtual Boy VSU | ✅ |
| **SAA1099** | 6 | ✅ | ✅ | SAM Coupé, etc. | ✅ |
| **ES5503** | 32 | — | — | Ensoniq ES5503 | ✅ |
| **ES5505 / ES5506** | 32 | — | — | Ensoniq ES5505 / ES5506 | ✅ |
| **X1-010** | 16 | — | — | Setia X1-010 | ✅ |
| **C352** | 32 | — | — | Namco C352 | ✅ |
| **GA20** | 4 | — | — | Irem GA20 | ✅ |
| **Mikey** | 4 | ✅ | ✅ | Atari Lynx | ✅ |

- YM2610B and Y8950 track FM/PSG as listed, not ADPCM. YMF278B tracks FM,
  not PCM; YMF271 tracks FM voices, not PCM or its linear waveform.
- NES APU includes FDS expansion audio as channel 5. FDS pitch excludes
  modulation, and envelope amplitude is not simulated.
- Mikey tone frequency represents the channel timer; linked clocks do not have
  a directly calculated frequency.

## License

MIT License
