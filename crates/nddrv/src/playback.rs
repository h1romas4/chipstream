use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chipstream::audio::StereoBlock;
use chipstream::sequencer::{MdxSequencer, MdxSequencerOptions, VgmSequencer};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;

use crate::library::{extension, read_track};

const CHUNK_FRAMES: usize = 1024;
const QUEUE_BLOCKS: usize = 8;

pub enum Event {
    Finished(u64),
    Error(u64, String),
}

pub struct Playback {
    control: Arc<PlaybackControl>,
    volume: Arc<AtomicU32>,
    generation: u64,
    backend: Arc<dyn PlaybackBackend>,
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
    volume: Arc<AtomicU32>,
}

impl PlaybackControl {
    fn new(volume: Arc<AtomicU32>) -> Self {
        Self {
            cancel: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            volume,
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
        let volume = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        Self {
            control: Arc::new(PlaybackControl::new(volume.clone())),
            volume,
            generation: 0,
            backend: Arc::new(EmulationBackend),
            sender,
            events,
        }
    }

    pub fn play(&mut self, path: PathBuf, looped: bool) {
        self.stop();
        self.control = Arc::new(PlaybackControl::new(self.volume.clone()));
        let control = self.control.clone();
        let sender = self.sender.clone();
        let backend = self.backend.clone();
        let generation = self.generation;
        thread::spawn(move || {
            let result = backend.play(&path, control.clone(), looped);
            if !control.cancel.load(Ordering::Relaxed) {
                let event = match result {
                    Ok(()) => Event::Finished(generation),
                    Err(error) => Event::Error(generation, format!("{error:#}")),
                };
                let _ = sender.send(event);
            }
        });
    }

    pub fn stop(&mut self) {
        self.control.cancel.store(true, Ordering::Relaxed);
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn set_paused(&self, paused: bool) {
        self.control.paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0., 1.).to_bits(), Ordering::Relaxed);
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
                loop_count: if looped { None } else { Some(1) },
                max_ticks: if looped {
                    None
                } else {
                    MdxSequencerOptions::default().max_ticks
                },
                ..MdxSequencerOptions::default()
            };
            Ok(Self::Mdx(MdxSequencer::with_options(
                package,
                rate,
                CHUNK_FRAMES,
                options,
            )?))
        } else {
            let mut sequencer = VgmSequencer::from_vgm(bytes, rate, CHUNK_FRAMES)?;
            sequencer.set_loop_count(if looped { None } else { Some(1) });
            Ok(Self::Vgm(sequencer))
        }
    }

    fn render(&mut self) -> Result<Option<StereoBlock>> {
        match self {
            Self::Vgm(player) => Ok(player.render()?),
            Self::Mdx(player) => Ok(player.render()?),
        }
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
        out.fill(T::from_sample(0.0));
        if self.ended
            || self.control.paused.load(Ordering::Relaxed)
            || self.control.cancel.load(Ordering::Relaxed)
        {
            return;
        }
        let volume = f32::from_bits(self.control.volume.load(Ordering::Relaxed));
        let mut played = 0;
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
                        let _ = self.events.send(Ok(Instant::now() + latency + remaining));
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
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device")?;
    let supported = device
        .default_output_config()
        .context("no default audio configuration")?;
    let config = supported.config();
    let mut sequencer = Sequencer::new(path, config.sample_rate, looped)?;
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
    for _ in 0..4 {
        match sequencer.render()? {
            Some(block) if !block.frames().is_empty() => {
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
    stream.play()?;
    while !control.cancel.load(Ordering::Relaxed) {
        if let Ok(result) = event_rx.try_recv() {
            return wait_for_audio(result?, &control);
        }
        if !exhausted {
            let block = match sequencer.render()? {
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
                            return wait_for_audio(result?, &control);
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
    Ok(())
}

fn wait_for_audio(deadline: Instant, control: &PlaybackControl) -> Result<()> {
    while (Instant::now() < deadline || control.paused.load(Ordering::Relaxed))
        && !control.cancel.load(Ordering::Relaxed)
    {
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        player.set_volume(0.5);
        player.set_paused(false);
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0.4, -0.2]);
        assert_eq!(player.elapsed_seconds(), 0);
        player.set_paused(true);
        consumer.fill(&mut out, Duration::ZERO);
        assert_eq!(out, [0., 0.]);
        assert_eq!(control.frames.load(Ordering::Relaxed), 1);
        player.set_volume(0.);
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
        player.set_volume(0.25);
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
        assert_eq!(f32::from_bits(second.volume.load(Ordering::Relaxed)), 0.25);
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
    fn loop_toggle_follows_vgm_and_mdx_song_loops() {
        use soundlog::vgm::command::WaitSamples;
        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("loop.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        builder.add_vgm_command(WaitSamples(3_000));
        builder.set_loop_index(1);
        std::fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();
        let mdx = dir.path().join("loop.mdx");
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
            let mut once = Sequencer::new(&path, 44_100, false).unwrap();
            let mut count = 0;
            while once.render().unwrap().is_some() {
                count += 1;
                assert!(count < 1000, "{} did not end", path.display());
            }
            assert!(count > 0);
            let mut looped = Sequencer::new(&path, 44_100, true).unwrap();
            for _ in 0..(count * 2 + 5) {
                assert!(
                    looped.render().unwrap().is_some(),
                    "{} stopped while looping",
                    path.display()
                );
            }
        }
        let no_loop = dir.path().join("no-loop.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        std::fs::write(&no_loop, Vec::<u8>::from(builder.finalize())).unwrap();
        let mut player = Sequencer::new(&no_loop, 44_100, true).unwrap();
        let mut ended = false;
        for _ in 0..10 {
            if player.render().unwrap().is_none() {
                ended = true;
                break;
            }
        }
        assert!(ended, "a file without a loop point should still end");
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
