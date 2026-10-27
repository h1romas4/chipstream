use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chipstream::audio::StereoBlock;
use chipstream::sequencer::{
    Chip, Instance, MdxSequencer, MdxSequencerOptions, VgmSequencer, VgmSequencerError,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::input::Input;

const CHUNK_FRAMES: usize = 1024;
const QUEUE_BLOCKS: usize = 8;
const PREFILL_BLOCKS: usize = 4;

#[derive(Clone)]
pub struct Options {
    pub loop_count: Option<u32>,
    pub fadeout_samples: Option<u32>,
    pub loop_modifier: Option<u8>,
    pub loop_base: Option<i8>,
    pub compat_vgm_pwm: Option<bool>,
    pub ym2608_rhythm_rom: Option<Vec<u8>>,
}

enum Message {
    Frames(Vec<[f32; 2]>),
    End,
    Error(String),
}

enum PlaybackEvent {
    Finished(Instant),
    Error(String),
}

impl Options {
    pub(super) fn validate(&self, input: &Input) -> Result<()> {
        if matches!(input, Input::Mdx(_))
            && (self.loop_modifier.is_some()
                || self.loop_base.is_some()
                || self.compat_vgm_pwm.is_some())
        {
            bail!("--loop-modifier, --loop-base and --compat-vgm-pwm apply only to VGM/VGZ");
        }
        if matches!(input, Input::Mdx(_)) && self.ym2608_rhythm_rom.is_some() {
            bail!("--ym2608-rhythm-rom applies only to VGM/VGZ");
        }
        if self.ym2608_rhythm_rom.as_ref().is_some_and(Vec::is_empty) {
            bail!("YM2608 rhythm ROM is empty");
        }
        Ok(())
    }
}

pub(super) enum Player {
    Vgm(VgmSequencer),
    Mdx(MdxSequencer),
}

impl Player {
    pub(super) fn new(input: Input, rate: u32, options: &Options) -> Result<Self> {
        options.validate(&input)?;
        Ok(match input {
            Input::Vgm(data) => {
                let mut sequencer = VgmSequencer::from_vgm(data, rate, CHUNK_FRAMES)
                    .context("failed to create VGM sequencer")?;
                if let Some(rom) = &options.ym2608_rhythm_rom {
                    sequencer
                        .set_ym2608_rhythm_rom(Instance::Primary, rom)
                        .context("failed to configure primary YM2608 rhythm ROM")?;
                    match sequencer.set_ym2608_rhythm_rom(Instance::Secondary, rom) {
                        Ok(())
                        | Err(VgmSequencerError::ChipNotFound {
                            chip: Chip::Ym2608,
                            instance: Instance::Secondary,
                        }) => {}
                        Err(error) => {
                            return Err(error)
                                .context("failed to configure secondary YM2608 rhythm ROM");
                        }
                    }
                }
                sequencer.set_pwm_compatibility(options.compat_vgm_pwm.unwrap_or(true))?;
                sequencer.set_loop_count(options.loop_count);
                sequencer.set_fadeout_samples(options.fadeout_samples)?;
                if let Some(modifier) = options.loop_modifier {
                    sequencer.set_loop_modifier(modifier);
                }
                if let Some(base) = options.loop_base {
                    sequencer.set_loop_base(base);
                }
                Self::Vgm(sequencer)
            }
            Input::Mdx(package) => {
                let settings = MdxSequencerOptions {
                    loop_count: options.loop_count,
                    max_ticks: None,
                    ..MdxSequencerOptions::default()
                };
                Self::Mdx(
                    MdxSequencer::with_options_and_fadeout(
                        *package,
                        rate,
                        CHUNK_FRAMES,
                        settings,
                        options.fadeout_samples,
                    )
                    .context("failed to create MDX sequencer")?,
                )
            }
        })
    }

    pub(super) fn render(&mut self) -> Result<Option<StereoBlock>> {
        match self {
            Self::Vgm(player) => player.render().context("VGM rendering failed"),
            Self::Mdx(player) => player.render().context("MDX rendering failed"),
        }
    }
}

fn produce(
    data: impl Into<Input>,
    rate: u32,
    options: Options,
    sender: &SyncSender<Message>,
) -> Result<()> {
    let mut player = Player::new(data.into(), rate, &options)?;
    loop {
        let Some(block) = player.render()? else {
            break;
        };
        if sender
            .send(Message::Frames(block.frames().to_vec()))
            .is_err()
        {
            return Ok(());
        }
    }
    let _ = sender.send(Message::End);
    Ok(())
}

struct Consumer {
    receiver: Receiver<Message>,
    queued: VecDeque<Message>,
    frames: Vec<[f32; 2]>,
    position: usize,
    terminal: bool,
}

enum FillResult {
    Continue,
    End(usize),
    Error(String),
}

impl Consumer {
    fn fill<T: SizedSample + FromSample<f32>>(
        &mut self,
        output: &mut [T],
        channels: usize,
    ) -> FillResult {
        output.fill(T::from_sample(0.0));
        if self.terminal {
            return FillResult::Continue;
        }
        if !output.len().is_multiple_of(channels) {
            self.terminal = true;
            return FillResult::Error("audio callback returned a partial frame".into());
        }
        for (frame_index, output_frame) in output.chunks_mut(channels).enumerate() {
            while self.position == self.frames.len() {
                let message = match self.queued.pop_front() {
                    Some(message) => Ok(message),
                    None => self.receiver.try_recv(),
                };
                match message {
                    Ok(Message::Frames(frames)) if !frames.is_empty() => {
                        self.frames = frames;
                        self.position = 0;
                    }
                    Ok(Message::Frames(_)) => {
                        self.terminal = true;
                        return FillResult::Error("sequencer returned an empty PCM block".into());
                    }
                    Ok(Message::End) => {
                        self.terminal = true;
                        return FillResult::End(frame_index);
                    }
                    Ok(Message::Error(error)) => {
                        self.terminal = true;
                        return FillResult::Error(error);
                    }
                    Err(TryRecvError::Empty) => {
                        self.terminal = true;
                        return FillResult::Error("audio buffer underrun while rendering".into());
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.terminal = true;
                        return FillResult::Error("rendering thread stopped unexpectedly".into());
                    }
                }
            }
            let [left, right] = self.frames[self.position];
            self.position += 1;
            if !left.is_finite() || !right.is_finite() {
                self.terminal = true;
                return FillResult::Error("sequencer produced a non-finite PCM sample".into());
            }
            let clip = |sample: f32| sample.clamp(-1.0, 1.0 - f32::EPSILON);
            if channels == 1 {
                output_frame[0] = T::from_sample(clip(left * 0.5 + right * 0.5));
            } else {
                output_frame[0] = T::from_sample(clip(left));
                output_frame[1] = T::from_sample(clip(right));
            }
        }
        FillResult::Continue
    }
}

fn build_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut consumer: Consumer,
    sender: mpsc::Sender<PlaybackEvent>,
) -> Result<cpal::Stream> {
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let errors = sender.clone();
    device
        .build_output_stream(
            *config,
            move |output: &mut [T], info: &cpal::OutputCallbackInfo| match consumer
                .fill(output, channels)
            {
                FillResult::Continue => {}
                FillResult::Error(error) => {
                    let _ = sender.send(PlaybackEvent::Error(error));
                }
                FillResult::End(frames) => {
                    let timestamp = info.timestamp();
                    let latency = timestamp
                        .playback
                        .saturating_duration_since(timestamp.callback);
                    let duration = Duration::from_secs_f64(frames as f64 / f64::from(rate));
                    let deadline = Instant::now() + latency + duration;
                    let _ = sender.send(PlaybackEvent::Finished(deadline));
                }
            },
            move |error| {
                let _ = errors.send(PlaybackEvent::Error(format!(
                    "audio output failed: {error}"
                )));
            },
            None,
        )
        .context("failed to create audio output stream")
}

fn run(
    device: cpal::Device,
    supported: cpal::SupportedStreamConfig,
    receiver: Receiver<Message>,
) -> Result<()> {
    let config = supported.config();
    if config.channels == 0 || config.sample_rate == 0 {
        bail!("audio device returned an invalid output configuration");
    }
    let mut queued = VecDeque::new();
    for _ in 0..PREFILL_BLOCKS {
        let message = receiver
            .recv()
            .context("rendering thread stopped before playback")?;
        match message {
            Message::Error(error) => bail!("{error}"),
            Message::End => {
                queued.push_back(Message::End);
                break;
            }
            Message::Frames(frames) => queued.push_back(Message::Frames(frames)),
        }
    }
    let consumer = Consumer {
        receiver,
        queued,
        frames: Vec::new(),
        position: 0,
        terminal: false,
    };
    let (sender, events) = mpsc::channel();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, consumer, sender),
        SampleFormat::F64 => build_stream::<f64>(&device, &config, consumer, sender),
        SampleFormat::I8 => build_stream::<i8>(&device, &config, consumer, sender),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, consumer, sender),
        SampleFormat::I24 => build_stream::<cpal::I24>(&device, &config, consumer, sender),
        SampleFormat::I32 => build_stream::<i32>(&device, &config, consumer, sender),
        SampleFormat::I64 => build_stream::<i64>(&device, &config, consumer, sender),
        SampleFormat::U8 => build_stream::<u8>(&device, &config, consumer, sender),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, consumer, sender),
        SampleFormat::U24 => build_stream::<cpal::U24>(&device, &config, consumer, sender),
        SampleFormat::U32 => build_stream::<u32>(&device, &config, consumer, sender),
        SampleFormat::U64 => build_stream::<u64>(&device, &config, consumer, sender),
        format => bail!("unsupported audio sample format: {format:?}"),
    }?;
    stream.play().context("failed to start audio output")?;
    let deadline = match events.recv().context("audio output stopped unexpectedly")? {
        PlaybackEvent::Finished(deadline) => deadline,
        PlaybackEvent::Error(error) => bail!("{error}"),
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        match events.recv_timeout(remaining) {
            Ok(PlaybackEvent::Error(error)) => bail!("{error}"),
            Ok(PlaybackEvent::Finished(_)) => bail!("audio output reported completion twice"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("audio output stopped while draining")
            }
        }
    }
    drop(stream);
    Ok(())
}

fn select_output_config(
    default: cpal::SupportedStreamConfig,
    ranges: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
    rate: u32,
) -> Result<cpal::SupportedStreamConfig> {
    if rate == 0 {
        bail!("sample rate must be greater than zero");
    }
    let mut candidates = ranges
        .into_iter()
        .filter_map(|range| range.try_with_sample_rate(rate))
        .filter(|config| config.channels() != 0 && supported_sample_format(config.sample_format()))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|config| {
        (
            config.channels() != default.channels(),
            config.sample_format() != default.sample_format(),
        )
    });
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("audio output device does not support sample rate {rate} Hz"))
}

fn supported_sample_format(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::F64
            | SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U24
            | SampleFormat::U32
            | SampleFormat::U64
    )
}

pub fn play(data: Input, options: Options, sample_rate: Option<u32>) -> Result<()> {
    options.validate(&data)?;
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("no default audio output device"))?;
    let default = device
        .default_output_config()
        .context("failed to obtain default audio output configuration")?;
    let supported = if let Some(rate) = sample_rate.filter(|rate| *rate != default.sample_rate()) {
        let ranges = device
            .supported_output_configs()
            .context("failed to enumerate audio output configurations")?;
        select_output_config(default, ranges, rate)?
    } else {
        default
    };
    let rate = supported.sample_rate();
    let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
    let worker = thread::spawn(move || {
        if let Err(error) = produce(data, rate, options, &sender) {
            let _ = sender.send(Message::Error(format!("{error:#}")));
        }
    });
    let result = run(device, supported, receiver);
    if worker.join().is_err() {
        return Err(anyhow!("rendering thread panicked"));
    }
    result
}

#[cfg(test)]
mod playback_tests {
    use super::*;
    use soundlog::vgm::VgmBuilder;
    use soundlog::vgm::command::WaitSamples;

    fn consumer(messages: Vec<Message>) -> Consumer {
        let (sender, receiver) = mpsc::channel();
        for message in messages {
            sender.send(message).unwrap();
        }
        drop(sender);
        Consumer {
            receiver,
            queued: VecDeque::new(),
            frames: Vec::new(),
            position: 0,
            terminal: false,
        }
    }

    #[test]
    fn stereo_mono_surround_and_eof() {
        let frames = vec![[0.25, -0.5], [1.5, -2.0]];
        let mut output = [0.0; 9];
        let mut player = consumer(vec![Message::Frames(frames.clone()), Message::End]);
        assert!(matches!(player.fill(&mut output, 3), FillResult::End(2)));
        assert_eq!(
            output,
            [
                0.25,
                -0.5,
                0.0,
                1.0 - f32::EPSILON,
                -1.0,
                0.0,
                0.0,
                0.0,
                0.0
            ]
        );
        let mut mono = [0.0; 2];
        let mut player = consumer(vec![Message::Frames(frames), Message::End]);
        assert!(matches!(player.fill(&mut mono, 1), FillResult::Continue));
        assert_eq!(mono, [-0.125, -0.25]);
        assert!(matches!(player.fill(&mut mono, 1), FillResult::End(0)));
    }

    #[test]
    fn underrun_disconnect_and_nonfinite_are_errors() {
        let (sender, receiver) = mpsc::channel();
        let mut player = Consumer {
            receiver,
            queued: VecDeque::new(),
            frames: Vec::new(),
            position: 0,
            terminal: false,
        };
        assert!(matches!(
            player.fill(&mut [0.0_f32; 2], 2),
            FillResult::Error(_)
        ));
        drop(sender);
        let mut player = consumer(vec![]);
        assert!(matches!(
            player.fill(&mut [0.0_f32; 2], 2),
            FillResult::Error(_)
        ));
        let mut player = consumer(vec![Message::Frames(vec![[f32::NAN, 0.0]])]);
        assert!(matches!(
            player.fill(&mut [0.0_f32; 2], 2),
            FillResult::Error(_)
        ));
    }

    fn looping_vgm() -> Vec<u8> {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(100));
        builder.set_loop_index(1);
        builder.add_vgm_command(WaitSamples(300));
        builder.finalize().into()
    }

    #[test]
    fn ym2608_rhythm_rom_reaches_each_vgm_chip_and_errors_are_reported() {
        use soundlog::chip::Ym2608Spec;

        let rom = vec![0x77; 8192];
        for instance in [Instance::Primary, Instance::Secondary] {
            let mut builder = VgmBuilder::new();
            builder.register_chip(Chip::Ym2608, Instance::Primary, 8_000_000);
            if instance == Instance::Secondary {
                builder.register_chip(Chip::Ym2608, Instance::Secondary, 8_000_000);
            }
            for (register, value) in [(0x11, 0x3f), (0x18, 0xdf), (0x10, 0x01)] {
                builder.add_chip_write(
                    instance,
                    Ym2608Spec {
                        port: 0,
                        register,
                        value,
                    },
                );
            }
            builder.add_vgm_command(WaitSamples(4410));
            let data: Vec<u8> = builder.finalize().into();
            let mut reference = VgmSequencer::from_bytes(&data, 44_100, CHUNK_FRAMES).unwrap();
            reference.set_ym2608_rhythm_rom(instance, &rom).unwrap();
            let mut expected = Vec::new();
            while let Some(block) = reference.render().unwrap() {
                expected.extend_from_slice(block.frames());
            }
            assert!(expected.iter().flatten().any(|sample| sample.abs() > 0.001));
            let options = Options {
                loop_count: Some(1),
                loop_modifier: None,
                loop_base: None,
                compat_vgm_pwm: None,
                ym2608_rhythm_rom: Some(rom.clone()),
                fadeout_samples: None,
            };
            let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
            produce(data, 44_100, options, &sender).unwrap();
            let mut actual = Vec::new();
            loop {
                match receiver.recv().unwrap() {
                    Message::Frames(frames) => actual.extend(frames),
                    Message::End => break,
                    Message::Error(error) => panic!("{error}"),
                }
            }
            assert_eq!(actual, expected);
        }
        let options = Options {
            loop_count: Some(1),
            loop_modifier: None,
            loop_base: None,
            compat_vgm_pwm: None,
            ym2608_rhythm_rom: Some(rom),
            fadeout_samples: None,
        };
        let (sender, _) = mpsc::sync_channel(1);
        let error = produce(looping_vgm(), 44_100, options.clone(), &sender).unwrap_err();
        assert!(format!("{error:#}").contains("primary YM2608"));
        let mdx = soundlog::mdx::package::MdxPackage::parse(
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
            None,
        )
        .unwrap();
        assert!(produce(Input::Mdx(Box::new(mdx)), 44_100, options, &sender).is_err());
    }

    #[test]
    fn pwm_compatibility_option_controls_rendered_output() {
        let mut builder = VgmBuilder::new();
        builder.register_chip(
            soundlog::chip::Chip::Pwm,
            soundlog::vgm::command::Instance::Primary,
            44_100,
        );
        builder.add_chip_write(
            soundlog::vgm::command::Instance::Primary,
            soundlog::chip::PwmSpec {
                register: 1,
                value: 3,
            },
        );
        for _ in 0..31 {
            builder.add_chip_write(
                soundlog::vgm::command::Instance::Primary,
                soundlog::chip::PwmSpec {
                    register: 4,
                    value: 0x100,
                },
            );
            builder.add_vgm_command(WaitSamples(2));
        }
        let data: Vec<u8> = builder.finalize().into();
        for disabled in [false, true] {
            let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
            produce(
                data.clone(),
                44_100,
                Options {
                    loop_count: Some(1),
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: Some(!disabled),
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                &sender,
            )
            .unwrap();
            let mut audible = false;
            loop {
                match receiver.recv().unwrap() {
                    Message::Frames(frames) => {
                        audible |= frames.iter().flatten().any(|sample| sample.abs() > 0.001)
                    }
                    Message::End => break,
                    Message::Error(error) => panic!("{error}"),
                }
            }
            assert_eq!(audible, !disabled);
        }
    }

    #[test]
    fn producer_applies_loop_options() {
        for (options, expected) in [
            (
                Options {
                    loop_count: Some(0),
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                400,
            ),
            (
                Options {
                    loop_count: Some(2),
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                700,
            ),
            (
                Options {
                    loop_count: Some(2),
                    loop_modifier: Some(0x20),
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                1300,
            ),
            (
                Options {
                    loop_count: Some(2),
                    loop_modifier: None,
                    loop_base: Some(1),
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                400,
            ),
        ] {
            let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
            produce(looping_vgm(), 44_100, options, &sender).unwrap();
            let mut frames = 0;
            loop {
                match receiver.recv().unwrap() {
                    Message::Frames(block) => frames += block.len(),
                    Message::End => break,
                    Message::Error(error) => panic!("{error}"),
                }
            }
            assert_eq!(frames, expected);
        }
    }

    #[test]
    fn infinite_producer_stops_when_consumer_is_dropped() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            produce(
                looping_vgm(),
                44_100,
                Options {
                    loop_count: None,
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                &sender,
            )
        });
        for _ in 0..3 {
            assert!(matches!(receiver.recv().unwrap(), Message::Frames(_)));
        }
        drop(receiver);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn producer_preserves_sequencer_pcm_and_reports_parse_errors() {
        let data = include_bytes!("../../chipstream/tests/fixtures/ym2151_a4.vgm").to_vec();
        let mut reference = VgmSequencer::from_bytes(&data, 48_000, CHUNK_FRAMES).unwrap();
        let mut expected = Vec::new();
        while let Some(block) = reference.render().unwrap() {
            expected.extend_from_slice(block.frames());
        }
        let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
        let worker = thread::spawn(move || {
            produce(
                data,
                48_000,
                Options {
                    loop_count: Some(1),
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                &sender,
            )
        });
        let mut actual = Vec::new();
        loop {
            match receiver.recv().unwrap() {
                Message::Frames(frames) => actual.extend(frames),
                Message::End => break,
                Message::Error(error) => panic!("{error}"),
            }
        }
        worker.join().unwrap().unwrap();
        assert_eq!(actual, expected);
        assert!(actual.iter().flatten().any(|sample| sample.abs() > 0.001));
        let (sender, _) = mpsc::sync_channel(1);
        assert!(
            produce(
                b"not a VGM".to_vec(),
                48_000,
                Options {
                    loop_count: Some(1),
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                &sender
            )
            .is_err()
        );
    }

    #[test]
    fn mdx_producer_matches_sequencer_and_rejects_vgm_options() {
        let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
        let pdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");
        let package = soundlog::mdx::package::MdxPackage::parse(mdx, Some(pdx)).unwrap();
        let options = Options {
            loop_count: Some(2),
            loop_modifier: None,
            loop_base: None,
            compat_vgm_pwm: None,
            ym2608_rhythm_rom: None,
            fadeout_samples: None,
        };
        let mut reference = MdxSequencer::with_options(
            package.clone(),
            48_000,
            CHUNK_FRAMES,
            MdxSequencerOptions {
                loop_count: Some(2),
                max_ticks: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut expected = Vec::new();
        while let Some(block) = reference.render().unwrap() {
            expected.extend_from_slice(block.frames());
        }
        let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
        let worker_options = options.clone();
        let worker = thread::spawn(move || {
            produce(
                Input::Mdx(Box::new(package)),
                48_000,
                worker_options,
                &sender,
            )
        });
        let mut actual = Vec::new();
        loop {
            match receiver.recv().unwrap() {
                Message::Frames(frames) => actual.extend(frames),
                Message::End => break,
                Message::Error(error) => panic!("{error}"),
            }
        }
        worker.join().unwrap().unwrap();
        assert_eq!(actual, expected);
        assert!(!actual.is_empty());
        let package = soundlog::mdx::package::MdxPackage::parse(mdx, Some(pdx)).unwrap();
        let (sender, _) = mpsc::sync_channel(1);
        let bad = Options {
            loop_modifier: Some(16),
            ..options.clone()
        };
        assert!(produce(Input::Mdx(Box::new(package.clone())), 48_000, bad, &sender).is_err());
        let bad = Options {
            compat_vgm_pwm: Some(false),
            ..options
        };
        assert!(produce(Input::Mdx(Box::new(package)), 48_000, bad, &sender).is_err());
    }

    #[test]
    fn infinite_mdx_producer_stops_when_consumer_is_dropped() {
        let package = soundlog::mdx::package::MdxPackage::parse(
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
            None,
        )
        .unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            produce(
                Input::Mdx(Box::new(package)),
                44_100,
                Options {
                    loop_count: None,
                    loop_modifier: None,
                    loop_base: None,
                    compat_vgm_pwm: None,
                    ym2608_rhythm_rom: None,
                    fadeout_samples: None,
                },
                &sender,
            )
        });
        for _ in 0..3 {
            assert!(matches!(receiver.recv().unwrap(), Message::Frames(_)));
        }
        drop(receiver);
        worker.join().unwrap().unwrap();
    }

    mod output_tests {
        use super::*;
        use crate::output::write_wav;
        use cpal::Sample;
        use soundlog::mdx::command::{MdxNote, MdxVoiceOrPcmBank};
        use soundlog::mdx::document::MdxBuilder;
        use soundlog::mdx::package::MdxPackage;
        use soundlog::vgm::VgmBuilder;
        use soundlog::vgm::command::WaitSamples;
        use std::path::Path;

        fn options() -> Options {
            Options {
                loop_count: Some(1),
                loop_modifier: None,
                loop_base: None,
                compat_vgm_pwm: None,
                ym2608_rhythm_rom: None,
                fadeout_samples: None,
            }
        }

        fn read_wav(path: &Path) -> (hound::WavSpec, Vec<i16>) {
            let mut wav = hound::WavReader::open(path).unwrap();
            let spec = wav.spec();
            (
                spec,
                wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap(),
            )
        }

        #[test]
        fn vgm_and_mdx_wav_samples_match_sequencer_at_requested_rates() {
            let dir = tempfile::tempdir().unwrap();
            let vgm = include_bytes!("../../chipstream/tests/fixtures/ym2151_a4.vgm").to_vec();
            let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
            let pdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");
            let package = MdxPackage::parse(mdx, Some(pdx)).unwrap();
            for rate in [44_100, 48_000] {
                for mdx_input in [false, true] {
                    let input = || {
                        if mdx_input {
                            Input::Mdx(Box::new(package.clone()))
                        } else {
                            Input::Vgm(vgm.clone())
                        }
                    };
                    let path = dir.path().join(format!("{rate}-{mdx_input}.wav"));
                    let mut reference = Player::new(input(), rate, &options()).unwrap();
                    let mut expected = Vec::new();
                    while let Some(block) = reference.render().unwrap() {
                        expected.extend(block.frames().iter().flatten().map(|sample| {
                            i16::from_sample(sample.clamp(-1.0, 1.0 - f32::EPSILON))
                        }));
                    }
                    write_wav(input(), options(), rate, &path).unwrap();
                    let (spec, actual) = read_wav(&path);
                    assert_eq!(spec.channels, 2);
                    assert_eq!(spec.sample_rate, rate);
                    assert_eq!(spec.bits_per_sample, 16);
                    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
                    assert_eq!(actual, expected);
                    assert!(actual.iter().any(|sample| *sample != 0));
                }
            }
        }

        #[test]
        fn wav_rendering_preserves_existing_output_on_failures() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("output.wav");
            std::fs::write(&path, b"existing").unwrap();
            let input = || {
                Input::Mdx(Box::new(MdxPackage {
                    mdx: builder_document(),
                    pdx: None,
                }))
            };
            fn builder_document() -> soundlog::mdx::document::MdxDocument {
                let mut builder = MdxBuilder::new();
                builder.add_mdx_command(0, MdxVoiceOrPcmBank { value: 42 });
                builder.add_mdx_command(
                    0,
                    MdxNote {
                        note: 0x80,
                        length: 8,
                    },
                );
                builder.finalize().unwrap()
            }
            assert!(write_wav(input(), options(), 44_100, &path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"existing");
            let mut infinite = options();
            infinite.loop_count = None;
            assert!(write_wav(input(), infinite, 44_100, &path).is_err());
            for rate in [0, u32::MAX] {
                assert!(write_wav(input(), options(), rate, &path).is_err());
            }
            assert_eq!(std::fs::read(&path).unwrap(), b"existing");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }

        #[test]
        fn supported_device_rate_selection_prefers_default_configuration() {
            use cpal::{SupportedBufferSize, SupportedStreamConfigRange};
            let range = |channels, min, max, format| {
                SupportedStreamConfigRange::new(
                    channels,
                    min,
                    max,
                    SupportedBufferSize::Unknown,
                    format,
                )
            };
            let preferred = range(2, 44_100, 48_000, SampleFormat::F32);
            let default = preferred.with_sample_rate(44_100);
            let other = range(1, 44_100, 96_000, SampleFormat::I16);
            let selected = select_output_config(default, [other, preferred], 48_000).unwrap();
            assert_eq!(selected.channels(), 2);
            assert_eq!(selected.sample_format(), SampleFormat::F32);
            assert_eq!(selected.sample_rate(), 48_000);
            let selected = select_output_config(default, [preferred, other], 96_000).unwrap();
            assert_eq!(selected.channels(), 1);
            assert_eq!(selected.sample_rate(), 96_000);
            assert!(select_output_config(default, [preferred], 96_000).is_err());
            assert!(select_output_config(default, [preferred], 0).is_err());
        }

        #[test]
        fn loop_count_is_applied_to_wav_duration() {
            let mut builder = VgmBuilder::new();
            builder.add_vgm_command(WaitSamples(100));
            builder.set_loop_index(1);
            builder.add_vgm_command(WaitSamples(300));
            let data: Vec<u8> = builder.finalize().into();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("loop.wav");
            let mut options = options();
            options.loop_count = Some(2);
            write_wav(Input::Vgm(data), options, 44_100, &path).unwrap();
            assert_eq!(read_wav(&path).1.len(), 700 * 2);
        }

        fn collect(mut player: Player) -> Vec<[f32; 2]> {
            let mut frames = Vec::new();
            while let Some(block) = player.render().unwrap() {
                frames.extend_from_slice(block.frames());
                assert!(frames.len() < 1_000_000, "playback did not terminate");
            }
            frames
        }

        fn fade_vgm(looping: bool) -> Vec<u8> {
            use soundlog::chip::{Chip, PsgSpec};
            use soundlog::vgm::command::Instance;
            let mut builder = VgmBuilder::new();
            builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
            for value in [0x8d, 0x26, 0x90] {
                builder.add_chip_write(Instance::Primary, PsgSpec { value });
            }
            builder.add_vgm_command(WaitSamples(503));
            if looping {
                builder.set_loop_index(4);
            }
            builder.add_vgm_command(WaitSamples(1501));
            builder.finalize().into()
        }

        #[test]
        fn vgm_and_mdx_fade_from_loop_boundary_with_exact_output_duration() {
            let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
            let pdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");
            let package = MdxPackage::parse(mdx, Some(pdx)).unwrap();
            for rate in [44_100, 48_000] {
                for is_mdx in [false, true] {
                    let input = || {
                        if is_mdx {
                            Input::Mdx(Box::new(package.clone()))
                        } else {
                            Input::Vgm(fade_vgm(true))
                        }
                    };
                    let mut opts = options();
                    opts.loop_count = Some(2);
                    let baseline = collect(Player::new(input(), rate, &opts).unwrap());
                    opts.fadeout_samples = Some(2205);
                    let faded = collect(Player::new(input(), rate, &opts).unwrap());
                    opts.fadeout_samples = Some(4410);
                    let longer = collect(Player::new(input(), rate, &opts).unwrap());
                    let start = baseline.len();
                    let ticks_before = if is_mdx { None } else { Some(503 + 1501 * 2) };
                    if let Some(ticks) = ticks_before {
                        assert_eq!(start, ((ticks * rate + 22_050) / 44_100) as usize);
                    }
                    assert_eq!(
                        faded.len(),
                        start + (rate / 20) as usize,
                        "MDX={is_mdx}, rate={rate}"
                    );
                    assert_eq!(
                        faded[..start.saturating_sub(1024)]
                            .iter()
                            .zip(&baseline)
                            .position(|(a, b)| a != b),
                        None,
                        "MDX={is_mdx}, rate={rate}, baseline frames={start}"
                    );
                    assert!(
                        faded[start..]
                            .iter()
                            .flatten()
                            .any(|sample| sample.abs() > 0.00001)
                    );
                    assert_eq!(faded.last(), Some(&[0.0, 0.0]));
                    let short_length = faded.len() - start;
                    let long_length = longer.len() - start;
                    for index in 0..short_length.saturating_sub(1024) {
                        let short_gain =
                            (short_length - index - 1) as f32 / (short_length - 1) as f32;
                        let long_gain = (long_length - index - 1) as f32 / (long_length - 1) as f32;
                        for channel in 0..2 {
                            let a = faded[start + index][channel] * long_gain;
                            let b = longer[start + index][channel] * short_gain;
                            assert!((a - b).abs() < 0.000001, "MDX={is_mdx}, frame={index}");
                        }
                    }
                }
            }
        }

        #[test]
        fn fade_is_ignored_for_no_loop_or_zero_duration() {
            let input = || Input::Vgm(fade_vgm(false));
            let baseline = collect(Player::new(input(), 44_100, &options()).unwrap());
            for samples in [0, 1000] {
                let mut opts = options();
                opts.fadeout_samples = Some(samples);
                assert_eq!(
                    collect(Player::new(input(), 44_100, &opts).unwrap()),
                    baseline
                );
            }
            let mut builder = MdxBuilder::new();
            builder.add_mdx_command(0, soundlog::mdx::command::MdxRest { ticks: 4 });
            let package = MdxPackage {
                mdx: builder.finalize().unwrap(),
                pdx: None,
            };
            let mdx = || Input::Mdx(Box::new(package.clone()));
            let baseline = collect(Player::new(mdx(), 44_100, &options()).unwrap());
            let mut opts = options();
            opts.fadeout_samples = Some(1000);
            assert_eq!(
                collect(Player::new(mdx(), 44_100, &opts).unwrap()),
                baseline
            );
        }

        #[test]
        fn vgm_fade_duration_and_pcm_are_independent_of_chunk_size() {
            let data = fade_vgm(true);
            let mut reference = None;
            for chunk in [1, 73, 1024, 4096] {
                let mut player = VgmSequencer::from_vgm(data.clone(), 48_000, chunk).unwrap();
                player.set_loop_count(Some(2));
                player.set_fadeout_samples(Some(2111)).unwrap();
                let mut frames = Vec::new();
                while let Some(block) = player.render().unwrap() {
                    frames.extend_from_slice(block.frames());
                }

                assert_eq!(
                    frames.len(),
                    ((503 + 1501 * 2 + 2111) * 48_000 + 22_050) / 44_100
                );
                if let Some(expected) = &reference {
                    assert_eq!(frames, *expected, "chunk size {chunk}");
                } else {
                    reference = Some(frames);
                }
            }
        }

        #[test]
        fn short_fades_end_at_silence() {
            for samples in [1, 2] {
                let mut player = VgmSequencer::from_vgm(fade_vgm(true), 48_000, 73).unwrap();
                player.set_fadeout_samples(Some(samples)).unwrap();
                let mut frames = Vec::new();
                while let Some(block) = player.render().unwrap() {
                    frames.extend_from_slice(block.frames());
                }
                assert_eq!(
                    frames.len(),
                    ((503 + 1501 + samples as usize) * 48_000 + 22_050) / 44_100
                );
                assert_eq!(frames.last(), Some(&[0.0, 0.0]));
            }
        }

        #[test]
        fn infinite_playback_ignores_fade_for_vgm_and_mdx() {
            let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
            let pdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");
            let package = MdxPackage::parse(mdx, Some(pdx)).unwrap();
            for is_mdx in [false, true] {
                let input = || {
                    if is_mdx {
                        Input::Mdx(Box::new(package.clone()))
                    } else {
                        Input::Vgm(fade_vgm(true))
                    }
                };
                let mut opts = options();
                opts.loop_count = None;
                let mut reference = Player::new(input(), 44_100, &opts).unwrap();
                opts.fadeout_samples = Some(100);
                let mut faded = Player::new(input(), 44_100, &opts).unwrap();
                for _ in 0..5 {
                    let a = reference.render().unwrap().unwrap();
                    let b = faded.render().unwrap().unwrap();
                    assert_eq!(a.frames(), b.frames());
                }
            }
        }

        #[test]
        fn mdx_fade_pcm_is_independent_of_chunk_size() {
            let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
            let pdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");
            let package = MdxPackage::parse(mdx, Some(pdx)).unwrap();
            let settings = MdxSequencerOptions {
                loop_count: Some(2),
                max_ticks: None,
                ..Default::default()
            };
            let mut reference = None;
            for chunk in [1, 73, 1024, 4096] {
                let mut player = MdxSequencer::with_options_and_fadeout(
                    package.clone(),
                    48_000,
                    chunk,
                    settings,
                    Some(2111),
                )
                .unwrap();
                let mut frames = Vec::new();
                while let Some(block) = player.render().unwrap() {
                    frames.extend_from_slice(block.frames());
                }
                if let Some(expected) = &reference {
                    assert_eq!(frames, *expected, "chunk size {chunk}");
                } else {
                    reference = Some(frames);
                }
            }
        }

        #[test]
        fn device_producer_uses_the_same_fade_as_wav_rendering() {
            let data = fade_vgm(true);
            let mut opts = options();
            opts.fadeout_samples = Some(2111);
            let expected = collect(Player::new(Input::Vgm(data.clone()), 48_000, &opts).unwrap());
            let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
            produce(data, 48_000, opts, &sender).unwrap();
            let mut actual = Vec::new();
            loop {
                match receiver.recv().unwrap() {
                    Message::Frames(frames) => actual.extend(frames),
                    Message::End => break,
                    Message::Error(error) => panic!("{error}"),
                }
            }
            assert_eq!(actual, expected);
        }
    }
}
