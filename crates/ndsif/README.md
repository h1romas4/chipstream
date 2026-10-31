# ndsif

Allocation-free `no_std` Rust codec for the NDSIF v1 draft specification.
The v1 draft is not wire-compatible with the previous
b protocol even though both use wire version `01`; select the profile
explicitly at the transport boundary.
This codec implements API specification revision 39; the frame wire version
remains `01`.

## Scope

- COBS framing, CRC-16/CCITT-FALSE, bounded frame buffers and incremental
  decoding.
- Generic VGM register-write requests, control requests, capabilities,
  MSM6258-compatible stream requests, typed responses and transaction
  correlation.
- Local argument and message-shape validation, allocation-free batch encoding,
  and exact audio tick/sample conversion.

The crate does not perform serial I/O, device-state validation, transport
retries, playback scheduling, synthesis, ADPCM encoding or capability
discovery. Request and response payloads borrow from caller-owned data/frames.

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

`Frame::encode()` includes both zero delimiters; `Frame::decode()` accepts
only the COBS body without delimiters. `Decoder` handles split and concatenated
transport reads, discards through the next delimiter after overflow, and waits
for a synchronization delimiter after reset. The caller measures 500 ms of
inactivity and calls `expire_partial_frame()` for an incomplete frame.

`MessageType` distinguishes no-response requests, response-requesting controls,
and responses. The response repeats the request's `command_id` and
`transaction_id`; command IDs are never changed to encode response status.
`Command::expects_response()` exposes which commands need response tracking.

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

The crate provides typed codecs for RESET, PING, GET_INFO, SET_OUTPUT_VOLUME,
SET_CHIP_CLOCK, GET_CAPABILITIES and AUDIO_STATUS. Capability pages expose
their 9-byte entries as a borrowing iterator, including unknown, fixed and
variable logical clock modes. Fixed entries provide their input frequency;
variable entries advertise clock-setting support. Invalid modes, inconsistent
clock metadata and duplicate pages are rejected. Device and capability support
remain device-specific and must be read from GET_CAPABILITIES rather than
inferred from a product name. API revision 39 is not wire-compatible with the
previous 4-byte capability-entry format.

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

`CommandEncoder` splits register BURSTs at 127 pairs, positioned register
events at 123 pairs, and AUDIO_DATA at 250 bytes. It advances transaction IDs
modulo 65536. These IDs correlate responses; they do not provide duplicate
suppression or retransmission guarantees. Empty batch inputs emit nothing.
Positions count ADPCM bytes, not samples or ticks, and never wrap.

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
