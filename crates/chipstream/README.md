# chipstream

`chipstream` provides retro sound-chip emulation and device-independent VGM and
MDX playback for games. It is in early development; the sound engine is not yet
provided.

## APIs

| Module | API | Purpose |
| --- | --- | --- |
| `sequencer` | `VgmSequencer`, `MdxSequencer`, `ChipFactoryRegistry` | Render songs to stereo PCM with typed register-write callbacks, optional state tracking, and replaceable chip backends. |
| `chip` | `SoundChip`, chip implementations | Apply typed soundlog writes and generate native-rate samples. |
| `audio` | `ChipRoute`, `NativeBlock` | Capture native samples and chip metadata. |
| `audio` | `StereoMatrix`, `StereoBlock` | Convert native outputs to stereo. |
| `audio` | `StereoResampler`, `StereoMixer`, `StereoEffect` | Resample, mix, and process stereo blocks. |

These components can be used without an audio device or an engine. For API
details and limitations, see the rustdoc (`cargo doc -p chipstream --open`).

## Cargo features

YMFM chips are always available. Optional features are disabled by default:

| Feature | Chip |
| --- | --- |
| `c140` | C140 / ASIC219 |
| `okim6258` | OKIM6258 |
| `okim6295` | OKIM6295 |
| `pwm` | PWM |
| `segapcm` | SegaPCM |
| `sn76496` | SN76489 / SN76496 |

Enable the chips your application needs, for example:

```toml
chipstream = { version = "0.1", features = ["okim6258", "sn76496"] }
```

When a VGM header declares one of these chips without its feature,
sequencer construction returns
`VgmSequencerError::FeatureDisabled`; unsupported chips without implementations
remain silent. MDX playback with a supplied PDX requires `okim6258` even when
the song does not use PCM notes. Without a PDX, FM-only MDX playback does not
require it.

## Sequencers

Render VGM containing a YM2151:

```rust,no_run
use chipstream::sequencer::{Chip, Instance, VgmSequencer};

let bytes = std::fs::read("song.vgm")?;
let mut player = VgmSequencer::from_vgm(bytes, 48_000, 512)?;
player.set_gain(Chip::Ym2151, Instance::Primary, 0.5)?;
while let Some(block) = player.render()? {
    // Send block.frames() to the host.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Render MDX with a PDX loaded by the host (requires the `okim6258` feature):

```rust,no_run
use chipstream::sequencer::{Chip, Instance, MdxSequencer};

let mdx = std::fs::read("song.mdx")?;
let pdx = std::fs::read("song.pdx")?;
let mut player = MdxSequencer::from_mdx(mdx, Some(pdx), 48_000, 512)?;
player.set_gain(Chip::Ym2151, Instance::Primary, 0.8)?;
player.set_gain(Chip::Okim6258, Instance::Primary, 0.5)?;
while let Some(block) = player.render()? {
    // Send block.frames() to the host.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Both sequencers default to one traversal without fading. Use `PlaybackOptions`
for loop count and fade duration; MDX includes it in `MdxSequencerOptions`:

```rust,no_run
use std::time::Duration;
use chipstream::sequencer::{MdxSequencer, MdxSequencerOptions, PlaybackOptions, VgmSequencer};
use soundlog::mdx::package::MdxPackage;

let playback = PlaybackOptions { loop_count: Some(2), fadeout: Some(Duration::from_secs(2)) };
let mut vgm = VgmSequencer::from_vgm(std::fs::read("song.vgm")?, 48_000, 512)?;
vgm.set_playback_options(playback)?;
let package = MdxPackage::parse_owned(std::fs::read("song.mdx")?, None)?;
let mdx = MdxSequencer::with_options(package, 48_000, 512, MdxSequencerOptions {
    playback,
    ..Default::default()
})?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`None` as the loop count allows unlimited looping. For indefinite MDX playback
also set `max_ticks` to `None`; other resource limits remain active. PDX lookup
is the caller's responsibility, and omitting PDX disables the PCM path even
when the MDX names a PDX.

`measure_vgm_duration_frames` and `measure_mdx_duration_frames` take the same
playback settings and return `PlaybackDuration::Finite`, `Unbounded`, or
`Unknown`. Scans are bounded; unlimited MDX loops that do not end within the
scan limit are `Unknown`, not assumed unbounded. Durations exceeding `u32::MAX`
output frames are also `Unknown`; playback is not limited by this measurement.

`set_gain` applies a linear gain to a chip instance and returns an error if it
is absent. `VgmSequencer::set_opn_gains` independently adjusts the FM/ADPCM and
SSG outputs of YM2203, YM2608, and YM2610(B), multiplying the chip gain. Its
changes apply to newly captured samples; already buffered audio and resampler
history retain their previous levels. Both sequencers expose `chip_instances`
and support typed write callbacks. Register callbacks
with `on_write` and state tracking with `track_state` before rendering:

```rust,no_run
use chipstream::sequencer::{Instance, StateEvent, VgmSequencer};
use soundlog::chip::{Ym2151Spec, state::Ym2151State};

let bytes = std::fs::read("song.vgm")?;
let mut player = VgmSequencer::from_vgm(bytes, 48_000, 512)?;
player.track_state::<Ym2151State>(Instance::Primary, 3_579_545.0);
player.on_write(|instance, spec: Ym2151Spec, sample, events| {
    if let Some(events) = events {
        for event in events {
            if let StateEvent::KeyOn { channel, tone } = event {
                println!("{instance:?} sample {sample}: channel {channel} at {:?}", tone.freq_hz);
            }
        }
    }
});
while let Some(block) = player.render()? {
    // Send block.frames() to the host.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Callbacks receive the instance, a chip-specific write specification, the
absolute output-frame position at the sequencer's sample rate, and optional
state changes. They run synchronously during `render`; rendering may allocate
and is not guaranteed real-time safe. Output is not clipped.
SN76489 state tracking uses the VGM header's clock-divider and zero-period
settings. `track_state_with_voice_clocks` also accounts for per-instance
extra-header clock overrides.

Both sequencers also provide `filter_write` to transform a typed register write
before it reaches the chip, including external chip backends:

```rust,no_run
use chipstream::sequencer::{Instance, VgmSequencer};
use soundlog::chip::Ym2151Spec;

let bytes = std::fs::read("song.vgm")?;
let mut player = VgmSequencer::from_vgm(bytes, 48_000, 512)?;
player.filter_write(|instance, mut write: Ym2151Spec, _frame| {
    if instance == Instance::Primary && write.register == 0x20 {
        write.value &= 0x3f;
    }
    write
});
while let Some(block) = player.render()? {
    // Send block.frames() to the host.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

A filter returns one write of the same type. Registering another filter for
that type replaces it; filters and observers are independent. `on_write` and
state trackers see original writes. Filters run when a write is applied and
may execute ahead of returned audio, without global ordering across voices.
They cannot change already generated audio or queued hardware events. Filter
closure state is not part of sequencer checkpoints: callers must restore it
before restoring or seeking, and seeking calls the filter while advancing.

## Chips

Use soundlog's chip-specific `*Spec` values with `SoundChip::write`:

```rust
use chipstream::chip::{SoundChip, YmfmChip};
use soundlog::chip::{Chip, Ym2151Spec};

let mut chip = YmfmChip::new(Chip::Ym2151, 4_000_000)?;
chip.write(Ym2151Spec { register: 0x20, value: 0xc7 })?;
let mut samples = vec![0_i32; chip.channels() * 256];
chip.generate(&mut samples)?;
# Ok::<(), chipstream::chip::ChipError>(())
```

The `chip` module provides `YmfmChip` and, with their respective features,
`Okim6258`, `Okim6258Stream`, `Okim6295`, `Sn76496`, `SegaPcm`, `Pwm`, and
`C140`.

`Okim6258` models direct writes with a single-byte latch. VGM DAC streams and
the `chipstream-ndsif` software fallback use
[`Okim6258Stream`](src/chip/okim6258_stream.rs) for ordered byte delivery.
VGM software playback applies a 20 Hz high-pass filter to the decoded output
to reduce DC-related clicks; direct chip use and MDX playback are unchanged.
The same mono filter is available independently as `audio::DcBlocker`.

See its API documentation for queue, underrun, and stop behavior.
`Okim6258Adpcm` provides the shared 12-bit predictor and encoder separately
from chip registers and stream delivery.

### External chip backends

Implement `SoundChip` and register a factory with `ChipFactoryRegistry` to
replace a built-in backend. Factories are selected independently by chip type;
unregistered types keep using their built-in implementations. Pass the
registry to `VgmSequencer::from_vgm_with_factories`,
`VgmSequencer::new_with_factories`, or the corresponding
`MdxSequencer` constructor.

| Factory configuration | Description |
|---|---|
| `chip()` | Chip type selected by the VGM/MDX stream |
| `instance()` | Primary or secondary instance |
| `clock()` | Effective input clock in Hz |
| `output_rate()` | Sequencer output rate in frames per second |
| `header()` | Parsed VGM header, or synthesized MDX header |
| `extra_header()` | Optional VGM extra header |

`register_optional` can select the built-in backend for individual instances
by returning `Ok(None)`; errors are propagated rather than triggering fallback.
`register_fallback` can add a backend for instances declined by an existing
factory. AY8910 has no default backend, so a declined instance is an error.
To opt into YM2149 approximation, register a factory using
`YmfmChip::new_ym2149(clock, divide_clock)` for `Chip::Ay8910`; callers
must interpret and validate the VGM AY type and flags. The three YM2149 outputs
are mixed as AY channels, including VGM per-channel stereo masks.

YMFM-backed chip types can be replaced individually:

| Chip type | Native outputs per frame |
|---|---:|
| `Ym2413` | 2 |
| `Ym2151` | 2 |
| `Ym2203` | 4 |
| `Ym2608` | 3 |
| `Ym2610b` | 3 |
| `Ym2612` | 2 |
| `Ym3526` | 1 |
| `Y8950` | 1 |
| `Ym3812` | 1 |
| `Ymf262` | 4 |
| `Ymf278b` | 6 |

The returned backend must report the configured chip type and clock and match
the native output count above. Its `generate()` method still supplies samples
to the sequencer; routing register writes to physical hardware does not by
itself route that hardware's audio into the host PCM output. Implement
`save_state()` and `restore_state()` to support checkpoint seeking. Backends
that schedule timed output expose `ScheduledChip` through `SoundChip::scheduled`;
they must also opt into `supports_checkpoint` when their scheduled state can
be restored. DAC-interrupting backends cannot currently use checkpoints. Backend
errors can be returned as `ChipError::External`.

The VGM sequencer applies backend output balancing to built-in and custom
backends alike: SN76489 is multiplied by 0.5, YM2203 SSG by 0.25, and
YM2608/YM2610(B) SSG by 0.375. When the VGM declares a YM2612, other chips
are additionally multiplied by 2/3 while the YM2612 level remains unchanged.
VGM header volume settings and `set_gain` multiply these levels.

### NDSIF scheduled output

Use `chipstream-ndsif` for hardware-backed chip adapters and routing. Hardware
audio is emitted by the physical device and must be mixed separately from
host PCM. See the `chipstream-ndsif` README for supported chips and examples.

## Audio processing

Mix YM2203's four native outputs into stereo, then resample and add them to a
stereo bus. In this example, FM is centered and SSG channels A and B are
panned left and right:

```rust
use chipstream::audio::{ChipRoute, StereoBlock, StereoMatrix, StereoMixer, StereoResampler};
use chipstream::chip::{SampleRate, SoundChip, YmfmChip};
use soundlog::chip::{Chip, Ym2203Spec};

let chip = YmfmChip::new(Chip::Ym2203, 4_000_000)?;
let native_rate = chip.sample_rate();
let mut route = ChipRoute::new(chip);
for (register, value) in [(0, 0x24), (1, 0x01), (2, 0x90), (3, 0x01),
                          (7, 0x3c), (8, 0x0f), (9, 0x0f)] {
    route.chip_mut().write(Ym2203Spec { register, value })?;
}
let scale = 1.0 / 32768.0;
let matrix = StereoMatrix::new(vec![
    [scale, scale], // FM
    [scale, 0.0],   // SSG A
    [0.0, scale],   // SSG B
    [scale, scale], // SSG C
])?;
let mut src = StereoResampler::new(48_000, native_rate, native_rate, 4096)?;

let mut samples = [0; 4 * 4096];
let native = route.capture(&mut samples)?;
# assert!(native.samples().chunks_exact(4).any(|frame| frame[1] != 0 && frame[2] != 0));
let stereo = matrix.convert(&native)?;
# assert!(stereo.frames().iter().any(|frame| frame[0] != frame[1]));
let converted = src.process(&stereo)?;
let tail = src.finish()?; // Drain once at the end of the stream.
# assert!(converted.frames().iter().chain(tail.frames()).any(|frame| frame[0] != 0.0 && frame[1] != 0.0));
for block in [converted, tail] {
    let mut bus = StereoBlock::silence(SampleRate::from_hz(48_000), block.frames().len())?;
    StereoMixer::mix(&mut bus, &block, 1.0)?;
    // Send bus.frames() to the host.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The coefficients control routing and scaling; mixing does not clip. Mix
additional stereo sources into the bus after aligning their rates and frame
counts. `Gain` and `StereoBalance` provide additional processing.

## License

BSD-3-Clause License

## Third-party notices

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for implementation
attributions and licenses. Distributors should also review the licenses of
transitive dependencies.
