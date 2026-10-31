# ndsif

Allocation-free `no_std` Rust codec for the NDSIF v1 draft.

The v1 draft shares wire version `01` with the previous b protocol but is not
wire-compatible. Select the profile explicitly at the transport boundary.

API revision: 40. Frame wire version: `01`.
Device firmware: [NanoDrive6](https://github.com/Fujix1/NanoDrive6),
[NanoDrive8](https://github.com/Fujix1/NanoDrive8).
The optional timed DAC profile follows [NanoDrive6 NDSIF API section 9](https://github.com/Fujix1/NanoDrive6/blob/main/NDSIF_API.md)
and remains an RC extension, not a requirement for every NDSIF device.

## Scope

| Provides | Not provided |
|---|---|
| `no_std`, allocation-free framing, CRC, and incremental decoding | Serial I/O, retries, or capability discovery |
| Typed commands, responses, capabilities, and audio-stream messages | Remote-device state validation |
| YM2612 timed DAC profile 1 commands and status codecs | DAC playback scheduling or VGM stream expansion |
| Argument validation, batch encoding, and transaction correlation | Playback scheduling, synthesis, or ADPCM encoding |
| Audio timing and local stream-position/status tracking | |

`AudioStreamState` tracks host-side protocol state; it does not communicate with
the device. Request and response payloads borrow caller-owned data or frames.

## Framing and commands

```rust
use ndsif::{Command, Error, MessageType, Request, WriteId};

let writes = [
    ndsif::RegisterWrite::new(0x20, 0xc7),
    ndsif::RegisterWrite::new(0x08, 0x78),
];
let frame = Command::WriteRegisters {
    write_id: WriteId::YM2151,
    instance: 0,
    page: 0,
    writes: &writes,
}.to_frame(7)?;
assert_eq!(frame.message_type(), MessageType::NoResponse);
assert_eq!(frame.command_id(), 0x54);
assert_eq!(frame.transaction_id(), 7);
assert!(matches!(Request::decode(&frame)?, Request::WriteRegisters { .. }));
let wire = frame.encode();
assert_eq!(wire.as_bytes()[0], 0);
# Ok::<(), Error>(())
```

### Frame and decoder reference

| API / condition | Behavior |
|---|---|
| `Frame::encode()` | Includes both zero delimiters |
| `Frame::decode()` | Accepts the COBS body without delimiters |
| `Decoder` input | Handles split and concatenated transport reads |
| `Decoder` overflow | Discards input through the next delimiter |
| `Decoder` reset | Waits for a synchronization delimiter |
| Incomplete frame | After 500 ms of inactivity, the caller invokes `expire_partial_frame()` |

### Message reference

| API / field | Behavior |
|---|---|
| `MessageType` | Distinguishes no-response requests, response-requesting controls, and responses |
| Response correlation | Repeats the request's `command_id` and `transaction_id` |
| Command ID | Identifies the command; response status does not alter it |
| `Command::expects_response()` | Indicates whether a command requires response tracking |

## Controls and replies

```rust
use ndsif::{Command, Error, Reply, Response};

let request = Command::Ping(b"ND8").to_frame(0x1234)?;
let response = Response::for_request(&request, Reply::Ping(b"ND8"))?;
let response_frame = response.to_frame()?;
let decoded = Response::decode(&response_frame)?;
decoded.matches_request(&request)?;
assert_eq!(decoded.reply, Reply::Ping(b"ND8"));
# Ok::<(), Error>(())
```

| API / data | Behavior |
|---|---|
| Control codecs | Typed codecs for RESET, PING, GET_INFO, SET_OUTPUT_VOLUME, SET_CHIP_CLOCK, GET_CAPABILITIES and AUDIO_STATUS |
| Capability entries | Borrowing iterator over 9-byte entries; supports unknown, fixed and variable clock modes |
| Fixed clock mode | Entry provides the input frequency |
| Variable clock mode | Entry advertises clock-setting support |
| Capability validation | Rejects invalid modes, inconsistent clock metadata and duplicate pages |
| Device support | Query GET_CAPABILITIES; support is device-specific and must not be inferred from product name |
| API revision 40 | Adds the optional, capability-gated YM2612 timed DAC profile 1 |
| Profile availability | Requires control feature bit 1 and a matching `GET_DAC_CAPABILITIES` response |

## Register and stream batching

```rust
use ndsif::{BytePosition, CommandEncoder, Error, RegisterWrite, WriteId};

let writes = [RegisterWrite::new(0x20, 0xc7); 129];
let adpcm = [0x12; 501];
let mut encoded_bytes = 0;
let mut encoder = CommandEncoder::new(7, |frame: &[u8]| {
    encoded_bytes += frame.len();
});
encoder.register_burst(WriteId::YM2151, 0, 0, &writes)?;
encoder.register_event(
    0xb7, 0, BytePosition::new(0), WriteId::YM2151, 0, 0, &writes,
)?;
let end = encoder.audio_data(0xb7, 0, BytePosition::new(0), &adpcm)?;
assert_eq!(end.get(), 501);
assert_eq!(encoder.count(), 7);
assert!(encoded_bytes > adpcm.len());
# Ok::<(), Error>(())
```

| `CommandEncoder` behavior | Limit / semantics |
|---|---|
| Register BURST | Up to 127 register/value pairs per frame |
| Positioned register event | Up to 123 register/value pairs per frame |
| AUDIO_DATA | Up to 250 bytes per frame |
| Transaction IDs | Advance modulo 65536; correlate responses only, with no duplicate suppression or retransmission guarantees |
| Empty batches | Emit no frames |
| Positions | Count ADPCM bytes, not samples or ticks; never wrap |

## Timing helpers

```rust
use ndsif::{AudioTiming, Divider, Error, OkiClock};

let timing = AudioTiming::new(OkiClock::Mhz8, Divider::Div512);
assert_eq!(timing.ticks_from_bytes(160, 1_000_000)?, 20_480);
assert_eq!(timing.bytes_from_ticks(20_480, 1_000_000)?, 160);
let mut converter = timing.sample_converter(44_100)?;
assert_eq!(converter.advance(1)?, 0);
assert_eq!(converter.advance(44_099)?, 15_625);
# Ok::<(), Error>(())
```

`AudioStreamState` provides the local position and lifecycle bookkeeping shared
by transports. Playback buffering thresholds remain caller policy.

```rust
use ndsif::{AudioStreamState, AudioTiming, BytePosition, Divider, Error, OkiClock};

let mut stream = AudioStreamState::new(
    AudioTiming::new(OkiClock::Mhz4, Divider::Div1024),
    44_100,
)?;
let position = stream.position_for_frame(441, 44_100)?;
assert_eq!(position, BytePosition::new(19));
stream.record_data_accepted(BytePosition::new(0), 32)?;
assert!(stream.event_is_ready(position));
# Ok::<(), Error>(())
```
