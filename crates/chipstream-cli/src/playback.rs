use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use soundlog::mdx::convert::{
    MdxLoopMarker, MdxToVgmOptions, to_vgm_stream_generator,
    to_vgm_stream_generator_with_loop_marker,
};
use soundlog::vgm::command::VgmCommand;
use soundlog::vgm::stream::{StreamResult, VgmStream};
use terminal_size::{Width, terminal_size};

use crate::input::{self, Input};

const CHUNK_FRAMES: usize = 1024;
const QUEUE_BLOCKS: usize = 8;
const PREFILL_BLOCKS: usize = 4;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

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
    Frames {
        start_frame: u64,
        frames: Vec<[f32; 2]>,
    },
    End {
        final_frame: u64,
    },
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

#[cfg(test)]
fn produce(
    data: impl Into<Input>,
    rate: u32,
    options: Options,
    sender: &SyncSender<Message>,
) -> Result<()> {
    produce_with_seek(data, rate, options, sender, &AtomicU64::new(0))
}

fn produce_with_seek(
    data: impl Into<Input>,
    rate: u32,
    options: Options,
    sender: &SyncSender<Message>,
    seek_target: &AtomicU64,
) -> Result<()> {
    let mut player = Player::new(data.into(), rate, &options)?;
    let mut rendered_frames = 0u64;
    loop {
        let Some(block) = player.render()? else {
            break;
        };
        let block_frames = block.frames();
        let next_rendered_frames = rendered_frames
            .checked_add(block_frames.len() as u64)
            .ok_or_else(|| anyhow!("playback position exceeds the supported range"))?;
        let skip_frames = seek_target
            .load(Ordering::Relaxed)
            .saturating_sub(rendered_frames)
            .min(block_frames.len() as u64) as usize;
        rendered_frames = next_rendered_frames;
        if skip_frames == block_frames.len() {
            continue;
        }
        if sender
            .send(Message::Frames {
                start_frame: rendered_frames - (block_frames.len() - skip_frames) as u64,
                frames: block_frames[skip_frames..].to_vec(),
            })
            .is_err()
        {
            return Ok(());
        }
    }
    let _ = sender.send(Message::End {
        final_frame: rendered_frames,
    });
    Ok(())
}

fn wait_samples(command: &VgmCommand) -> u64 {
    match command {
        VgmCommand::WaitSamples(wait) => u64::from(wait.0),
        VgmCommand::Wait735Samples(_) => 735,
        VgmCommand::Wait882Samples(_) => 882,
        VgmCommand::WaitNSample(wait) => u64::from(wait.0) + 1,
        VgmCommand::YM2612Port0Address2AWriteAndWaitN(wait) => u64::from(wait.0),
        _ => 0,
    }
}

fn count_stream_samples(
    mut stream: VgmStream,
    marker: Option<MdxLoopMarker>,
    fadeout_samples: Option<u32>,
) -> Result<u64> {
    let mut samples = 0u64;
    loop {
        let next = stream.next();
        if marker.as_ref().is_some_and(MdxLoopMarker::reached) {
            return samples
                .checked_add(u64::from(fadeout_samples.unwrap_or(0)))
                .ok_or_else(|| anyhow!("playback duration exceeds the supported range"));
        }
        let Some(result) = next else {
            return Ok(samples);
        };
        match result? {
            StreamResult::EndOfStream => return Ok(samples),
            StreamResult::NeedsMoreData => bail!("soundlog stream unexpectedly needs more data"),
            StreamResult::Command(command) => {
                samples = samples
                    .checked_add(wait_samples(&command))
                    .ok_or_else(|| anyhow!("playback duration exceeds the supported range"))?;
            }
        }
    }
}

fn count_samples(data: &Input, options: &Options) -> Result<Option<u64>> {
    if options.loop_count.is_none() {
        return Ok(None);
    }
    let (stream, marker, fadeout_samples) = match data {
        Input::Vgm(bytes) => {
            let mut stream = VgmStream::from_vgm(bytes.clone())?;
            stream.set_loop_count(options.loop_count);
            if let Some(modifier) = options.loop_modifier {
                stream.set_loop_modifier(modifier);
            }
            if let Some(base) = options.loop_base {
                stream.set_loop_base(base);
            }
            stream.set_fadeout_samples(
                options
                    .fadeout_samples
                    .filter(|samples| *samples != 0)
                    .map(|samples| samples as usize),
            );
            (stream, None, None)
        }
        Input::Mdx(package) => {
            let generator_options = MdxToVgmOptions {
                loop_count: options.loop_count,
                max_ticks: None,
                ..MdxToVgmOptions::default()
            };
            let fadeout_samples = options.fadeout_samples.filter(|samples| *samples != 0);
            if fadeout_samples.is_some() {
                let (generator, marker) = to_vgm_stream_generator_with_loop_marker(
                    (**package).clone(),
                    generator_options,
                )?;
                (
                    VgmStream::from_generator(generator),
                    Some(marker),
                    fadeout_samples,
                )
            } else {
                let generator = to_vgm_stream_generator((**package).clone(), generator_options)?;
                (VgmStream::from_generator(generator), None, None)
            }
        }
    };
    count_stream_samples(stream, marker, fadeout_samples).map(Some)
}

fn playback_time(frames: u64, rate: u32) -> String {
    let seconds = frames / u64::from(rate);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn progress_line(
    frames: u64,
    rate: u32,
    total_samples: Option<u64>,
    columns: usize,
    finished: bool,
) -> String {
    let time = playback_time(frames, rate);
    let Some(total_samples) = total_samples else {
        return format!("\r{time} / --:--:-- --%");
    };
    let label = format!(
        "{time} / {}",
        playback_time(total_samples, soundlog::vgm::VGM_SAMPLE_RATE)
    );
    let played_samples =
        (u128::from(frames) * u128::from(soundlog::vgm::VGM_SAMPLE_RATE)) / u128::from(rate);
    let percentage = if finished || total_samples == 0 {
        100
    } else {
        ((played_samples * 100) / u128::from(total_samples)).min(100)
    };
    let bar_width = columns.saturating_sub(label.len() + 3).max(1);
    let filled = (percentage * bar_width as u128 / 100) as usize;
    let midpoint = bar_width / 2;
    let bar = (0..bar_width)
        .map(|position| {
            if position == midpoint {
                '|'
            } else if position < filled {
                'o'
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("\r{label} [{bar}]")
}

fn terminal_columns() -> usize {
    terminal_size()
        .map(|(Width(width), _)| usize::from(width))
        .filter(|width| *width > 0)
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.parse::<usize>().ok())
                .filter(|width| *width > 0)
        })
        .unwrap_or(80)
}

fn update_progress(
    writer: &mut impl Write,
    frames: u64,
    rate: u32,
    total_samples: Option<u64>,
    finished: bool,
) -> Result<()> {
    write!(
        writer,
        "{}",
        progress_line(frames, rate, total_samples, terminal_columns(), finished)
    )
    .context("failed to write playback progress")?;
    writer.flush().context("failed to flush playback progress")
}

fn next_seek_target(
    playback_position: u64,
    current_target: u64,
    rate: u32,
    total_samples: Option<u64>,
) -> u64 {
    let target = playback_position
        .max(current_target)
        .saturating_add(u64::from(rate).saturating_mul(5));
    total_samples.map_or(target, |samples| {
        let total_frames = (u128::from(samples) * u128::from(rate)
            + u128::from(soundlog::vgm::VGM_SAMPLE_RATE / 2))
            / u128::from(soundlog::vgm::VGM_SAMPLE_RATE);
        target.min(u64::try_from(total_frames).unwrap_or(u64::MAX))
    })
}

struct RawModeGuard {
    enabled: bool,
}

impl RawModeGuard {
    fn new() -> Result<Self> {
        let enabled = io::stdin().is_terminal();
        if enabled {
            enable_raw_mode().context("failed to enable terminal raw mode")?;
        }
        Ok(Self { enabled })
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn restore(mut self) -> Result<()> {
        if self.enabled {
            disable_raw_mode().context("failed to restore terminal input mode")?;
            self.enabled = false;
        }
        Ok(())
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = disable_raw_mode();
        }
    }
}

fn poll_seek_keys(
    enabled: bool,
    seek_target: &AtomicU64,
    playback_position: &AtomicU64,
    rate: u32,
    total_samples: Option<u64>,
) -> Result<bool> {
    while enabled && event::poll(Duration::ZERO).context("failed to poll terminal input")? {
        if let Event::Key(key) = event::read().context("failed to read terminal input")?
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            if is_interrupt_key(key) {
                return Ok(true);
            }
            if key.code == KeyCode::Right {
                let position = playback_position.load(Ordering::Relaxed);
                let current_target = seek_target.load(Ordering::Relaxed);
                seek_target.store(
                    next_seek_target(position, current_target, rate, total_samples),
                    Ordering::Relaxed,
                );
            }
        }
    }
    Ok(false)
}

fn is_interrupt_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

struct Consumer {
    receiver: Receiver<Message>,
    queued: VecDeque<Message>,
    frames: Vec<[f32; 2]>,
    frames_start: u64,
    position: usize,
    playback_position: Arc<AtomicU64>,
    seek_target: Arc<AtomicU64>,
    terminal: bool,
}

enum FillResult {
    Continue(u64),
    End(usize, u64),
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
            return FillResult::Continue(self.playback_position.load(Ordering::Relaxed));
        }
        if !output.len().is_multiple_of(channels) {
            self.terminal = true;
            return FillResult::Error("audio callback returned a partial frame".into());
        }
        let mut playback_position = self.playback_position.load(Ordering::Relaxed);
        for (frame_index, output_frame) in output.chunks_mut(channels).enumerate() {
            while self.position == self.frames.len() {
                let message = match self.queued.pop_front() {
                    Some(message) => Ok(message),
                    None => self.receiver.try_recv(),
                };
                match message {
                    Ok(Message::Frames {
                        start_frame,
                        frames,
                    }) if !frames.is_empty() => {
                        self.frames = frames;
                        self.frames_start = start_frame;
                        self.position = 0;
                    }
                    Ok(Message::Frames { .. }) => {
                        self.terminal = true;
                        return FillResult::Error("sequencer returned an empty PCM block".into());
                    }
                    Ok(Message::End { final_frame }) => {
                        self.terminal = true;
                        return FillResult::End(frame_index, final_frame);
                    }
                    Ok(Message::Error(error)) => {
                        self.terminal = true;
                        return FillResult::Error(error);
                    }
                    Err(TryRecvError::Empty) => {
                        if self.seek_target.load(Ordering::Relaxed) > playback_position {
                            return FillResult::Continue(playback_position);
                        }
                        self.terminal = true;
                        return FillResult::Error("audio buffer underrun while rendering".into());
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.terminal = true;
                        return FillResult::Error("rendering thread stopped unexpectedly".into());
                    }
                }
            }
            let frame_position = self.frames_start.saturating_add(self.position as u64);
            let [left, right] = self.frames[self.position];
            self.position += 1;
            if frame_position < self.seek_target.load(Ordering::Relaxed) {
                continue;
            }
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
            playback_position = frame_position.saturating_add(1);
        }
        FillResult::Continue(playback_position)
    }
}

fn build_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut consumer: Consumer,
    sender: mpsc::Sender<PlaybackEvent>,
    played_frames: Arc<AtomicU64>,
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
                FillResult::Continue(position) => {
                    played_frames.store(position, Ordering::Relaxed);
                }
                FillResult::Error(error) => {
                    let _ = sender.send(PlaybackEvent::Error(error));
                }
                FillResult::End(frames, position) => {
                    played_frames.store(position, Ordering::Relaxed);
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
    total_samples: Option<u64>,
    seek_target: Arc<AtomicU64>,
    no_progress: bool,
) -> Result<()> {
    let config = supported.config();
    if config.channels == 0 || config.sample_rate == 0 {
        bail!("audio device returned an invalid output configuration");
    }
    let rate = config.sample_rate;
    let mut queued = VecDeque::new();
    for _ in 0..PREFILL_BLOCKS {
        let message = receiver
            .recv()
            .context("rendering thread stopped before playback")?;
        match message {
            Message::Error(error) => bail!("{error}"),
            Message::End { .. } => {
                queued.push_back(message);
                break;
            }
            Message::Frames { .. } => queued.push_back(message),
        }
    }
    let played_frames = Arc::new(AtomicU64::new(0));
    let consumer = Consumer {
        receiver,
        queued,
        frames: Vec::new(),
        frames_start: 0,
        position: 0,
        playback_position: Arc::clone(&played_frames),
        seek_target: Arc::clone(&seek_target),
        terminal: false,
    };
    let (sender, events) = mpsc::channel();
    let callback_frames = Arc::clone(&played_frames);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => {
            build_stream::<f32>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::F64 => {
            build_stream::<f64>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::I8 => build_stream::<i8>(&device, &config, consumer, sender, callback_frames),
        SampleFormat::I16 => {
            build_stream::<i16>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::I24 => {
            build_stream::<cpal::I24>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::I32 => {
            build_stream::<i32>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::I64 => {
            build_stream::<i64>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::U8 => build_stream::<u8>(&device, &config, consumer, sender, callback_frames),
        SampleFormat::U16 => {
            build_stream::<u16>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::U24 => {
            build_stream::<cpal::U24>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::U32 => {
            build_stream::<u32>(&device, &config, consumer, sender, callback_frames)
        }
        SampleFormat::U64 => {
            build_stream::<u64>(&device, &config, consumer, sender, callback_frames)
        }
        format => bail!("unsupported audio sample format: {format:?}"),
    }?;
    let raw_mode = RawModeGuard::new()?;
    let playback_result = (|| {
        stream.play().context("failed to start audio output")?;
        let mut stdout = io::stdout().lock();
        let mut deadline = None;
        while deadline.is_none() {
            if poll_seek_keys(
                raw_mode.enabled(),
                &seek_target,
                &played_frames,
                rate,
                total_samples,
            )? {
                if !no_progress {
                    writeln!(stdout).context("failed to end playback progress line")?;
                }
                return Ok(());
            }
            match events.recv_timeout(PROGRESS_INTERVAL) {
                Ok(PlaybackEvent::Finished(finished_at)) => deadline = Some(finished_at),
                Ok(PlaybackEvent::Error(error)) => bail!("{error}"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("audio output stopped unexpectedly");
                }
            }
            if !no_progress {
                update_progress(
                    &mut stdout,
                    played_frames.load(Ordering::Relaxed),
                    rate,
                    total_samples,
                    false,
                )?;
            }
        }
        let Some(deadline) = deadline else {
            bail!("audio output ended without a completion deadline");
        };
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match events.recv_timeout(remaining.min(PROGRESS_INTERVAL)) {
                Ok(PlaybackEvent::Error(error)) => bail!("{error}"),
                Ok(PlaybackEvent::Finished(_)) => {
                    bail!("audio output reported completion twice");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("audio output stopped while draining");
                }
            }
            if !no_progress {
                update_progress(
                    &mut stdout,
                    played_frames.load(Ordering::Relaxed),
                    rate,
                    total_samples,
                    false,
                )?;
            }
        }
        if !no_progress {
            update_progress(
                &mut stdout,
                played_frames.load(Ordering::Relaxed),
                rate,
                total_samples,
                true,
            )?;
            writeln!(stdout).context("failed to end playback progress line")?;
        }
        Ok(())
    })();
    drop(stream);
    match (playback_result, raw_mode.restore()) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(restore_error)) => Err(error.context(format!(
            "also failed to restore terminal input mode: {restore_error:#}"
        ))),
    }
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

pub fn play(
    data: Input,
    options: Options,
    sample_rate: Option<u32>,
    no_progress: bool,
) -> Result<()> {
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
    let total_samples =
        count_samples(&data, &options).context("failed to determine playback duration")?;
    let metadata = input::metadata_line(&data)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{metadata}").context("failed to write playback metadata")?;
    stdout
        .flush()
        .context("failed to flush playback metadata")?;
    drop(stdout);
    let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
    let seek_target = Arc::new(AtomicU64::new(0));
    let worker_seek_target = Arc::clone(&seek_target);
    let worker = thread::spawn(move || {
        if let Err(error) = produce_with_seek(data, rate, options, &sender, &worker_seek_target) {
            let _ = sender.send(Message::Error(format!("{error:#}")));
        }
    });
    let result = run(
        device,
        supported,
        receiver,
        total_samples,
        seek_target,
        no_progress,
    );
    if worker.join().is_err() {
        return Err(anyhow!("rendering thread panicked"));
    }
    result
}

#[cfg(test)]
mod playback_tests {
    use super::*;
    use soundlog::mdx::package::MdxPackage;
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
            frames_start: 0,
            position: 0,
            playback_position: Arc::new(AtomicU64::new(0)),
            seek_target: Arc::new(AtomicU64::new(0)),
            terminal: false,
        }
    }

    #[test]
    fn playback_progress_formats_elapsed_time_and_percentage() {
        let mut output = Vec::new();
        update_progress(&mut output, 61 * 44_100, 44_100, Some(122 * 44_100), false).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .starts_with("\r00:01:01 / 00:02:02 [")
        );

        let mut output = Vec::new();
        update_progress(&mut output, 3 * 44_100, 44_100, None, false).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "\r00:00:03 / --:--:-- --%"
        );
    }

    #[test]
    fn progress_bar_uses_full_terminal_width() {
        let line = progress_line(50 * 44_100, 44_100, Some(100 * 44_100), 80, false);
        assert_eq!(line.len(), 81);
        assert_eq!(line.matches('-').count(), 28);
        assert_eq!(line.matches('o').count(), 29);
        assert_eq!(line.matches('|').count(), 1);
        assert!(line.starts_with("\r00:00:50 / 00:01:40 [o"));
        assert!(line.ends_with("-]"));
        assert_eq!(line.find('|'), Some(line.find('[').unwrap() + 1 + 29));

        let empty = progress_line(0, 44_100, Some(100), 80, false);
        assert_eq!(empty.matches('-').count(), 57);
        assert_eq!(empty.matches('o').count(), 0);
        assert_eq!(empty.matches('|').count(), 1);
        assert!(empty.starts_with("\r00:00:00 / 00:00:00 [-"));

        let complete = progress_line(98, 44_100, Some(100), 80, true);
        assert_eq!(complete.len(), 81);
        assert_eq!(complete.matches('-').count(), 0);
        assert_eq!(complete.matches('o').count(), 57);
        assert_eq!(complete.matches('|').count(), 1);
        assert!(complete.starts_with("\r00:00:00 / 00:00:00 [o"));
    }

    #[test]
    fn right_arrow_seek_target_advances_five_seconds_and_stops_at_end() {
        assert_eq!(next_seek_target(48_000, 0, 48_000, None), 288_000);
        assert_eq!(next_seek_target(48_000, 288_000, 48_000, None), 528_000);
        assert_eq!(
            next_seek_target(48_000, 0, 48_000, Some(4 * 44_100)),
            192_000
        );
    }

    #[test]
    fn ctrl_c_is_recognized_as_a_playback_interrupt() {
        assert!(is_interrupt_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_interrupt_key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn consumer_silences_frames_before_seek_target() {
        let frames = vec![[0.25, 0.5]; 5];
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Message::Frames {
                start_frame: 0,
                frames,
            })
            .unwrap();
        drop(sender);
        let playback_position = Arc::new(AtomicU64::new(0));
        let mut player = Consumer {
            receiver,
            queued: VecDeque::new(),
            frames: Vec::new(),
            frames_start: 0,
            position: 0,
            playback_position: Arc::clone(&playback_position),
            seek_target: Arc::new(AtomicU64::new(3)),
            terminal: false,
        };
        let mut output = [0.0; 8];
        assert!(matches!(
            player.fill(&mut output, 2),
            FillResult::Continue(4)
        ));
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.25, 0.5]);
        assert_eq!(playback_position.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn producer_starts_pcm_at_the_requested_seek_position() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1500));
        let data: Vec<u8> = builder.finalize().into();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
        let options = Options {
            loop_count: Some(1),
            fadeout_samples: None,
            loop_modifier: None,
            loop_base: None,
            compat_vgm_pwm: None,
            ym2608_rhythm_rom: None,
        };
        let seek_target = AtomicU64::new(1100);
        produce_with_seek(data, 44_100, options, &sender, &seek_target).unwrap();
        match receiver.recv().unwrap() {
            Message::Frames {
                start_frame,
                frames,
            } => {
                assert_eq!(start_frame, 1100);
                assert_eq!(frames.len(), 400);
            }
            _ => panic!("expected a PCM block"),
        }
        assert!(matches!(
            receiver.recv().unwrap(),
            Message::End { final_frame: 1500 }
        ));
    }

    #[test]
    fn count_samples_includes_loops_and_fadeout() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(100));
        builder.set_loop_index(1);
        builder.add_vgm_command(WaitSamples(1000));
        let data = Input::from(Vec::<u8>::from(builder.finalize()));
        let options = Options {
            loop_count: Some(2),
            fadeout_samples: Some(88_200),
            loop_modifier: None,
            loop_base: None,
            compat_vgm_pwm: None,
            ym2608_rhythm_rom: None,
        };
        assert_eq!(count_samples(&data, &options).unwrap(), Some(90_300));
    }

    #[test]
    fn count_samples_matches_mdx_playback_with_fadeout() {
        let package = MdxPackage::parse(
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
            Some(include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx")),
        )
        .unwrap();
        let options = Options {
            loop_count: Some(2),
            fadeout_samples: Some(88_200),
            loop_modifier: None,
            loop_base: None,
            compat_vgm_pwm: None,
            ym2608_rhythm_rom: None,
        };
        let data = Input::Mdx(Box::new(package));
        let samples = count_samples(&data, &options).unwrap().unwrap();
        let mut player = Player::new(data, 44_100, &options).unwrap();
        let mut frames = 0u64;
        while let Some(block) = player.render().unwrap() {
            frames += block.frames().len() as u64;
        }
        assert_eq!(samples, frames);
    }

    #[test]
    fn stereo_mono_surround_and_eof() {
        let frames = vec![[0.25, -0.5], [1.5, -2.0]];
        let mut output = [0.0; 9];
        let mut player = consumer(vec![
            Message::Frames {
                start_frame: 0,
                frames: frames.clone(),
            },
            Message::End { final_frame: 2 },
        ]);
        assert!(matches!(player.fill(&mut output, 3), FillResult::End(2, 2)));
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
        let mut player = consumer(vec![
            Message::Frames {
                start_frame: 0,
                frames,
            },
            Message::End { final_frame: 2 },
        ]);
        assert!(matches!(player.fill(&mut mono, 1), FillResult::Continue(2)));
        assert_eq!(mono, [-0.125, -0.25]);
        assert!(matches!(player.fill(&mut mono, 1), FillResult::End(0, 2)));
    }

    #[test]
    fn underrun_disconnect_and_nonfinite_are_errors() {
        let (sender, receiver) = mpsc::channel();
        let mut player = Consumer {
            receiver,
            queued: VecDeque::new(),
            frames: Vec::new(),
            frames_start: 0,
            position: 0,
            playback_position: Arc::new(AtomicU64::new(0)),
            seek_target: Arc::new(AtomicU64::new(0)),
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
        let mut player = consumer(vec![Message::Frames {
            start_frame: 0,
            frames: vec![[f32::NAN, 0.0]],
        }]);
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
                    Message::Frames { frames, .. } => actual.extend(frames),
                    Message::End { .. } => break,
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
                    Message::Frames { frames, .. } => {
                        audible |= frames.iter().flatten().any(|sample| sample.abs() > 0.001)
                    }
                    Message::End { .. } => break,
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
                    Message::Frames { frames: block, .. } => frames += block.len(),
                    Message::End { .. } => break,
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
            assert!(matches!(receiver.recv().unwrap(), Message::Frames { .. }));
        }
        drop(receiver);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn producer_preserves_sequencer_pcm_and_reports_parse_errors() {
        let data = include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm").to_vec();
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
                Message::Frames { frames, .. } => actual.extend(frames),
                Message::End { .. } => break,
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
                Message::Frames { frames, .. } => actual.extend(frames),
                Message::End { .. } => break,
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
            assert!(matches!(receiver.recv().unwrap(), Message::Frames { .. }));
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
            let vgm = include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm").to_vec();
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
                    Message::Frames { frames, .. } => actual.extend(frames),
                    Message::End { .. } => break,
                    Message::Error(error) => panic!("{error}"),
                }
            }
            assert_eq!(actual, expected);
        }
    }
}
