# ndsif

NDSIF is a retro sound-chip playback interface developed by the NanoDrive
Project.

This crate provides an allocation-free `no_std` Rust codec
implementing the NDSIF v1 for both host and guest implementations. It does
not communicate with devices; use
[`ndsif-transport`](https://crates.io/crates/ndsif-transport) for serial
access to physical NanoDrive hardware.

## NDSIF overview

NDSIF defines messages between a host and NanoDrive firmware for controlling
sound-chip register writes and device-side audio playback. The protocol is
transport-independent; [`ndsif-transport`](https://crates.io/crates/ndsif-transport)
provides serial communication.

Each frame carries a message type, command ID, transaction ID, payload length,
payload, and CRC-16 checksum. On the wire, the frame is COBS-encoded and
surrounded by zero delimiters. The decoded payload is limited to 256 bytes.
Requests that expect a reply are correlated by their command and transaction
IDs; no-response commands do not require a reply. This correlation does not
provide retransmission or duplicate suppression.

The command set includes device information and capability queries, immediate
register writes, and ADPCM streaming with events positioned by byte offset.
Hosts should use the device's advertised capabilities to select supported chip
instances, pages, clocks, and stream limits rather than infer support from the
product name. The YM2612 timed DAC profile is an optional extension and must be
checked through capability discovery before use.

## Supported Devices

| Device | Repository |
|---|---|
| NanoDrive8 | [Fujix1/NanoDrive8](https://github.com/Fujix1/NanoDrive8) |
| NanoDrive7 | [Fujix1/NanoDrive7](https://github.com/Fujix1/NanoDrive7) |
| NanoDrive6 | [Fujix1/NanoDrive6](https://github.com/Fujix1/NanoDrive6) |

## Scope

| Provides | Not provided |
|---|---|
| `no_std`, allocation-free framing, CRC, and incremental decoding | Serial I/O, retries, or capability discovery |
| Typed commands, responses, capabilities, and audio-stream messages | Remote-device state validation |
| YM2612 timed DAC profile 1 commands and status codecs | DAC playback scheduling or VGM stream expansion |
| Draft live DAC profile 2 and ordered live profile 3 codecs | Live clock conversion, pacing, or device acceptance guarantees |
| Argument validation, batch encoding, and transaction correlation | Playback scheduling, synthesis, or ADPCM encoding |
| Audio timing and local stream-position/status tracking | |

For serial I/O, device capability discovery, chipstream integration, and
scheduled playback on supported NanoDrive devices, use
[`ndsif-transport`](https://crates.io/crates/ndsif-transport). It builds on this crate's
protocol codecs and provides the transport and scheduling layer.

`AudioStreamState` tracks host-side protocol state; it does not communicate with
the device. Request and response payloads borrow caller-owned data or frames.

## Draft live profiles

The optional live profiles are independent of timed DAC profile 1. Discover
them separately using `CONTROL_FEATURE_LIVE_DAC` and
`CONTROL_FEATURE_ORDERED_LIVE`. The stable `SPEC_REVISION` remains 40; these
live profiles are draft extensions. Profile 2 uses revision 1. Profile 3
revision 4 supports YM2612 writes, while revision 5 additionally supports
SN76489 PSG writes.

`LiveDacRecord` and `OrderedLiveRecord` store waits in 44,100 Hz ticks.
Profile 2 waits refer to the preceding DAC value; profile 3 waits refer to the
preceding ordered write of any kind. The first wait is zero. Zero waits and
repeated values must be retained. The caller converts source timestamps,
checks their ordering and overflow, obeys device capability limits, and uses
RESET before switching profiles.

`LiveDacData` and `OrderedLiveData` have no response. A successfully encoded or
sent frame is not proof that the device accepted or played it. Profile 2 may
drop an entire frame if its FIFO is full; profile 3 may apply transport
backpressure instead. An empty FIFO is not an underflow fault. Profile 3
sequence numbers start at zero and wrap at `u32::MAX`. Its optional status is
a diagnostic snapshot, not a physical playback-completion fence. Immediate
FM writes are not ordered relative to profile 2 DAC writes; profile 3 keeps
all supported writes in a single ordered timeline.

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

For a changing clock or divider, `AudioPositionClock` preserves the fractional
byte position when creating a new timeline segment:

```rust
use ndsif::{AudioPositionClock, AudioTiming, Divider, Error, OkiClock};

let initial = AudioPositionClock::new(
    AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100,
)?;
let position = initial.position_for_frame(4410, 44_100)?;
assert_eq!(position.get(), 195);
let changed = initial.change_at(
    4410, 44_100, position, AudioTiming::new(OkiClock::Mhz8, Divider::Div768),
)?;
assert_eq!(changed.position_for_frame(8820, 44_100)?.get(), 716);
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

Use this crate on either side of an NDSIF connection to encode and decode
protocol messages. Transport I/O and device-specific state management are left
to the application.

## Examples

Run the loopback example to exercise fragmented frame decoding and request/
response correlation without a device:

```sh
cargo run -p ndsif --example loopback
```

## License

BSD-3-Clause License
