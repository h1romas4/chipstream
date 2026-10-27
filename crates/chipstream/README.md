# chipstream

`chipstream` provides retro sound-chip emulation and device-independent VGM and
MDX playback for games. It is in early development; the sound engine is not yet
provided.

## APIs

| Module | API | Purpose |
| --- | --- | --- |
| `sequencer` | `VgmSequencer`, `MdxSequencer` | Render songs to stereo PCM, optionally reporting register and state events. |
| `chip` | `SoundChip`, chip implementations | Apply typed soundlog writes and generate native-rate samples. |
| `audio` | `ChipRoute`, `NativeBlock` | Capture native samples and chip metadata. |
| `audio` | `StereoMatrix`, `StereoBlock` | Convert native outputs to stereo. |
| `audio` | `StereoResampler`, `StereoMixer`, `StereoEffect` | Resample, mix, and process stereo blocks. |

These components can be used without an audio device or an engine. For API
details and limitations, see the rustdoc (`cargo doc -p chipstream --open`).

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

Render MDX with an optional PDX loaded by the host:

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

`set_gain` applies a linear gain to a chip instance and returns an error if it
is absent. Both sequencers also support optional write and state events. Their
output is not clipped, and rendering may allocate.

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

The `chip` module provides `YmfmChip`, `Okim6258`, `Okim6295`, `Sn76496`,
`SegaPcm`, `Pwm`, and `C140`.

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

## Third-party notices

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for implementation
attributions and licenses. Distributors should also review the licenses of
transitive dependencies.
