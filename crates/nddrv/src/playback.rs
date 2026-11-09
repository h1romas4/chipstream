use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chipstream::audio::StereoBlock;
use chipstream::sequencer::{
    Chip, ChipFactoryRegistry, MdxSequencer, MdxSequencerOptions, VgmSequencer,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ndsif_transport::{NdsifHostTimeline, NdsifOutputGain};
use soundlog::chip::event::StateEvent;
use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::convert::{to_vgm_stream_generator, to_vgm_stream_generator_with_loop_marker};
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::command::{Instance, VgmCommand};
use soundlog::vgm::{VGM_SAMPLE_RATE, VgmCallbackStream, stream::StreamResult};

use crate::device::{DeviceHandle, DeviceLease};
use crate::fm_mute::FmMute;
use crate::library::{
    PlaybackSettings, RecordingSettings, VgmSettings, extension, read_bounded, read_track,
};
use crate::routing::{PlaybackPlan, RoutingPlan};
use crate::visualizer::{PianoRoll, RollMode, Snapshot, WINDOW_SECONDS};

const CHUNK_FRAMES: usize = 1024;
const QUEUE_BLOCKS: usize = 16;
const PREBUFFER_BLOCKS: usize = 8;
#[cfg(test)]
const FADEOUT_SAMPLES: u32 = 2 * 44_100;
const YM2608_RHYTHM_ROM: &str = "ym2608_adpcm_rom.bin";

pub enum Event {
    Finished(u64),
    Error(u64, String),
    RecordingSaved(crate::recording::SavedRecording),
    RecordingError(String),
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
    settings: PlaybackSettings,
    vgm_settings: VgmSettings,
    audio_output_device: Option<String>,
    audio_sample_rate: Option<u32>,
    recording: Option<RecordingSettings>,
    volume: Arc<NdsifOutputGain>,
    gains: Arc<GainState>,
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

fn record_and_play<C>(
    recording: Option<RecordingSettings>,
    control: &PlaybackControl,
    sender: &mpsc::Sender<Event>,
    start: impl FnOnce(&RecordingSettings) -> Result<C>,
    play: impl FnOnce() -> Result<()>,
    finish: impl FnOnce(C) -> Result<Option<crate::recording::SavedRecording>>,
) -> Result<()> {
    if control.cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    let capture = recording
        .map(|settings| start(&settings))
        .transpose()
        .context("could not start recording")?;
    let playback = if control.cancel.load(Ordering::Acquire) {
        Ok(())
    } else {
        play()
    };
    match capture.map(finish).transpose() {
        Ok(Some(Some(path))) => {
            let _ = sender.send(Event::RecordingSaved(path));
            playback
        }
        Ok(Some(None)) if !control.cancel.load(Ordering::Acquire) && playback.is_ok() => {
            bail!("audio input produced no recording samples")
        }
        Ok(_) => playback,
        Err(error) if control.cancel.load(Ordering::Acquire) => {
            let _ = sender.send(Event::RecordingError(format!("{error:#}")));
            playback
        }
        Err(error) => Err(error.context("could not save recording")),
    }
}

impl PlaybackBackend for EmulationBackend {
    fn play(&self, path: &Path, control: Arc<PlaybackControl>, looped: bool) -> Result<()> {
        run(path, control, looped)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum GainPart {
    Chip,
    Fm,
    Ssg,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GainSource {
    pub chip: Chip,
    pub instance: Instance,
    pub hardware: bool,
    pub visual_only: bool,
    pub target: Option<crate::routing::Target>,
    pub target_device_name: Option<String>,
}

#[derive(Default)]
struct GainState {
    values: Mutex<HashMap<(Chip, GainPart), f32>>,
    revision: AtomicU64,
}

struct PlaybackControl {
    settings: PlaybackSettings,
    vgm_settings: VgmSettings,
    audio_output_device: Option<String>,
    audio_sample_rate: Option<u32>,
    cancel: AtomicBool,
    recording_failed: Arc<AtomicBool>,
    paused: AtomicBool,
    frames: AtomicU64,
    sample_rate: AtomicU32,
    visual_clock: PresentationClock,
    silence_clock: PresentationClock,
    visualizer: Mutex<PianoRoll>,
    preview: Mutex<PreviewState>,
    volume: Arc<NdsifOutputGain>,
    gains: Arc<GainState>,
    fm_mute: Arc<FmMute>,
    gain_sources: Mutex<Option<Vec<GainSource>>>,
    volume_mode: AtomicU8,
    first_presentation: OnceLock<Instant>,
    hardware_timeline: OnceLock<Arc<NdsifHostTimeline>>,
}

impl PlaybackControl {
    #[cfg(test)]
    fn new(volume: Arc<NdsifOutputGain>) -> Self {
        Self::with_gains(volume, Arc::new(GainState::default()))
    }

    fn with_gains(volume: Arc<NdsifOutputGain>, gains: Arc<GainState>) -> Self {
        Self {
            settings: PlaybackSettings::default(),
            vgm_settings: VgmSettings::default(),
            audio_output_device: None,
            audio_sample_rate: None,
            cancel: AtomicBool::new(false),
            recording_failed: Arc::new(AtomicBool::new(false)),
            paused: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            visual_clock: PresentationClock::new(),
            silence_clock: PresentationClock::new(),
            visualizer: Mutex::new(PianoRoll::default()),
            preview: Mutex::new(PreviewState::default()),
            volume,
            gains,
            fm_mute: Arc::new(FmMute::default()),
            gain_sources: Mutex::new(None),
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

struct PresentationClock {
    epoch: Instant,
    sequence: AtomicU64,
    start_frame: AtomicU64,
    end_frame: AtomicU64,
    start_nanos: AtomicU64,
}

impl PresentationClock {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            sequence: AtomicU64::new(0),
            start_frame: AtomicU64::new(0),
            end_frame: AtomicU64::new(0),
            start_nanos: AtomicU64::new(0),
        }
    }

    fn publish(&self, start: u64, count: u64, presentation: Instant) -> Result<()> {
        let nanos = u64::try_from(
            presentation
                .saturating_duration_since(self.epoch)
                .as_nanos(),
        )
        .context("audio presentation time overflow")?;
        let end = start
            .checked_add(count)
            .context("audio frame count overflow")?;
        self.sequence.fetch_add(1, Ordering::AcqRel);
        self.start_frame.store(start, Ordering::Relaxed);
        self.end_frame.store(end, Ordering::Relaxed);
        self.start_nanos.store(nanos, Ordering::Relaxed);
        self.sequence.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn position(&self, now: Instant, rate: u32) -> Result<u64> {
        if rate == 0 {
            return Ok(0);
        }
        for _ in 0..8 {
            let before = self.sequence.load(Ordering::Acquire);
            if before == 0 {
                return Ok(0);
            }
            if before % 2 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let start = self.start_frame.load(Ordering::Relaxed);
            let end = self.end_frame.load(Ordering::Relaxed);
            let at = self.start_nanos.load(Ordering::Relaxed);
            if before != self.sequence.load(Ordering::Acquire) {
                continue;
            }
            let now = now.saturating_duration_since(self.epoch).as_nanos();
            let at = u128::from(at);
            let distance = now.abs_diff(at) * u128::from(rate) / 1_000_000_000;
            let distance = u64::try_from(distance).unwrap_or(u64::MAX);
            return Ok(if now < at {
                start.saturating_sub(distance)
            } else {
                start.saturating_add(distance).min(end)
            });
        }
        bail!("piano roll presentation clock is busy")
    }
}

impl Playback {
    pub fn new() -> Self {
        let (sender, events) = mpsc::channel();
        let volume = Arc::new(NdsifOutputGain::new());
        let gains = Arc::new(GainState::default());
        Self {
            control: Arc::new(PlaybackControl::with_gains(volume.clone(), gains.clone())),
            settings: PlaybackSettings::default(),
            vgm_settings: VgmSettings::default(),
            audio_output_device: None,
            audio_sample_rate: None,
            recording: None,
            volume,
            gains,
            generation: 0,
            backend: Arc::new(EmulationBackend),
            worker_gate: Arc::new(Mutex::new(())),
            sender,
            events,
        }
    }

    pub fn play(&mut self, path: PathBuf, looped: bool, preview_mute: Option<&FmMute>) {
        self.start(path, looped, None, preview_mute);
    }

    pub fn set_settings(&mut self, settings: PlaybackSettings) {
        self.settings = settings;
    }

    pub fn set_vgm_settings(&mut self, settings: VgmSettings) {
        self.vgm_settings = settings;
    }

    pub fn active_vgm_settings(&self) -> VgmSettings {
        self.control.vgm_settings
    }

    pub fn set_audio_output_device(&mut self, device_id: Option<String>) {
        self.audio_output_device = device_id;
    }

    pub fn set_audio_sample_rate(&mut self, sample_rate: Option<u32>) {
        self.audio_sample_rate = sample_rate;
    }

    pub fn set_recording(&mut self, recording: Option<RecordingSettings>) {
        self.recording = recording;
    }

    pub fn play_hardware(
        &mut self,
        path: PathBuf,
        looped: bool,
        devices: DeviceHandle<ndsif_transport::NdsifSerialTransport>,
        plan: PlaybackPlan,
        preview_mute: Option<&FmMute>,
    ) {
        self.start(path, looped, Some((devices, plan)), preview_mute);
    }

    fn start(
        &mut self,
        path: PathBuf,
        looped: bool,
        hardware: Option<(
            DeviceHandle<ndsif_transport::NdsifSerialTransport>,
            PlaybackPlan,
        )>,
        preview_mute: Option<&FmMute>,
    ) {
        self.stop();
        let mut control = PlaybackControl::with_gains(self.volume.clone(), self.gains.clone());
        control.settings = self.settings;
        control.vgm_settings = self.vgm_settings;
        control.audio_output_device = self.audio_output_device.clone();
        control.audio_sample_rate = self.audio_sample_rate;
        self.control = Arc::new(control);
        if let Some(preview_mute) = preview_mute {
            self.control.fm_mute.copy_selections_from(preview_mute);
        }
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
        let recording = self.recording.clone();
        thread::spawn(move || {
            let result = match worker_gate.lock() {
                Ok(_guard) if control.cancel.load(Ordering::Acquire) => Ok(()),
                Ok(_guard) => record_and_play(
                    recording,
                    &control,
                    &sender,
                    |settings| {
                        crate::recording::Capture::start(
                            settings,
                            &path,
                            control.recording_failed.clone(),
                        )
                    },
                    || match hardware {
                        Some((devices, plan)) => {
                            run_hardware(&path, control.clone(), looped, devices, plan)
                        }
                        None => backend.play(&path, control.clone(), looped),
                    },
                    crate::recording::Capture::finish,
                ),
                Err(_) => Err(anyhow::anyhow!("previous playback worker failed")),
            };
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
        self.control.cancel.store(true, Ordering::Release);
        if let Some(timeline) = self.control.hardware_timeline.get() {
            timeline.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn stop_and_wait(&mut self) {
        self.stop();
        let _guard = self.worker_gate.lock();
    }

    pub fn set_paused(&self, paused: bool) {
        self.control.paused.store(paused, Ordering::Relaxed);
    }

    pub fn set_volume(&self, volume: f32) -> Result<()> {
        self.volume.set(volume).map_err(anyhow::Error::msg)
    }

    pub fn set_chip_gain(&self, chip: Chip, part: GainPart, gain: f32) -> Result<()> {
        if !gain.is_finite() || !(0.0..=1.0).contains(&gain) {
            bail!("chip gain must be between 0% and 100%");
        }
        let mut values = self
            .gains
            .values
            .lock()
            .map_err(|_| anyhow::anyhow!("chip gain state is unavailable"))?;
        values.insert((chip, part), gain);
        self.gains.revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub fn fm_mute(&self) -> &FmMute {
        &self.control.fm_mute
    }

    pub fn gain_sources(&self) -> Result<Option<Vec<GainSource>>> {
        self.control
            .gain_sources
            .lock()
            .map(|sources| sources.clone())
            .map_err(|_| anyhow::anyhow!("chip gain sources are unavailable"))
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

    pub fn waiting_for_start(&self) -> bool {
        self.waiting_for_start_at(Instant::now())
    }

    fn waiting_for_start_at(&self, now: Instant) -> bool {
        self.control.settings.start_delay_seconds > 0
            && self
                .control
                .first_presentation
                .get()
                .is_none_or(|presentation| now < *presentation)
    }

    pub fn visual_snapshot(&self, mode: RollMode) -> Result<Snapshot> {
        self.visual_snapshot_at(Instant::now(), mode)
    }

    pub fn visual_snapshot_with_preview(
        &self,
        mode: RollMode,
        preview: Option<&IdlePreview>,
    ) -> Result<Snapshot> {
        let live = self.visual_snapshot(mode)?;
        if mode == RollMode::Incoming
            && (live.rate == 0 || live.preparing)
            && let Some(preview) = preview
        {
            return preview.visual_snapshot_for_playback(&live);
        }
        Ok(live)
    }

    fn visual_snapshot_at(&self, now: Instant, mode: RollMode) -> Result<Snapshot> {
        let rate = self.control.sample_rate.load(Ordering::Relaxed);
        let position = self.control.visual_clock.position(now, rate)?;
        let started = self
            .control
            .first_presentation
            .get()
            .is_some_and(|presentation| now >= *presentation);
        let (notes, scanned, incomplete, preparing, scanning) = match mode {
            RollMode::Trail => {
                let mut visualizer = self
                    .control
                    .visualizer
                    .lock()
                    .map_err(|_| anyhow::anyhow!("piano roll state is unavailable"))?;
                let incomplete = visualizer.incomplete();
                let mut notes = if started {
                    visualizer.snapshot(position, rate, mode)
                } else {
                    Vec::new()
                };
                drop(visualizer);
                let has_ay = self
                    .control
                    .gain_sources
                    .lock()
                    .map_err(|_| anyhow::anyhow!("chip gain sources are unavailable"))?
                    .as_ref()
                    .is_some_and(|sources| {
                        sources.iter().any(|source| source.chip == Chip::Ay8910)
                    });
                let mut incomplete = incomplete;
                if has_ay {
                    let mut preview = self
                        .control
                        .preview
                        .lock()
                        .map_err(|_| anyhow::anyhow!("piano roll preview is unavailable"))?;
                    if let Some(error) = &preview.error {
                        bail!("piano roll preview: {error}");
                    }
                    if started {
                        notes.extend(
                            preview
                                .roll
                                .snapshot(position, rate, mode)
                                .into_iter()
                                .filter(|note| note.chip == crate::visualizer::AY8910_INDEX),
                        );
                    }
                    incomplete |= preview.roll.incomplete();
                }
                (notes, position, incomplete, false, false)
            }
            RollMode::Incoming => {
                let mut preview = self
                    .control
                    .preview
                    .lock()
                    .map_err(|_| anyhow::anyhow!("piano roll preview is unavailable"))?;
                if let Some(error) = &preview.error {
                    bail!("piano roll preview: {error}");
                }
                let incomplete = preview.roll.incomplete();
                let preparing = !preview.done
                    && (rate == 0 || preview.scanned < position.saturating_add(u64::from(rate)));
                let scanning = !preview.done
                    && (rate == 0
                        || preview.scanned
                            < position.saturating_add(u64::from(rate) * WINDOW_SECONDS));
                (
                    preview.roll.snapshot(position, rate, mode),
                    preview.scanned,
                    incomplete,
                    preparing,
                    scanning,
                )
            }
        };
        let leading_silence = if mode == RollMode::Incoming {
            let total = u64::from(self.control.settings.start_delay_seconds) * u64::from(rate);
            total.saturating_sub(self.control.silence_clock.position(now, rate)?)
        } else {
            0
        };
        Ok(Snapshot {
            position,
            rate,
            notes,
            scanned,
            incomplete,
            started,
            preparing,
            scanning,
            ..Snapshot::default()
        }
        .with_leading_silence(leading_silence))
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

type VisualEvents = Rc<RefCell<Vec<(u8, Instance, usize, Vec<StateEvent>)>>>;

#[derive(Default)]
struct PreviewState {
    roll: PianoRoll,
    scanned: u64,
    done: bool,
    error: Option<String>,
}

pub struct IdlePreview {
    control: Arc<PlaybackControl>,
}

impl IdlePreview {
    #[cfg(test)]
    pub fn start(path: PathBuf, looped: bool, plan: Option<&PlaybackPlan>) -> Result<Self> {
        Self::start_with_settings(
            path,
            looped,
            plan,
            PlaybackSettings::default(),
            VgmSettings::default(),
        )
    }

    pub fn start_with_settings(
        path: PathBuf,
        looped: bool,
        plan: Option<&PlaybackPlan>,
        settings: PlaybackSettings,
        vgm_settings: VgmSettings,
    ) -> Result<Self> {
        let mut control = PlaybackControl::with_gains(
            Arc::new(NdsifOutputGain::new()),
            Arc::new(GainState::default()),
        );
        control.settings = settings;
        control.vgm_settings = vgm_settings;
        let control = Arc::new(control);
        let rate = VGM_SAMPLE_RATE;
        control.sample_rate.store(rate, Ordering::Release);
        let routes = plan.map_or_else(Vec::new, |plan| plan.routes.clone());
        let plan_for_preview = plan.cloned();
        let state = Arc::clone(&control);
        thread::Builder::new()
            .name("piano-roll-idle-preview".into())
            .spawn(move || {
                let _guard = PreviewPanicGuard(Arc::clone(&state));
                let result = (|| {
                    let mut factories = ChipFactoryRegistry::new();
                    let routing = plan_for_preview.as_ref().map_or_else(
                        || RoutingPlan::new(&[], vgm_settings),
                        |plan| plan.routing.clone(),
                    );
                    crate::adapter::register_software_adapters(
                        &mut factories,
                        &routing,
                        crate::adapter::AdapterMode::Preview,
                    );
                    let sequencer =
                        Sequencer::create(&path, rate, looped, factories, None, settings)?;
                    if state.cancel.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let mut sources = Sequencer::gain_sources(&sequencer, &routes);
                    add_visual_ay_sources(&path, &routes, &mut sources)?;
                    set_source_targets(&mut sources, &routing, plan_for_preview.as_ref());
                    run_preview(&path, rate, looped, &state, &state.cancel, Some(sources))
                })();
                if let Err(error) = result {
                    if let Ok(mut preview) = state.preview.lock() {
                        preview.error = Some(format!("{error:#}"));
                    }
                }
            })
            .context("start piano roll preview worker")?;
        Ok(Self { control })
    }
}

#[cfg(test)]
fn register_ay_emulation(factories: &mut ChipFactoryRegistry, settings: VgmSettings) {
    crate::adapter::register_software_adapters(
        factories,
        &RoutingPlan::new(&[], settings),
        crate::adapter::AdapterMode::Playback,
    );
}

impl IdlePreview {
    pub fn gain_sources(&self) -> Result<Option<Vec<GainSource>>> {
        self.control
            .gain_sources
            .lock()
            .map(|sources| sources.clone())
            .map_err(|_| anyhow::anyhow!("chip gain sources are unavailable"))
    }

    pub fn fm_mute(&self) -> &FmMute {
        &self.control.fm_mute
    }

    pub fn visual_snapshot(&self, mode: RollMode) -> Result<Snapshot> {
        self.snapshot_at(mode, 0, false).map(|snapshot| {
            let silence = if mode == RollMode::Incoming {
                u64::from(self.control.settings.start_delay_seconds) * u64::from(snapshot.rate)
            } else {
                0
            };
            snapshot.with_leading_silence(silence)
        })
    }

    fn visual_snapshot_for_playback(&self, live: &Snapshot) -> Result<Snapshot> {
        let rate = self.control.sample_rate.load(Ordering::Acquire);
        let position = if live.rate == 0 {
            0
        } else {
            u64::try_from(u128::from(live.position) * u128::from(rate) / u128::from(live.rate))
                .context("piano roll preview position overflow")?
        };
        let leading_silence = if live.rate == 0 {
            u64::from(self.control.settings.start_delay_seconds) * u64::from(rate)
        } else {
            u64::try_from(
                u128::from(live.leading_silence) * u128::from(rate) / u128::from(live.rate),
            )
            .context("piano roll preview delay overflow")?
        };
        self.snapshot_at(RollMode::Incoming, position, live.started)
            .map(|snapshot| snapshot.with_leading_silence(leading_silence))
    }

    fn snapshot_at(&self, mode: RollMode, position: u64, started: bool) -> Result<Snapshot> {
        let mut preview = self
            .control
            .preview
            .lock()
            .map_err(|_| anyhow::anyhow!("piano roll preview is unavailable"))?;
        if let Some(error) = &preview.error {
            bail!("piano roll preview: {error}");
        }
        let rate = self.control.sample_rate.load(Ordering::Acquire);
        let scanning = !preview.done
            && preview.scanned < position.saturating_add(u64::from(rate) * WINDOW_SECONDS);
        let preparing = mode == RollMode::Incoming
            && !preview.done
            && preview.scanned < position.saturating_add(u64::from(rate));
        let scanned = preview.scanned;
        let incomplete = preview.roll.incomplete();
        let notes = if mode == RollMode::Incoming {
            preview.roll.snapshot(position, rate, mode)
        } else {
            Vec::new()
        };
        Ok(Snapshot {
            rate,
            position,
            started,
            notes,
            scanned,
            incomplete,
            preparing,
            scanning,
            ..Snapshot::default()
        })
    }
}

impl Drop for IdlePreview {
    fn drop(&mut self) {
        self.control.cancel.store(true, Ordering::Release);
    }
}

struct PreviewWorker {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

struct PreviewPanicGuard(Arc<PlaybackControl>);

impl Drop for PreviewPanicGuard {
    fn drop(&mut self) {
        if thread::panicking() {
            if let Ok(mut preview) = self.0.preview.lock() {
                preview.error = Some("preview worker panicked".into());
            }
        }
    }
}

impl PreviewWorker {
    fn start(path: &Path, rate: u32, looped: bool, control: &Arc<PlaybackControl>) -> Result<Self> {
        let path = path.to_owned();
        let control = Arc::clone(control);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("piano-roll-preview".into())
            .spawn(move || {
                let _panic_guard = PreviewPanicGuard(Arc::clone(&control));
                if let Err(error) = run_preview(&path, rate, looped, &control, &worker_stop, None) {
                    if let Ok(mut preview) = control.preview.lock() {
                        preview.error = Some(format!("{error:#}"));
                    }
                }
            })
            .context("start piano roll preview worker")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn mdx_package(path: &Path, bytes: Vec<u8>) -> Result<MdxPackage> {
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
    Ok(MdxPackage::parse_owned(normalized.into_owned(), pdx)?)
}

fn mdx_options(looped: bool, settings: PlaybackSettings) -> MdxSequencerOptions {
    MdxSequencerOptions {
        loop_count: if looped {
            None
        } else {
            Some(settings.loop_count)
        },
        max_ticks: if looped {
            None
        } else {
            MdxSequencerOptions::default().max_ticks
        },
        ..MdxSequencerOptions::default()
    }
}

fn add_visual_ay_sources(
    path: &Path,
    routes: &[ndsif_transport::NdsifRoute],
    sources: &mut Vec<GainSource>,
) -> Result<()> {
    if extension(path) == "mdx" {
        return Ok(());
    }
    let bytes = read_track(path)?;
    let header = soundlog::vgm::VgmHeader::from_bytes(&bytes)?;
    for (instance, chip, _) in header.chip_instances().iter() {
        if *chip == Chip::Ay8910
            && !sources
                .iter()
                .any(|source| source.chip == *chip && source.instance == *instance)
        {
            let hardware = routes
                .iter()
                .any(|route| route.chip == *chip && route.source == *instance);
            sources.push(GainSource {
                chip: chip.clone(),
                instance: *instance,
                hardware,
                visual_only: !hardware,
                target: None,
                target_device_name: None,
            });
        }
    }
    Ok(())
}

fn set_source_targets(
    sources: &mut [GainSource],
    routing: &RoutingPlan,
    plan: Option<&PlaybackPlan>,
) {
    for source in sources {
        if source.visual_only {
            source.target = Some(crate::routing::Target::Silent);
            continue;
        }
        let instance = match source.instance {
            Instance::Primary => crate::routing::ChipInstance::Primary,
            Instance::Secondary => crate::routing::ChipInstance::Secondary,
        };
        source.target = routing
            .assignments()
            .iter()
            .find(|assignment| {
                assignment.source.chip == source.chip && assignment.source.instance == instance
            })
            .filter(|assignment| {
                matches!(assignment.target, crate::routing::Target::Hardware { .. })
                    == source.hardware
            })
            .map(|assignment| assignment.target.clone());
        source.target_device_name = match &source.target {
            Some(crate::routing::Target::Hardware { device_id, .. }) => plan
                .and_then(|plan| plan.device_names.get(device_id))
                .filter(|name| !name.is_empty())
                .cloned(),
            _ => None,
        };
    }
}

fn run_preview(
    path: &Path,
    rate: u32,
    looped: bool,
    control: &PlaybackControl,
    stop: &AtomicBool,
    mut initial_sources: Option<Vec<GainSource>>,
) -> Result<()> {
    if rate == 0 {
        bail!("piano roll preview sample rate must be nonzero");
    }
    let bytes = read_track(path)?;
    let (mut stream, marker) = if extension(path) == "mdx" {
        let package = mdx_package(path, bytes)?;
        let options = mdx_options(looped, control.settings);
        let (generator, marker) = if looped {
            (to_vgm_stream_generator(package, options)?, None)
        } else {
            let (generator, marker) = to_vgm_stream_generator_with_loop_marker(package, options)?;
            (generator, Some(marker))
        };
        let mut stream = VgmCallbackStream::from_generator(generator);
        stream.track_state::<soundlog::chip::state::Ym2151State>(
            Instance::Primary,
            options.ym2151_clock as f32,
        );
        (stream, marker)
    } else {
        let header = soundlog::vgm::VgmHeader::from_bytes(&bytes)?;
        let extra = header.extra_header_from_bytes(&bytes)?;
        let mut instances = header.chip_instances();
        if let Some(extra) = extra {
            use soundlog::vgm::header::ChipId;
            for (instance, chip, clock) in &mut instances.0 {
                let id = match chip {
                    Chip::Sn76489 => ChipId::Sn76489,
                    Chip::Ay8910 => ChipId::Ay8910,
                    Chip::Ym2151 => ChipId::Ym2151,
                    Chip::Ym2203 => ChipId::Ym2203,
                    Chip::Ym2413 => ChipId::Ym2413,
                    Chip::Ym2608 => ChipId::Ym2608,
                    Chip::Ym2610b => ChipId::Ym2610,
                    Chip::Ym2612 => ChipId::Ym2612,
                    Chip::Ym3526 => ChipId::Ym3526,
                    Chip::Y8950 => ChipId::Y8950,
                    Chip::Ym3812 => ChipId::Ym3812,
                    Chip::Ymf262 => ChipId::Ymf262,
                    Chip::Ymf278b => ChipId::Ymf278b,
                    _ => continue,
                };
                if let Some(entry) = extra
                    .chip_clocks
                    .iter()
                    .find(|entry| entry.chip_id == id && entry.instance == *instance)
                {
                    *clock = (entry.clock & 0x3fff_ffff) as f32;
                }
            }
        }
        let mut stream = VgmCallbackStream::from_vgm(bytes)?;
        stream.track_chips(&instances);
        for (instance, chip, clock) in &instances.0 {
            if *chip == Chip::Sn76489 {
                stream.track_state_with(
                    *instance,
                    soundlog::chip::state::Sn76489State::with_config(
                        *clock,
                        header.sn76489_flags.clock_divider_8,
                        header.sn76489_flags.frequency_0_is_400,
                    ),
                );
            } else if *chip == Chip::Ay8910 {
                use soundlog::vgm::header::Ay8910ChipType;
                let clock = if header.ay8910_flags.ym_pin26_low
                    && matches!(
                        header.ay_chip_type,
                        Ay8910ChipType::Ym2149
                            | Ay8910ChipType::Ym3439
                            | Ay8910ChipType::Ymz284
                            | Ay8910ChipType::Ymz294
                    ) {
                    *clock / 2.0
                } else {
                    *clock
                };
                stream.track_state_with(*instance, soundlog::chip::state::Ay8910State::new(clock));
            }
        }
        stream.set_loop_count(if looped {
            None
        } else {
            Some(control.settings.loop_count)
        });
        if !looped {
            stream.set_loop_base(0);
            stream.set_loop_modifier(0x10);
            stream.set_fadeout_samples(Some(control.settings.fadeout_samples() as usize));
        }
        (stream, None)
    };
    let initial_modes = initial_sources.is_some();
    let events: VisualEvents = Rc::new(RefCell::new(Vec::new()));
    let frame = Rc::new(Cell::new(0_usize));
    macro_rules! register {
        ($chip:expr, $spec:ty) => {
            register!($chip, $spec, |_, _: &$spec, _| {});
        };
        ($chip:expr, $spec:ty, $observe:expr) => {{
            let observe = $observe;
            let events = Rc::clone(&events);
            let frame = Rc::clone(&frame);
            stream.on_write(move |instance, write: $spec, _, changes| {
                observe(instance, &write, frame.get());
                if let Some(changes) = changes {
                    events
                        .borrow_mut()
                        .push(($chip, instance, frame.get(), changes));
                }
            });
        }};
    }
    use soundlog::chip::{
        Ay8910Spec, PsgSpec, Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec,
        Ym2612Spec, Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
    };
    register!(0, Ym2151Spec);
    register!(1, Ym2203Spec);
    {
        let state = Arc::clone(&control.fm_mute);
        register!(2, Ym2413Spec, move |instance, write: &Ym2413Spec, frame| {
            if initial_modes && frame == 0 && write.register == 0x0e {
                state.opll_write(instance, write.register, write.value);
            }
        });
    }
    register!(3, Ym2608Spec);
    register!(4, Ym2610Spec);
    register!(5, Ym2612Spec);
    macro_rules! register_opl {
        ($index:expr, $spec:ty, $chip:expr, $port:expr) => {{
            let state = Arc::clone(&control.fm_mute);
            register!($index, $spec, move |instance, write: &$spec, frame| {
                let port = ($port)(write);
                if initial_modes
                    && frame == 0
                    && ((port == 0 && write.register == 0xbd)
                        || (port == 1 && write.register == 0x04))
                {
                    state.opl_write(&$chip, instance, port, write.register, write.value);
                }
            });
        }};
    }
    register_opl!(6, Ym3526Spec, Chip::Ym3526, |_: &Ym3526Spec| 0);
    register_opl!(7, Y8950Spec, Chip::Y8950, |_: &Y8950Spec| 0);
    register_opl!(8, Ym3812Spec, Chip::Ym3812, |_: &Ym3812Spec| 0);
    register_opl!(9, Ymf262Spec, Chip::Ymf262, |write: &Ymf262Spec| write.port);
    register_opl!(10, Ymf278bSpec, Chip::Ymf278b, |write: &Ymf278bSpec| write
        .port);
    register!(crate::visualizer::SN76489_INDEX, PsgSpec);
    register!(crate::visualizer::AY8910_INDEX, Ay8910Spec);

    let mut samples = 0_u64;
    let mut scanned = 0_u64;
    let mut wait = 0_u64;
    let mut fade_end = None;
    while !stop.load(Ordering::Acquire) && !control.cancel.load(Ordering::Acquire) {
        let position = control.visual_clock.position(Instant::now(), rate)?;
        let target = position.saturating_add(u64::from(rate) * WINDOW_SECONDS);
        if scanned >= target {
            if initial_modes {
                break;
            }
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        let mut finished = false;
        for _ in 0..256 {
            if fade_end.is_some_and(|end| samples >= end) {
                finished = true;
                break;
            }
            if wait > 0 {
                let step = wait
                    .min(1024)
                    .min(fade_end.map_or(u64::MAX, |end| end - samples));
                samples = samples
                    .checked_add(step)
                    .context("piano roll preview sample overflow")?;
                wait -= step;
                scanned = u64::try_from(
                    (u128::from(samples) * u128::from(rate) + u128::from(VGM_SAMPLE_RATE / 2))
                        / u128::from(VGM_SAMPLE_RATE),
                )
                .context("piano roll preview frame overflow")?;
                frame.set(usize::try_from(scanned).context("piano roll preview frame overflow")?);
                if scanned >= target {
                    break;
                }
                continue;
            }
            let result = stream.next().transpose()?;
            if fade_end.is_none() && marker.as_ref().is_some_and(|marker| marker.reached()) {
                fade_end = Some(
                    samples
                        .checked_add(u64::from(control.settings.fadeout_samples()))
                        .context("piano roll preview sample overflow")?,
                );
            }
            match result {
                Some(StreamResult::Command(VgmCommand::WaitSamples(duration))) => {
                    wait = u64::from(duration.0)
                }
                Some(StreamResult::Command(_)) => {}
                Some(StreamResult::NeedsMoreData) => bail!("incomplete piano roll preview stream"),
                Some(StreamResult::EndOfStream) | None => {
                    finished = true;
                    break;
                }
            }
        }
        let mut preview = control
            .preview
            .lock()
            .map_err(|_| anyhow::anyhow!("piano roll preview state is unavailable"))?;
        let mut pending = events.borrow_mut();
        for (chip, instance, frame, events) in pending.drain(..) {
            preview.roll.apply(chip, instance, frame, events);
        }
        preview.scanned = scanned;
        if finished {
            preview.roll.finish(scanned);
            preview.done = true;
        } else {
            preview.roll.prune(position, rate);
        }
        drop(pending);
        drop(preview);
        if (scanned > 0 || finished)
            && let Some(sources) = initial_sources.take()
        {
            *control
                .gain_sources
                .lock()
                .map_err(|_| anyhow::anyhow!("chip gain sources are unavailable"))? = Some(sources);
        }
        if finished {
            break;
        }
    }
    Ok(())
}

fn flush_visual_events(
    control: &PlaybackControl,
    pending: &VisualEvents,
    overflow: bool,
) -> Result<()> {
    let mut pending = pending.borrow_mut();
    if pending.is_empty() && !overflow {
        return Ok(());
    }
    let mut visualizer = control
        .visualizer
        .lock()
        .map_err(|_| anyhow::anyhow!("piano roll state is unavailable"))?;
    if overflow {
        pending.clear();
        visualizer.report_overflow();
    } else {
        for (chip, instance, frame, events) in pending.drain(..) {
            visualizer.apply(chip, instance, frame, events);
        }
    }
    Ok(())
}

impl Sequencer {
    fn filter_fm_mutes(&mut self, mute: Arc<FmMute>) {
        macro_rules! register {
            ($player:expr) => {{
                use soundlog::chip::{
                    Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec,
                    Ym2612Spec, Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
                };
                let state = Arc::clone(&mute);
                $player.filter_write(move |instance, mut write: Ym2151Spec, _| {
                    if write.register == 0x08
                        && state.is_effectively_muted(&Chip::Ym2151, instance, write.value & 7)
                    {
                        write.value &= 7;
                    }
                    write
                });
                let state = Arc::clone(&mute);
                $player.filter_write(move |instance, mut write: Ym2203Spec, _| {
                    write.value =
                        state.opn_write(&Chip::Ym2203, instance, 0, write.register, write.value);
                    write
                });
                let state = Arc::clone(&mute);
                $player.filter_write(move |instance, mut write: Ym2413Spec, _| {
                    write.value = state.opll_write(instance, write.register, write.value);
                    write
                });
                macro_rules! opn {
                    ($spec:ty, $chip:expr) => {{
                        let state = Arc::clone(&mute);
                        $player.filter_write(move |instance, mut write: $spec, _| {
                            write.value = state.opn_write(
                                &$chip,
                                instance,
                                write.port,
                                write.register,
                                write.value,
                            );
                            write
                        });
                    }};
                }
                opn!(Ym2608Spec, Chip::Ym2608);
                opn!(Ym2610Spec, Chip::Ym2610b);
                opn!(Ym2612Spec, Chip::Ym2612);
                macro_rules! opl {
                    ($spec:ty, $chip:expr) => {{
                        let state = Arc::clone(&mute);
                        $player.filter_write(move |instance, mut write: $spec, _| {
                            write.value =
                                state.opl_write(&$chip, instance, 0, write.register, write.value);
                            write
                        });
                    }};
                }
                opl!(Ym3526Spec, Chip::Ym3526);
                opl!(Y8950Spec, Chip::Y8950);
                opl!(Ym3812Spec, Chip::Ym3812);
                macro_rules! opl_banked {
                    ($spec:ty, $chip:expr) => {{
                        let state = Arc::clone(&mute);
                        $player.filter_write(move |instance, mut write: $spec, _| {
                            write.value = state.opl_write(
                                &$chip,
                                instance,
                                write.port,
                                write.register,
                                write.value,
                            );
                            write
                        });
                    }};
                }
                opl_banked!(Ymf262Spec, Chip::Ymf262);
                opl_banked!(Ymf278bSpec, Chip::Ymf278b);
            }};
        }
        match self {
            Self::Vgm(player) => register!(player),
            Self::Mdx(player) => register!(player),
        }
    }

    fn gain_sources(
        sequencer: &Sequencer,
        routes: &[ndsif_transport::NdsifRoute],
    ) -> Vec<GainSource> {
        sequencer
            .chip_instances()
            .into_iter()
            .map(|(chip, instance)| GainSource {
                hardware: routes
                    .iter()
                    .any(|route| route.chip == chip && route.source == instance),
                chip,
                instance,
                visual_only: false,
                target: None,
                target_device_name: None,
            })
            .collect()
    }

    fn chip_instances(&self) -> Vec<(Chip, Instance)> {
        match self {
            Self::Vgm(player) => player.chip_instances().collect(),
            Self::Mdx(player) => player.chip_instances().collect(),
        }
    }

    fn set_gain(&mut self, chip: Chip, instance: Instance, gain: f32) -> Result<()> {
        match self {
            Self::Vgm(player) => player.set_gain(chip, instance, gain)?,
            Self::Mdx(player) => player.set_gain(chip, instance, gain)?,
        }
        Ok(())
    }

    fn set_opn_gains(&mut self, chip: Chip, instance: Instance, fm: f32, ssg: f32) -> Result<()> {
        match self {
            Self::Vgm(player) => player.set_opn_gains(chip, instance, fm, ssg)?,
            Self::Mdx(_) => bail!("MDX does not support OPN chip gains"),
        }
        Ok(())
    }

    #[cfg(test)]
    fn new(path: &Path, rate: u32, looped: bool) -> Result<Self> {
        Self::with_factories(path, rate, looped, ChipFactoryRegistry::new())
    }

    fn apply_chip_gains(
        sequencer: &mut Sequencer,
        sources: &[GainSource],
        gains: &GainState,
        applied_revision: &mut Option<u64>,
    ) -> Result<()> {
        let revision = gains.revision.load(Ordering::Acquire);
        if *applied_revision == Some(revision) {
            return Ok(());
        }
        let values = gains
            .values
            .lock()
            .map_err(|_| anyhow::anyhow!("chip gain state is unavailable"))?;
        for source in sources
            .iter()
            .filter(|source| !source.hardware && !source.visual_only)
        {
            let value = |part| {
                values
                    .get(&(source.chip.clone(), part))
                    .copied()
                    .unwrap_or(1.0)
            };
            sequencer.set_gain(source.chip.clone(), source.instance, value(GainPart::Chip))?;
            if matches!(source.chip, Chip::Ym2203 | Chip::Ym2608 | Chip::Ym2610b) {
                sequencer.set_opn_gains(
                    source.chip.clone(),
                    source.instance,
                    value(GainPart::Fm),
                    value(GainPart::Ssg),
                )?;
            }
        }
        *applied_revision = Some(revision);
        Ok(())
    }

    #[cfg(test)]
    fn with_factories(
        path: &Path,
        rate: u32,
        looped: bool,
        factories: ChipFactoryRegistry,
    ) -> Result<Self> {
        Self::create(
            path,
            rate,
            looped,
            factories,
            None,
            PlaybackSettings::default(),
        )
    }

    #[cfg(test)]
    fn for_playback(
        path: &Path,
        rate: u32,
        looped: bool,
        factories: ChipFactoryRegistry,
        rom_directory: &Path,
        routes: &[ndsif_transport::NdsifRoute],
    ) -> Result<Self> {
        Self::for_playback_with_settings(
            path,
            rate,
            looped,
            factories,
            rom_directory,
            routes,
            PlaybackSettings::default(),
        )
    }

    fn for_playback_with_settings(
        path: &Path,
        rate: u32,
        looped: bool,
        factories: ChipFactoryRegistry,
        rom_directory: &Path,
        routes: &[ndsif_transport::NdsifRoute],
        settings: PlaybackSettings,
    ) -> Result<Self> {
        Self::create(
            path,
            rate,
            looped,
            factories,
            Some((rom_directory, routes)),
            settings,
        )
    }

    fn create(
        path: &Path,
        rate: u32,
        looped: bool,
        factories: ChipFactoryRegistry,
        rom: Option<(&Path, &[ndsif_transport::NdsifRoute])>,
        settings: PlaybackSettings,
    ) -> Result<Self> {
        let bytes = read_track(path)?;
        if extension(path) == "mdx" {
            let package = mdx_package(path, bytes)?;
            let options = mdx_options(looped, settings);
            Ok(Self::Mdx(
                MdxSequencer::with_options_and_fadeout_with_factories(
                    package,
                    rate,
                    CHUNK_FRAMES,
                    options,
                    (!looped).then_some(settings.fadeout_samples()),
                    factories,
                )?,
            ))
        } else {
            let ym2608_clock = if rom.is_some() {
                soundlog::vgm::VgmHeader::from_bytes(&bytes)?.ym2608_clock
            } else {
                0
            };
            let mut sequencer =
                VgmSequencer::from_vgm_with_factories(bytes, rate, CHUNK_FRAMES, factories)?;
            if let Some((directory, routes)) = rom {
                Self::configure_ym2608_rhythm_rom(
                    &mut sequencer,
                    ym2608_clock,
                    path,
                    directory,
                    routes,
                )?;
            }
            sequencer.set_loop_count(if looped {
                None
            } else {
                Some(settings.loop_count)
            });
            if !looped {
                sequencer.set_loop_base(0);
                sequencer.set_loop_modifier(0x10);
                sequencer.set_fadeout_samples(Some(settings.fadeout_samples()))?;
            }
            Ok(Self::Vgm(sequencer))
        }
    }

    fn configure_ym2608_rhythm_rom(
        sequencer: &mut VgmSequencer<'_>,
        clock: u32,
        track: &Path,
        directory: &Path,
        routes: &[ndsif_transport::NdsifRoute],
    ) -> Result<()> {
        if clock & 0x3fff_ffff == 0 {
            return Ok(());
        }
        let instances: Vec<_> = [Instance::Primary, Instance::Secondary]
            .into_iter()
            .filter(|instance| {
                (*instance == Instance::Primary || clock & 0x4000_0000 != 0)
                    && !routes
                        .iter()
                        .any(|route| route.chip == Chip::Ym2608 && route.source == *instance)
            })
            .collect();
        if instances.is_empty() {
            return Ok(());
        }
        for path in [
            track.with_file_name(YM2608_RHYTHM_ROM),
            directory.join(YM2608_RHYTHM_ROM),
        ] {
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => bail!("YM2608 rhythm ROM is not a file: {}", path.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect YM2608 rhythm ROM {}", path.display()));
                }
            }
            let file = std::fs::File::open(&path)
                .with_context(|| format!("open YM2608 rhythm ROM {}", path.display()))?;
            if !file
                .metadata()
                .with_context(|| format!("inspect YM2608 rhythm ROM {}", path.display()))?
                .is_file()
            {
                bail!("YM2608 rhythm ROM is not a file: {}", path.display());
            }
            let data = read_bounded(file)
                .with_context(|| format!("read YM2608 rhythm ROM {}", path.display()))?;
            if data.is_empty() {
                bail!("YM2608 rhythm ROM is empty: {}", path.display());
            }
            for instance in instances {
                sequencer
                    .set_ym2608_rhythm_rom(instance, &data)
                    .with_context(|| {
                        format!(
                            "configure YM2608 {instance:?} rhythm ROM {}",
                            path.display()
                        )
                    })?;
            }
            break;
        }
        Ok(())
    }

    fn render(&mut self) -> Result<Option<StereoBlock>> {
        match self {
            Self::Vgm(player) => Ok(player.render()?),
            Self::Mdx(player) => Ok(player.render()?),
        }
    }

    fn track_visual_events(&mut self, pending: VisualEvents) {
        macro_rules! register {
            ($player:expr, $chip:expr, $spec:ty, $state:ty) => {{
                $player.track_state_with_voice_clocks::<$state>();
                let pending = Rc::clone(&pending);
                $player.on_state_change::<$spec, _>(move |instance, frame, events| {
                    pending.borrow_mut().push(($chip, instance, frame, events));
                });
            }};
        }
        macro_rules! configure {
            ($player:expr) => {{
                use soundlog::chip::state::{
                    Sn76489State, Y8950State, Ym2151State, Ym2203State, Ym2413State, Ym2608State,
                    Ym2610bState, Ym2612State, Ym3526State, Ym3812State, Ymf262State, Ymf278bState,
                };
                use soundlog::chip::{
                    PsgSpec, Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec,
                    Ym2612Spec, Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
                };
                register!($player, 0, Ym2151Spec, Ym2151State);
                register!($player, 1, Ym2203Spec, Ym2203State);
                register!($player, 2, Ym2413Spec, Ym2413State);
                register!($player, 3, Ym2608Spec, Ym2608State);
                register!($player, 4, Ym2610Spec, Ym2610bState);
                register!($player, 5, Ym2612Spec, Ym2612State);
                register!($player, 6, Ym3526Spec, Ym3526State);
                register!($player, 7, Y8950Spec, Y8950State);
                register!($player, 8, Ym3812Spec, Ym3812State);
                register!($player, 9, Ymf262Spec, Ymf262State);
                register!($player, 10, Ymf278bSpec, Ymf278bState);
                register!(
                    $player,
                    crate::visualizer::SN76489_INDEX,
                    PsgSpec,
                    Sn76489State
                );
            }};
        }
        match self {
            Self::Vgm(player) => configure!(player),
            Self::Mdx(player) => configure!(player),
        }
    }

    fn take_visual_overflow(&mut self) -> bool {
        match self {
            Self::Vgm(player) => player.take_state_event_overflow(),
            Self::Mdx(player) => player.take_state_event_overflow(),
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
                .schedule_output_fade(
                    start,
                    rate,
                    Duration::from_secs(u64::from(control.settings.fadeout_seconds)),
                )
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
    silence_remaining: u64,
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
        let mut silence = 0_u64;
        let total_silence =
            u64::from(self.control.settings.start_delay_seconds) * u64::from(self.rate);
        let silence_start = total_silence.saturating_sub(self.silence_remaining);
        let mut needs_origin = self.control.first_presentation.get().is_none();
        for (frame_index, frame) in out.chunks_mut(self.channels).enumerate() {
            if self.silence_remaining > 0 {
                self.silence_remaining -= 1;
                silence += 1;
                continue;
            }
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
        if silence > 0 {
            let result = callback_time
                .checked_add(latency)
                .context("audio presentation time overflow")
                .and_then(|time| {
                    self.control
                        .silence_clock
                        .publish(silence_start, silence, time)
                });
            if let Err(error) = result {
                self.ended = true;
                let _ = self.events.send(Err(error));
            }
        }
        let start = self.control.frames.fetch_add(played, Ordering::Relaxed);
        if played > 0 {
            let result = callback_time
                .checked_add(latency)
                .and_then(|time| {
                    time.checked_add(Duration::from_secs_f64(
                        silence as f64 / f64::from(self.rate),
                    ))
                })
                .context("audio presentation time overflow")
                .and_then(|time| self.control.visual_clock.publish(start, played, time));
            if let Err(error) = result {
                self.ended = true;
                let _ = self.events.send(Err(error));
            }
        }
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
    let silence_remaining =
        u64::from(control.settings.start_delay_seconds) * u64::from(config.sample_rate);
    let mut consumer = AudioConsumer {
        rx,
        events,
        control,
        frames: Vec::new(),
        position: 0,
        ended: false,
        channels,
        rate: config.sample_rate,
        silence_remaining,
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

pub fn audio_output_devices() -> Result<Vec<(String, String)>> {
    let host = cpal::default_host();
    host.output_devices()
        .context("could not enumerate audio output devices")?
        .map(|device| {
            let name = device.to_string();
            let id = device
                .id()
                .context("could not read audio output device ID")?
                .to_string();
            Ok((id, name))
        })
        .collect()
}

pub fn audio_sample_rates(device_id: Option<&str>) -> Result<Vec<u32>> {
    let device = audio_output_device(device_id)?;
    let ranges = device
        .supported_output_configs()
        .context("could not enumerate supported audio output configurations")?
        .collect::<Vec<_>>();
    let mut rates = std::collections::BTreeSet::new();
    for rate in [44_100, 48_000, 96_000, 192_000] {
        if ranges
            .iter()
            .any(|range| range.try_with_sample_rate(rate).is_some())
        {
            rates.insert(rate);
        }
    }
    Ok(rates.into_iter().collect())
}

fn audio_output_device(device_id: Option<&str>) -> Result<cpal::Device> {
    match device_id {
        Some(device_id) => {
            let id = device_id
                .parse::<cpal::DeviceId>()
                .with_context(|| format!("invalid audio output device ID: {device_id}"))?;
            let host = cpal::host_from_id(id.host())
                .with_context(|| format!("audio host for device {device_id} is unavailable"))?;
            host.device_by_id(&id)
                .with_context(|| format!("audio output device is unavailable: {device_id}"))
        }
        None => cpal::default_host()
            .default_output_device()
            .context("no default audio output device"),
    }
}

fn audio_output_config(
    device: &cpal::Device,
    sample_rate: Option<u32>,
) -> Result<cpal::SupportedStreamConfig> {
    let default = device
        .default_output_config()
        .context("no default audio configuration")?;
    let Some(sample_rate) = sample_rate else {
        return Ok(default);
    };
    if sample_rate == default.sample_rate() {
        return Ok(default);
    }
    let ranges = device
        .supported_output_configs()
        .context("could not enumerate supported audio output configurations")?
        .collect::<Vec<_>>();
    let range = ranges
        .iter()
        .copied()
        .find(|range| {
            range.channels() == default.channels()
                && range.sample_format() == default.sample_format()
                && range.try_with_sample_rate(sample_rate).is_some()
        })
        .or_else(|| {
            ranges
                .iter()
                .copied()
                .find(|range| range.try_with_sample_rate(sample_rate).is_some())
        })
        .with_context(|| format!("audio output device does not support {sample_rate} Hz"))?;
    range
        .try_with_sample_rate(sample_rate)
        .with_context(|| format!("audio output device does not support {sample_rate} Hz"))
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
    if !plan.nd7_devices.is_empty() && extension(path) != "mdx" {
        let bytes = read_track(path)?;
        let header = soundlog::vgm::VgmHeader::from_bytes(&bytes)?;
        let extra = header.extra_header_from_bytes(&bytes)?;
        plan.validate_nd7(&header, extra.as_ref())?;
    }
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
    let device = audio_output_device(control.audio_output_device.as_deref())?;
    let supported = audio_output_config(&device, control.audio_sample_rate)?;
    let config = supported.config();
    let routes = hardware.map_or(&[][..], |(_, plan)| plan.routes.as_slice());
    let playback_plan = hardware.map(|(_, plan)| plan);
    let routing = hardware.map_or_else(
        || RoutingPlan::new(&[], control.vgm_settings),
        |(_, plan)| plan.routing.clone(),
    );
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
        let mut factories = ndsif_transport::create_routed_transport_factories_with_nd7(
            &recorders,
            &plan.routes,
            &plan.nd7_devices,
        )?;
        crate::adapter::register_software_adapters(
            &mut factories,
            &routing,
            crate::adapter::AdapterMode::Playback,
        );
        let sequencer = Sequencer::for_playback_with_settings(
            path,
            config.sample_rate,
            looped,
            factories,
            Path::new("."),
            &plan.routes,
            control.settings,
        )?;
        let workers = crate::hardware::HardwareWorkers::start(lease, &recorders, timeline)?;
        (sequencer, Some((recorders, workers)))
    } else {
        let mut factories = ChipFactoryRegistry::new();
        crate::adapter::register_software_adapters(
            &mut factories,
            &routing,
            crate::adapter::AdapterMode::Playback,
        );
        (
            Sequencer::for_playback_with_settings(
                path,
                config.sample_rate,
                looped,
                factories,
                Path::new("."),
                &[],
                control.settings,
            )?,
            None,
        )
    };
    let mut sources = Sequencer::gain_sources(&sequencer, routes);
    add_visual_ay_sources(path, routes, &mut sources)?;
    set_source_targets(&mut sources, &routing, playback_plan);
    sequencer.filter_fm_mutes(Arc::clone(&control.fm_mute));
    *control
        .gain_sources
        .lock()
        .map_err(|_| anyhow::anyhow!("chip gain sources are unavailable"))? = Some(sources.clone());
    let mut gain_revision = None;
    let visual_events: VisualEvents = Rc::new(RefCell::new(Vec::new()));
    sequencer.track_visual_events(Rc::clone(&visual_events));
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
    let mut hardware_prepared = false;
    for _ in 0..PREBUFFER_BLOCKS {
        Sequencer::apply_chip_gains(&mut sequencer, &sources, &control.gains, &mut gain_revision)?;
        let block = sequencer.render()?;
        flush_visual_events(&control, &visual_events, sequencer.take_visual_overflow())?;
        sequencer.schedule_hardware_fade(
            &control,
            config.sample_rate,
            &mut hardware_fade_scheduled,
        )?;
        if let Some((recorders, workers)) = &hardware {
            if hardware_prepared {
                workers.enqueue(recorders, &control.cancel)?;
            } else {
                hardware_prepared = workers.enqueue_prebuffer(
                    recorders,
                    prebuffered_frames,
                    config.sample_rate,
                    &control.cancel,
                )?;
            }
        }
        match block {
            Some(block) if !block.frames().is_empty() => {
                prebuffered_frames = prebuffered_frames
                    .checked_add(block.frames().len())
                    .context("audio prebuffer frame count overflow")?;
                tx.send(Block::Audio(block.frames().to_vec()))?;
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
        && !hardware_prepared
        && let Some((_, workers)) = &hardware
    {
        workers.prepare_audio(prebuffered_frames, config.sample_rate, &control.cancel)?;
    }
    if control.cancel.load(Ordering::Acquire) {
        return Ok(());
    }
    stream.play()?;
    let _preview_worker = match PreviewWorker::start(path, config.sample_rate, looped, &control) {
        Ok(worker) => Some(worker),
        Err(error) => {
            let mut preview = control
                .preview
                .lock()
                .map_err(|_| anyhow::anyhow!("piano roll preview is unavailable"))?;
            preview.error = Some(format!("{error:#}"));
            None
        }
    };
    'playback: while !control.cancel.load(Ordering::Relaxed) {
        if control.recording_failed.load(Ordering::Acquire) {
            bail!("audio input failed");
        }
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
            Sequencer::apply_chip_gains(
                &mut sequencer,
                &sources,
                &control.gains,
                &mut gain_revision,
            )?;
            let rendered = sequencer.render()?;
            flush_visual_events(&control, &visual_events, sequencer.take_visual_overflow())?;
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
                        if control.recording_failed.load(Ordering::Acquire) {
                            bail!("audio input failed");
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
        if control.recording_failed.load(Ordering::Acquire) {
            bail!("audio input failed");
        }
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
    fn recording_starts_before_each_track_and_finalizes_after_playback() {
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let (sender, events) = mpsc::channel();
        let sequence = RefCell::new(Vec::new());
        for title in ["first", "second"] {
            record_and_play(
                Some(RecordingSettings::default()),
                &control,
                &sender,
                |_| {
                    sequence.borrow_mut().push(format!("start {title}"));
                    Ok(title.to_owned())
                },
                || {
                    sequence.borrow_mut().push(format!("play {title}"));
                    Ok(())
                },
                |title| {
                    sequence.borrow_mut().push(format!("finish {title}"));
                    Ok(Some(crate::recording::SavedRecording {
                        path: PathBuf::from(format!("{title}.wav")),
                        xruns: u64::from(title == "second"),
                    }))
                },
            )
            .unwrap();
        }
        assert_eq!(
            *sequence.borrow(),
            [
                "start first",
                "play first",
                "finish first",
                "start second",
                "play second",
                "finish second"
            ]
        );
        assert!(
            matches!(events.try_recv(), Ok(Event::RecordingSaved(saved)) if saved.path == Path::new("first.wav") && saved.xruns == 0)
        );
        assert!(
            matches!(events.try_recv(), Ok(Event::RecordingSaved(saved)) if saved.path == Path::new("second.wav") && saved.xruns == 1)
        );
    }

    #[test]
    fn recording_failure_prevents_playback_and_cancelled_save_errors_are_reported() {
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let (sender, events) = mpsc::channel();
        let played = Cell::new(false);
        let result = record_and_play(
            Some(RecordingSettings::default()),
            &control,
            &sender,
            |_| -> Result<()> { bail!("input unavailable") },
            || {
                played.set(true);
                Ok(())
            },
            |_| Ok(None),
        );
        assert!(result.is_err());
        assert!(!played.get());
        record_and_play(
            Some(RecordingSettings::default()),
            &control,
            &sender,
            |_| Ok(()),
            || {
                control.cancel.store(true, Ordering::Release);
                Ok(())
            },
            |_| bail!("disk full"),
        )
        .unwrap();
        assert!(
            matches!(events.try_recv(), Ok(Event::RecordingError(error)) if error.contains("disk full"))
        );
    }

    #[test]
    fn recording_save_failure_fails_active_playback() {
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let (sender, _) = mpsc::channel();
        let result = record_and_play(
            Some(RecordingSettings::default()),
            &control,
            &sender,
            |_| Ok(()),
            || Ok(()),
            |_| bail!("disk full"),
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("disk full"));
    }

    #[test]
    fn recording_without_input_samples_fails_playback() {
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let (sender, _) = mpsc::channel();
        let result = record_and_play(
            Some(RecordingSettings::default()),
            &control,
            &sender,
            |_| Ok(()),
            || Ok(()),
            |_| Ok(None),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no recording samples")
        );
    }

    #[test]
    fn cancelled_playback_does_not_open_an_input_device() {
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        control.cancel.store(true, Ordering::Release);
        let (sender, _) = mpsc::channel();
        let started = Cell::new(false);
        record_and_play(
            Some(RecordingSettings::default()),
            &control,
            &sender,
            |_| {
                started.set(true);
                Ok(())
            },
            || Ok(()),
            |_| Ok(None),
        )
        .unwrap();
        assert!(!started.get());
    }

    #[test]
    fn input_failure_interrupts_audio_presentation_wait() {
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        control.recording_failed.store(true, Ordering::Release);
        assert!(wait_for_audio(Instant::now() + Duration::from_secs(60), &control).is_err());
    }

    #[test]
    fn fm_mute_filters_live_vgm_hardware_writes_per_instance() {
        use chipstream::chip::{
            NdsifRegisterEvent, NdsifTransport, ndsif::register_ymfm_factories,
        };
        use soundlog::chip::Ym2151Spec;
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        #[derive(Default)]
        struct Recorder(Vec<NdsifRegisterEvent>);
        impl NdsifTransport for Recorder {
            fn configure_chip(&mut self, _: ndsif::WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
                self.0.push(event);
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.vgm");
        let mut builder = VgmBuilder::new();
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.register_chip(Chip::Ym2151, instance, 4_000_000);
            builder.add_chip_write(
                instance,
                Ym2151Spec {
                    register: 8,
                    value: 0x78,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(44_100));
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.add_chip_write(
                instance,
                Ym2151Spec {
                    register: 8,
                    value: 0x78,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(44_100));
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.add_chip_write(
                instance,
                Ym2151Spec {
                    register: 8,
                    value: 0x78,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(1_000));
        std::fs::write(&path, Vec::<u8>::from(builder.finalize())).unwrap();

        let recorder = Rc::new(RefCell::new(Recorder::default()));
        let mut factories = ChipFactoryRegistry::new();
        register_ymfm_factories(&mut factories, Rc::clone(&recorder)).unwrap();
        let mut player = Sequencer::with_factories(&path, 44_100, false, factories).unwrap();
        let mute = Arc::new(FmMute::default());
        assert!(mute.set_muted(&Chip::Ym2151, Instance::Primary, 0, true));
        player.filter_fm_mutes(Arc::clone(&mute));
        let observed = Rc::new(RefCell::new(Vec::new()));
        if let Sequencer::Vgm(vgm) = &mut player {
            let observed = Rc::clone(&observed);
            vgm.on_write(move |instance, write: Ym2151Spec, _, _| {
                observed.borrow_mut().push((instance, write.value));
            });
        }
        player.render().unwrap().unwrap();
        assert!(mute.set_muted(&Chip::Ym2151, Instance::Primary, 0, false));
        assert!(mute.set_soloed(&Chip::Ym2151, Instance::Secondary, 0, true));
        while recorder.borrow().0.len() < 4 {
            player.render().unwrap().unwrap();
        }
        assert!(mute.set_soloed(&Chip::Ym2151, Instance::Secondary, 0, false));
        while player.render().unwrap().is_some() {}
        let events = &recorder.borrow().0;
        let writes: Vec<_> = events
            .iter()
            .map(|event| (event.instance, event.write))
            .collect();
        assert_eq!(
            writes,
            vec![
                (0, ndsif::RegisterWrite::new(8, 0)),
                (1, ndsif::RegisterWrite::new(8, 0x78)),
                (0, ndsif::RegisterWrite::new(8, 0)),
                (1, ndsif::RegisterWrite::new(8, 0x78)),
                (0, ndsif::RegisterWrite::new(8, 0x78)),
                (1, ndsif::RegisterWrite::new(8, 0x78)),
            ]
        );
        assert!(observed.borrow().iter().all(|(_, value)| *value == 0x78));
        assert_eq!(observed.borrow().len(), 6);
    }

    #[test]
    fn fm_mute_register_filters_cover_all_tracked_ymfm_chips() {
        use chipstream::chip::{
            NdsifRegisterEvent, NdsifTransport, ndsif::register_ymfm_factories,
        };
        use soundlog::chip::{
            Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec,
            Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
        };
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        #[derive(Default)]
        struct Recorder(Vec<NdsifRegisterEvent>);
        impl NdsifTransport for Recorder {
            fn configure_chip(&mut self, _: ndsif::WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
                self.0.push(event);
                Ok(())
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("chip.vgm");
        for chip in [
            Chip::Ym2151,
            Chip::Ym2203,
            Chip::Ym2413,
            Chip::Ym2608,
            Chip::Ym2610b,
            Chip::Ym2612,
            Chip::Ym3526,
            Chip::Y8950,
            Chip::Ym3812,
            Chip::Ymf262,
            Chip::Ymf278b,
        ] {
            let mut builder = VgmBuilder::new();
            builder.register_chip(chip.clone(), Instance::Primary, 8_000_000);
            let (register, value, expected) = match chip {
                Chip::Ym2151 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym2151Spec {
                            register: 8,
                            value: 0x78,
                        },
                    );
                    (8, 0x78, 0)
                }
                Chip::Ym2203 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym2203Spec {
                            register: 0x28,
                            value: 0xf0,
                        },
                    );
                    (0x28, 0xf0, 0)
                }
                Chip::Ym2413 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym2413Spec {
                            register: 0x20,
                            value: 0x1f,
                        },
                    );
                    (0x20, 0x1f, 0x0f)
                }
                Chip::Ym2608 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym2608Spec {
                            port: 0,
                            register: 0x28,
                            value: 0xf0,
                        },
                    );
                    (0x28, 0xf0, 0)
                }
                Chip::Ym2610b => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym2610Spec {
                            port: 0,
                            register: 0x28,
                            value: 0xf0,
                        },
                    );
                    (0x28, 0xf0, 0)
                }
                Chip::Ym2612 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym2612Spec {
                            port: 0,
                            register: 0x28,
                            value: 0xf0,
                        },
                    );
                    (0x28, 0xf0, 0)
                }
                Chip::Ym3526 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym3526Spec {
                            register: 0xb0,
                            value: 0x3f,
                        },
                    );
                    (0xb0, 0x3f, 0x1f)
                }
                Chip::Y8950 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Y8950Spec {
                            register: 0xb0,
                            value: 0x3f,
                        },
                    );
                    (0xb0, 0x3f, 0x1f)
                }
                Chip::Ym3812 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ym3812Spec {
                            register: 0xb0,
                            value: 0x3f,
                        },
                    );
                    (0xb0, 0x3f, 0x1f)
                }
                Chip::Ymf262 => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ymf262Spec {
                            port: 0,
                            register: 0xb0,
                            value: 0x3f,
                        },
                    );
                    (0xb0, 0x3f, 0x1f)
                }
                Chip::Ymf278b => {
                    builder.add_chip_write(
                        Instance::Primary,
                        Ymf278bSpec {
                            port: 0,
                            register: 0xb0,
                            value: 0x3f,
                        },
                    );
                    (0xb0, 0x3f, 0x1f)
                }
                _ => unreachable!(),
            };
            builder.add_vgm_command(WaitSamples(441));
            std::fs::write(&path, Vec::<u8>::from(builder.finalize())).unwrap();
            let recorder = Rc::new(RefCell::new(Recorder::default()));
            let mut factories = ChipFactoryRegistry::new();
            register_ymfm_factories(&mut factories, Rc::clone(&recorder)).unwrap();
            let mut player = Sequencer::with_factories(&path, 44_100, false, factories).unwrap();
            let mute = Arc::new(FmMute::default());
            assert!(mute.set_muted(&chip, Instance::Primary, 0, true));
            player.filter_fm_mutes(mute);
            while player.render().unwrap().is_some() {}
            let writes = &recorder.borrow().0;
            assert!(
                writes
                    .iter()
                    .any(|event| event.write == ndsif::RegisterWrite::new(register, expected)),
                "{chip:?}: expected {expected:#x} instead of {value:#x}"
            );
        }
    }

    #[test]
    fn fm_mute_silences_mdx_without_affecting_original_track() {
        use soundlog::mdx::command::{MdxNote, MdxPan, MdxVoiceOrPcmBank};
        use soundlog::mdx::document::MdxBuilder;
        use soundlog::mdx::tone::{MdxOperator, MdxTone};

        let mut builder = MdxBuilder::new();
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
            .add_mdx_command(1, MdxVoiceOrPcmBank { value: 0 })
            .add_mdx_command(1, MdxPan::Center)
            .add_mdx_command(
                1,
                MdxNote {
                    note: 0x9c,
                    length: 32,
                },
            );
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        let render = |muted: [bool; 2], soloed: [bool; 2]| {
            let player = MdxSequencer::new(package.clone(), 44_100, 1024).unwrap();
            let mut player = Sequencer::Mdx(player);
            let mute = Arc::new(FmMute::default());
            for (channel, muted) in muted.into_iter().enumerate() {
                assert!(mute.set_muted(&Chip::Ym2151, Instance::Primary, channel as u8, muted));
            }
            for (channel, soloed) in soloed.into_iter().enumerate() {
                assert!(mute.set_soloed(&Chip::Ym2151, Instance::Primary, channel as u8, soloed));
            }
            player.filter_fm_mutes(mute);
            let mut peak = 0.0_f32;
            while let Some(block) = player.render().unwrap() {
                for sample in block.frames().iter().flatten() {
                    peak = peak.max(sample.abs());
                }
            }
            peak
        };
        assert!(render([false, false], [false, false]) > 0.001);
        assert!(render([true, false], [false, false]) > 0.001);
        assert!(render([false, true], [false, false]) > 0.001);
        assert_eq!(render([true, true], [false, false]), 0.0);
        assert_eq!(
            render([true, false], [false, false]),
            render([false, false], [false, true])
        );
        assert_eq!(
            render([false, true], [false, false]),
            render([true, true], [true, false])
        );
    }

    #[test]
    fn fm_mute_silences_ymfm_fixtures_without_muting_ssg() {
        let fixtures: &[(Chip, &[u8])] = &[
            (
                Chip::Ym2151,
                include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
            ),
            (
                Chip::Ym2203,
                include_bytes!("../../soundlog/assets/vgm/ym2203_fm_a4.vgm"),
            ),
            (
                Chip::Ym2413,
                include_bytes!("../../soundlog/assets/vgm/ym2413_fm_a4.vgm"),
            ),
            (
                Chip::Ym2608,
                include_bytes!("../../soundlog/assets/vgm/ym2608_fm_a4.vgm"),
            ),
            (
                Chip::Ym2610b,
                include_bytes!("../../soundlog/assets/vgm/ym2610b_fm_a4.vgm"),
            ),
            (
                Chip::Ym2612,
                include_bytes!("../../soundlog/assets/vgm/ym2612_a4.vgm"),
            ),
            (
                Chip::Ym3526,
                include_bytes!("../../soundlog/assets/vgm/ym3526_fm_a4.vgm"),
            ),
            (
                Chip::Y8950,
                include_bytes!("../../soundlog/assets/vgm/y8950_fm_a4.vgm"),
            ),
            (
                Chip::Ym3812,
                include_bytes!("../../soundlog/assets/vgm/ym3812_fm_a4.vgm"),
            ),
            (
                Chip::Ymf262,
                include_bytes!("../../soundlog/assets/vgm/ymf262_fm_a4.vgm"),
            ),
            (
                Chip::Ymf278b,
                include_bytes!("../../soundlog/assets/vgm/ymf278b_fm_a4.vgm"),
            ),
        ];
        let render = |chip: &Chip, data: &[u8], muted: Option<bool>| {
            let mut player = Sequencer::Vgm(VgmSequencer::from_vgm(data, 44_100, 1024).unwrap());
            if let Some(muted) = muted {
                let mute = Arc::new(FmMute::default());
                for channel in 0..crate::fm_mute::fm_channels(chip).unwrap() {
                    assert!(mute.set_muted(chip, Instance::Primary, channel, muted));
                }
                player.filter_fm_mutes(mute);
            }
            let mut frames = Vec::new();
            while let Some(block) = player.render().unwrap() {
                frames.extend_from_slice(block.frames());
                assert!(frames.len() < 44_100 * 10);
            }
            frames
        };
        for (chip, data) in fixtures {
            let original = render(chip, data, None);
            let unmuted = render(chip, data, Some(false));
            let muted = render(chip, data, Some(true));
            assert_eq!(original.len(), muted.len());
            assert_eq!(original, unmuted, "{chip:?}");
            // YM2612's modeled DAC ladder has a nonzero idle output.
            let idle_limit: f32 = if *chip == Chip::Ym2612 { 0.02 } else { 0.00001 };
            assert!(
                original
                    .iter()
                    .flatten()
                    .any(|sample| sample.abs() > (idle_limit * 2.0).max(0.001)),
                "{chip:?}"
            );
            assert!(
                muted
                    .iter()
                    .flatten()
                    .all(|sample| sample.abs() < idle_limit),
                "{chip:?}"
            );
        }
        for (chip, data) in [
            (
                Chip::Ym2203,
                &include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..],
            ),
            (
                Chip::Ym2608,
                &include_bytes!("../../soundlog/assets/vgm/ym2608_psg_a4.vgm")[..],
            ),
            (
                Chip::Ym2610b,
                &include_bytes!("../../soundlog/assets/vgm/ym2610b_psg_a4.vgm")[..],
            ),
        ] {
            let original = render(&chip, data, None);
            assert!(
                original.iter().flatten().any(|sample| sample.abs() > 0.001),
                "{chip:?}"
            );
            assert_eq!(original, render(&chip, data, Some(true)), "{chip:?}");
        }
    }

    #[test]
    fn chip_gain_accepts_only_zero_to_one_hundred_percent() {
        let player = Playback::new();
        for part in [GainPart::Chip, GainPart::Fm, GainPart::Ssg] {
            for gain in [0.0, 0.5, 1.0] {
                player.set_chip_gain(Chip::Ym2203, part, gain).unwrap();
            }
            for gain in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
                assert!(player.set_chip_gain(Chip::Ym2203, part, gain).is_err());
                assert_eq!(
                    player.gains.values.lock().unwrap()[&(Chip::Ym2203, part)],
                    1.0
                );
            }
        }
    }

    #[test]
    fn chip_gains_persist_across_playback_controls_and_skip_hardware() {
        let directory = tempfile::tempdir().unwrap();
        let track = directory.path().join("psg.vgm");
        std::fs::write(
            &track,
            include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm"),
        )
        .unwrap();
        let player = Playback::new();
        player
            .set_chip_gain(Chip::Ym2203, GainPart::Chip, 1.0)
            .unwrap();
        player
            .set_chip_gain(Chip::Ym2203, GainPart::Ssg, 0.0)
            .unwrap();
        assert!(
            player
                .set_chip_gain(Chip::Ym2203, GainPart::Ssg, 1.01)
                .is_err()
        );
        let source = GainSource {
            chip: Chip::Ym2203,
            instance: Instance::Primary,
            hardware: false,
            visual_only: false,
            target: None,
            target_device_name: None,
        };
        let render = |hardware| {
            let control =
                PlaybackControl::with_gains(Arc::clone(&player.volume), Arc::clone(&player.gains));
            let mut sequencer = Sequencer::new(&track, 44_100, false).unwrap();
            let mut revision = None;
            Sequencer::apply_chip_gains(
                &mut sequencer,
                &[GainSource {
                    hardware,
                    ..source.clone()
                }],
                &control.gains,
                &mut revision,
            )
            .unwrap();
            sequencer.render().unwrap().unwrap().frames().to_vec()
        };
        let muted_first = render(false);
        let muted_next = render(false);
        let hardware = render(true);
        assert_eq!(muted_first, muted_next);
        assert!(muted_first.iter().all(|frame| frame[0].abs() < 0.00001));
        assert!(hardware.iter().any(|frame| frame[0].abs() > 0.001));
        let mut live = Sequencer::new(&track, 44_100, false).unwrap();
        let mut revision = None;
        Sequencer::apply_chip_gains(
            &mut live,
            std::slice::from_ref(&source),
            &player.gains,
            &mut revision,
        )
        .unwrap();
        assert!(
            live.render()
                .unwrap()
                .unwrap()
                .frames()
                .iter()
                .all(|frame| frame[0].abs() < 0.00001)
        );
        player
            .set_chip_gain(Chip::Ym2203, GainPart::Ssg, 1.0)
            .unwrap();
        Sequencer::apply_chip_gains(
            &mut live,
            std::slice::from_ref(&source),
            &player.gains,
            &mut revision,
        )
        .unwrap();
        let mut live_audible = false;
        for _ in 0..4 {
            live_audible |= live
                .render()
                .unwrap()
                .unwrap()
                .frames()
                .iter()
                .any(|frame| frame[0].abs() > 0.001);
        }
        assert!(live_audible);
        let audible = render(false);
        assert!(audible.iter().any(|frame| frame[0].abs() > 0.001));
    }

    fn rhythm_vgm(instance: Instance) -> Vec<u8> {
        use soundlog::chip::Ym2608Spec;
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

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
        builder.finalize().into()
    }

    fn rhythm_audio(
        track: &Path,
        rom_dir: &Path,
        routes: &[ndsif_transport::NdsifRoute],
    ) -> Vec<[f32; 2]> {
        let mut player = Sequencer::for_playback(
            track,
            44_100,
            false,
            ChipFactoryRegistry::new(),
            rom_dir,
            routes,
        )
        .unwrap();
        let mut output = Vec::new();
        while let Some(block) = player.render().unwrap() {
            output.extend_from_slice(block.frames());
        }
        output
    }

    #[test]
    fn ym2608_rhythm_rom_loads_for_software_primary_secondary_and_vgz() {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let rom_dir = dir.path().join("working");
        let song_dir = dir.path().join("music");
        std::fs::create_dir(&rom_dir).unwrap();
        std::fs::create_dir(&song_dir).unwrap();
        for instance in [Instance::Primary, Instance::Secondary] {
            for compressed in [false, true] {
                let track = song_dir.join(if compressed { "track.vgz" } else { "track.vgm" });
                let source = rhythm_vgm(instance);
                let bytes = if compressed {
                    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                    encoder.write_all(&source).unwrap();
                    encoder.finish().unwrap()
                } else {
                    source
                };
                std::fs::write(&track, bytes).unwrap();
                let absent = rhythm_audio(&track, &rom_dir, &[]);
                std::fs::write(rom_dir.join(YM2608_RHYTHM_ROM), [0x77; 8192]).unwrap();
                let loaded = rhythm_audio(&track, &rom_dir, &[]);
                assert!(loaded.iter().flatten().any(|sample| sample.abs() > 0.001));
                let mut reference =
                    VgmSequencer::from_vgm(read_track(&track).unwrap(), 44_100, CHUNK_FRAMES)
                        .unwrap();
                reference.set_loop_count(Some(2));
                reference.set_loop_base(0);
                reference.set_loop_modifier(0x10);
                reference
                    .set_fadeout_samples(Some(FADEOUT_SAMPLES))
                    .unwrap();
                reference
                    .set_ym2608_rhythm_rom(instance, &[0x77; 8192])
                    .unwrap();
                let mut expected = Vec::new();
                while let Some(block) = reference.render().unwrap() {
                    expected.extend_from_slice(block.frames());
                }
                assert_eq!(loaded, expected);
                assert_ne!(loaded, absent);
                std::fs::write(song_dir.join(YM2608_RHYTHM_ROM), [0x11; 8192]).unwrap();
                let adjacent = rhythm_audio(&track, &rom_dir, &[]);
                assert_ne!(adjacent, loaded);
                std::fs::remove_file(rom_dir.join(YM2608_RHYTHM_ROM)).unwrap();
                assert_eq!(rhythm_audio(&track, &rom_dir, &[]), adjacent);
                std::fs::write(rom_dir.join(YM2608_RHYTHM_ROM), [0x77; 8192]).unwrap();
                std::fs::write(song_dir.join(YM2608_RHYTHM_ROM), []).unwrap();
                assert!(
                    format!(
                        "{:#}",
                        Sequencer::for_playback(
                            &track,
                            44_100,
                            false,
                            ChipFactoryRegistry::new(),
                            &rom_dir,
                            &[],
                        )
                        .err()
                        .unwrap()
                    )
                    .contains("empty")
                );
                std::fs::remove_file(song_dir.join(YM2608_RHYTHM_ROM)).unwrap();
                if instance == Instance::Secondary {
                    let route = ndsif_transport::NdsifRoute {
                        chip: Chip::Ym2608,
                        source: Instance::Primary,
                        device: 0,
                        target: Instance::Primary,
                    };
                    assert_eq!(rhythm_audio(&track, &rom_dir, &[route]), loaded);
                }
                let route = ndsif_transport::NdsifRoute {
                    chip: Chip::Ym2608,
                    source: instance,
                    device: 0,
                    target: Instance::Primary,
                };
                assert_eq!(rhythm_audio(&track, &rom_dir, &[route]), absent);
                std::fs::remove_file(rom_dir.join(YM2608_RHYTHM_ROM)).unwrap();
            }
        }
    }

    #[test]
    fn ym2608_rom_errors_only_affect_relevant_software_playback() {
        let dir = tempfile::tempdir().unwrap();
        let track = dir.path().join("track.vgm");
        std::fs::write(&track, rhythm_vgm(Instance::Primary)).unwrap();
        let rom = dir.path().join(YM2608_RHYTHM_ROM);
        std::fs::write(&rom, []).unwrap();
        let make = |routes: &[ndsif_transport::NdsifRoute]| {
            Sequencer::for_playback(
                &track,
                44_100,
                false,
                ChipFactoryRegistry::new(),
                dir.path(),
                routes,
            )
        };
        assert!(format!("{:#}", make(&[]).err().unwrap()).contains("empty"));
        let route = ndsif_transport::NdsifRoute {
            chip: Chip::Ym2608,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        };
        assert!(make(&[route]).is_ok());
        std::fs::remove_file(&rom).unwrap();
        std::fs::create_dir(&rom).unwrap();
        assert!(format!("{:#}", make(&[]).err().unwrap()).contains("not a file"));
        std::fs::remove_dir(&rom).unwrap();
        let file = std::fs::File::create(&rom).unwrap();
        file.set_len(64 * 1024 * 1024 + 1).unwrap();
        assert!(format!("{:#}", make(&[]).err().unwrap()).contains("exceeds"));
        let other_track = dir.path().join("other.vgm");
        std::fs::write(
            &other_track,
            Vec::<u8>::from(soundlog::vgm::VgmBuilder::new().finalize()),
        )
        .unwrap();
        assert!(
            Sequencer::for_playback(
                &other_track,
                44_100,
                false,
                ChipFactoryRegistry::new(),
                dir.path(),
                &[],
            )
            .is_ok()
        );
        assert!(Sequencer::new(&track, 44_100, false).is_ok());
    }

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
            silence_remaining: 0,
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
    fn leading_silence_delays_audio_and_hardware_presentation() {
        let mut player = Playback::new();
        assert!(!player.waiting_for_start_at(Instant::now()));
        Arc::get_mut(&mut player.control)
            .unwrap()
            .settings
            .start_delay_seconds = 1;
        let control = player.control.clone();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        assert!(control.hardware_timeline.set(Arc::clone(&timeline)).is_ok());
        let (sender, receiver) = mpsc::channel();
        let (events, _) = mpsc::channel();
        sender
            .send(Block::Audio(vec![[0.25, 0.5], [0.5, 0.25]]))
            .unwrap();
        let mut consumer = AudioConsumer {
            rx: receiver,
            events,
            control: control.clone(),
            frames: Vec::new(),
            position: 0,
            ended: false,
            channels: 2,
            rate: 4,
            silence_remaining: 4,
        };
        let now = Instant::now();
        let latency = Duration::from_millis(100);
        let mut output = [1.0_f32; 4];
        consumer.fill_at(&mut output, now, latency);
        assert_eq!(output, [0.0; 4]);
        assert_eq!(consumer.silence_remaining, 2);
        assert_eq!(control.frames.load(Ordering::Relaxed), 0);
        assert!(control.first_presentation.get().is_none());
        assert!(player.waiting_for_start_at(now));
        player.set_paused(true);
        consumer.fill_at(&mut output, now, latency);
        assert_eq!(consumer.silence_remaining, 2);
        player.set_paused(false);
        let mut output = [1.0_f32; 6];
        consumer.fill_at(&mut output, now + Duration::from_millis(500), latency);
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 0.25, 0.5]);
        let presentation = now + latency + Duration::from_secs(1);
        assert_eq!(control.first_presentation.get(), Some(&presentation));
        assert!(player.waiting_for_start_at(presentation - Duration::from_nanos(1)));
        assert!(!player.waiting_for_start_at(presentation));
        assert!(timeline.start(now).is_err());
        assert_eq!(control.silence_clock.position(presentation, 4).unwrap(), 4);
        assert_eq!(control.visual_clock.position(presentation, 4).unwrap(), 0);
        assert_eq!(
            control
                .visual_clock
                .position(presentation + Duration::from_millis(250), 4)
                .unwrap(),
            1
        );
    }

    #[test]
    fn incoming_piano_roll_tracks_the_presented_leading_silence() {
        use soundlog::chip::event::{StateEvent, ToneInfo};

        let mut player = Playback::new();
        Arc::get_mut(&mut player.control)
            .unwrap()
            .settings
            .start_delay_seconds = 1;
        let control = player.control.clone();
        control.sample_rate.store(4, Ordering::Relaxed);
        {
            let mut preview = control.preview.lock().unwrap();
            preview.roll.apply(
                0,
                Instance::Primary,
                0,
                vec![StateEvent::KeyOn {
                    channel: 0,
                    tone: ToneInfo::new(0, 0, Some(440.0)),
                }],
            );
            preview.scanned = 4;
            preview.done = true;
        }
        let (sender, receiver) = mpsc::channel();
        let (events, _) = mpsc::channel();
        sender.send(Block::Audio(vec![[0.25, 0.5]])).unwrap();
        let mut consumer = AudioConsumer {
            rx: receiver,
            events,
            control: control.clone(),
            frames: Vec::new(),
            position: 0,
            ended: false,
            channels: 2,
            rate: 4,
            silence_remaining: 4,
        };
        let now = Instant::now();
        let latency = Duration::from_millis(100);
        consumer.fill_at(&mut [0.0_f32; 4], now, latency);
        let before = player
            .visual_snapshot_at(now + latency, RollMode::Incoming)
            .unwrap();
        assert_eq!((before.leading_silence, before.notes[0].start), (4, 4));
        let midway = player
            .visual_snapshot_at(
                now + latency + Duration::from_millis(250),
                RollMode::Incoming,
            )
            .unwrap();
        assert_eq!((midway.leading_silence, midway.notes[0].start), (3, 3));
        assert!(!midway.started);
        let held = player
            .visual_snapshot_at(now + Duration::from_secs(2), RollMode::Incoming)
            .unwrap();
        assert_eq!((held.leading_silence, held.notes[0].start), (2, 2));
        consumer.fill_at(&mut [0.0_f32; 6], now + Duration::from_secs(2), latency);
        let started = player
            .visual_snapshot_at(
                now + Duration::from_secs(2) + latency + Duration::from_millis(500),
                RollMode::Incoming,
            )
            .unwrap();
        assert_eq!((started.leading_silence, started.notes[0].start), (0, 0));
        assert!(started.started);
    }

    #[test]
    fn idle_piano_preview_includes_configured_leading_silence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.vgm");
        std::fs::write(
            &path,
            include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
        )
        .unwrap();
        let settings = PlaybackSettings {
            start_delay_seconds: 2,
            ..PlaybackSettings::default()
        };
        let preview =
            IdlePreview::start_with_settings(path, false, None, settings, VgmSettings::default())
                .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let snapshot = loop {
            let snapshot = preview.visual_snapshot(RollMode::Incoming).unwrap();
            if !snapshot.scanning {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(snapshot.leading_silence, 2 * u64::from(snapshot.rate));
        assert!(!snapshot.notes.is_empty());
        assert!(
            snapshot
                .notes
                .iter()
                .all(|note| note.start >= snapshot.leading_silence)
        );

        let mut player = Playback::new();
        Arc::get_mut(&mut player.control).unwrap().settings = settings;
        let fallback = player
            .visual_snapshot_with_preview(RollMode::Incoming, Some(&preview))
            .unwrap();
        assert_eq!(fallback.leading_silence, 88_200);
        player.control.sample_rate.store(48_000, Ordering::Release);
        let now = Instant::now();
        player
            .control
            .silence_clock
            .publish(48_000, 0, now)
            .unwrap();
        let fallback = player
            .visual_snapshot_with_preview(RollMode::Incoming, Some(&preview))
            .unwrap();
        assert_eq!(fallback.leading_silence, 44_100);
        let trail = preview.visual_snapshot(RollMode::Trail).unwrap();
        assert_eq!(trail.leading_silence, 0);
        assert!(trail.notes.is_empty());
    }

    #[test]
    fn visual_clock_tracks_presentation_and_freezes_at_buffer_end() {
        let clock = PresentationClock::new();
        let origin = clock.epoch + Duration::from_secs(1);
        assert_eq!(clock.position(origin, 100).unwrap(), 0);
        clock.publish(100, 50, origin).unwrap();
        assert_eq!(
            clock
                .position(origin - Duration::from_millis(500), 100)
                .unwrap(),
            50
        );
        assert_eq!(clock.position(origin, 100).unwrap(), 100);
        assert_eq!(
            clock
                .position(origin + Duration::from_millis(250), 100)
                .unwrap(),
            125
        );
        assert_eq!(
            clock
                .position(origin + Duration::from_secs(2), 100)
                .unwrap(),
            150
        );
    }

    #[test]
    fn visual_notes_wait_for_first_presentation() {
        use soundlog::chip::event::{StateEvent, ToneInfo};
        use soundlog::vgm::command::Instance;

        let player = Playback::new();
        let origin = Instant::now() + Duration::from_secs(1);
        player.control.sample_rate.store(100, Ordering::Relaxed);
        player.control.visualizer.lock().unwrap().apply(
            1,
            Instance::Primary,
            0,
            vec![StateEvent::KeyOn {
                channel: 0,
                tone: ToneInfo::new(0, 0, Some(440.0)),
            }],
        );
        assert!(
            player
                .visual_snapshot_at(origin, RollMode::Trail)
                .unwrap()
                .notes
                .is_empty()
        );
        assert!(player.control.first_presentation.set(origin).is_ok());
        player.control.visual_clock.publish(0, 10, origin).unwrap();
        assert!(
            player
                .visual_snapshot_at(origin - Duration::from_millis(1), RollMode::Trail)
                .unwrap()
                .notes
                .is_empty()
        );
        assert_eq!(
            player
                .visual_snapshot_at(origin, RollMode::Trail)
                .unwrap()
                .notes
                .len(),
            1
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
                silence_remaining: 0,
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
                silence_remaining: 0,
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
            silence_remaining: 0,
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
            silence_remaining: 0,
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
        player
            .set_chip_gain(Chip::Ym2203, GainPart::Ssg, 0.6)
            .unwrap();
        let preview_mute = FmMute::default();
        assert!(preview_mute.set_muted(&Chip::Ym2151, Instance::Primary, 0, true));
        assert!(preview_mute.set_soloed(&Chip::Ym2151, Instance::Primary, 1, true));
        player.play(PathBuf::from("first.vgm"), true, Some(&preview_mute));
        let first_generation = player.generation;
        let (looped, first) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(looped);
        assert!(first.fm_mute.is_muted(&Chip::Ym2151, Instance::Primary, 0));
        assert!(first.fm_mute.is_soloed(&Chip::Ym2151, Instance::Primary, 1));
        assert!(
            player
                .fm_mute()
                .set_muted(&Chip::Ym2151, Instance::Primary, 0, true)
        );
        assert!(
            player
                .fm_mute()
                .set_soloed(&Chip::Ym2151, Instance::Primary, 1, true)
        );
        player.set_paused(true);
        assert!(first.paused.load(Ordering::Relaxed));
        player.play(PathBuf::from("second.vgm"), false, None);
        let (looped, second) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!looped);
        assert!(first.cancel.load(Ordering::Relaxed));
        assert!(!second.paused.load(Ordering::Relaxed));
        assert!(!second.fm_mute.is_muted(&Chip::Ym2151, Instance::Primary, 0));
        assert!(first.fm_mute.is_muted(&Chip::Ym2151, Instance::Primary, 0));
        assert!(
            !second
                .fm_mute
                .is_soloed(&Chip::Ym2151, Instance::Primary, 1)
        );
        assert!(first.fm_mute.is_soloed(&Chip::Ym2151, Instance::Primary, 1));
        assert_eq!(second.volume.get(), 0.25);
        assert!(Arc::ptr_eq(&first.gains, &second.gains));
        assert_eq!(
            second
                .gains
                .values
                .lock()
                .unwrap()
                .get(&(Chip::Ym2203, GainPart::Ssg)),
            Some(&0.6)
        );
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
        player.play(PathBuf::from("first"), false, None);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "first"
        );
        player.play(PathBuf::from("second"), false, None);
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

            let settings = PlaybackSettings {
                loop_count: 3,
                fadeout_seconds: 1,
                start_delay_seconds: 0,
            };
            let mut configured_vgm = Sequencer::create(
                &vgm,
                rate,
                false,
                ChipFactoryRegistry::new(),
                None,
                settings,
            )
            .unwrap();
            let expected_vgm = ((10_000 + u64::from(settings.fadeout_samples())) * u64::from(rate)
                + 22_050)
                / 44_100;
            assert_eq!(render_frames(&mut configured_vgm), expected_vgm as usize);
            let configured_mdx_duration = measure_mdx_duration_frames(
                MdxPackage::parse(mdx_bytes, Some(pdx_bytes)).unwrap(),
                rate,
                mdx_options(false, settings),
                Some(settings.fadeout_samples()),
            )
            .unwrap()
            .unwrap();
            let mut configured_mdx = Sequencer::create(
                &mdx,
                rate,
                false,
                ChipFactoryRegistry::new(),
                None,
                settings,
            )
            .unwrap();
            assert_eq!(
                render_frames(&mut configured_mdx),
                configured_mdx_duration as usize
            );

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
    fn visualization_tracks_vgm_and_mdx_without_changing_pcm() {
        use soundlog::chip::{Chip, Ym2151Spec};
        use soundlog::vgm::command::WaitSamples;

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("track.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
            builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(44_100));
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x08,
                value: 0,
            },
        );
        builder.add_vgm_command(WaitSamples(44_100));
        std::fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();
        let mdx = dir.path().join("track.mdx");
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

        for path in [&vgm, &mdx] {
            let mut plain = Sequencer::new(path, 44_100, false).unwrap();
            let mut tracked = Sequencer::new(path, 44_100, false).unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            tracked.track_visual_events(Rc::clone(&events));
            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            let mut position = 0;
            let mut observed = false;
            for _ in 0..180 {
                let expected = plain.render().unwrap();
                let actual = tracked.render().unwrap();
                assert_eq!(actual, expected, "PCM changed for {}", path.display());
                flush_visual_events(&control, &events, tracked.take_visual_overflow()).unwrap();
                let Some(block) = actual else {
                    break;
                };
                position += block.frames().len() as u64;
                observed |= !control
                    .visualizer
                    .lock()
                    .unwrap()
                    .snapshot(position, 44_100, RollMode::Trail)
                    .is_empty();
            }
            assert!(observed, "no notes captured for {}", path.display());
        }
    }

    #[test]
    fn preview_stream_waits_accumulate_across_loops() {
        use soundlog::chip::{Chip, Ym2151Spec};
        use soundlog::vgm::command::WaitSamples;
        use std::cell::Cell;

        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.set_loop_index(0);
        builder.add_vgm_command(WaitSamples(441));
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 8,
                value: 0x78,
            },
        );
        builder.add_vgm_command(WaitSamples(882));
        let mut stream = soundlog::vgm::VgmCallbackStream::from_document(builder.finalize());
        stream.set_loop_count(None);
        let position = Rc::new(Cell::new(0_u64));
        let waiting = Rc::clone(&position);
        stream.on_wait(move |wait, _, _| waiting.set(waiting.get() + u64::from(wait.0)));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&writes);
        stream.on_write(move |_, _: Ym2151Spec, _, _| recorded.borrow_mut().push(position.get()));
        while writes.borrow().len() < 2 {
            stream.next().unwrap().unwrap();
        }
        assert_eq!(*writes.borrow(), [441, 1764]);
    }

    #[test]
    fn playback_keeps_idle_preview_until_live_preview_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.vgm");
        std::fs::write(
            &path,
            include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
        )
        .unwrap();
        let preview = IdlePreview::start(path, false, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while preview
            .visual_snapshot(RollMode::Incoming)
            .unwrap()
            .scanning
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let player = Playback::new();
        let idle = preview.visual_snapshot(RollMode::Incoming).unwrap();
        assert!(!idle.notes.is_empty());
        let initial = player
            .visual_snapshot_with_preview(RollMode::Incoming, Some(&preview))
            .unwrap();
        assert_eq!(initial.notes.len(), idle.notes.len());
        assert_eq!(initial.position, 0);
        assert!(!initial.started);
        assert!(
            player
                .visual_snapshot_with_preview(RollMode::Trail, Some(&preview))
                .unwrap()
                .notes
                .is_empty()
        );

        player.control.sample_rate.store(48_000, Ordering::Release);
        let now = Instant::now();
        player.control.visual_clock.publish(24_000, 0, now).unwrap();
        assert!(player.control.first_presentation.set(now).is_ok());
        let advancing = player
            .visual_snapshot_with_preview(RollMode::Incoming, Some(&preview))
            .unwrap();
        assert_eq!(advancing.position, 22_050);
        assert_eq!(advancing.rate, 44_100);
        assert!(advancing.started);

        player.control.preview.lock().unwrap().done = true;
        let live = player
            .visual_snapshot_with_preview(RollMode::Incoming, Some(&preview))
            .unwrap();
        assert_eq!(live.rate, 48_000);
        assert_eq!(live.position, 24_000);
        assert!(live.notes.is_empty());

        player.control.preview.lock().unwrap().error = Some("test failure".into());
        assert!(
            player
                .visual_snapshot_with_preview(RollMode::Incoming, Some(&preview))
                .err()
                .expect("live preview error must be shown")
                .to_string()
                .contains("test failure")
        );
    }

    #[test]
    fn idle_preview_reads_vgm_and_mdx_without_playback() {
        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("notes.vgm");
        let mdx = dir.path().join("notes.mdx");
        std::fs::write(
            &vgm,
            include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm"),
        )
        .unwrap();
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
        let plan = PlaybackPlan {
            device_ids: vec![1],
            device_names: HashMap::new(),
            nd7_devices: vec![],
            routing: RoutingPlan::new(
                &[crate::routing::Connection {
                    chip: crate::routing::HardwareChip::Ym2151,
                    source: crate::routing::ChipInstance::Primary,
                    device_id: 1,
                    target: crate::routing::ChipInstance::Primary,
                }],
                VgmSettings::default(),
            ),
            routes: vec![ndsif_transport::NdsifRoute {
                chip: Chip::Ym2151,
                source: Instance::Primary,
                device: 0,
                target: Instance::Primary,
            }],
        };
        for path in [vgm, mdx] {
            let preview = IdlePreview::start(path.clone(), false, Some(&plan)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let snapshot = preview.visual_snapshot(RollMode::Incoming).unwrap();
                let sources = preview.gain_sources().unwrap();
                if !snapshot.scanning && sources.is_some() {
                    assert!(!snapshot.notes.is_empty(), "{}", path.display());
                    assert_eq!(snapshot.position, 0);
                    assert!(!snapshot.started);
                    assert!(sources.unwrap().iter().any(|source| {
                        source.chip == Chip::Ym2151
                            && source.instance == Instance::Primary
                            && source.hardware
                    }));
                    let trail = preview.visual_snapshot(RollMode::Trail).unwrap();
                    assert!(trail.notes.is_empty());
                    assert!(!trail.preparing);
                    break;
                }
                assert!(Instant::now() < deadline, "{}", path.display());
                thread::sleep(Duration::from_millis(10));
            }
            let control = Arc::clone(&preview.control);
            drop(preview);
            assert!(control.cancel.load(Ordering::Acquire));
        }
    }

    #[test]
    fn idle_preview_uses_initial_opl_mode_not_future_mode() {
        use soundlog::chip::Ymf262Spec;
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opl3.vgm");
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ymf262, Instance::Primary, 14_318_180);
        builder.add_chip_write(
            Instance::Primary,
            Ymf262Spec {
                port: 1,
                register: 4,
                value: 1,
            },
        );
        builder.add_chip_write(
            Instance::Primary,
            Ymf262Spec {
                port: 0,
                register: 0xbd,
                value: 0x20,
            },
        );
        builder.add_vgm_command(WaitSamples(44_100));
        builder.add_chip_write(
            Instance::Primary,
            Ymf262Spec {
                port: 1,
                register: 4,
                value: 0,
            },
        );
        builder.add_vgm_command(WaitSamples(44_100));
        std::fs::write(&path, Vec::<u8>::from(builder.finalize())).unwrap();
        let preview = IdlePreview::start(path, false, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while preview.gain_sources().unwrap().is_none() {
            preview.visual_snapshot(RollMode::Incoming).unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            preview
                .fm_mute()
                .paired_channel(&Chip::Ymf262, Instance::Primary, 0),
            Some(3)
        );
        assert!(
            preview
                .fm_mute()
                .rhythm_channel(&Chip::Ymf262, Instance::Primary, 6)
        );
    }

    #[test]
    fn idle_preview_reports_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let preview = IdlePreview::start(dir.path().join("missing.vgm"), false, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match preview.visual_snapshot(RollMode::Incoming) {
                Err(error) => {
                    assert!(format!("{error:#}").contains("missing.vgm"));
                    break;
                }
                Ok(_) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    #[test]
    fn preview_reads_upcoming_vgm_and_mdx_notes_and_follows_position() {
        use soundlog::chip::{Chip, Ym2151Spec};
        use soundlog::vgm::command::WaitSamples;

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("preview.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.set_loop_index(0);
        builder.add_vgm_command(WaitSamples(44_100));
        for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
            builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(44_100));
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x08,
                value: 0,
            },
        );
        std::fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();
        let mdx = dir.path().join("preview.mdx");
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

        let rate = 8_000;
        for path in [&vgm, &mdx] {
            let control = Arc::new(PlaybackControl::new(Arc::new(NdsifOutputGain::new())));
            let worker = PreviewWorker::start(path, rate, true, &control).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let mut preview = control.preview.lock().unwrap();
                assert!(preview.error.is_none(), "{:?}", preview.error);
                if preview.scanned >= u64::from(rate) * WINDOW_SECONDS {
                    let notes = preview.roll.snapshot(0, rate, RollMode::Incoming);
                    assert!(
                        !notes.is_empty(),
                        "no notes for {}: {notes:?}",
                        path.display(),
                    );
                    if path == &vgm {
                        assert!(
                            notes.iter().any(|note| {
                                (8_000..=8_010).contains(&note.start)
                                    && note.end.is_some_and(|end| (16_000..=16_010).contains(&end))
                            }),
                            "first delayed note missing or mistimed: {notes:?}"
                        );
                        assert!(
                            notes.iter().any(|note| note.start >= 24_000),
                            "looped notes missing: {notes:?}"
                        );
                    }
                    break;
                }
                drop(preview);
                assert!(
                    Instant::now() < deadline,
                    "preview timed out for {}",
                    path.display()
                );
                thread::sleep(Duration::from_millis(10));
            }
            control
                .visual_clock
                .publish(24_000, 1, Instant::now())
                .unwrap();
            loop {
                let preview = control.preview.lock().unwrap();
                assert!(preview.error.is_none(), "{:?}", preview.error);
                if preview.scanned >= 24_000 + u64::from(rate) * WINDOW_SECONDS {
                    break;
                }
                drop(preview);
                assert!(Instant::now() < deadline, "preview did not advance");
                thread::sleep(Duration::from_millis(10));
            }
            drop(worker);
        }
    }

    #[test]
    fn preview_uses_per_instance_extra_header_clocks() {
        use soundlog::chip::Ym2151Spec;
        use soundlog::vgm::command::WaitSamples;
        use soundlog::vgm::header::{ChipClock, ChipId, VgmExtraHeader};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clocks.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Ym2151, Instance::Secondary, 4_000_000);
        builder.set_extra_header(VgmExtraHeader {
            chip_clocks: vec![ChipClock::new(
                ChipId::Ym2151,
                Instance::Secondary,
                2_000_000,
            )],
            ..Default::default()
        });
        builder.add_vgm_command(WaitSamples(4_410));
        for instance in [Instance::Primary, Instance::Secondary] {
            for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
                builder.add_chip_write(instance, Ym2151Spec { register, value });
            }
        }
        builder.add_vgm_command(WaitSamples(4_410));
        std::fs::write(&path, Vec::<u8>::from(builder.finalize())).unwrap();

        for rate in [8_000, 44_100, 48_000] {
            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            run_preview(&path, rate, false, &control, &AtomicBool::new(false), None).unwrap();
            let mut preview = control.preview.lock().unwrap();
            let mut notes = preview.roll.snapshot(0, rate, RollMode::Incoming);
            notes.sort_by_key(|note| note.pitch);
            assert_eq!(notes.len(), 2);
            assert_eq!(notes[1].pitch - notes[0].pitch, 12);
            assert!(notes.iter().all(|note| note.start == u64::from(rate) / 10));
            assert!(
                notes
                    .iter()
                    .all(|note| note.end == Some(u64::from(rate) / 5))
            );
        }
    }

    #[test]
    fn ay_preview_and_trail_follow_volume_clocks_and_routes() {
        use soundlog::chip::Ay8910Spec;
        use soundlog::vgm::command::WaitSamples;
        use soundlog::vgm::header::{Ay8910ChipType, ChipClock, ChipId, VgmExtraHeader};

        let dir = tempfile::tempdir().unwrap();
        for divider in [false, true] {
            let path = dir.path().join(format!("ay-{divider}.vgm"));
            let mut builder = soundlog::vgm::VgmBuilder::new();
            for instance in [Instance::Primary, Instance::Secondary] {
                builder.register_chip(Chip::Ay8910, instance, 1_792_000);
            }
            builder.set_extra_header(VgmExtraHeader {
                chip_clocks: vec![ChipClock::new(ChipId::Ay8910, Instance::Secondary, 896_000)],
                ..Default::default()
            });
            for instance in [Instance::Primary, Instance::Secondary] {
                for channel in 0..3 {
                    builder.add_chip_write(
                        instance,
                        Ay8910Spec {
                            register: 2 * channel,
                            value: 255,
                        },
                    );
                }
                builder.add_chip_write(
                    instance,
                    Ay8910Spec {
                        register: 7,
                        value: 0xf8,
                    },
                );
                for channel in 0..3 {
                    builder.add_chip_write(
                        instance,
                        Ay8910Spec {
                            register: 8 + channel,
                            value: 15,
                        },
                    );
                }
            }
            builder.add_vgm_command(WaitSamples(4_410));
            for instance in [Instance::Primary, Instance::Secondary] {
                for channel in 0..3 {
                    builder.add_chip_write(
                        instance,
                        Ay8910Spec {
                            register: 8 + channel,
                            value: 0,
                        },
                    );
                }
            }
            builder.add_vgm_command(WaitSamples(100));
            let mut document = builder.finalize();
            document.header.ay_chip_type = Ay8910ChipType::Ym2149;
            document.header.ay8910_flags.ym_pin26_low = divider;
            std::fs::write(&path, Vec::<u8>::from(document)).unwrap();

            let mut sources = Vec::new();
            add_visual_ay_sources(&path, &[], &mut sources).unwrap();
            assert_eq!(sources.len(), 2);
            assert!(
                sources
                    .iter()
                    .all(|source| source.visual_only && !source.hardware)
            );
            let route = ndsif_transport::NdsifRoute {
                chip: Chip::Ay8910,
                source: Instance::Primary,
                device: 0,
                target: Instance::Secondary,
            };
            let mut routed = Vec::new();
            add_visual_ay_sources(&path, &[route], &mut routed).unwrap();
            assert!(
                routed
                    .iter()
                    .any(|source| source.instance == Instance::Primary
                        && source.hardware
                        && !source.visual_only)
            );
            assert!(
                routed
                    .iter()
                    .any(|source| source.instance == Instance::Secondary && source.visual_only)
            );

            let player = Playback::new();
            *player.control.gain_sources.lock().unwrap() = Some(sources);
            player.control.sample_rate.store(44_100, Ordering::Relaxed);
            run_preview(
                &path,
                44_100,
                false,
                &player.control,
                &AtomicBool::new(false),
                None,
            )
            .unwrap();
            let now = Instant::now();
            assert!(
                player
                    .visual_snapshot_at(now, RollMode::Trail)
                    .unwrap()
                    .notes
                    .is_empty()
            );
            player.control.first_presentation.set(now).unwrap();
            player.control.visual_clock.publish(2_205, 0, now).unwrap();
            let incoming = player.visual_snapshot_at(now, RollMode::Incoming).unwrap();
            let trail = player.visual_snapshot_at(now, RollMode::Trail).unwrap();
            for notes in [incoming.notes, trail.notes] {
                assert_eq!(notes.len(), 6);
                for note in notes {
                    assert_eq!(note.chip, crate::visualizer::AY8910_INDEX);
                    assert!(note.channel < 3);
                    assert_eq!(
                        note.pitch,
                        69 - 12 * note.instance - if divider { 12 } else { 0 }
                    );
                    assert_eq!((note.start, note.end), (0, Some(4_410)));
                }
            }
        }
    }

    #[test]
    fn ay_nd7_hardware_route_and_preview_run_together() {
        use chipstream::chip::{
            NdsifAyCompatibility, NdsifPsgEvent, NdsifRegisterEvent, NdsifSoundChip, NdsifTransport,
        };
        use soundlog::chip::Ay8910Spec;
        use soundlog::vgm::command::WaitSamples;
        use soundlog::vgm::header::Ay8910ChipType;

        #[derive(Default)]
        struct Recorder(Vec<NdsifRegisterEvent>);
        impl NdsifTransport for Recorder {
            fn configure_chip(&mut self, _: ndsif::WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_psg_write(&mut self, _: NdsifPsgEvent) -> Result<(), String> {
                Err("unexpected PSG write".into())
            }
            fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
                self.0.push(event);
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nd7.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Ay8910, Instance::Primary, 1_792_000);
        for (register, value) in [(0, 255), (7, 0xfe), (8, 15)] {
            builder.add_chip_write(Instance::Primary, Ay8910Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(4_410));
        builder.add_chip_write(
            Instance::Primary,
            Ay8910Spec {
                register: 8,
                value: 0,
            },
        );
        builder.add_vgm_command(WaitSamples(100));
        let mut document = builder.finalize();
        document.header.ay_chip_type = Ay8910ChipType::Ym2149;
        std::fs::write(&path, Vec::<u8>::from(document)).unwrap();

        let transport = Rc::new(RefCell::new(Recorder::default()));
        let mut factories = ChipFactoryRegistry::new();
        let target = Rc::clone(&transport);
        factories.register(Chip::Ay8910, move |config| {
            Ok(Box::new(NdsifSoundChip::with_target_ay_compatibility(
                config,
                Instance::Secondary,
                Rc::clone(&target),
                NdsifAyCompatibility::Ay8910AndYm2149,
            )?))
        });
        let mut sequencer = Sequencer::with_factories(&path, 44_100, false, factories).unwrap();
        let route = ndsif_transport::NdsifRoute {
            chip: Chip::Ay8910,
            source: Instance::Primary,
            device: 0,
            target: Instance::Secondary,
        };
        let mut sources = Sequencer::gain_sources(&sequencer, std::slice::from_ref(&route));
        add_visual_ay_sources(&path, &[route], &mut sources).unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].hardware && !sources[0].visual_only);
        while sequencer.render().unwrap().is_some() {}
        assert!(!transport.borrow().0.is_empty());
        assert!(
            transport
                .borrow()
                .0
                .iter()
                .all(|event| event.instance == 1 && event.chip == Chip::Ay8910)
        );

        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        run_preview(
            &path,
            44_100,
            false,
            &control,
            &AtomicBool::new(false),
            None,
        )
        .unwrap();
        let notes = control
            .preview
            .lock()
            .unwrap()
            .roll
            .snapshot(0, 44_100, RollMode::Incoming);
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].pitch, notes[0].start, notes[0].end),
            (69, 0, Some(4_410))
        );
    }

    #[test]
    fn source_targets_snapshot_multiple_devices_and_software() {
        use crate::routing::{ChipInstance, Connection, HardwareChip, SoftwareTarget, Target};

        let connections = [
            Connection {
                chip: HardwareChip::Ay8910,
                source: ChipInstance::Primary,
                device_id: 1,
                target: ChipInstance::Secondary,
            },
            Connection {
                chip: HardwareChip::Ay8910,
                source: ChipInstance::Secondary,
                device_id: 2,
                target: ChipInstance::Primary,
            },
        ];
        let mut plan = PlaybackPlan {
            device_ids: vec![1, 2],
            device_names: HashMap::from([(1, "NanoDrive 7".into()), (2, "NanoDrive 6".into())]),
            routes: vec![],
            nd7_devices: vec![],
            routing: RoutingPlan::new(&connections, VgmSettings::default()),
        };
        let mut sources = vec![
            GainSource {
                chip: Chip::Ay8910,
                instance: Instance::Primary,
                hardware: true,
                visual_only: false,
                target: None,
                target_device_name: None,
            },
            GainSource {
                chip: Chip::Ay8910,
                instance: Instance::Secondary,
                hardware: true,
                visual_only: false,
                target: None,
                target_device_name: None,
            },
            GainSource {
                chip: Chip::Ym2151,
                instance: Instance::Primary,
                hardware: false,
                visual_only: false,
                target: None,
                target_device_name: None,
            },
        ];
        set_source_targets(&mut sources, &plan.routing, Some(&plan));
        plan.device_names.insert(1, "Renamed".into());
        plan.routing = RoutingPlan::new(&[], VgmSettings::default());
        assert_eq!(
            sources[0].target_device_name.as_deref(),
            Some("NanoDrive 7")
        );
        assert_eq!(
            sources[1].target_device_name.as_deref(),
            Some("NanoDrive 6")
        );
        assert_eq!(
            sources[0].target,
            Some(Target::Hardware {
                device_id: 1,
                chip: HardwareChip::Ay8910,
                instance: ChipInstance::Secondary,
            })
        );
        assert_eq!(
            sources[1].target,
            Some(Target::Hardware {
                device_id: 2,
                chip: HardwareChip::Ay8910,
                instance: ChipInstance::Primary,
            })
        );
        assert_eq!(
            sources[2].target,
            Some(Target::Software {
                chip: SoftwareTarget::Native(Chip::Ym2151),
                instance: ChipInstance::Primary,
            })
        );
    }

    #[test]
    fn ay_emulation_respects_settings_and_per_instance_hardware_routes() {
        use crate::routing::{ChipInstance, HardwareChip, SoftwareTarget, Target};
        use chipstream::chip::{NdsifPsgEvent, NdsifRegisterEvent, NdsifTransport};
        use soundlog::chip::Ay8910Spec;
        use soundlog::vgm::command::WaitSamples;
        use soundlog::vgm::header::Ay8910ChipType;

        fn assert_targets(sources: &[GainSource], hardware: bool, enabled: bool) {
            assert_eq!(sources.len(), 2);
            for source in sources {
                let instance = match source.instance {
                    Instance::Primary => ChipInstance::Primary,
                    Instance::Secondary => ChipInstance::Secondary,
                };
                let is_hardware = hardware && source.instance == Instance::Primary;
                let expected = if is_hardware {
                    Target::Hardware {
                        device_id: 1,
                        chip: HardwareChip::Ay8910,
                        instance: ChipInstance::Secondary,
                    }
                } else if enabled {
                    Target::Software {
                        chip: SoftwareTarget::Ym2149,
                        instance,
                    }
                } else {
                    Target::Silent
                };
                assert_eq!(source.hardware, is_hardware);
                assert_eq!(source.visual_only, !is_hardware && !enabled);
                assert_eq!(source.target.as_ref(), Some(&expected));
                assert_eq!(
                    source.target_device_name.as_deref(),
                    is_hardware.then_some("NanoDrive 7")
                );
            }
        }

        #[derive(Default)]
        struct Recorder(Vec<NdsifRegisterEvent>, bool);
        impl NdsifTransport for Recorder {
            fn configure_chip(&mut self, _: ndsif::WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_psg_write(&mut self, _: NdsifPsgEvent) -> Result<(), String> {
                Err("unexpected PSG write".into())
            }
            fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
                self.0.push(event);
                Ok(())
            }
        }
        impl ndsif_transport::NdsifRoutingTransport for Recorder {
            fn supports_route_clock(&self, _: ndsif::WriteId, _: u8, _: u32) -> bool {
                !self.1
            }
            fn supports_route_audio(&self, _: u8, _: u32) -> bool {
                false
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ay.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.register_chip(Chip::Ay8910, instance, 1_792_000);
            for (register, value) in [(0, 255), (7, 0xfe), (8, 15)] {
                builder.add_chip_write(instance, Ay8910Spec { register, value });
            }
        }
        builder.add_vgm_command(WaitSamples(4_410));
        let mut document = builder.finalize();
        document.header.ay_chip_type = Ay8910ChipType::Ym2149;
        std::fs::write(&path, Vec::<u8>::from(&document)).unwrap();
        let route = ndsif_transport::NdsifRoute {
            chip: Chip::Ay8910,
            source: Instance::Primary,
            device: 0,
            target: Instance::Secondary,
        };
        for enabled in [false, true] {
            for hardware in [false, true] {
                let transport = Rc::new(RefCell::new(Recorder::default()));
                let settings = VgmSettings {
                    ay8910_ym2149: enabled,
                };
                let mut plan = if hardware {
                    let device = crate::routing::Device {
                        id: 1,
                        name: "NanoDrive 7".into(),
                        port: "test".into(),
                        baud_rate: 115_200,
                    };
                    let capability = ndsif::CapabilityEntry {
                        write_id: 0xa0,
                        instance: 1,
                        page: 0,
                        features: 0,
                        clock_mode: ndsif::ClockMode::Variable,
                        fixed_clock_hz: 0,
                    };
                    let mut session = crate::routing::SessionRouting::default();
                    session.refresh(
                        std::slice::from_ref(&device),
                        &HashMap::from([(1, vec![capability])]),
                    );
                    let plan = session.playback_plan(&[device], settings).unwrap();
                    assert_eq!(plan.routes, [route.clone()]);
                    assert_eq!(plan.nd7_devices, [0]);
                    plan.validate_nd7(&document.header, None).unwrap();
                    Some(plan)
                } else {
                    None
                };
                let routes = plan.as_ref().map_or(&[][..], |plan| plan.routes.as_slice());
                let routing = plan.as_ref().map_or_else(
                    || RoutingPlan::new(&[], settings),
                    |plan| plan.routing.clone(),
                );
                let mut factories = if let Some(plan) = &plan {
                    ndsif_transport::create_routed_transport_factories_with_nd7(
                        std::slice::from_ref(&transport),
                        routes,
                        &plan.nd7_devices,
                    )
                    .unwrap()
                } else {
                    ChipFactoryRegistry::new()
                };
                crate::adapter::register_software_adapters(
                    &mut factories,
                    &routing,
                    crate::adapter::AdapterMode::Playback,
                );
                let player =
                    Sequencer::for_playback(&path, 44_100, false, factories, dir.path(), routes);
                if hardware && !enabled {
                    assert!(player.is_err());
                } else {
                    let mut player = player.unwrap();
                    let mut sources = Sequencer::gain_sources(&player, routes);
                    assert_eq!(sources.len(), if enabled { 2 } else { 0 });
                    if hardware {
                        assert!(
                            sources
                                .iter()
                                .any(|s| s.instance == Instance::Primary && s.hardware)
                        );
                    }
                    if enabled {
                        assert!(sources.iter().any(|s| s.instance == Instance::Secondary
                            && !s.hardware
                            && !s.visual_only));
                    }
                    add_visual_ay_sources(&path, routes, &mut sources).unwrap();
                    set_source_targets(&mut sources, &routing, plan.as_ref());
                    assert_targets(&sources, hardware, enabled);
                    let mut peak = 0.0_f32;
                    while let Some(block) = player.render().unwrap() {
                        for sample in block.frames().iter().flatten() {
                            peak = peak.max(sample.abs());
                        }
                    }
                    assert_eq!(peak > 0.001, enabled, "hardware={hardware}, peak={peak}");
                    assert_eq!(!transport.borrow().0.is_empty(), hardware);
                    assert!(transport.borrow().0.iter().all(|event| event.instance == 1));
                }

                let preview = IdlePreview::start_with_settings(
                    path.clone(),
                    false,
                    plan.as_ref(),
                    PlaybackSettings::default(),
                    VgmSettings {
                        ay8910_ym2149: enabled,
                    },
                )
                .unwrap();
                if let Some(plan) = &mut plan {
                    plan.device_names.insert(1, "Renamed".into());
                    plan.routing = RoutingPlan::new(&[], VgmSettings::default());
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                let sources = loop {
                    if let Some(sources) = preview.gain_sources().unwrap() {
                        break sources;
                    }
                    assert!(Instant::now() < deadline, "AY idle preview did not start");
                    thread::sleep(Duration::from_millis(10));
                };
                assert_targets(&sources, hardware, enabled);
            }
        }

        for (chip_type, raw_output) in [
            (Ay8910ChipType::Ay8930, false),
            (Ay8910ChipType::Ym2149, true),
        ] {
            document.header.ay_chip_type = chip_type;
            document.header.ay8910_flags.raw_output = raw_output;
            std::fs::write(&path, Vec::<u8>::from(&document)).unwrap();
            let mut factories = ChipFactoryRegistry::new();
            register_ay_emulation(
                &mut factories,
                VgmSettings {
                    ay8910_ym2149: true,
                },
            );
            assert!(
                Sequencer::for_playback(&path, 44_100, false, factories, dir.path(), &[]).is_err()
            );
        }

        let transport = Rc::new(RefCell::new(Recorder(Vec::new(), true)));
        let connections = [crate::routing::Connection {
            chip: crate::routing::HardwareChip::Ay8910,
            source: crate::routing::ChipInstance::Primary,
            device_id: 1,
            target: crate::routing::ChipInstance::Secondary,
        }];
        let routing = RoutingPlan::new(
            &connections,
            VgmSettings {
                ay8910_ym2149: true,
            },
        );
        document.header.ay_chip_type = Ay8910ChipType::Ym2149;
        document.header.ay8910_flags.raw_output = false;
        std::fs::write(&path, Vec::<u8>::from(&document)).unwrap();
        let mut missing_hardware = ChipFactoryRegistry::new();
        crate::adapter::register_software_adapters(
            &mut missing_hardware,
            &routing,
            crate::adapter::AdapterMode::Playback,
        );
        let error = Sequencer::for_playback(
            &path,
            44_100,
            false,
            missing_hardware,
            dir.path(),
            std::slice::from_ref(&route),
        )
        .err()
        .expect("a missing hardware factory must not fall back");
        assert!(format!("{error:#}").contains("hardware route has no active factory"));
        let mut factories = ndsif_transport::create_routed_transport_factories_with_nd7(
            std::slice::from_ref(&transport),
            std::slice::from_ref(&route),
            &[0],
        )
        .unwrap();
        crate::adapter::register_software_adapters(
            &mut factories,
            &routing,
            crate::adapter::AdapterMode::Playback,
        );
        let error = Sequencer::for_playback(&path, 44_100, false, factories, dir.path(), &[route])
            .err()
            .expect("hardware errors must not fall back to software");
        assert!(format!("{error:#}").contains("does not support"));
        assert!(transport.borrow().0.is_empty());
    }

    #[test]
    fn ay_emulation_pitch_and_stereo_match_vgm_and_piano_roll() {
        use soundlog::chip::Ay8910Spec;
        use soundlog::vgm::command::{Ay8910StereoMask, WaitSamples};
        use soundlog::vgm::header::Ay8910ChipType;

        let dir = tempfile::tempdir().unwrap();
        for divider in [false, true] {
            let path = dir.path().join(format!("ym2149-{divider}.vgm"));
            let mut builder = soundlog::vgm::VgmBuilder::new();
            builder.register_chip(Chip::Ay8910, Instance::Primary, 1_792_000);
            builder.add_vgm_command(Ay8910StereoMask::from_mask(0x02));
            for (register, value) in [(0, 255), (7, 0xfe), (8, 15)] {
                builder.add_chip_write(Instance::Primary, Ay8910Spec { register, value });
            }
            builder.add_vgm_command(WaitSamples(44_100));
            let mut document = builder.finalize();
            document.header.ay_chip_type = Ay8910ChipType::Ym2149;
            document.header.ay8910_flags.ym_pin26_low = divider;
            std::fs::write(&path, Vec::<u8>::from(document)).unwrap();
            let mut factories = ChipFactoryRegistry::new();
            register_ay_emulation(
                &mut factories,
                VgmSettings {
                    ay8910_ym2149: true,
                },
            );
            let mut player =
                Sequencer::for_playback(&path, 44_100, false, factories, dir.path(), &[]).unwrap();
            let mut frames = Vec::new();
            while let Some(block) = player.render().unwrap() {
                frames.extend_from_slice(block.frames());
            }
            assert!(frames.iter().all(|frame| frame[0].abs() < 0.00001));
            let peak = frames.iter().map(|frame| frame[1]).fold(0.0_f32, f32::max);
            assert!(peak > 0.001);
            let mut previous = false;
            let mut rising = 0;
            for frame in &frames[100..44_000] {
                let high = frame[1] > peak / 2.0;
                rising += usize::from(high && !previous);
                previous = high;
            }
            let expected = if divider { 219 } else { 437 };
            assert!((rising as isize - expected).abs() <= 2, "{rising}");

            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            run_preview(
                &path,
                44_100,
                false,
                &control,
                &AtomicBool::new(false),
                None,
            )
            .unwrap();
            let notes =
                control
                    .preview
                    .lock()
                    .unwrap()
                    .roll
                    .snapshot(0, 44_100, RollMode::Incoming);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].pitch, if divider { 57 } else { 69 });

            let mut factories = ChipFactoryRegistry::new();
            register_ay_emulation(
                &mut factories,
                VgmSettings {
                    ay8910_ym2149: true,
                },
            );
            let mut muted =
                Sequencer::for_playback(&path, 44_100, false, factories, dir.path(), &[]).unwrap();
            let sources = Sequencer::gain_sources(&muted, &[]);
            let gains = GainState::default();
            gains
                .values
                .lock()
                .unwrap()
                .insert((Chip::Ay8910, GainPart::Chip), 0.0);
            Sequencer::apply_chip_gains(&mut muted, &sources, &gains, &mut None).unwrap();
            assert!(
                muted
                    .render()
                    .unwrap()
                    .unwrap()
                    .frames()
                    .iter()
                    .flatten()
                    .all(|sample| sample.abs() < 0.00001)
            );
        }
    }

    #[test]
    fn sn76489_preview_and_playback_track_tones_per_instance() {
        use soundlog::chip::{GameGearPsgSpec, PsgSpec};
        use soundlog::vgm::command::WaitSamples;
        use soundlog::vgm::header::{ChipClock, ChipId, VgmExtraHeader};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("psg.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.register_chip(Chip::Sn76489, instance, 3_579_545);
        }
        builder.set_extra_header(VgmExtraHeader {
            chip_clocks: vec![ChipClock::new(
                ChipId::Sn76489,
                Instance::Secondary,
                1_789_772,
            )],
            ..Default::default()
        });
        for (instance, channel) in [
            (Instance::Primary, 0),
            (Instance::Primary, 1),
            (Instance::Primary, 2),
            (Instance::Secondary, 0),
        ] {
            for value in [0x80 | (channel << 5), 0x0f, 0x90 | (channel << 5)] {
                builder.add_chip_write(instance, PsgSpec { value });
            }
        }
        builder.add_chip_write(Instance::Primary, PsgSpec { value: 0xe0 });
        builder.add_chip_write(Instance::Primary, PsgSpec { value: 0xf0 });
        builder.add_vgm_command(WaitSamples(4_410));
        builder.add_chip_write(Instance::Primary, GameGearPsgSpec { value: 0x9f });
        for (instance, channel) in [
            (Instance::Primary, 0),
            (Instance::Primary, 1),
            (Instance::Primary, 2),
            (Instance::Secondary, 0),
        ] {
            builder.add_chip_write(
                instance,
                PsgSpec {
                    value: 0x8f | (channel << 5),
                },
            );
        }
        builder.add_vgm_command(WaitSamples(4_410));
        for (instance, channel) in [
            (Instance::Primary, 0),
            (Instance::Primary, 1),
            (Instance::Primary, 2),
            (Instance::Secondary, 0),
        ] {
            builder.add_chip_write(
                instance,
                PsgSpec {
                    value: 0x9f | (channel << 5),
                },
            );
        }
        builder.add_vgm_command(WaitSamples(100));
        std::fs::write(&path, Vec::<u8>::from(builder.finalize())).unwrap();

        let preview_control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        run_preview(
            &path,
            44_100,
            false,
            &preview_control,
            &AtomicBool::new(false),
            None,
        )
        .unwrap();
        let mut preview = preview_control.preview.lock().unwrap();
        let mut preview_notes = preview.roll.snapshot(0, 44_100, RollMode::Incoming);
        drop(preview);

        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let pending = Rc::new(RefCell::new(Vec::new()));
        let mut player = Sequencer::new(&path, 44_100, false).unwrap();
        player.track_visual_events(Rc::clone(&pending));
        while player.render().unwrap().is_some() {
            flush_visual_events(&control, &pending, player.take_visual_overflow()).unwrap();
        }
        let mut playback_notes =
            control
                .visualizer
                .lock()
                .unwrap()
                .snapshot(0, 44_100, RollMode::Incoming);
        let key = |note: &crate::visualizer::Note| (note.instance, note.channel, note.start);
        preview_notes.sort_by_key(key);
        playback_notes.sort_by_key(key);
        assert_eq!(preview_notes.len(), 8);
        assert_eq!(
            preview_notes
                .iter()
                .map(|n| (n.chip, n.instance, n.channel, n.pitch, n.start, n.end))
                .collect::<Vec<_>>(),
            playback_notes
                .iter()
                .map(|n| (n.chip, n.instance, n.channel, n.pitch, n.start, n.end))
                .collect::<Vec<_>>()
        );
        for note in preview_notes {
            assert_eq!(note.chip, crate::visualizer::SN76489_INDEX);
            assert!(note.channel < 3);
            assert_eq!(
                note.pitch,
                if note.instance == 0 {
                    if note.start == 0 { 70 } else { 69 }
                } else if note.start == 0 {
                    58
                } else {
                    57
                }
            );
            assert_eq!(
                (note.start, note.end),
                if note.start == 0 {
                    (0, Some(4_410))
                } else {
                    (4_410, Some(8_820))
                }
            );
        }
    }

    #[test]
    fn sn76489_preview_and_playback_honor_header_frequency_flags() {
        use soundlog::chip::PsgSpec;
        use soundlog::vgm::command::WaitSamples;

        let dir = tempfile::tempdir().unwrap();
        for (zero_is_1024, divider_8, expected_pitch) in [
            (false, true, None),
            (true, true, Some(45)),
            (true, false, Some(81)),
        ] {
            let path = dir.path().join(format!("{zero_is_1024}-{divider_8}.vgm"));
            let mut builder = soundlog::vgm::VgmBuilder::new();
            builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
            for value in [0x80, 0x00, 0x90] {
                builder.add_chip_write(Instance::Primary, PsgSpec { value });
            }
            builder.add_vgm_command(WaitSamples(4_410));
            builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x9f });
            builder.add_vgm_command(WaitSamples(100));
            let mut document = builder.finalize();
            document.header.sn76489_flags.frequency_0_is_400 = zero_is_1024;
            document.header.sn76489_flags.clock_divider_8 = divider_8;
            std::fs::write(&path, Vec::<u8>::from(document)).unwrap();

            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            run_preview(
                &path,
                44_100,
                false,
                &control,
                &AtomicBool::new(false),
                None,
            )
            .unwrap();
            let notes =
                control
                    .preview
                    .lock()
                    .unwrap()
                    .roll
                    .snapshot(0, 44_100, RollMode::Incoming);
            assert_eq!(notes.first().map(|note| note.pitch), expected_pitch);

            let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
            let pending = Rc::new(RefCell::new(Vec::new()));
            let mut player = Sequencer::new(&path, 44_100, false).unwrap();
            let mut plain = Sequencer::new(&path, 44_100, false).unwrap();
            player.track_visual_events(Rc::clone(&pending));
            loop {
                let tracked = player.render().unwrap();
                let untracked = plain.render().unwrap();
                assert_eq!(
                    tracked.as_ref().map(|block| block.frames()),
                    untracked.as_ref().map(|block| block.frames())
                );
                flush_visual_events(&control, &pending, player.take_visual_overflow()).unwrap();
                if tracked.is_none() {
                    break;
                }
            }
            let notes = control
                .visualizer
                .lock()
                .unwrap()
                .snapshot(0, 44_100, RollMode::Incoming);
            assert_eq!(notes.first().map(|note| note.pitch), expected_pitch);
        }
    }

    #[test]
    fn sn76489_hardware_writes_produce_piano_notes() {
        use chipstream::chip::{NdsifPsgEvent, NdsifRegisterEvent, NdsifSn76489, NdsifTransport};
        use soundlog::chip::PsgSpec;
        use soundlog::vgm::command::WaitSamples;

        #[derive(Default)]
        struct Recorder(Vec<NdsifPsgEvent>);
        impl NdsifTransport for Recorder {
            fn configure_chip(&mut self, _: ndsif::WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
                self.0.push(event);
                Ok(())
            }
            fn schedule_register_write(&mut self, _: NdsifRegisterEvent) -> Result<(), String> {
                Err("unexpected register write".into())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hardware.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
        for value in [0x8e, 0x0f, 0x90] {
            builder.add_chip_write(Instance::Primary, PsgSpec { value });
        }
        builder.add_vgm_command(WaitSamples(4_410));
        builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x9f });
        builder.add_vgm_command(WaitSamples(100));
        std::fs::write(&path, Vec::<u8>::from(builder.finalize())).unwrap();

        let transport = Rc::new(RefCell::new(Recorder::default()));
        let mut factories = ChipFactoryRegistry::new();
        let target = Rc::clone(&transport);
        factories.register(Chip::Sn76489, move |config| {
            Ok(Box::new(NdsifSn76489::with_target_instance(
                config,
                Instance::Secondary,
                Rc::clone(&target),
            )?))
        });
        let mut player = Sequencer::with_factories(&path, 44_100, false, factories).unwrap();
        let control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
        let pending = Rc::new(RefCell::new(Vec::new()));
        player.track_visual_events(Rc::clone(&pending));
        while player.render().unwrap().is_some() {
            flush_visual_events(&control, &pending, player.take_visual_overflow()).unwrap();
        }
        let notes = control
            .visualizer
            .lock()
            .unwrap()
            .snapshot(0, 44_100, RollMode::Incoming);
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (
                notes[0].chip,
                notes[0].instance,
                notes[0].channel,
                notes[0].pitch
            ),
            (crate::visualizer::SN76489_INDEX, 0, 0, 69)
        );
        assert!(notes[0].end.is_some());
        assert_eq!(
            transport
                .borrow()
                .0
                .iter()
                .map(|event| event.value)
                .collect::<Vec<_>>(),
            vec![0x8e, 0x0f, 0x90, 0x9f]
        );
    }

    #[test]
    fn finite_preview_matches_playback_duration() {
        use soundlog::vgm::command::WaitSamples;

        let dir = tempfile::tempdir().unwrap();
        let vgm = dir.path().join("finite.vgm");
        let mut builder = soundlog::vgm::VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        builder.set_loop_index(1);
        builder.add_vgm_command(WaitSamples(3_000));
        std::fs::write(&vgm, Vec::<u8>::from(builder.finalize())).unwrap();
        let mdx = dir.path().join("finite.mdx");
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

        for settings in [
            PlaybackSettings::default(),
            PlaybackSettings {
                loop_count: 3,
                fadeout_seconds: 1,
                start_delay_seconds: 0,
            },
            PlaybackSettings {
                loop_count: 1,
                fadeout_seconds: 0,
                start_delay_seconds: 0,
            },
        ] {
            for rate in [8_000, 44_100, 48_000] {
                for path in [&vgm, &mdx] {
                    let mut player = Sequencer::create(
                        path,
                        rate,
                        false,
                        ChipFactoryRegistry::new(),
                        None,
                        settings,
                    )
                    .unwrap();
                    let mut expected = 0_u64;
                    while let Some(block) = player.render().unwrap() {
                        expected += block.frames().len() as u64;
                        assert!(expected < u64::from(rate) * 300);
                    }
                    let mut control = PlaybackControl::new(Arc::new(NdsifOutputGain::new()));
                    control.settings = settings;
                    control
                        .visual_clock
                        .publish(u64::from(rate) * 300, 1, Instant::now())
                        .unwrap();
                    run_preview(path, rate, false, &control, &AtomicBool::new(false), None)
                        .unwrap();
                    let preview = control.preview.lock().unwrap();
                    assert!(preview.done);
                    assert_eq!(
                        preview.scanned,
                        expected,
                        "{} at {rate} Hz with {settings:?}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn preview_errors_do_not_cancel_playback() {
        let control = Arc::new(PlaybackControl::new(Arc::new(NdsifOutputGain::new())));
        let worker = PreviewWorker::start(
            Path::new("/nonexistent/nddrv-preview.vgm"),
            8_000,
            false,
            &control,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if control.preview.lock().unwrap().error.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "preview error not reported");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!control.cancel.load(Ordering::Acquire));
        drop(worker);
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
        use crate::routing::Device;
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
        let service = DeviceService::new();
        service.connect(device.clone()).unwrap();
        let capabilities = match service.events.recv_timeout(Duration::from_secs(2)).unwrap() {
            DeviceEvent::Connected(7, _, capabilities) => capabilities,
            _ => panic!("expected connected device"),
        };
        let mut routing = crate::routing::SessionRouting::default();
        routing.refresh(
            std::slice::from_ref(&device),
            &std::collections::HashMap::from([(7, capabilities)]),
        );
        let plan = routing
            .playback_plan(&[device], VgmSettings::default())
            .unwrap();
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
