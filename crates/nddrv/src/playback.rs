use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chipstream::audio::StereoBlock;
use chipstream::sequencer::{ChipFactoryRegistry, MdxSequencer, MdxSequencerOptions, VgmSequencer};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ndsif_transport::{NdsifHostTimeline, NdsifOutputGain};
use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;

use crate::device::{DeviceHandle, DeviceLease};
use crate::library::{extension, read_track};
use crate::routing::PlaybackPlan;

const CHUNK_FRAMES: usize = 1024;
const QUEUE_BLOCKS: usize = 8;
const FADEOUT_SECONDS: u32 = 2;
const FADEOUT_SAMPLES: u32 = FADEOUT_SECONDS * 44_100;

pub enum Event {
    Finished(u64),
    Error(u64, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VolumeMode {
    Master,
    SoftwareOnly,
    Preparing,
}

pub struct Playback {
    control: Arc<PlaybackControl>,
    volume: Arc<NdsifOutputGain>,
    generation: u64,
    backend: Arc<dyn PlaybackBackend>,
    worker_gate: Arc<Mutex<()>>,
    sender: mpsc::Sender<Event>,
    pub events: mpsc::Receiver<Event>,
}

trait PlaybackBackend: Send + Sync {
    fn play(&self, path: &Path, control: Arc<PlaybackControl>, looped: bool) -> Result<()>;
}

struct EmulationBackend;

impl PlaybackBackend for EmulationBackend {
    fn play(&self, path: &Path, control: Arc<PlaybackControl>, looped: bool) -> Result<()> {
        run(path, control, looped)
    }
}

struct PlaybackControl {
    cancel: AtomicBool,
    paused: AtomicBool,
    frames: AtomicU64,
    sample_rate: AtomicU32,
    volume: Arc<NdsifOutputGain>,
    volume_mode: AtomicU8,
    first_presentation: OnceLock<Instant>,
    hardware_timeline: OnceLock<Arc<NdsifHostTimeline>>,
}

impl PlaybackControl {
    fn new(volume: Arc<NdsifOutputGain>) -> Self {
        Self {
            cancel: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            volume,
            volume_mode: AtomicU8::new(VolumeMode::Master as u8),
            first_presentation: OnceLock::new(),
            hardware_timeline: OnceLock::new(),
        }
    }

    fn elapsed_seconds(&self) -> u64 {
        let rate = self.sample_rate.load(Ordering::Relaxed);
        if rate == 0 {
            0
        } else {
            self.frames.load(Ordering::Relaxed) / u64::from(rate)
        }
    }
}

impl Playback {
    pub fn new() -> Self {
        let (sender, events) = mpsc::channel();
        let volume = Arc::new(NdsifOutputGain::new());
        Self {
            control: Arc::new(PlaybackControl::new(volume.clone())),
            volume,
            generation: 0,
            backend: Arc::new(EmulationBackend),
            worker_gate: Arc::new(Mutex::new(())),
            sender,
            events,
        }
    }

    pub fn play(&mut self, path: PathBuf, looped: bool) {
        self.start(path, looped, None);
    }

    pub fn play_hardware(
        &mut self,
        path: PathBuf,
        looped: bool,
        devices: DeviceHandle<ndsif_transport::NdsifSerialTransport>,
        plan: PlaybackPlan,
    ) {
        self.start(path, looped, Some((devices, plan)));
    }

    fn start(
        &mut self,
        path: PathBuf,
        looped: bool,
        hardware: Option<(
            DeviceHandle<ndsif_transport::NdsifSerialTransport>,
            PlaybackPlan,
        )>,
    ) {
        self.stop();
        self.control = Arc::new(PlaybackControl::new(self.volume.clone()));
        if hardware.is_some() {
            self.control
                .volume_mode
                .store(VolumeMode::Preparing as u8, Ordering::Release);
        }
        let control = self.control.clone();
        let sender = self.sender.clone();
        let backend = self.backend.clone();
        let worker_gate = self.worker_gate.clone();
        let generation = self.generation;
        thread::spawn(move || {
            let result = match worker_gate.lock() {
                Ok(_guard) if control.cancel.load(Ordering::Acquire) => Ok(()),
                Ok(_guard) => match hardware {
                    Some((devices, plan)) => {
                        run_hardware(&path, control.clone(), looped, devices, plan)
                    }
                    None => backend.play(&path, control.clone(), looped),
                },
                Err(_) => Err(anyhow::anyhow!("previous playback worker failed")),
            };
            if !control.cancel.load(Ordering::Relaxed) {
                let event = match result {
                    Ok(()) => Event::Finished(generation),
                    Err(error) => Event::Error(generation, format!("{error:#}")),
                };
                let _ = sender.send(event);
            } else if let Err(error) = result {
                eprintln!("Playback cleanup failed after cancellation: {error:#}");
            }
        });
    }

    pub fn stop(&mut self) {
        self.control.cancel.store(true, Ordering::Release);
        if let Some(timeline) = self.control.hardware_timeline.get() {
            timeline.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn set_paused(&self, paused: bool) {
        self.control.paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_volume(&self, volume: f32) -> Result<()> {
        self.volume.set(volume).map_err(anyhow::Error::msg)
    }

    pub fn volume_mode(&self) -> VolumeMode {
        match self.control.volume_mode.load(Ordering::Acquire) {
            0 => VolumeMode::Master,
            1 => VolumeMode::SoftwareOnly,
            2 => VolumeMode::Preparing,
            _ => unreachable!("invalid volume mode"),
        }
    }

    pub fn elapsed_seconds(&self) -> u64 {
        self.control.elapsed_seconds()
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Sequencer {
    Vgm(VgmSequencer<'static>),
    Mdx(MdxSequencer<'static>),
}

impl Sequencer {
    fn new(path: &Path, rate: u32, looped: bool) -> Result<Self> {
        Self::with_factories(path, rate, looped, ChipFactoryRegistry::new())
    }

    fn with_factories(
        path: &Path,
        rate: u32,
        looped: bool,
        factories: ChipFactoryRegistry,
    ) -> Result<Self> {
        let bytes = read_track(path)?;
        if extension(path) == "mdx" {
            let normalized = normalize_mdx_for_playback(&bytes).map_err(anyhow::Error::msg)?;
            let mdx = MdxDocument::parse(normalized.as_ref()).context("parse MDX")?;
            let pdx = mdx
                .header
                .pdx_name
                .as_deref()
                .map(|name| resolve_pdx(path, name))
                .transpose()?
                .map(|path| read_track(&path))
                .transpose()?;
            let package = MdxPackage::parse_owned(normalized.into_owned(), pdx)?;
            let options = MdxSequencerOptions {
                loop_count: if looped { None } else { Some(2) },
                max_ticks: if looped {
                    None
                } else {
                    MdxSequencerOptions::default().max_ticks
                },
                ..MdxSequencerOptions::default()
            };
            Ok(Self::Mdx(
                MdxSequencer::with_options_and_fadeout_with_factories(
                    package,
                    rate,
                    CHUNK_FRAMES,
                    options,
                    (!looped).then_some(FADEOUT_SAMPLES),
                    factories,
                )?,
            ))
        } else {
            let mut sequencer =
                VgmSequencer::from_vgm_with_factories(bytes, rate, CHUNK_FRAMES, factories)?;
            sequencer.set_loop_count(if looped { None } else { Some(2) });
            if !looped {
                sequencer.set_loop_base(0);
                sequencer.set_loop_modifier(0x10);
                sequencer.set_fadeout_samples(Some(FADEOUT_SAMPLES))?;
            }
            Ok(Self::Vgm(sequencer))
        }
    }

    fn render(&mut self) -> Result<Option<StereoBlock>> {
        match self {
            Self::Vgm(player) => Ok(player.render()?),
            Self::Mdx(player) => Ok(player.render()?),
        }
    }

    fn fade_start_frame(&self) -> Option<usize> {
        match self {
            Self::Vgm(player) => player.fade_start_frame(),
            Self::Mdx(player) => player.fade_start_frame(),
        }
    }

    fn schedule_hardware_fade(
        &self,
        control: &PlaybackControl,
        rate: u32,
        scheduled: &mut bool,
    ) -> Result<()> {
        if !*scheduled
            && let Some(start) = self.fade_start_frame()
            && let Some(timeline) = control.hardware_timeline.get()
        {
            timeline
                .schedule_output_fade(start, rate, Duration::from_secs(u64::from(FADEOUT_SECONDS)))
                .map_err(anyhow::Error::msg)?;
            *scheduled = true;
        }
        Ok(())
    }
}

fn resolve_pdx(path: &Path, name: &str) -> Result<PathBuf> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        bail!("invalid PDX filename");
    }
    let mut names = vec![name.to_owned()];
    if Path::new(name).extension().is_none() {
        names.push(format!("{name}.pdx"));
    }
    let dir = path.parent().context("MDX file has no parent directory")?;
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(dir).context("search PDX directory")? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && names.iter().any(|name| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(name)
            })
        {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        0 => bail!("PDX file '{name}' not found next to {}", path.display()),
        1 => Ok(matches.remove(0)),
        _ => bail!("multiple matches for PDX file '{name}'"),
    }
}

enum Block {
    Audio(Vec<[f32; 2]>),
    End,
}

struct AudioConsumer {
    rx: mpsc::Receiver<Block>,
    events: mpsc::Sender<Result<Instant>>,
    control: Arc<PlaybackControl>,
    frames: Vec<[f32; 2]>,
    position: usize,
    ended: bool,
    channels: usize,
    rate: u32,
}

impl AudioConsumer {
    fn fill<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T], latency: Duration) {
        self.fill_at(out, Instant::now(), latency);
    }

    fn fill_at<T: SizedSample + FromSample<f32>>(
        &mut self,
        out: &mut [T],
        callback_time: Instant,
        latency: Duration,
    ) {
        out.fill(T::from_sample(0.0));
        if self.ended
            || self.control.paused.load(Ordering::Relaxed)
            || self.control.cancel.load(Ordering::Relaxed)
        {
            return;
        }
        let volume = self.control.volume.get();
        let mut played = 0;
        let mut needs_origin = self.control.first_presentation.get().is_none();
        for (frame_index, frame) in out.chunks_mut(self.channels).enumerate() {
            if self.position == self.frames.len() {
                match self.rx.try_recv() {
                    Ok(Block::Audio(block)) => {
                        self.frames = block;
                        self.position = 0;
                    }
                    Ok(Block::End) => {
                        self.ended = true;
                        let remaining =
                            Duration::from_secs_f64(frame_index as f64 / f64::from(self.rate));
                        let _ = self.events.send(Ok(callback_time + latency + remaining));
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        self.ended = true;
                        let _ = self
                            .events
                            .send(Err(anyhow::anyhow!("audio buffer underrun")));
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.ended = true;
                        let _ = self
                            .events
                            .send(Err(anyhow::anyhow!("audio producer disconnected")));
                        break;
                    }
                }
            }
            if let Some(&[l, r]) = self.frames.get(self.position) {
                self.position += 1;
                if !l.is_finite() || !r.is_finite() {
                    self.ended = true;
                    let _ = self
                        .events
                        .send(Err(anyhow::anyhow!("non-finite audio sample")));
                    break;
                }
                if needs_origin {
                    let offset = Duration::from_secs_f64(frame_index as f64 / f64::from(self.rate));
                    let Some(presentation) = callback_time
                        .checked_add(latency)
                        .and_then(|time| time.checked_add(offset))
                    else {
                        self.ended = true;
                        let _ = self
                            .events
                            .send(Err(anyhow::anyhow!("audio presentation time overflow")));
                        break;
                    };
                    if self.control.first_presentation.set(presentation).is_err() {
                        self.ended = true;
                        let _ = self.events.send(Err(anyhow::anyhow!(
                            "audio presentation clock already started"
                        )));
                        break;
                    }
                    if let Some(timeline) = self.control.hardware_timeline.get()
                        && let Err(error) = timeline.start(presentation)
                    {
                        self.ended = true;
                        let _ = self.events.send(Err(error.into()));
                        break;
                    }
                    needs_origin = false;
                }
                played += 1;
                let clip = |v: f32| v.clamp(-1.0, 1.0 - f32::EPSILON);
                if self.channels == 1 {
                    frame[0] = T::from_sample(clip((l + r) * 0.5 * volume));
                } else {
                    frame[0] = T::from_sample(clip(l * volume));
                    frame[1] = T::from_sample(clip(r * volume));
                }
            }
        }
        self.control.frames.fetch_add(played, Ordering::Relaxed);
    }
}

fn stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    rx: mpsc::Receiver<Block>,
    events: mpsc::Sender<Result<Instant>>,
    control: Arc<PlaybackControl>,
) -> Result<cpal::Stream> {
    let channels = usize::from(config.channels);
    if channels == 0 {
        bail!("audio device has no channels");
    }
    let errors = events.clone();
    let mut consumer = AudioConsumer {
        rx,
        events,
        control,
        frames: Vec::new(),
        position: 0,
        ended: false,
        channels,
        rate: config.sample_rate,
    };
    Ok(device.build_output_stream(
        *config,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            let timestamps = info.timestamp();
            let latency = timestamps
                .playback
                .saturating_duration_since(timestamps.callback);
            consumer.fill(out, latency);
        },
        move |error| {
            let _ = errors.send(Err(anyhow::anyhow!("audio device: {error}")));
        },
        None,
    )?)
}

fn run(path: &Path, control: Arc<PlaybackControl>, looped: bool) -> Result<()> {
    run_inner(path, control, looped, None)
}

fn run_hardware(
    path: &Path,
    control: Arc<PlaybackControl>,
    looped: bool,
    devices: DeviceHandle<ndsif_transport::NdsifSerialTransport>,
    plan: PlaybackPlan,
) -> Result<()> {
    let lease = devices.acquire(plan.device_ids.clone())?;
    let supports_master = crate::hardware::supports_master_volume(&lease)?;
    control.volume_mode.store(
        if supports_master {
            VolumeMode::Master as u8
        } else {
            VolumeMode::SoftwareOnly as u8
        },
        Ordering::Release,
    );
    let playback = if control.cancel.load(Ordering::Acquire) {
        Ok(())
    } else {
        run_inner(path, control, looped, Some((&lease, &plan)))
    };
    let cleanup = lease.release();
    match (playback, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.context("return NanoDrive devices")),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("return NanoDrive devices: {cleanup:#}")))
        }
    }
}

fn run_inner(
    path: &Path,
    control: Arc<PlaybackControl>,
    looped: bool,
    hardware: Option<(
        &DeviceLease<ndsif_transport::NdsifSerialTransport>,
        &PlaybackPlan,
    )>,
) -> Result<()> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device")?;
    let supported = device
        .default_output_config()
        .context("no default audio configuration")?;
    let config = supported.config();
    let (mut sequencer, hardware) = if let Some((lease, plan)) = hardware {
        let timeline = Arc::new(NdsifHostTimeline::pending());
        if control.volume_mode.load(Ordering::Acquire) == VolumeMode::Master as u8 {
            timeline
                .set_output_gain(Arc::clone(&control.volume))
                .map_err(anyhow::Error::msg)?;
        }
        control
            .hardware_timeline
            .set(Arc::clone(&timeline))
            .map_err(|_| anyhow::anyhow!("hardware timeline already configured"))?;
        if control.cancel.load(Ordering::Acquire) {
            timeline.cancel();
            return Ok(());
        }
        let recorders = crate::hardware::recorders(lease)?;
        let factories =
            ndsif_transport::create_routed_transport_factories(&recorders, &plan.routes)?;
        let sequencer = Sequencer::with_factories(path, config.sample_rate, looped, factories)?;
        let workers = crate::hardware::HardwareWorkers::start(lease, &recorders, timeline)?;
        (sequencer, Some((recorders, workers)))
    } else {
        (Sequencer::new(path, config.sample_rate, looped)?, None)
    };
    control
        .sample_rate
        .store(config.sample_rate, Ordering::Relaxed);
    let (tx, rx) = mpsc::sync_channel(QUEUE_BLOCKS);
    let (events, event_rx) = mpsc::channel();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => stream::<f32>(&device, &config, rx, events, control.clone())?,
        SampleFormat::F64 => stream::<f64>(&device, &config, rx, events, control.clone())?,
        SampleFormat::I8 => stream::<i8>(&device, &config, rx, events, control.clone())?,
        SampleFormat::I16 => stream::<i16>(&device, &config, rx, events, control.clone())?,
        SampleFormat::I32 => stream::<i32>(&device, &config, rx, events, control.clone())?,
        SampleFormat::I64 => stream::<i64>(&device, &config, rx, events, control.clone())?,
        SampleFormat::U8 => stream::<u8>(&device, &config, rx, events, control.clone())?,
        SampleFormat::U16 => stream::<u16>(&device, &config, rx, events, control.clone())?,
        SampleFormat::U32 => stream::<u32>(&device, &config, rx, events, control.clone())?,
        SampleFormat::U64 => stream::<u64>(&device, &config, rx, events, control.clone())?,
        other => bail!("unsupported audio sample format: {other:?}"),
    };
    let mut exhausted = false;
    let mut hardware_fade_scheduled = false;
    let mut prebuffered_frames = 0_usize;
    for _ in 0..4 {
        let block = sequencer.render()?;
        sequencer.schedule_hardware_fade(
            &control,
            config.sample_rate,
            &mut hardware_fade_scheduled,
        )?;
        if let Some((recorders, workers)) = &hardware {
            workers.enqueue(recorders, &control.cancel)?;
        }
        match block {
            Some(block) if !block.frames().is_empty() => {
                prebuffered_frames = prebuffered_frames
                    .checked_add(block.frames().len())
                    .context("audio prebuffer frame count overflow")?;
                tx.send(Block::Audio(block.frames().to_vec()))?
            }
            Some(_) => bail!("sequencer produced an empty block"),
            None => {
                tx.send(Block::End)?;
                exhausted = true;
                break;
            }
        }
    }
    if control.cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    if prebuffered_frames > 0
        && let Some((_, workers)) = &hardware
    {
        workers.prepare_audio(prebuffered_frames, config.sample_rate, &control.cancel)?;
    }
    if control.cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    stream.play()?;
    'playback: while !control.cancel.load(Ordering::Relaxed) {
        if let Some((_, workers)) = &hardware
            && let Err(error) = workers.check()
        {
            if control.cancel.load(Ordering::Acquire) {
                break;
            }
            return Err(error);
        }
        if let Ok(result) = event_rx.try_recv() {
            wait_for_audio_with_hardware(
                result?,
                &control,
                hardware.as_ref().map(|(_, workers)| workers),
            )?;
            break 'playback;
        }
        if !exhausted {
            let rendered = sequencer.render()?;
            sequencer.schedule_hardware_fade(
                &control,
                config.sample_rate,
                &mut hardware_fade_scheduled,
            )?;
            if let Some((recorders, workers)) = &hardware {
                workers.enqueue(recorders, &control.cancel)?;
            }
            let block = match rendered {
                Some(block) if !block.frames().is_empty() => Block::Audio(block.frames().to_vec()),
                Some(_) => bail!("sequencer produced an empty block"),
                None => {
                    exhausted = true;
                    Block::End
                }
            };
            let mut pending = block;
            loop {
                match tx.try_send(pending) {
                    Ok(()) => break,
                    Err(mpsc::TrySendError::Full(block)) => {
                        if control.cancel.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                        if let Ok(result) = event_rx.try_recv() {
                            wait_for_audio_with_hardware(
                                result?,
                                &control,
                                hardware.as_ref().map(|(_, workers)| workers),
                            )?;
                            break 'playback;
                        }
                        pending = block;
                        thread::sleep(Duration::from_millis(5));
                    }

                    Err(mpsc::TrySendError::Disconnected(_)) => bail!("audio output disconnected"),
                }
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
    drop(stream);
    drop(sequencer);
    if let Some((recorders, workers)) = hardware {
        drop(recorders);
        if control.cancel.load(Ordering::Acquire) || control.first_presentation.get().is_none() {
            if let Some(timeline) = control.hardware_timeline.get() {
                timeline.cancel();
            }
        }
        workers.finish()?;
    }
    Ok(())
}

#[cfg(test)]
fn wait_for_audio(deadline: Instant, control: &PlaybackControl) -> Result<()> {
    wait_for_audio_with_hardware(deadline, control, None)
}

fn wait_for_audio_with_hardware(
    deadline: Instant,
    control: &PlaybackControl,
    workers: Option<&crate::hardware::HardwareWorkers>,
) -> Result<()> {
    while (Instant::now() < deadline || control.paused.load(Ordering::Relaxed))
        && !control.cancel.load(Ordering::Relaxed)
    {
        if let Some(workers) = workers {
            workers.check()?;
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_presentation_uses_the_first_played_frame_and_device_latency() {
        let player = Playback::new();
        let control = player.control.clone();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
        let (tx, rx) = mpsc::channel();
        let (events, _event_rx) = mpsc::channel();
        tx.send(Block::Audio(vec![[0.1, 0.2], [0.3, 0.4]])).unwrap();
        let mut consumer = AudioConsumer {
            rx,
            events,
            control: control.clone(),
            frames: Vec::new(),
            position: 0,
            ended: false,
            channels: 2,
            rate: 44_100,
        };
        let base = Instant::now();
        let latency = Duration::from_millis(40);
        let mut out = [0.0_f32; 2];
        player.set_paused(true);
        consumer.fill_at(&mut out, base, latency);
        assert!(control.first_presentation.get().is_none());
        player.set_paused(false);
        let first_callback = base + Duration::from_millis(10);
        consumer.fill_at(&mut out, first_callback, latency);
        assert_eq!(
            control.first_presentation.get(),
            Some(&(first_callback + latency))
        );
        assert!(timeline.start(first_callback).is_err());
        consumer.fill_at(&mut out, base + Duration::from_secs(1), latency);
        assert_eq!(
            control.first_presentation.get(),
            Some(&(first_callback + latency))
        );
    }

    #[test]
    fn no_presentation_origin_is_set_for_end_or_invalid_audio() {
        for block in [Block::End, Block::Audio(vec![[f32::NAN, 0.0]])] {
            let player = Playback::new();
            let control = player.control.clone();
            let timeline = Arc::new(NdsifHostTimeline::pending());
            assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
            let (tx, rx) = mpsc::channel();
            let (events, _event_rx) = mpsc::channel();
            tx.send(block).unwrap();
            let mut consumer = AudioConsumer {
                rx,
                events,
                control: control.clone(),
                frames: Vec::new(),
                position: 0,
                ended: false,
                channels: 2,
                rate: 44_100,
            };
            consumer.fill_at(&mut [0.0_f32; 2], Instant::now(), Duration::ZERO);
            assert!(control.first_presentation.get().is_none());
            assert!(timeline.start(Instant::now()).is_ok());
        }
    }

    #[test]
    fn paused_or_cancelled_audio_does_not_start_hardware_timeline() {
        for cancelled in [false, true] {
            let player = Playback::new();
            let control = player.control.clone();
            let timeline = Arc::new(NdsifHostTimeline::pending());
            assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
            control.paused.store(!cancelled, Ordering::Relaxed);
            control.cancel.store(cancelled, Ordering::Relaxed);
            let (tx, rx) = mpsc::channel();
            let (events, _) = mpsc::channel();
            tx.send(Block::Audio(vec![[0.1, 0.2]])).unwrap();
            let mut consumer = AudioConsumer {
                rx,
                events,
                control,
                frames: Vec::new(),
                position: 0,
                ended: false,
                channels: 2,
                rate: 44_100,
            };
            consumer.fill_at(&mut [0.0_f32; 2], Instant::now(), Duration::ZERO);
            assert!(timeline.start(Instant::now()).is_ok());
        }
    }

    #[test]
    fn audio_pause_resume_volume_and_clock_preserve_queued_frames() {
        let player = Playback::new();
        let control = player.control.clone();
        control.sample_rate.store(2, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let (events, event_rx) = mpsc::channel();
        tx.send(Block::Audio(vec![[0.8, -0.4], [0.4, 0.2]]))
            .unwrap();
        tx.send(Block::End).unwrap();
        let mut consumer = AudioConsumer {
            rx,
            events,
            control: control.clone(),
            frames: Vec::new(),
            position: 0,
            ended: false,
            channels: 2,
            rate: 2,
        };
        let mut out = [0.0_f32; 2];
        player.set_paused(true);
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0., 0.]);
        assert_eq!(control.frames.load(Ordering::Relaxed), 0);
        player.set_volume(0.5).unwrap();
        player.set_paused(false);
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0.4, -0.2]);
        assert_eq!(player.elapsed_seconds(), 0);
        player.set_paused(true);
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0., 0.]);
        assert_eq!(control.frames.load(Ordering::Relaxed), 1);
        player.set_volume(0.).unwrap();
        player.set_paused(false);
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0., 0.]);
        assert_eq!(player.elapsed_seconds(), 1);
        consumer.fill(&mut out, Duration::ZERO);
        assert!(event_rx.try_recv().unwrap().is_ok());
        assert!(consumer.ended);
    }

    #[test]
    fn cancellation_outputs_silence_without_consuming_audio() {
        let mut player = Playback::new();
        let control = player.control.clone();
        let (tx, rx) = mpsc::channel();
        let (events, _) = mpsc::channel();
        tx.send(Block::Audio(vec![[0.5, 0.5]])).unwrap();
        let mut consumer = AudioConsumer {
            rx,
            events,
            control,
            frames: Vec::new(),
            position: 0,
            ended: false,
            channels: 2,
            rate: 44_100,
        };
        player.stop();
        let mut out = [1.; 2];
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0., 0.]);
        assert_eq!(consumer.position, 0);
        assert_eq!(player.elapsed_seconds(), 0);
    }

    #[test]
    fn paused_tail_does_not_finish_until_resume_or_cancel() {
        let player = Playback::new();
        let control = player.control.clone();
        player.set_paused(true);
        let (tx, rx) = mpsc::channel();
        let worker_control = control.clone();
        let worker = thread::spawn(move || {
            wait_for_audio(Instant::now(), &worker_control).unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        player.set_paused(false);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn replacing_track_cancels_old_backend_and_preserves_volume() {
        struct TestBackend(mpsc::Sender<(bool, Arc<PlaybackControl>)>);
        impl PlaybackBackend for TestBackend {
            fn play(&self, _: &Path, control: Arc<PlaybackControl>, looped: bool) -> Result<()> {
                self.0.send((looped, control.clone()))?;
                while !control.cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(1));
                }

                bail!("cancelled test backend")
            }
        }
        let (tx, rx) = mpsc::channel();
        let mut player = Playback::new();
        player.backend = Arc::new(TestBackend(tx));
        player.set_volume(0.25).unwrap();
        player.play(PathBuf::from("first.vgm"), true);
        let first_generation = player.generation;
        let (looped, first) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(looped);
        player.set_paused(true);
        assert!(first.paused.load(Ordering::Relaxed));
        player.play(PathBuf::from("second.vgm"), false);
        let (looped, second) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!looped);
        assert!(first.cancel.load(Ordering::Relaxed));
        assert!(!second.paused.load(Ordering::Relaxed));
        assert_eq!(second.volume.get(), 0.25);
        assert!(!player.is_current(first_generation));
        player.stop();
        assert!(second.cancel.load(Ordering::Relaxed));
        assert!(
            player
                .events
                .recv_timeout(Duration::from_millis(50))
                .is_err()
        );
    }

    #[test]
    fn replacing_track_waits_for_previous_worker_cleanup() {
        struct TestBackend {
            started: mpsc::Sender<String>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl PlaybackBackend for TestBackend {
            fn play(&self, path: &Path, control: Arc<PlaybackControl>, _: bool) -> Result<()> {
                self.started.send(path.display().to_string())?;
                while !control.cancel.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
                if path == Path::new("first") {
                    self.release
                        .lock()
                        .map_err(|_| anyhow::anyhow!("test lock poisoned"))?
                        .recv()?;
                }
                Ok(())
            }
        }
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut player = Playback::new();
        player.backend = Arc::new(TestBackend {
            started: started_tx,
            release: Mutex::new(release_rx),
        });
        player.play(PathBuf::from("first"), false);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "first"
        );
        player.play(PathBuf::from("second"), false);
        assert!(started_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release_tx.send(()).unwrap();
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "second"
        );
        player.stop();
    }

    #[test]
    fn loop_toggle_follows_vgm_and_mdx_song_loops() {
        use chipstream::sequencer::measure_mdx_duration_frames;
        use soundlog::vgm::command::WaitSamples;

        fn render_frames(player: &mut Sequencer) -> usize {
            let mut frames = 0;
            while let Some(block) = player.render().unwrap() {
                frames += block.frames().len();
                assert!(frames < 44_100 * 300, "playback did not end");
            }
            frames
        }

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("loop.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.set_version(0x171);
        builder.add_vgm_command(WaitSamples(1_000));
        builder.add_vgm_command(WaitSamples(3_000));
        builder.set_loop_index(1);
        let mut vgm_bytes = Vec::<u8>::from(builder.finalize());
        vgm_bytes[0x7e] = 1;
        vgm_bytes[0x7f] = 0x20;
        std::fs::write(&vgm, vgm_bytes).unwrap();
        let mdx = dir.path().join("loop.mdx");
        let mdx_bytes = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
        let pdx_bytes = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx");
        std::fs::write(&mdx, mdx_bytes).unwrap();
        std::fs::write(dir.path().join("mdx_fm_loop.pdx"), pdx_bytes).unwrap();
        let no_loop = dir.path().join("no-loop.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        std::fs::write(&no_loop, Vec::<u8>::from(builder.finalize())).unwrap();
        let no_loop_mdx = dir.path().join("no-loop.mdx");
        let mut builder = soundlog::mdx::document::MdxBuilder::new();
        builder.add_mdx_command(0, soundlog::mdx::command::MdxRest { ticks: 8 });
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        std::fs::write(&no_loop_mdx, package.to_mdx_bytes().unwrap()).unwrap();

        for rate in [44_100_u32, 48_000] {
            let mut finite_vgm = Sequencer::new(&vgm, rate, false).unwrap();
            let expected = ((7_000 + FADEOUT_SAMPLES) as u64 * u64::from(rate) + 22_050) / 44_100;
            assert_eq!(render_frames(&mut finite_vgm), expected as usize);
            let fade_start = (7_000_u64 * u64::from(rate) + 22_050) / 44_100;
            assert_eq!(finite_vgm.fade_start_frame(), Some(fade_start as usize));

            let package = MdxPackage::parse(mdx_bytes, Some(pdx_bytes)).unwrap();
            let options = MdxSequencerOptions {
                loop_count: Some(2),
                ..MdxSequencerOptions::default()
            };
            let expected =
                measure_mdx_duration_frames(package, rate, options, Some(FADEOUT_SAMPLES))
                    .unwrap()
                    .unwrap();
            let mut finite_mdx = Sequencer::new(&mdx, rate, false).unwrap();
            assert_eq!(render_frames(&mut finite_mdx), expected as usize);

            for path in [&vgm, &mdx] {
                let mut looped = Sequencer::new(path, rate, true).unwrap();
                for _ in 0..(expected as usize / CHUNK_FRAMES + 10) {
                    assert!(
                        looped.render().unwrap().is_some(),
                        "{} stopped while looping",
                        path.display()
                    );
                }
                assert_eq!(looped.fade_start_frame(), None);
            }
            for looped in [false, true] {
                let mut player = Sequencer::new(&no_loop, rate, looped).unwrap();
                let expected = (1_000_u64 * u64::from(rate) + 22_050) / 44_100;
                assert_eq!(render_frames(&mut player), expected as usize);
            }
            let mut finite = Sequencer::new(&no_loop_mdx, rate, false).unwrap();
            let mut infinite = Sequencer::new(&no_loop_mdx, rate, true).unwrap();
            assert_eq!(render_frames(&mut finite), render_frames(&mut infinite));
            assert_eq!(finite.fade_start_frame(), None);
            assert_eq!(infinite.fade_start_frame(), None);
        }
    }

    #[test]
    fn finite_vgm_fades_linearly_after_two_traversals() {
        use soundlog::chip::{Chip, PsgSpec};
        use soundlog::vgm::command::{Instance, WaitSamples};

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("loop.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
        for value in [0x80, 0x20, 0x90] {
            builder.add_chip_write(Instance::Primary, PsgSpec { value });
        }
        builder.set_loop_index(3);
        builder.add_vgm_command(WaitSamples(3_000));
        std::fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();

        let mut finite = Sequencer::new(&vgm, 44_100, false).unwrap();
        let mut samples = Vec::new();
        while let Some(block) = finite.render().unwrap() {
            samples.extend_from_slice(block.frames());
            assert!(samples.len() < 44_100 * 300, "playback did not end");
        }
        let fade_start = samples.len() - FADEOUT_SAMPLES as usize;
        assert_eq!(fade_start, 6_000);
        assert_eq!(finite.fade_start_frame(), Some(fade_start));
        let mut infinite = Sequencer::new(&vgm, 44_100, true).unwrap();
        let mut reference = Vec::new();
        while reference.len() < samples.len() {
            reference.extend_from_slice(infinite.render().unwrap().unwrap().frames());
        }
        let mut audible = 0;
        for (index, (actual, original)) in samples.iter().zip(&reference).enumerate() {
            let gain = if index < fade_start {
                1.0
            } else {
                1.0 - (index - fade_start) as f32 / (FADEOUT_SAMPLES - 1) as f32
            };
            for channel in 0..2 {
                if original[channel].abs() > 0.001 && index >= fade_start {
                    audible += 1;
                }
                assert!(
                    (actual[channel] - original[channel] * gain).abs() < 0.0001,
                    "frame {index}, channel {channel}, gain {gain}"
                );
            }
        }
        assert!(audible > 100, "fade test must contain audible samples");
    }

    #[test]
    fn finite_mdx_fades_linearly_after_two_traversals() {
        use chipstream::sequencer::measure_mdx_duration_frames;
        use soundlog::mdx::command::{
            MdxCommand, MdxNote, MdxPan, MdxRelativeOffset, MdxVoiceOrPcmBank,
        };
        use soundlog::mdx::tone::{MdxOperator, MdxTone};

        let dir = tempfile::tempdir().unwrap();
        let mdx = dir.path().join("loop.mdx");
        let mut builder = soundlog::mdx::document::MdxBuilder::new();
        builder
            .append_tone(MdxTone {
                voice_number: 0,
                con: 7,
                fl: 0,
                op: 0x78,
                operators: [MdxOperator {
                    ar: 31,
                    rr: 15,
                    ml: 1,
                    ..Default::default()
                }; 4],
            })
            .add_mdx_command(0, MdxVoiceOrPcmBank { value: 0 })
            .add_mdx_command(0, MdxPan::Center)
            .add_mdx_command(
                0,
                MdxNote {
                    note: 0x98,
                    length: 32,
                },
            )
            .add_mdx_command(
                0,
                MdxCommand::EndOfTrackLoop(MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: -5,
                }),
            );
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        std::fs::write(&mdx, package.to_mdx_bytes().unwrap()).unwrap();

        for rate in [44_100, 48_000] {
            let options = MdxSequencerOptions {
                loop_count: Some(2),
                ..MdxSequencerOptions::default()
            };
            let boundary = measure_mdx_duration_frames(package.clone(), rate, options, None)
                .unwrap()
                .unwrap() as usize;
            let mut finite = Sequencer::new(&mdx, rate, false).unwrap();
            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            let timeline = Arc::new(NdsifHostTimeline::pending());
            assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
            let mut scheduled = false;
            assert_eq!(finite.fade_start_frame(), None);
            let mut samples = Vec::new();
            while let Some(block) = finite.render().unwrap() {
                finite
                    .schedule_hardware_fade(&control, rate, &mut scheduled)
                    .unwrap();
                samples.extend_from_slice(block.frames());
                assert!(samples.len() < rate as usize * 300, "playback did not end");
            }
            assert_eq!(samples.len(), boundary + rate as usize * 2);
            assert_eq!(finite.fade_start_frame(), Some(boundary));
            assert!(scheduled);
            assert!(
                timeline
                    .schedule_output_fade(0, rate, Duration::from_secs(2))
                    .is_err()
            );

            let mut infinite = Sequencer::new(&mdx, rate, true).unwrap();
            let mut reference = Vec::new();
            while reference.len() < samples.len() {
                reference.extend_from_slice(infinite.render().unwrap().unwrap().frames());
            }
            let fade_frames = rate as usize * 2;
            let mut audible = 0;
            for (index, (actual, original)) in samples.iter().zip(&reference).enumerate() {
                let gain = if index < boundary {
                    1.0
                } else {
                    1.0 - (index - boundary) as f32 / (fade_frames - 1) as f32
                };
                for channel in 0..2 {
                    if original[channel].abs() > 0.001 && index >= boundary {
                        audible += 1;
                    }
                    assert!(
                        (actual[channel] - original[channel] * gain).abs() < 0.0001,
                        "rate {rate}, frame {index}, channel {channel}, gain {gain}"
                    );
                }
            }
            assert!(audible > 100, "fade test must contain audible samples");
        }
    }

    #[test]
    fn short_mdx_schedules_hardware_fade_during_prebuffer() {
        use soundlog::mdx::command::{MdxCommand, MdxRelativeOffset, MdxRest};

        let dir = tempfile::tempdir().unwrap();
        let mdx = dir.path().join("short.mdx");
        let mut builder = soundlog::mdx::document::MdxBuilder::new();
        builder
            .add_mdx_command(0, MdxRest { ticks: 1 })
            .add_mdx_command(
                0,
                MdxCommand::EndOfTrackLoop(MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: -4,
                }),
            );
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        std::fs::write(&mdx, package.to_mdx_bytes().unwrap()).unwrap();

        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let timeline = Arc::new(NdsifHostTimeline::pending());
        assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
        let mut player = Sequencer::new(&mdx, 44_100, false).unwrap();
        let mut scheduled = false;
        for _ in 0..4 {
            assert!(player.render().unwrap().is_some());
            player
                .schedule_hardware_fade(&control, 44_100, &mut scheduled)
                .unwrap();
        }
        assert!(scheduled);
        assert!(player.fade_start_frame().unwrap() < 4 * CHUNK_FRAMES);
        assert!(
            timeline
                .schedule_output_fade(0, 44_100, Duration::from_secs(2))
                .is_err()
        );
    }

    #[test]
    fn short_vgm_schedules_hardware_fade_during_prebuffer() {
        use soundlog::vgm::command::WaitSamples;

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("short.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.set_loop_index(0);
        builder.add_vgm_command(WaitSamples(100));
        std::fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let timeline = Arc::new(NdsifHostTimeline::pending());
        control
            .hardware_timeline
            .set(Arc::clone(&timeline))
            .unwrap_or_else(|_| panic!("timeline already set"));
        let mut player = Sequencer::new(&vgm, 44_100, false).unwrap();
        let mut scheduled = false;
        for _ in 0..4 {
            player.render().unwrap().unwrap();
            player
                .schedule_hardware_fade(&control, 44_100, &mut scheduled)
                .unwrap();
        }
        assert!(scheduled);
        assert_eq!(player.fade_start_frame(), Some(200));
        assert!(
            timeline
                .schedule_output_fade(0, 44_100, Duration::from_secs(2))
                .is_err()
        );

        let no_loop = dir.path().join("no-loop.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(100));
        std::fs::write(&no_loop, Vec::<u8>::from(builder.finalize())).unwrap();
        for (path, looped) in [(&vgm, true), (&no_loop, false)] {
            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            let timeline = Arc::new(NdsifHostTimeline::pending());
            assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
            let mut player = Sequencer::new(path, 44_100, looped).unwrap();
            let mut scheduled = false;
            for _ in 0..4 {
                if player.render().unwrap().is_none() {
                    break;
                }
                player
                    .schedule_hardware_fade(&control, 44_100, &mut scheduled)
                    .unwrap();
            }
            assert!(!scheduled);
            assert!(
                timeline
                    .schedule_output_fade(0, 44_100, Duration::from_secs(2))
                    .is_ok()
            );
        }
    }

    #[test]
    fn sequencers_render_vgm_and_mdx() {
        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("song.vgm");
        std::fs::write(
            &vgm,
            include_bytes!("../../soundlog/assets/vgm/ym2612_a4.vgm"),
        )
        .unwrap();
        let mdx = dir.path().join("song.mdx");
        std::fs::write(
            &mdx,
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("mdx_fm_loop.pdx"),
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx"),
        )
        .unwrap();
        for path in [vgm, mdx] {
            let mut sequencer = Sequencer::new(&path, 44_100, false).unwrap();
            let block = sequencer.render().unwrap().expect("expected PCM");
            assert!(!block.frames().is_empty());
            assert!(
                block
                    .frames()
                    .iter()
                    .flatten()
                    .all(|sample| sample.is_finite())
            );
        }
    }

    #[test]
    fn sequencers_use_supplied_factories_and_propagate_errors() {
        use chipstream::chip::ChipError;
        use chipstream::sequencer::Chip;

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("song.vgm");
        std::fs::write(
            &vgm,
            include_bytes!("../../soundlog/assets/vgm/ym2612_a4.vgm"),
        )
        .unwrap();
        let mdx = dir.path().join("song.mdx");
        std::fs::write(
            &mdx,
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("mdx_fm_loop.pdx"),
            include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.pdx"),
        )
        .unwrap();
        for (path, chip) in [(vgm, Chip::Ym2612), (mdx, Chip::Ym2151)] {
            let mut factories = ChipFactoryRegistry::new();
            factories.register_optional(chip.clone(), move |_| {
                Err(ChipError::External {
                    chip: chip.clone(),
                    message: "injected factory failure".into(),
                })
            });
            let error = match Sequencer::with_factories(&path, 44_100, false, factories) {
                Ok(_) => panic!("{} unexpectedly used emulation", path.display()),
                Err(error) => error,
            };
            assert!(format!("{error:#}").contains("injected factory failure"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn routed_render_records_before_device_io_and_returns_the_connection() {
        run_recorded_routing(false);
    }

    #[cfg(unix)]
    #[test]
    fn recorded_device_error_stops_replay_and_returns_the_connection() {
        run_recorded_routing(true);
    }

    #[cfg(unix)]
    fn run_recorded_routing(reject_clock: bool) {
        use crate::device::{DeviceEvent, DeviceService};
        use crate::routing::{
            ChipInstance, Connection, Device, HardwareChip, Profile, RoutingConfig,
        };
        use chipstream::sequencer::{Chip, Instance};
        use ndsif::{Capabilities, Decoder, Reply, Request, Response};
        use ndsif_transport::{NdsifRecordingTransport, create_routed_transport_factories};
        use serialport::SerialPort;
        use soundlog::chip::Ym2151Spec;
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};
        use std::cell::RefCell;
        use std::io::{Read, Write};
        use std::num::NonZeroUsize;
        use std::rc::Rc;

        let (mut fake_device, port) = serialport::TTYPort::pair().unwrap();
        let path = port.name().unwrap();
        fake_device.set_timeout(Duration::from_secs(2)).unwrap();
        let (release_device, wait_for_release) = mpsc::channel();
        let (device_started, device_started_rx) = mpsc::channel();
        let device_worker = thread::spawn(move || {
            let mut decoder = Decoder::new();
            for index in 0..if reject_clock { 5 } else { 6 } {
                let mut frame = None;
                while frame.is_none() {
                    let mut byte = [0];
                    fake_device.read_exact(&mut byte).unwrap();
                    decoder.push(&byte, |result| frame = Some(result.unwrap()));
                }
                let frame = frame.unwrap();
                if index == 3 {
                    device_started.send(()).unwrap();
                }
                let reply = match (index, Request::decode(&frame).unwrap()) {
                    (0, Request::Ping(data)) => Some(Reply::Ping(data)),
                    (1, Request::GetCapabilities { offset: 0 }) => {
                        let entries = &[0x54, 0, 0, 4, 2, 0, 0, 0, 0];
                        Some(Reply::Capabilities(
                            Capabilities::new(256, 0, 0, 0, u16::MAX, entries).unwrap(),
                        ))
                    }
                    (2, Request::GetInfo) => Some(Reply::Info(ndsif::DeviceInfo {
                        model: "Test NanoDrive",
                        firmware: "test",
                    })),
                    (3, Request::SetChipClock { hz: 4_000_000, .. }) => Some(if reject_clock {
                        Reply::Rejected
                    } else {
                        Reply::Complete
                    }),
                    (4, Request::WriteRegisters { writes, .. }) if !reject_clock => {
                        assert_eq!(writes.as_bytes(), &[0x20, 0xc7]);
                        None
                    }
                    (4, Request::Reset) if reject_clock => Some(Reply::Complete),
                    (5, Request::Reset) => Some(Reply::Complete),
                    (_, request) => panic!("unexpected request {index}: {request:?}"),
                };
                if let Some(reply) = reply {
                    let response = Response::for_request(&frame, reply)
                        .unwrap()
                        .to_frame()
                        .unwrap()
                        .encode();
                    fake_device.write_all(response.as_bytes()).unwrap();
                }
            }
            wait_for_release.recv().unwrap();
        });

        let device = Device {
            id: 7,
            name: "Test NanoDrive".into(),
            port: path,
            baud_rate: crate::device::BAUD_RATE,
        };
        let config = RoutingConfig {
            devices: vec![device.clone()],
            profiles: vec![Profile {
                id: 3,
                name: "Test".into(),
                connections: vec![Connection {
                    chip: HardwareChip::Ym2151,
                    source: ChipInstance::Primary,
                    device_id: device.id,
                    target: ChipInstance::Primary,
                }],
            }],
            selected_profile: Some(3),
        };
        let plan = config.playback_plan().unwrap().unwrap();
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(7, _, _)
        ));
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("song.vgm");
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        builder.add_vgm_command(WaitSamples(441));
        std::fs::write(&song, Vec::<u8>::from(builder.finalize())).unwrap();

        let handle = service.playback_handle();
        thread::spawn(move || {
            let lease = handle.acquire(plan.device_ids).unwrap();
            let recorder = Rc::new(RefCell::new(NdsifRecordingTransport::new(
                &lease.devices()[0].1.lock().unwrap(),
                NonZeroUsize::new(4096).unwrap(),
            )));
            let factories =
                create_routed_transport_factories(std::slice::from_ref(&recorder), &plan.routes)
                    .unwrap();
            let mut sequencer = Sequencer::with_factories(&song, 44_100, false, factories).unwrap();
            let mut batches = Vec::new();
            loop {
                let block = sequencer.render().unwrap();
                let batch = recorder.borrow_mut().take_batch().unwrap();
                if !batch.is_empty() {
                    batches.push(batch);
                }
                if block.is_none() {
                    break;
                }
            }
            drop(sequencer);
            assert!(
                device_started_rx
                    .recv_timeout(Duration::from_millis(100))
                    .is_err(),
                "rendering must not send commands to the device"
            );
            let mut error = None;
            for batch in batches {
                if let Err(failure) = batch.replay(&mut *lease.devices()[0].1.lock().unwrap()) {
                    error = Some(failure);
                    break;
                }
            }
            if reject_clock {
                let error = error.expect("a rejected clock must stop replay");
                assert_eq!(error.index, 0);
                assert!(error.message.contains("rejected"));
            } else {
                assert!(error.is_none());
            }
            device_started_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            lease.release().unwrap();
        })
        .join()
        .unwrap();
        release_device.send(()).unwrap();
        device_worker.join().unwrap();
        drop(port);
    }

    #[test]
    fn pdx_lookup_reports_missing_and_ambiguous_files() {
        let dir = tempfile::tempdir().unwrap();
        let mdx = dir.path().join("song.mdx");
        assert!(resolve_pdx(&mdx, "sample").is_err());
        std::fs::write(dir.path().join("sample.PDX"), []).unwrap();
        assert_eq!(
            resolve_pdx(&mdx, "sample").unwrap(),
            dir.path().join("sample.PDX")
        );
        std::fs::write(dir.path().join("sample"), []).unwrap();
        assert!(resolve_pdx(&mdx, "sample").is_err());
    }
}
