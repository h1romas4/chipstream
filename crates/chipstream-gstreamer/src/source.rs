//! GStreamer source element, playback worker, input loading, and seek handling.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chipstream::metadata::{MetadataError, TrackMetadata, probe_mdx_bytes, probe_vgm_bytes};
use flate2::read::GzDecoder;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::base_src::CreateSuccess;
use gst_base::subclass::prelude::*;
use gstreamer as gst;
use gstreamer_base as gst_base;
use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::header::MdxHeader;
use soundlog::mdx::package::MdxPackage;

use chipstream::audio::StereoBlock;
use chipstream::sequencer::{
    Chip, ChipFactoryRegistry, Instance, MdxSequencer, MdxSequencerError, MdxSequencerOptions,
    VgmSequencer, VgmSequencerCheckpoint, VgmSequencerError, measure_mdx_duration_frames,
    measure_vgm_duration_frames,
};
use ndsif_transport::{
    NdsifHardwareWorkers, NdsifHostTimeline, NdsifRecorder, NdsifSerialTransport,
    NdsifSharedDevice, create_automatic_transport_factories, create_recorders,
};

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 192_000;
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_PLAY_COUNT: u32 = 2;
const DEFAULT_FADEOUT_SECONDS: f64 = 2.0;
const MAX_FADEOUT_SECONDS: f64 = 3_600.0;
const FADEOUT_REFERENCE_RATE: u32 = 44_100;
const MIN_CHUNK_FRAMES: u32 = 64;
const MAX_CHUNK_FRAMES: u32 = 8_192;
const DEFAULT_CHUNK_FRAMES: u32 = 512;
const DEFAULT_NDSIF_BAUD_RATE: u32 = 115_200;
const DEFAULT_NDSIF_TIMEOUT_MS: u32 = 500;
const QUEUE_BLOCKS: usize = 4;
const HARDWARE_QUEUE_BLOCKS: usize = 8;
const MAX_HARDWARE_OPERATIONS: usize = 16_384;
const CHECKPOINT_INTERVAL_SECONDS: usize = 30;
const RECENT_CHECKPOINTS: usize = 2;
const RECEIVE_INTERVAL: Duration = Duration::from_millis(50);
const URI_SCHEME: &str = "chipstream";

#[derive(Clone)]
struct Settings {
    location: Option<String>,
    pdx_location: Option<String>,
    sample_rate: u32,
    chunk_frames: u32,
    play_count: u32,
    fadeout_seconds: f64,
    ndsif_port: Option<String>,
    ndsif_ports: Option<String>,
    ndsif_baud_rate: u32,
    ndsif_timeout_ms: u32,
    ndsif_reset: bool,
}

impl Settings {
    /// Return whether either NDSIF port property is configured.
    fn ndsif_enabled(&self) -> bool {
        self.ndsif_port.is_some() || self.ndsif_ports.is_some()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            location: None,
            pdx_location: None,
            sample_rate: DEFAULT_SAMPLE_RATE,
            chunk_frames: DEFAULT_CHUNK_FRAMES,
            play_count: DEFAULT_PLAY_COUNT,
            fadeout_seconds: DEFAULT_FADEOUT_SECONDS,
            ndsif_port: None,
            ndsif_ports: None,
            ndsif_baud_rate: DEFAULT_NDSIF_BAUD_RATE,
            ndsif_timeout_ms: DEFAULT_NDSIF_TIMEOUT_MS,
            ndsif_reset: true,
        }
    }
}

#[derive(Clone)]
struct NdsifSettings {
    port: Option<String>,
    ports: Option<String>,
    baud_rate: u32,
    timeout_ms: u32,
    reset_device: bool,
}

struct HardwarePlayback {
    devices: Vec<NdsifSharedDevice>,
    recorders: Vec<NdsifRecorder>,
    workers: Option<NdsifHardwareWorkers>,
    timeline: Arc<NdsifHostTimeline>,
}

#[derive(Clone, Copy)]
struct RenderSettings {
    sample_rate: u32,
    chunk_frames: usize,
    play_count: u32,
    fadeout_samples: Option<u32>,
}

enum WorkerMessage {
    Metadata(TrackMetadata),
    Warning(String),
    DurationChanged,
    Frames {
        start_frame: u64,
        frames: Vec<[f32; 2]>,
    },
    Error(String),
    Eos,
}

enum WorkerControl {
    Seek {
        target_frame: u64,
        output: SyncSender<WorkerMessage>,
    },
}

enum Sequencer {
    Vgm(VgmSequencer<'static>),
    Mdx(MdxSequencer<'static>),
}

enum SequencerCheckpoint {
    Vgm(VgmSequencerCheckpoint),
    Mdx(VgmSequencerCheckpoint),
}

impl Sequencer {
    /// Render a block from either supported sequencer.
    fn render(&mut self) -> Result<Option<StereoBlock>, String> {
        match self {
            Self::Vgm(player) => player.render().map_err(|error| error.to_string()),
            Self::Mdx(player) => player.render().map_err(|error| error.to_string()),
        }
    }

    /// Return the current output-frame position.
    fn position_frames(&self) -> usize {
        match self {
            Self::Vgm(player) => player.position_frames(),
            Self::Mdx(player) => player.position_frames(),
        }
    }

    fn fade_start_frame(&self) -> Option<usize> {
        match self {
            Self::Vgm(player) => player.fade_start_frame(),
            Self::Mdx(player) => player.fade_start_frame(),
        }
    }

    /// Capture a format-specific playback checkpoint.
    fn checkpoint(&mut self) -> Result<SequencerCheckpoint, String> {
        match self {
            Self::Vgm(player) => player
                .checkpoint()
                .map(SequencerCheckpoint::Vgm)
                .map_err(|error| error.to_string()),
            Self::Mdx(player) => player
                .checkpoint()
                .map(SequencerCheckpoint::Mdx)
                .map_err(|error| error.to_string()),
        }
    }

    /// Restore a matching checkpoint and seek, reporting an unreachable target.
    fn seek_to_frame(
        &mut self,
        checkpoint: &SequencerCheckpoint,
        target: usize,
        cancelled: &AtomicBool,
    ) -> Result<bool, String> {
        match (self, checkpoint) {
            (Self::Vgm(player), SequencerCheckpoint::Vgm(checkpoint)) => {
                match player.seek_to_frame_with_cancel(checkpoint, target, || {
                    cancelled.load(Ordering::Acquire)
                }) {
                    Ok(()) => Ok(true),
                    Err(VgmSequencerError::SeekTargetBeyondEnd { .. }) => Ok(false),
                    Err(error) => Err(error.to_string()),
                }
            }
            (Self::Mdx(player), SequencerCheckpoint::Mdx(checkpoint)) => {
                match player.seek_to_frame_with_cancel(checkpoint, target, || {
                    cancelled.load(Ordering::Acquire)
                }) {
                    Ok(()) => Ok(true),
                    Err(MdxSequencerError::Playback(VgmSequencerError::SeekTargetBeyondEnd {
                        ..
                    })) => Ok(false),
                    Err(error) => Err(error.to_string()),
                }
            }
            _ => Err("seek checkpoint does not match the loaded format".into()),
        }
    }
}

mod imp {
    use super::*;

    pub struct ChipstreamSrc {
        settings: Mutex<Settings>,
        receiver: Mutex<Option<Receiver<WorkerMessage>>>,
        worker: Mutex<Option<JoinHandle<()>>>,
        controls: Mutex<Option<Sender<WorkerControl>>>,
        cancelled: Arc<AtomicBool>,
        flushing: AtomicBool,
        hardware_timeline: Mutex<Option<Arc<NdsifHostTimeline>>>,
        hardware_started: AtomicBool,
        hardware_latency: Mutex<Option<gst::ClockTime>>,
        initial_ndsif_seek_pending: AtomicBool,
        duration: Arc<Mutex<Option<gst::ClockTime>>>,
    }

    impl Default for ChipstreamSrc {
        fn default() -> Self {
            Self {
                settings: Mutex::new(Settings::default()),
                receiver: Mutex::new(None),
                worker: Mutex::new(None),
                controls: Mutex::new(None),
                cancelled: Arc::new(AtomicBool::new(false)),
                flushing: AtomicBool::new(false),
                hardware_timeline: Mutex::new(None),
                hardware_started: AtomicBool::new(false),
                hardware_latency: Mutex::new(None),
                initial_ndsif_seek_pending: AtomicBool::new(false),
                duration: Arc::new(Mutex::new(None)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChipstreamSrc {
        const NAME: &'static str = "GstChipstreamSrc";
        type Type = super::ChipstreamSrc;
        type ParentType = gst_base::BaseSrc;
        type Interfaces = (gst::URIHandler,);
    }

    impl ObjectImpl for ChipstreamSrc {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                let mutable_ready = glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY;
                vec![
                    glib::ParamSpecString::builder("location")
                        .nick("Location")
                        .blurb("VGM, VGZ, or MDX input file")
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecString::builder("pdx-location")
                        .nick("PDX location")
                        .blurb("Optional PDX sample file; otherwise resolved from the MDX")
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecUInt::builder("sample-rate")
                        .nick("Sample rate")
                        .blurb("Output sample rate in Hz")
                        .minimum(MIN_SAMPLE_RATE)
                        .maximum(MAX_SAMPLE_RATE)
                        .default_value(DEFAULT_SAMPLE_RATE)
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecUInt::builder("chunk-frames")
                        .nick("Chunk frames")
                        .blurb("Number of stereo frames per output buffer")
                        .minimum(MIN_CHUNK_FRAMES)
                        .maximum(MAX_CHUNK_FRAMES)
                        .default_value(DEFAULT_CHUNK_FRAMES)
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecUInt::builder("play-count")
                        .nick("Play count")
                        .blurb("Total song traversals; 0 repeats indefinitely")
                        .minimum(0)
                        .maximum(u32::MAX)
                        .default_value(DEFAULT_PLAY_COUNT)
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecDouble::builder("fadeout-seconds")
                        .nick("Fadeout duration")
                        .blurb("Fadeout duration after the final loop point; 0 disables it")
                        .minimum(0.0)
                        .maximum(MAX_FADEOUT_SECONDS)
                        .default_value(DEFAULT_FADEOUT_SECONDS)
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecString::builder("ndsif-port")
                        .nick("NDSIF serial port")
                        .blurb("Optional serial port connected to a NanoDrive NDSIF device")
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecString::builder("ndsif-ports")
                        .nick("NDSIF serial ports")
                        .blurb("Comma-separated serial ports; compatible chips route to the first matching device")
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecUInt::builder("ndsif-baud-rate")
                        .nick("NDSIF baud rate")
                        .blurb("Serial port baud rate")
                        .minimum(1_200)
                        .maximum(4_000_000)
                        .default_value(DEFAULT_NDSIF_BAUD_RATE)
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecUInt::builder("ndsif-timeout-ms")
                        .nick("NDSIF timeout")
                        .blurb("Serial request/response timeout in milliseconds")
                        .minimum(10)
                        .maximum(10_000)
                        .default_value(DEFAULT_NDSIF_TIMEOUT_MS)
                        .flags(mutable_ready)
                        .build(),
                    glib::ParamSpecBoolean::builder("ndsif-reset")
                        .nick("Reset NDSIF device")
                        .blurb("Reset the device when opening the serial connection")
                        .default_value(true)
                        .flags(mutable_ready)
                        .build(),
                ]
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let mut settings = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match pspec.name() {
                "location" => settings.location = value.get().expect("location is a string"),
                "pdx-location" => {
                    settings.pdx_location = value.get().expect("pdx-location is a string")
                }
                "sample-rate" => {
                    settings.sample_rate = value.get().expect("sample-rate is an unsigned int")
                }
                "chunk-frames" => {
                    settings.chunk_frames = value.get().expect("chunk-frames is an unsigned int")
                }
                "play-count" => {
                    settings.play_count = value.get().expect("play-count is an unsigned int")
                }
                "fadeout-seconds" => {
                    settings.fadeout_seconds = value.get().expect("fadeout-seconds is a double")
                }
                "ndsif-port" => settings.ndsif_port = value.get().expect("ndsif-port is a string"),
                "ndsif-ports" => {
                    settings.ndsif_ports = value.get().expect("ndsif-ports is a string")
                }
                "ndsif-baud-rate" => {
                    settings.ndsif_baud_rate =
                        value.get().expect("ndsif-baud-rate is an unsigned int")
                }
                "ndsif-timeout-ms" => {
                    settings.ndsif_timeout_ms =
                        value.get().expect("ndsif-timeout-ms is an unsigned int")
                }
                "ndsif-reset" => {
                    settings.ndsif_reset = value.get().expect("ndsif-reset is a boolean")
                }
                _ => unreachable!("unknown property {}", pspec.name()),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let settings = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match pspec.name() {
                "location" => settings.location.to_value(),
                "pdx-location" => settings.pdx_location.to_value(),
                "sample-rate" => settings.sample_rate.to_value(),
                "chunk-frames" => settings.chunk_frames.to_value(),
                "play-count" => settings.play_count.to_value(),
                "fadeout-seconds" => settings.fadeout_seconds.to_value(),
                "ndsif-port" => settings.ndsif_port.to_value(),
                "ndsif-ports" => settings.ndsif_ports.to_value(),
                "ndsif-baud-rate" => settings.ndsif_baud_rate.to_value(),
                "ndsif-timeout-ms" => settings.ndsif_timeout_ms.to_value(),
                "ndsif-reset" => settings.ndsif_reset.to_value(),
                _ => unreachable!("unknown property {}", pspec.name()),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let source = self.obj();
            source.set_format(gst::Format::Time);
            source.set_blocksize(DEFAULT_CHUNK_FRAMES * 2 * size_of::<f32>() as u32);
        }
    }

    impl GstObjectImpl for ChipstreamSrc {}

    impl URIHandlerImpl for ChipstreamSrc {
        const URI_TYPE: gst::URIType = gst::URIType::Src;

        fn protocols() -> &'static [&'static str] {
            &[URI_SCHEME]
        }

        fn uri(&self) -> Option<String> {
            let settings = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let location = settings.location.as_deref()?;
            glib::filename_to_uri(location, None)
                .ok()
                .map(|uri| uri.replacen("file:", &format!("{URI_SCHEME}:"), 1))
        }

        fn set_uri(&self, uri: &str) -> Result<(), glib::Error> {
            if self.obj().current_state() > gst::State::Ready {
                return Err(glib::Error::new(
                    gst::URIError::BadState,
                    "URI can only be changed before playback starts",
                ));
            }
            let Some((scheme, remainder)) = uri.split_once(':') else {
                return Err(glib::Error::new(
                    gst::URIError::BadUri,
                    "URI is missing a scheme",
                ));
            };
            if !scheme.eq_ignore_ascii_case(URI_SCHEME) {
                return Err(glib::Error::new(
                    gst::URIError::UnsupportedProtocol,
                    "unsupported URI scheme",
                ));
            }
            let file_uri = format!("file:{remainder}");
            let (path, hostname) = glib::filename_from_uri(&file_uri)?;
            if hostname
                .as_deref()
                .is_some_and(|host| !host.is_empty() && !host.eq_ignore_ascii_case("localhost"))
            {
                return Err(glib::Error::new(
                    gst::URIError::BadUri,
                    "remote chipstream URIs are not supported",
                ));
            }
            if !path.is_absolute() {
                return Err(glib::Error::new(
                    gst::URIError::BadUri,
                    "chipstream URI must contain an absolute file path",
                ));
            }
            let location = path.into_os_string().into_string().map_err(|_| {
                glib::Error::new(
                    gst::URIError::BadUri,
                    "chipstream URI path is not valid UTF-8",
                )
            })?;
            self.settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .location = Some(location);
            self.obj().notify("location");
            Ok(())
        }
    }

    impl ElementImpl for ChipstreamSrc {
        fn change_state(
            &self,
            transition: gst::StateChange,
        ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
            if transition == gst::StateChange::PausedToPlaying
                && self.hardware_started.load(Ordering::Acquire)
            {
                gst::element_error!(
                    self.obj(),
                    gst::CoreError::StateChange,
                    ("NDSIF playback cannot resume after being paused; restart the pipeline")
                );
                return Err(gst::StateChangeError);
            }
            let result = self.parent_change_state(transition)?;
            if transition == gst::StateChange::PausedToPlaying {
                let timeline = self
                    .hardware_timeline
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if let Some(timeline) = timeline {
                    let source = self.obj();
                    let Some(clock) = source.clock() else {
                        gst::element_error!(
                            source,
                            gst::CoreError::Clock,
                            ("NDSIF playback requires a pipeline clock")
                        );
                        return Err(gst::StateChangeError);
                    };
                    let Some(base) = source.base_time() else {
                        gst::element_error!(
                            source,
                            gst::CoreError::Clock,
                            ("NDSIF playback clock is unavailable")
                        );
                        return Err(gst::StateChangeError);
                    };
                    let now = clock.time();
                    let mut parent = source.parent();
                    let mut latency = gst::ClockTime::ZERO;
                    while let Some(object) = parent {
                        if let Some(pipeline) = object.downcast_ref::<gst::Pipeline>() {
                            latency = pipeline.latency().unwrap_or(gst::ClockTime::ZERO);
                            break;
                        }
                        parent = object.parent();
                    }
                    if let Some(reported) = *self
                        .hardware_latency
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                    {
                        latency = reported;
                    }
                    let origin = presentation_instant(Instant::now(), base, now, latency);
                    let Some(origin) = origin else {
                        gst::element_error!(
                            source,
                            gst::CoreError::Clock,
                            ("NDSIF playback clock is out of range")
                        );
                        return Err(gst::StateChangeError);
                    };
                    timeline.start(origin).map_err(|_| gst::StateChangeError)?;
                    self.hardware_started.store(true, Ordering::Release);
                }
            } else if transition == gst::StateChange::PlayingToPaused
                && self.hardware_started.load(Ordering::Acquire)
            {
                self.stop_worker().map_err(|_| gst::StateChangeError)?;
            } else if transition == gst::StateChange::PausedToReady {
                self.hardware_started.store(false, Ordering::Release);
            }
            Ok(result)
        }

        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static METADATA: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(METADATA.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "Chipstream song source",
                    "Source/Audio",
                    "Renders VGM, VGZ, and MDX files to stereo PCM",
                    "h1romas4",
                )
            }))
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static PAD_TEMPLATES: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            PAD_TEMPLATES.get_or_init(|| {
                let caps = gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("layout", "interleaved")
                    .field("channels", 2i32)
                    .field(
                        "rate",
                        gst::IntRange::<i32>::new(MIN_SAMPLE_RATE as i32, MAX_SAMPLE_RATE as i32),
                    )
                    .build();
                vec![
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &caps,
                    )
                    .expect("valid source pad template"),
                ]
            })
        }
    }

    impl BaseSrcImpl for ChipstreamSrc {
        fn start(&self) -> Result<(), gst::ErrorMessage> {
            *self
                .hardware_latency
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            let settings = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            self.flushing.store(false, Ordering::Release);
            let ndsif_enabled = settings.ndsif_enabled();
            self.start_worker(settings)?;
            self.initial_ndsif_seek_pending
                .store(ndsif_enabled, Ordering::Release);
            Ok(())
        }

        fn stop(&self) -> Result<(), gst::ErrorMessage> {
            self.flushing.store(true, Ordering::Release);
            self.initial_ndsif_seek_pending
                .store(false, Ordering::Release);
            self.stop_worker()
        }

        fn is_seekable(&self) -> bool {
            !self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .ndsif_enabled()
        }

        fn size(&self) -> Option<u64> {
            None
        }

        fn event(&self, event: &gst::Event) -> bool {
            if let gst::EventView::Latency(latency) = event.view() {
                *self
                    .hardware_latency
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(latency.latency());
            }
            self.parent_event(event)
        }

        fn query(&self, query: &mut gst::QueryRef) -> bool {
            if let gst::QueryViewMut::Duration(duration) = query.view_mut()
                && duration.format() == gst::Format::Time
                && let Some(value) = *self
                    .duration
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
            {
                duration.set(value);
                return true;
            }
            gst_base::subclass::base_src::BaseSrcImplExt::parent_query(self, query)
        }

        fn caps(&self, filter: Option<&gst::Caps>) -> Option<gst::Caps> {
            let rate = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .sample_rate as i32;
            let caps = gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("layout", "interleaved")
                .field("channels", 2i32)
                .field("rate", rate)
                .build();
            Some(filter.map_or(caps.clone(), |filter| caps.intersect(filter)))
        }

        fn create(
            &self,
            _offset: u64,
            _buffer: Option<&mut gst::BufferRef>,
            _length: u32,
        ) -> Result<CreateSuccess, gst::FlowError> {
            loop {
                if self.flushing.load(Ordering::Acquire) {
                    return Err(gst::FlowError::Flushing);
                }
                let message = {
                    let receiver = self
                        .receiver
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let Some(receiver) = receiver.as_ref() else {
                        return Err(gst::FlowError::Flushing);
                    };
                    receiver.recv_timeout(RECEIVE_INTERVAL)
                };
                match message {
                    Ok(WorkerMessage::Metadata(metadata)) => {
                        push_metadata(&self.obj(), metadata);
                    }
                    Ok(WorkerMessage::Warning(warning)) => {
                        gst::element_warning!(
                            self.obj(),
                            gst::CoreError::Failed,
                            ("playback warning"),
                            ["{warning}"]
                        );
                    }
                    Ok(WorkerMessage::DurationChanged) => {
                        let source = self.obj();
                        let message = gst::message::DurationChanged::builder()
                            .src(source.as_ref())
                            .build();
                        let _ = source.post_message(message);
                    }
                    Ok(WorkerMessage::Frames {
                        start_frame,
                        frames,
                    }) => {
                        return make_buffer(start_frame, frames, self.sample_rate());
                    }
                    Ok(WorkerMessage::Error(error)) => {
                        gst::element_error!(
                            self.obj(),
                            gst::CoreError::Failed,
                            ("song rendering failed"),
                            ["{error}"]
                        );
                        return Err(gst::FlowError::Error);
                    }
                    Ok(WorkerMessage::Eos) => return Err(gst::FlowError::Eos),
                    Err(RecvTimeoutError::Disconnected) => return Err(gst::FlowError::Eos),
                    Err(RecvTimeoutError::Timeout) => continue,
                }
            }
        }

        fn unlock(&self) -> Result<(), gst::ErrorMessage> {
            self.flushing.store(true, Ordering::Release);
            Ok(())
        }

        fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
            self.flushing.store(false, Ordering::Release);
            Ok(())
        }

        fn do_seek(&self, segment: &mut gst::Segment) -> bool {
            let ndsif_enabled = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .ndsif_enabled();
            if ndsif_enabled {
                if is_initial_noop_time_segment(segment) {
                    return self
                        .initial_ndsif_seek_pending
                        .swap(false, Ordering::AcqRel);
                }
                return false;
            }
            if segment.format() != gst::Format::Time
                || segment.rate().to_bits() != 1.0_f64.to_bits()
            {
                return false;
            }
            let gst::GenericFormattedValue::Time(Some(target_time)) = segment.start() else {
                return false;
            };
            let settings = self
                .settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let target_frame = u128::from(target_time.nseconds())
                * u128::from(settings.sample_rate)
                / 1_000_000_000;
            let Ok(target_frame) = u64::try_from(target_frame) else {
                return false;
            };
            if self.request_seek(target_frame).is_err() {
                return false;
            }
            segment.set_position(target_time);
            true
        }
    }

    impl ChipstreamSrc {
        /// Replace any existing render worker with one using the current settings.
        fn start_worker(&self, settings: Settings) -> Result<(), gst::ErrorMessage> {
            self.stop_worker()?;
            *self
                .duration
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            let Some(location) = settings.location.clone() else {
                return Err(gst::error_msg!(
                    gst::CoreError::Failed,
                    ["location is required"]
                ));
            };
            self.cancelled.store(false, Ordering::Release);
            let (sender, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
            let (control_sender, control_receiver) = mpsc::channel();
            let cancelled = Arc::clone(&self.cancelled);
            let duration = Arc::clone(&self.duration);
            let render_settings = RenderSettings {
                sample_rate: settings.sample_rate,
                chunk_frames: settings.chunk_frames as usize,
                play_count: settings.play_count,
                fadeout_samples: fadeout_samples(settings.fadeout_seconds),
            };
            let ndsif_settings = NdsifSettings {
                port: settings.ndsif_port.clone(),
                ports: settings.ndsif_ports.clone(),
                baud_rate: settings.ndsif_baud_rate,
                timeout_ms: settings.ndsif_timeout_ms,
                reset_device: settings.ndsif_reset,
            };
            let timeline = Arc::new(NdsifHostTimeline::pending());
            if settings.ndsif_enabled() {
                *self
                    .hardware_timeline
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&timeline));
            }
            let worker = thread::Builder::new()
                .name("chipstream-render".to_owned())
                .spawn(move || {
                    render_file(
                        PathBuf::from(location),
                        settings.pdx_location.map(PathBuf::from),
                        render_settings,
                        ndsif_settings,
                        timeline,
                        cancelled,
                        duration,
                        sender,
                        control_receiver,
                    );
                })
                .map_err(|error| {
                    gst::error_msg!(
                        gst::CoreError::Failed,
                        ["failed to start renderer: {}", error]
                    )
                })?;
            *self
                .receiver
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(receiver);
            *self
                .controls
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(control_sender);
            *self
                .worker
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(worker);
            Ok(())
        }

        /// Send a seek request and replace the worker's output receiver.
        fn request_seek(&self, target_frame: u64) -> Result<(), ()> {
            let control_sender = self
                .controls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .cloned()
                .ok_or(())?;
            let (output, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
            control_sender
                .send(WorkerControl::Seek {
                    target_frame,
                    output,
                })
                .map_err(|_| ())?;
            let previous_receiver = self
                .receiver
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .replace(receiver);
            drop(previous_receiver);
            Ok(())
        }

        /// Cancel and join the render worker, releasing its channels.
        fn stop_worker(&self) -> Result<(), gst::ErrorMessage> {
            self.cancelled.store(true, Ordering::Release);
            if let Some(timeline) = self
                .hardware_timeline
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
            {
                timeline.cancel();
            }
            self.receiver
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            self.controls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(worker) = self
                .worker
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                && worker.join().is_err()
            {
                return Err(gst::error_msg!(
                    gst::CoreError::Failed,
                    ["renderer thread panicked"]
                ));
            }
            Ok(())
        }

        /// Read the configured output sample rate.
        fn sample_rate(&self) -> u32 {
            self.settings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .sample_rate
        }
    }
}

glib::wrapper! {
    pub struct ChipstreamSrc(ObjectSubclass<imp::ChipstreamSrc>)
        @extends gst_base::BaseSrc, gst::Element, gst::Object,
        @implements gst::URIHandler;
}

/// Register the source element with the plugin.
pub(super) fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "chipstreamsrc",
        gst::Rank::PRIMARY,
        ChipstreamSrc::static_type(),
    )
}

/// Run the render worker, forwarding metadata, audio, seek results, and errors.
fn render_file(
    location: PathBuf,
    pdx_location: Option<PathBuf>,
    settings: RenderSettings,
    ndsif_settings: NdsifSettings,
    timeline: Arc<NdsifHostTimeline>,
    cancelled: Arc<AtomicBool>,
    duration: Arc<Mutex<Option<gst::ClockTime>>>,
    sender: SyncSender<WorkerMessage>,
    controls: Receiver<WorkerControl>,
) {
    let result =
        create_chip_factories(&ndsif_settings, timeline).and_then(|(factories, hardware)| {
            load_sequencer_with_factories(&location, pdx_location.as_deref(), settings, factories)
                .map(|result| (result, hardware))
        });
    let (result, mut hardware) = match result {
        Ok(result) => result,
        Err(error) => {
            let mut output = sender;
            while let Ok(WorkerControl::Seek {
                output: replacement,
                ..
            }) = controls.try_recv()
            {
                output = replacement;
            }
            let _ = output.send(WorkerMessage::Error(error));
            return;
        }
    };
    let (mut player, metadata, metadata_warning, duration_frames) = result;
    if let Some(hardware) = &mut hardware {
        match NdsifHardwareWorkers::start(
            &hardware.devices,
            &hardware.recorders,
            Arc::clone(&hardware.timeline),
            NonZeroUsize::new(HARDWARE_QUEUE_BLOCKS).expect("nonzero queue size"),
        ) {
            Ok(workers) => hardware.workers = Some(workers),
            Err(error) => {
                let _ = sender.send(WorkerMessage::Error(error.to_string()));
                return;
            }
        }
    }
    let mut sender = sender;
    let mut pending_messages = VecDeque::new();
    if let Some(duration_frames) = duration_frames
        && let Some(duration_value) = checked_frame_time(duration_frames, settings.sample_rate)
    {
        *duration.lock().unwrap_or_else(|error| error.into_inner()) = Some(duration_value);
        pending_messages.push_back(WorkerMessage::DurationChanged);
    }
    if let Some(warning) = metadata_warning {
        pending_messages.push_back(WorkerMessage::Warning(warning));
    }
    let mut checkpoints = VecDeque::new();
    let mut checkpointing_enabled = hardware.is_none();
    let checkpoint_interval = settings.sample_rate as usize * CHECKPOINT_INTERVAL_SECONDS;
    let mut next_checkpoint_frame = checkpoint_interval;
    if checkpointing_enabled && let Err(error) = capture_checkpoint(&mut player, &mut checkpoints) {
        checkpointing_enabled = false;
        pending_messages.push_back(WorkerMessage::Warning(format!(
            "checkpoint-based seek is unavailable: {error}"
        )));
    }
    pending_messages.push_back(WorkerMessage::Metadata(metadata));
    let mut pending_frames = None;
    let mut start_frame = 0_u64;
    let mut at_eos = false;
    let mut eos_sent = false;
    let mut output_detached = false;
    let mut fade_scheduled = false;
    if let Some(hardware) = &hardware {
        let workers = hardware.workers.as_ref().expect("workers were started");
        let mut buffered_frames = 0_usize;
        let mut prepared = false;
        for _ in 0..QUEUE_BLOCKS {
            let rendered = match player.render() {
                Ok(block) => block,
                Err(error) => {
                    let _ = sender.send(WorkerMessage::Error(error));
                    return;
                }
            };
            if let Err(error) =
                schedule_hardware_fade(&player, hardware, settings, &mut fade_scheduled)
            {
                let _ = sender.send(WorkerMessage::Error(error));
                return;
            }
            match workers.enqueue_prebuffer(
                &hardware.recorders,
                buffered_frames,
                settings.sample_rate,
                &cancelled,
            ) {
                Ok(value) => prepared = value,
                Err(error) => {
                    let _ = sender.send(WorkerMessage::Error(error.to_string()));
                    return;
                }
            }
            let Some(block) = rendered else {
                at_eos = true;
                break;
            };
            let frames = block.frames().to_vec();
            if frames.is_empty() {
                let _ = sender.send(WorkerMessage::Error(
                    "sequencer produced an empty block".into(),
                ));
                return;
            }
            let Some(next) = start_frame.checked_add(frames.len() as u64) else {
                let _ = sender.send(WorkerMessage::Error("audio position overflow".into()));
                return;
            };
            pending_messages.push_back(WorkerMessage::Frames {
                start_frame,
                frames,
            });
            buffered_frames += (next - start_frame) as usize;
            start_frame = next;
            if prepared {
                break;
            }
        }
        if buffered_frames > 0
            && !prepared
            && let Err(error) =
                workers.prepare_audio(buffered_frames, settings.sample_rate, &cancelled)
        {
            let _ = sender.send(WorkerMessage::Error(error.to_string()));
            return;
        }
    }
    loop {
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        if let Some(workers) = hardware
            .as_ref()
            .and_then(|hardware| hardware.workers.as_ref())
            && let Err(error) = workers.check()
        {
            let _ = sender.send(WorkerMessage::Error(error.to_string()));
            return;
        }
        let control = if (at_eos && eos_sent) || output_detached {
            match controls.recv_timeout(RECEIVE_INTERVAL) {
                Ok(control) => Some(control),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match controls.try_recv() {
                Ok(control) => Some(control),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        };
        if let Some(WorkerControl::Seek {
            target_frame,
            output,
        }) = control
        {
            match seek_player(
                &mut player,
                &mut checkpoints,
                &location,
                pdx_location.as_deref(),
                settings,
                target_frame,
                &cancelled,
                &mut checkpointing_enabled,
                &mut next_checkpoint_frame,
                &output,
            ) {
                Ok((position, pending, reached_end)) => {
                    sender = output;
                    start_frame = position;
                    pending_frames = pending;
                    at_eos = reached_end;
                    eos_sent = false;
                    output_detached = false;
                }
                Err(error) => {
                    if cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    let _ = output.send(WorkerMessage::Error(error));
                    return;
                }
            }
            continue;
        }
        if let Some(message) = pending_messages.pop_front() {
            if let Err(error) = sender.send(message) {
                pending_messages.push_front(error.0);
                output_detached = true;
            }
            continue;
        }
        if at_eos {
            if !eos_sent {
                if let Some(hardware) = &mut hardware
                    && let Some(workers) = hardware.workers.take()
                {
                    if start_frame == 0 {
                        hardware.timeline.cancel();
                    }
                    if let Err(error) = workers.finish() {
                        if !cancelled.load(Ordering::Acquire) {
                            let _ = sender.send(WorkerMessage::Error(error.to_string()));
                        }
                        return;
                    }
                }
                if sender.send(WorkerMessage::Eos).is_err() {
                    output_detached = true;
                }
                eos_sent = true;
            }
            continue;
        }
        let frames = if let Some(frames) = pending_frames.take() {
            frames
        } else {
            let rendered = player.render();
            if let Some(hardware) = &hardware
                && let Err(error) =
                    schedule_hardware_fade(&player, hardware, settings, &mut fade_scheduled)
            {
                let _ = sender.send(WorkerMessage::Error(error));
                return;
            }
            if let Some(hardware) = &hardware
                && let Err(error) = hardware
                    .workers
                    .as_ref()
                    .expect("workers were started")
                    .enqueue(&hardware.recorders, &cancelled)
            {
                let _ = sender.send(WorkerMessage::Error(error.to_string()));
                return;
            }
            match rendered {
                Ok(Some(block)) => block.frames().to_vec(),
                Ok(None) => {
                    at_eos = true;
                    eos_sent = false;
                    continue;
                }
                Err(error) => {
                    let _ = sender.send(WorkerMessage::Error(error));
                    return;
                }
            }
        };
        if frames.is_empty() {
            continue;
        }
        let Ok(frame_count) = u64::try_from(frames.len()) else {
            let _ = sender.send(WorkerMessage::Error("audio frame count overflow".into()));
            return;
        };
        let Some(next_frame) = start_frame.checked_add(frame_count) else {
            let _ = sender.send(WorkerMessage::Error("audio position overflow".into()));
            return;
        };
        if checkpointing_enabled && player.position_frames() >= next_checkpoint_frame {
            match capture_checkpoint(&mut player, &mut checkpoints) {
                Ok(()) => {
                    next_checkpoint_frame =
                        player.position_frames().saturating_add(checkpoint_interval);
                }
                Err(error) => {
                    checkpointing_enabled = false;
                    pending_messages.push_back(WorkerMessage::Warning(format!(
                        "checkpoint-based seek is unavailable: {error}"
                    )));
                }
            }
        }
        if sender
            .send(WorkerMessage::Frames {
                start_frame,
                frames,
            })
            .is_err()
        {
            output_detached = true;
        }
        start_frame = next_frame;
    }
}

fn schedule_hardware_fade(
    player: &Sequencer,
    hardware: &HardwarePlayback,
    settings: RenderSettings,
    scheduled: &mut bool,
) -> Result<(), String> {
    if !*scheduled
        && let Some(start) = player.fade_start_frame()
        && let Some(samples) = settings.fadeout_samples
    {
        hardware.timeline.schedule_output_fade(
            start,
            settings.sample_rate,
            Duration::from_secs_f64(f64::from(samples) / f64::from(FADEOUT_REFERENCE_RATE)),
        )?;
        *scheduled = true;
    }
    Ok(())
}

fn presentation_instant(
    anchor: Instant,
    base: gst::ClockTime,
    clock: gst::ClockTime,
    latency: gst::ClockTime,
) -> Option<Instant> {
    let target = base.nseconds().checked_add(latency.nseconds())?;
    if target >= clock.nseconds() {
        anchor.checked_add(Duration::from_nanos(target - clock.nseconds()))
    } else {
        anchor.checked_sub(Duration::from_nanos(clock.nseconds() - target))
    }
}

/// Keep the origin checkpoint and a bounded set of recent checkpoints.
fn cache_checkpoint<T>(checkpoints: &mut VecDeque<(usize, T)>, frame: usize, checkpoint: T) {
    if let Some(index) = checkpoints
        .iter()
        .position(|(cached_frame, _)| *cached_frame == frame)
    {
        checkpoints.remove(index);
    }
    if frame == 0 {
        checkpoints.push_front((frame, checkpoint));
    } else {
        checkpoints.push_back((frame, checkpoint));
    }
    while checkpoints.len() > RECENT_CHECKPOINTS + 1 {
        checkpoints.remove(1);
    }
}

/// Find the latest checkpoint no later than the seek target.
fn nearest_checkpoint<T>(checkpoints: &VecDeque<(usize, T)>, target: usize) -> Option<&T> {
    checkpoints
        .iter()
        .filter(|(frame, _)| *frame <= target)
        .max_by_key(|(frame, _)| *frame)
        .map(|(_, checkpoint)| checkpoint)
}

/// Capture and cache the player's current playback state.
fn capture_checkpoint(
    player: &mut Sequencer,
    checkpoints: &mut VecDeque<(usize, SequencerCheckpoint)>,
) -> Result<(), String> {
    let frame = player.position_frames();
    let checkpoint = player.checkpoint()?;
    cache_checkpoint(checkpoints, frame, checkpoint);
    Ok(())
}

/// Seek from a cached checkpoint or rebuild the player when necessary.
fn seek_player(
    player: &mut Sequencer,
    checkpoints: &mut VecDeque<(usize, SequencerCheckpoint)>,
    location: &Path,
    pdx_location: Option<&Path>,
    settings: RenderSettings,
    target_frame: u64,
    cancelled: &AtomicBool,
    checkpointing_enabled: &mut bool,
    next_checkpoint_frame: &mut usize,
    output: &SyncSender<WorkerMessage>,
) -> Result<(u64, Option<Vec<[f32; 2]>>, bool), String> {
    let target = usize::try_from(target_frame).map_err(|_| "seek target is too large")?;
    if nearest_checkpoint(checkpoints, target).is_none() {
        let (fresh, _, _, _) = load_sequencer(location, pdx_location, settings)?;
        *player = fresh;
        checkpoints.clear();
        if *checkpointing_enabled {
            match capture_checkpoint(player, checkpoints) {
                Ok(()) => {}
                Err(error) => {
                    *checkpointing_enabled = false;
                    let _ = output.send(WorkerMessage::Warning(format!(
                        "checkpoint-based seek is unavailable: {error}"
                    )));
                }
            }
        }
    }

    let (reached_target, pending_frames) =
        if let Some(checkpoint) = nearest_checkpoint(checkpoints, target) {
            (player.seek_to_frame(checkpoint, target, cancelled)?, None)
        } else {
            let pending = discard_to_frame(player, target_frame, cancelled)?;
            (player.position_frames() >= target, pending)
        };
    let position = if reached_target {
        target
    } else {
        player.position_frames()
    };
    *next_checkpoint_frame =
        position.saturating_add(settings.sample_rate as usize * CHECKPOINT_INTERVAL_SECONDS);
    let position = u64::try_from(position).map_err(|_| "audio position overflow during seek")?;
    Ok((position, pending_frames, !reached_target))
}

/// Render and discard audio until a requested frame is reached.
fn discard_to_frame(
    player: &mut Sequencer,
    target_frame: u64,
    cancelled: &AtomicBool,
) -> Result<Option<Vec<[f32; 2]>>, String> {
    let mut remaining = target_frame;
    while remaining > 0 {
        if cancelled.load(Ordering::Acquire) {
            return Err("rendering was cancelled during seek".into());
        }
        let Some(block) = player.render()? else {
            return Ok(None);
        };
        let frames = block.frames();
        let frame_count = u64::try_from(frames.len())
            .map_err(|_| "audio frame count overflow during seek".to_owned())?;
        if remaining < frame_count {
            return Ok(Some(frames[remaining as usize..].to_vec()));
        }
        remaining -= frame_count;
    }
    Ok(None)
}

/// Load a software-only player, metadata, and measured duration.
fn load_sequencer(
    path: &Path,
    pdx_location: Option<&Path>,
    settings: RenderSettings,
) -> Result<(Sequencer, TrackMetadata, Option<String>, Option<u64>), String> {
    load_sequencer_with_factories(path, pdx_location, settings, ChipFactoryRegistry::new())
}

/// Load an MDX or VGM player with optional NDSIF chip factories.
fn load_sequencer_with_factories(
    path: &Path,
    pdx_location: Option<&Path>,
    settings: RenderSettings,
    factories: ChipFactoryRegistry,
) -> Result<(Sequencer, TrackMetadata, Option<String>, Option<u64>), String> {
    let bytes = read_path(path, MAX_FILE_BYTES)?;
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("mdx") => {
            let metadata =
                probe_mdx_bytes(&bytes).map_err(|error| format!("failed to probe MDX: {error}"))?;
            let normalized = normalize_mdx_for_playback(&bytes)
                .map_err(|error| format!("failed to normalize MDX input: {error}"))?;
            let (header, _) = MdxHeader::parse(normalized.as_ref())
                .map_err(|error| format!("failed to parse MDX header: {error}"))?;
            let pdx_path = match (pdx_location, header.pdx_name.as_deref()) {
                (Some(path), _) => Some(path.to_path_buf()),
                (None, Some(name)) => Some(resolve_pdx_path(path, name)?),
                (None, None) => None,
            };
            let pdx = pdx_path
                .as_deref()
                .map(|path| read_path(path, MAX_FILE_BYTES))
                .transpose()?;
            let package = MdxPackage::parse_owned(normalized.into_owned(), pdx)
                .map_err(|error| format!("failed to parse MDX/PDX package: {error}"))?;
            let mut options = MdxSequencerOptions {
                loop_count: (settings.play_count > 0).then_some(settings.play_count),
                ..MdxSequencerOptions::default()
            };
            if settings.play_count == 0 {
                options.max_ticks = None;
            }
            let duration_frames = measure_mdx_duration_frames(
                package.clone(),
                settings.sample_rate,
                options,
                settings.fadeout_samples,
            )
            .map_err(|error| format!("failed to measure MDX duration: {error}"))?
            .map(u64::from);
            let player = MdxSequencer::with_options_and_fadeout_with_factories(
                package,
                settings.sample_rate,
                settings.chunk_frames,
                options,
                settings.fadeout_samples,
                factories,
            )
            .map_err(|error| format!("failed to create MDX sequencer: {error}"))?;
            Ok((Sequencer::Mdx(player), metadata, None, duration_frames))
        }
        Some(extension)
            if extension.eq_ignore_ascii_case("vgm")
                || extension.eq_ignore_ascii_case("vgz")
                || extension.eq_ignore_ascii_case("gz") =>
        {
            if pdx_location.is_some() {
                return Err("pdx-location can only be used with an MDX file".into());
            }
            let bytes = decode_vgm(
                bytes,
                extension.eq_ignore_ascii_case("vgz") || extension.eq_ignore_ascii_case("gz"),
            )?;
            let play_count = (settings.play_count > 0).then_some(settings.play_count);
            let duration_frames = measure_vgm_duration_frames(
                &bytes,
                settings.sample_rate,
                play_count,
                settings.fadeout_samples,
            )
            .map_err(|error| format!("failed to measure VGM duration: {error}"))?
            .map(u64::from);
            let (metadata, warning) = match probe_vgm_bytes(&bytes) {
                Ok(metadata) => (metadata, None),
                Err(error @ MetadataError::InvalidGd3(_)) => {
                    (TrackMetadata::default(), Some(error.to_string()))
                }
                Err(error) => {
                    return Err(format!("failed to probe VGM metadata: {error}"));
                }
            };
            let mut player = VgmSequencer::from_vgm_with_factories(
                bytes,
                settings.sample_rate,
                settings.chunk_frames,
                factories,
            )
            .map_err(|error| format!("failed to create VGM sequencer: {error}"))?;
            configure_adjacent_ym2608_rhythm_rom(path, &mut player)?;
            player
                .set_pwm_compatibility(true)
                .map_err(|error| format!("failed to configure VGM playback: {error}"))?;
            player.set_loop_count(play_count);
            player
                .set_fadeout_samples(settings.fadeout_samples)
                .map_err(|error| format!("failed to configure VGM playback: {error}"))?;
            Ok((Sequencer::Vgm(player), metadata, warning, duration_frames))
        }
        _ => Err(format!(
            "unsupported input extension for {} (expected .vgm, .vgz, or .mdx)",
            path.display()
        )),
    }
}

/// Create hardware factories for the configured serial ports, if any.
fn create_chip_factories(
    settings: &NdsifSettings,
    timeline: Arc<NdsifHostTimeline>,
) -> Result<(ChipFactoryRegistry, Option<HardwarePlayback>), String> {
    let ports = parse_ndsif_ports(settings)?;
    if ports.is_empty() {
        return Ok((ChipFactoryRegistry::new(), None));
    }
    if settings.baud_rate == 0 || settings.timeout_ms == 0 {
        return Err("NDSIF baud rate and timeout must be nonzero".into());
    }
    for (index, port) in ports.iter().enumerate() {
        if ports[..index].contains(port) {
            return Err(format!("duplicate NDSIF port: {port}"));
        }
    }
    let mut devices = Vec::new();
    let mut supports_ay = false;
    for (index, port) in ports.into_iter().enumerate() {
        let transport = NdsifSerialTransport::open(
            &port,
            settings.baud_rate,
            Duration::from_millis(u64::from(settings.timeout_ms)),
            settings.reset_device,
        )
        .map_err(|error| format!("NDSIF device {port}: {error}"))?;
        supports_ay |= transport
            .capabilities()
            .iter()
            .any(|entry| entry.write_id == 0xa0);
        devices.push((index as u64, Arc::new(Mutex::new(transport))));
    }
    let recorders = create_recorders(
        &devices,
        NonZeroUsize::new(MAX_HARDWARE_OPERATIONS).expect("nonzero operation limit"),
    )
    .map_err(|error| error.to_string())?;
    let factories = create_automatic_transport_factories(&recorders, supports_ay);
    Ok((
        factories,
        Some(HardwarePlayback {
            devices,
            recorders,
            workers: None,
            timeline,
        }),
    ))
}

/// Parse mutually exclusive single-port and comma-separated port settings.
fn parse_ndsif_ports(settings: &NdsifSettings) -> Result<Vec<String>, String> {
    if settings.port.is_some() && settings.ports.is_some() {
        return Err("configure either ndsif-port or ndsif-ports, not both".to_owned());
    }
    let Some(ports) = settings.ports.as_deref().or(settings.port.as_deref()) else {
        return Ok(Vec::new());
    };
    let ports = ports.split(',').map(str::trim).collect::<Vec<_>>();
    if ports.is_empty() || ports.iter().any(|port| port.is_empty()) {
        return Err("NDSIF serial port list contains an empty entry".to_owned());
    }
    Ok(ports.into_iter().map(str::to_owned).collect())
}

/// Load the optional adjacent YM2608 rhythm ROM for available chip instances.
fn configure_adjacent_ym2608_rhythm_rom(
    input_path: &Path,
    player: &mut VgmSequencer<'_>,
) -> Result<(), String> {
    let rom_path = input_path.with_file_name("ym2608_adpcm_rom.bin");
    match fs::metadata(&rom_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(format!(
                "YM2608 ADPCM ROM is not a file: {}",
                rom_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to inspect YM2608 ADPCM ROM {}: {error}",
                rom_path.display()
            ));
        }
    }

    let rom = read_path(&rom_path, MAX_FILE_BYTES)?;
    if rom.is_empty() {
        return Err(format!("YM2608 ADPCM ROM is empty: {}", rom_path.display()));
    }

    for instance in [Instance::Primary, Instance::Secondary] {
        match player.set_ym2608_rhythm_rom(instance, &rom) {
            Ok(())
            | Err(VgmSequencerError::ChipNotFound {
                chip: Chip::Ym2608, ..
            }) => {}
            Err(error) => {
                return Err(format!(
                    "failed to configure YM2608 ADPCM ROM {}: {error}",
                    rom_path.display()
                ));
            }
        }
    }

    Ok(())
}

/// Convert fade seconds to the VGM reference sample rate.
fn fadeout_samples(seconds: f64) -> Option<u32> {
    let samples = (seconds * f64::from(FADEOUT_REFERENCE_RATE)).round() as u32;
    (samples > 0).then_some(samples)
}

/// Read a file with a size limit and contextualize I/O errors.
fn read_path(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file =
        File::open(path).map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    read_bounded(file, limit).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

/// Read at most the allowed size, rejecting inputs that exceed it.
fn read_bounded(reader: impl Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!(
            "input exceeds {limit} bytes"
        )));
    }
    Ok(bytes)
}

/// Decompress gzip VGM input when indicated by extension or content.
fn decode_vgm(bytes: Vec<u8>, gzip_extension: bool) -> Result<Vec<u8>, String> {
    if gzip_extension || bytes.starts_with(&[0x1f, 0x8b]) {
        read_bounded(GzDecoder::new(Cursor::new(bytes)), MAX_DECOMPRESSED_BYTES)
            .map_err(|error| format!("failed to decompress VGZ: {error}"))
    } else {
        Ok(bytes)
    }
}

/// Resolve an MDX-referenced PDX file next to the input.
fn resolve_pdx_path(input: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." {
        return Err("invalid PDX filename in MDX header".into());
    }
    let directory = input
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut names = vec![name.to_owned()];
    if Path::new(name).extension().is_none() {
        names.push(format!("{name}.PDX"));
        names.push(format!("{name}.pdx"));
    }
    for candidate in &names {
        let path = directory.join(candidate);
        if fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Ok(path);
        }
    }
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to search for PDX in {}: {error}",
            directory.display()
        )
    })?;
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to inspect PDX directory: {error}"))?;
        let filename = entry.file_name();
        if names
            .iter()
            .any(|candidate| filename.to_string_lossy().eq_ignore_ascii_case(candidate))
            && entry
                .file_type()
                .map_err(|error| format!("failed to inspect PDX entry: {error}"))?
                .is_file()
        {
            matches.push(entry.path());
        }
    }
    match matches.len() {
        0 => Err(format!(
            "PDX file '{name}' not found alongside {}",
            input.display()
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "multiple case-insensitive matches for PDX file '{name}'"
        )),
    }
}

/// Push nonempty track metadata as a downstream tag event.
fn push_metadata(source: &ChipstreamSrc, metadata: TrackMetadata) {
    if metadata.title.is_none()
        && metadata.artist.is_none()
        && metadata.album.is_none()
        && metadata.comment.is_none()
    {
        return;
    }
    let mut tags = gst::TagList::new();
    {
        let tags = tags.get_mut().expect("new tag list is writable");
        if let Some(title) = metadata.title {
            tags.add::<gst::tags::Title>(&title.as_str(), gst::TagMergeMode::Replace);
        }
        if let Some(artist) = metadata.artist {
            tags.add::<gst::tags::Artist>(&artist.as_str(), gst::TagMergeMode::Replace);
        }
        if let Some(album) = metadata.album {
            tags.add::<gst::tags::Album>(&album.as_str(), gst::TagMergeMode::Replace);
        }
        if let Some(comment) = metadata.comment {
            tags.add::<gst::tags::Comment>(&comment.as_str(), gst::TagMergeMode::Replace);
        }
    }
    source.src_pad().push_event(gst::event::Tag::new(tags));
}

/// Pack stereo frames into a timestamped little-endian GStreamer buffer.
fn make_buffer(
    start_frame: u64,
    frames: Vec<[f32; 2]>,
    sample_rate: u32,
) -> Result<CreateSuccess, gst::FlowError> {
    let frame_count = u64::try_from(frames.len()).map_err(|_| gst::FlowError::Error)?;
    let end_frame = start_frame
        .checked_add(frame_count)
        .ok_or(gst::FlowError::Error)?;
    let byte_len = frames
        .len()
        .checked_mul(2 * size_of::<f32>())
        .ok_or(gst::FlowError::Error)?;
    let mut buffer = gst::Buffer::with_size(byte_len).map_err(|_| gst::FlowError::Error)?;
    let buffer_ref = buffer.get_mut().ok_or(gst::FlowError::Error)?;
    {
        let mut map = buffer_ref
            .map_writable()
            .map_err(|_| gst::FlowError::Error)?;
        for (destination, sample) in map
            .as_mut_slice()
            .as_chunks_mut::<{ size_of::<f32>() }>()
            .0
            .iter_mut()
            .zip(frames.iter().flatten())
        {
            destination.copy_from_slice(&sample.to_le_bytes());
        }
    }
    let pts = frame_time(start_frame, sample_rate);
    let end_time = frame_time(end_frame, sample_rate);
    buffer_ref.set_pts(pts);
    buffer_ref.set_duration(end_time - pts);
    buffer_ref.set_offset(start_frame);
    buffer_ref.set_offset_end(end_frame);
    Ok(CreateSuccess::NewBuffer(buffer))
}

/// Convert a frame position to a saturated GStreamer timestamp.
fn frame_time(frame: u64, sample_rate: u32) -> gst::ClockTime {
    let nanoseconds = u128::from(frame) * 1_000_000_000 / u128::from(sample_rate);
    gst::ClockTime::from_nseconds(nanoseconds.min(u128::from(u64::MAX)) as u64)
}

/// Convert a frame position to a timestamp when it fits the valid range.
fn checked_frame_time(frame: u64, sample_rate: u32) -> Option<gst::ClockTime> {
    let nanoseconds = u128::from(frame) * 1_000_000_000 / u128::from(sample_rate);
    let nanoseconds = u64::try_from(nanoseconds).ok()?;
    (nanoseconds < u64::MAX).then(|| gst::ClockTime::from_nseconds(nanoseconds))
}

/// Identify the initial zero-position time segment that requires no seek.
fn is_initial_noop_time_segment(segment: &gst::Segment) -> bool {
    let zero = gst::GenericFormattedValue::Time(Some(gst::ClockTime::ZERO));
    segment.format() == gst::Format::Time
        && segment.rate().to_bits() == 1.0_f64.to_bits()
        && segment.start() == zero
        && segment.offset() == zero
        && segment.position() == zero
        && segment.base() == zero
        && segment.time() == zero
        && segment.stop() == gst::GenericFormattedValue::Time(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::chip::{Chip as SoundlogChip, Ym2608Spec, Ym2612Spec};
    use soundlog::vgm::VgmBuilder;
    use soundlog::vgm::command::{Instance as SoundlogInstance, WaitSamples};

    #[test]
    fn presentation_origin_uses_pipeline_latency_and_clock() {
        let anchor = Instant::now();
        let base = gst::ClockTime::from_seconds(10);
        let latency = gst::ClockTime::from_mseconds(50);
        assert_eq!(
            presentation_instant(anchor, base, gst::ClockTime::from_mseconds(10_020), latency),
            anchor.checked_add(Duration::from_millis(30))
        );
        assert_eq!(
            presentation_instant(anchor, base, gst::ClockTime::from_mseconds(10_100), latency),
            anchor.checked_sub(Duration::from_millis(50))
        );
        assert!(
            presentation_instant(anchor, gst::ClockTime::MAX, gst::ClockTime::ZERO, latency)
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn hardware_waits_for_playing_and_cannot_resume_after_pause() {
        use ndsif::{Capabilities, Decoder, Reply, Request, Response};
        use serialport::SerialPort;
        use std::io::{Read, Write};

        gst::init().unwrap();
        let (mut fake, port) = serialport::TTYPort::pair().unwrap();
        fake.set_timeout(Duration::from_millis(50)).unwrap();
        let port_name = port.name().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let fake_stop = Arc::clone(&stop);
        let (writes, observed) = mpsc::channel();
        let device = thread::spawn(move || {
            let mut decoder = Decoder::new();
            while !fake_stop.load(Ordering::Acquire) {
                let mut byte = [0];
                match fake.read_exact(&mut byte) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(error) => panic!("fake NDSIF read failed: {error}"),
                }
                let mut frame = None;
                decoder.push(&byte, |result| frame = Some(result.unwrap()));
                let Some(frame) = frame else { continue };
                let reply = match Request::decode(&frame).unwrap() {
                    Request::Ping(bytes) => Some(Reply::Ping(bytes)),
                    Request::GetCapabilities { offset: 0 } => Some(Reply::Capabilities(
                        Capabilities::new(256, 0, 0, 0, u16::MAX, &[0x51, 0, 0, 4, 2, 0, 0, 0, 0])
                            .unwrap(),
                    )),
                    Request::SetChipClock { .. } | Request::Reset => Some(Reply::Complete),
                    Request::WriteRegisters { .. } => {
                        let _ = writes.send(Instant::now());
                        None
                    }
                    request => panic!("unexpected NDSIF request: {request:?}"),
                };
                if let Some(reply) = reply {
                    let response = Response::for_request(&frame, reply)
                        .unwrap()
                        .to_frame()
                        .unwrap()
                        .encode();
                    fake.write_all(response.as_bytes()).unwrap();
                }
            }
        });
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../soundlog/assets/vgm/ym2413_fm_a4.vgm");
        let source = glib::Object::builder::<ChipstreamSrc>()
            .property("location", path.to_string_lossy().as_ref())
            .property("ndsif-port", port_name)
            .property("play-count", 1_u32)
            .property("fadeout-seconds", 0.0_f64)
            .build();
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([source.upcast_ref(), &sink]).unwrap();
        source.link(&sink).unwrap();
        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
        assert!(observed.recv_timeout(Duration::from_millis(100)).is_err());
        pipeline.set_state(gst::State::Playing).unwrap();
        assert!(observed.recv_timeout(Duration::from_secs(3)).is_ok());
        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
        assert!(pipeline.set_state(gst::State::Playing).is_err());
        pipeline.set_state(gst::State::Ready).unwrap();
        while observed.try_recv().is_ok() {}
        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
        assert!(observed.recv_timeout(Duration::from_millis(100)).is_err());
        pipeline.set_state(gst::State::Playing).unwrap();
        assert!(observed.recv_timeout(Duration::from_secs(3)).is_ok());
        pipeline.set_state(gst::State::Null).unwrap();
        stop.store(true, Ordering::Release);
        device.join().unwrap();
        drop(port);
    }

    #[cfg(unix)]
    #[test]
    fn timed_dac_is_prebuffered_before_pipeline_playback() {
        exercise_timed_dac_latency(true);
    }

    #[cfg(unix)]
    #[test]
    fn timed_dac_uses_latency_event() {
        exercise_timed_dac_latency(false);
    }

    #[cfg(unix)]
    fn exercise_timed_dac_latency(explicit_latency: bool) {
        use ndsif::{
            Capabilities, DacCapabilities, DacRecordKind, DacStatus, Decoder, Reply, Request,
            Response,
        };
        use serialport::SerialPort;
        use std::io::{Read, Write};

        gst::init().unwrap();
        let (mut fake, port) = serialport::TTYPort::pair().unwrap();
        fake.set_timeout(Duration::from_millis(50)).unwrap();
        let port_name = port.name().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let fake_stop = Arc::clone(&stop);
        let (starts, started) = mpsc::channel();
        let (samples, received_samples) = mpsc::channel();
        let device = thread::spawn(move || {
            let mut decoder = Decoder::new();
            let mut accepted = 0_u32;
            let mut tick = 0_u64;
            let mut events = 0_u32;
            let mut running = false;
            let mut sealed = false;
            while !fake_stop.load(Ordering::Acquire) {
                let mut byte = [0];
                match fake.read_exact(&mut byte) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(error) => panic!("fake NDSIF read failed: {error}"),
                }
                let mut frame = None;
                decoder.push(&byte, |result| frame = Some(result.unwrap()));
                let Some(frame) = frame else { continue };
                let reply = match Request::decode(&frame).unwrap() {
                    Request::Ping(bytes) => Some(Reply::Ping(bytes)),
                    Request::GetCapabilities { offset: 0 } => Some(Reply::Capabilities(
                        Capabilities::new(
                            256,
                            0,
                            0,
                            ndsif::CONTROL_FEATURE_DAC_PROFILE,
                            u16::MAX,
                            &[0x52, 0, 0, 4, 2, 0, 0, 0, 0],
                        )
                        .unwrap(),
                    )),
                    Request::GetDacCapabilities => Some(Reply::DacCapabilities(DacCapabilities {
                        profile: ndsif::DAC_PROFILE_YM2612,
                        revision: 40,
                        ticks_per_second: ndsif::DAC_TICKS_PER_SECOND,
                        record_capacity: 8192,
                        event_capacity: 1024,
                        max_records: 60,
                        record_bytes: 4,
                        max_dac_late_us: 0,
                    })),
                    Request::SetChipClock { .. } => Some(Reply::Complete),
                    Request::Reset => {
                        accepted = 0;
                        tick = 0;
                        events = 0;
                        running = false;
                        sealed = false;
                        Some(Reply::Complete)
                    }
                    Request::DacData { records, .. } => {
                        for record in records.iter() {
                            tick += u64::from(record.wait_ticks);
                            accepted += 1;
                            if record.kind == DacRecordKind::Dac {
                                let _ = samples.send(record.value);
                            }
                        }
                        None
                    }
                    Request::DacRegisterEvent { writes, .. } => {
                        events += (writes.len() / 2) as u32;
                        None
                    }
                    Request::WriteRegisters { .. } => None,
                    Request::DacEnd { .. } => {
                        sealed = true;
                        None
                    }
                    Request::DacStart { .. } => {
                        running = true;
                        let _ = starts.send(Instant::now());
                        Some(Reply::Complete)
                    }
                    Request::DacStatus { .. } => {
                        let done = running && sealed;
                        Some(Reply::DacStatus(DacStatus {
                            profile: ndsif::DAC_PROFILE_YM2612,
                            state: if done {
                                2
                            } else if running {
                                1
                            } else {
                                0
                            },
                            fault: 0,
                            sealed,
                            accepted,
                            consumed: if done { accepted } else { 0 },
                            queued: if done { 0 } else { accepted },
                            free: if done { 8192 } else { 8192 - accepted },
                            event_queued: if done { 0 } else { events },
                            event_consumed: if done { events } else { 0 },
                            underflows: 0,
                            overflows: 0,
                            rejected: 0,
                            late_dac: 0,
                            max_late_us: 0,
                            accepted_tick: tick,
                            played_tick: if done { tick } else { 0 },
                            executed_tick: if done { tick } else { 0 },
                        }))
                    }
                    request => panic!("unexpected NDSIF request: {request:?}"),
                };
                if let Some(reply) = reply {
                    let response = Response::for_request(&frame, reply)
                        .unwrap()
                        .to_frame()
                        .unwrap()
                        .encode();
                    fake.write_all(response.as_bytes()).unwrap();
                }
            }
        });
        let path = std::env::temp_dir().join(format!(
            "chipstream-gstreamer-dac-{}-{}.vgm",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut builder = VgmBuilder::new();
        builder.register_chip(SoundlogChip::Ym2612, SoundlogInstance::Primary, 7_670_454);
        builder.add_chip_write(
            SoundlogInstance::Primary,
            Ym2612Spec {
                port: 0,
                register: 0x2b,
                value: 0x80,
            },
        );
        for value in 0x80..0x94 {
            builder.add_chip_write(
                SoundlogInstance::Primary,
                Ym2612Spec {
                    port: 0,
                    register: 0x2a,
                    value,
                },
            );
            builder.add_vgm_command(WaitSamples(220));
        }
        let bytes: Vec<u8> = builder.finalize().into();
        fs::write(&path, bytes).unwrap();
        let source = glib::Object::builder::<ChipstreamSrc>()
            .property("location", path.to_string_lossy().as_ref())
            .property("ndsif-port", port_name)
            .property("play-count", 1_u32)
            .property("fadeout-seconds", 0.0_f64)
            .build();
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", true)
            .build()
            .unwrap();
        let pipeline = gst::Pipeline::new();
        if explicit_latency {
            pipeline.set_latency(gst::ClockTime::from_mseconds(250));
        } else {
            source.static_pad("src").unwrap().add_probe(
                gst::PadProbeType::QUERY_UPSTREAM,
                |_, info| {
                    if let Some(query) = info.query_mut()
                        && let gst::QueryViewMut::Latency(latency) = query.view_mut()
                    {
                        latency.set(
                            true,
                            gst::ClockTime::from_mseconds(250),
                            gst::ClockTime::NONE,
                        );
                        return gst::PadProbeReturn::Handled;
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        }
        pipeline.add_many([source.upcast_ref(), &sink]).unwrap();
        source.link(&sink).unwrap();
        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
        assert!(started.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(
            received_samples
                .recv_timeout(Duration::from_secs(3))
                .is_ok()
        );
        let before_playing = Instant::now();
        pipeline.set_state(gst::State::Playing).unwrap();
        let started_at = started.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(started_at.duration_since(before_playing) >= Duration::from_millis(200));
        let message = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("hardware playback should finish");
        assert_eq!(message.type_(), gst::MessageType::Eos, "{message:?}");
        pipeline.set_state(gst::State::Null).unwrap();
        stop.store(true, Ordering::Release);
        device.join().unwrap();
        drop(port);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn checkpoint_cache_keeps_origin_and_two_recent_entries() {
        let mut checkpoints = VecDeque::new();
        for frame in [0, 30, 60, 90] {
            cache_checkpoint(&mut checkpoints, frame, frame);
        }

        assert_eq!(
            checkpoints
                .iter()
                .map(|(frame, _)| *frame)
                .collect::<Vec<_>>(),
            [0, 60, 90]
        );
        assert_eq!(nearest_checkpoint(&checkpoints, 75), Some(&60));
    }

    #[test]
    fn buffer_contains_f32le_stereo_and_frame_timestamps() {
        gst::init().unwrap();
        let success = make_buffer(10, vec![[1.0, -0.5], [0.25, -1.0]], 10).unwrap();
        let CreateSuccess::NewBuffer(buffer) = success else {
            panic!("expected a new buffer");
        };

        assert_eq!(buffer.pts().unwrap().nseconds(), 1_000_000_000);
        assert_eq!(buffer.duration().unwrap().nseconds(), 200_000_000);
        assert_eq!(buffer.offset(), 10);
        assert_eq!(buffer.offset_end(), 12);

        let map = buffer.map_readable().unwrap();
        let (sample_bytes, remainder) = map.as_slice().as_chunks::<4>();
        assert!(remainder.is_empty());
        let samples = sample_bytes
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect::<Vec<_>>();
        assert_eq!(samples, [1.0, -0.5, 0.25, -1.0]);
    }

    #[test]
    fn ndsif_initial_seek_accepts_only_the_origin_segment() {
        gst::init().unwrap();
        let initial = gst::FormattedSegment::<gst::ClockTime>::new().upcast();
        assert!(is_initial_noop_time_segment(&initial));

        let mut later = gst::FormattedSegment::<gst::ClockTime>::new();
        later.set_start(gst::ClockTime::from_seconds(1));
        later.set_position(gst::ClockTime::from_seconds(1));
        assert!(!is_initial_noop_time_segment(&later.upcast()));
    }

    #[test]
    fn playback_properties_have_defaults_and_accept_overrides() {
        gst::init().unwrap();
        let defaults = glib::Object::builder::<ChipstreamSrc>().build();
        assert_eq!(defaults.property::<u32>("play-count"), DEFAULT_PLAY_COUNT);
        assert_eq!(
            defaults.property::<f64>("fadeout-seconds"),
            DEFAULT_FADEOUT_SECONDS
        );
        assert_eq!(
            defaults.property::<u32>("ndsif-baud-rate"),
            DEFAULT_NDSIF_BAUD_RATE
        );
        assert_eq!(
            defaults.property::<u32>("ndsif-timeout-ms"),
            DEFAULT_NDSIF_TIMEOUT_MS
        );
        assert!(defaults.property::<bool>("ndsif-reset"));
        assert_eq!(defaults.property::<Option<String>>("ndsif-port"), None);
        assert_eq!(defaults.property::<Option<String>>("ndsif-ports"), None);

        let configured = glib::Object::builder::<ChipstreamSrc>()
            .property("play-count", 3_u32)
            .property("fadeout-seconds", 0.5_f64)
            .property("ndsif-port", "/dev/ttyUSB0")
            .property("ndsif-baud-rate", 230_400_u32)
            .property("ndsif-timeout-ms", 1_000_u32)
            .property("ndsif-reset", false)
            .build();
        assert_eq!(configured.property::<u32>("play-count"), 3);
        assert_eq!(configured.property::<f64>("fadeout-seconds"), 0.5);
        assert_eq!(
            configured.property::<Option<String>>("ndsif-port"),
            Some("/dev/ttyUSB0".to_owned())
        );
        let configured_ports = glib::Object::builder::<ChipstreamSrc>()
            .property("ndsif-ports", "/dev/ttyACM0,/dev/ttyACM1")
            .build();
        assert_eq!(
            configured_ports.property::<Option<String>>("ndsif-ports"),
            Some("/dev/ttyACM0,/dev/ttyACM1".to_owned())
        );
        assert_eq!(configured.property::<u32>("ndsif-baud-rate"), 230_400);
        assert_eq!(configured.property::<u32>("ndsif-timeout-ms"), 1_000);
        assert!(!configured.property::<bool>("ndsif-reset"));
        assert_eq!(fadeout_samples(2.0), Some(88_200));
        assert_eq!(fadeout_samples(0.0), None);
    }

    #[test]
    fn ndsif_port_list_accepts_multiple_devices_and_rejects_ambiguous_values() {
        assert_eq!(
            parse_ndsif_ports(&NdsifSettings {
                port: None,
                ports: Some("/dev/ttyACM0, /dev/ttyACM1".to_owned()),
                baud_rate: DEFAULT_NDSIF_BAUD_RATE,
                timeout_ms: DEFAULT_NDSIF_TIMEOUT_MS,
                reset_device: true,
            })
            .unwrap(),
            ["/dev/ttyACM0", "/dev/ttyACM1"]
        );
        assert_eq!(
            parse_ndsif_ports(&NdsifSettings {
                port: Some("/dev/ttyUSB0".to_owned()),
                ports: None,
                baud_rate: DEFAULT_NDSIF_BAUD_RATE,
                timeout_ms: DEFAULT_NDSIF_TIMEOUT_MS,
                reset_device: true,
            })
            .unwrap(),
            ["/dev/ttyUSB0"]
        );
        assert!(
            parse_ndsif_ports(&NdsifSettings {
                port: Some("/dev/ttyUSB0".to_owned()),
                ports: Some("/dev/ttyACM0".to_owned()),
                baud_rate: DEFAULT_NDSIF_BAUD_RATE,
                timeout_ms: DEFAULT_NDSIF_TIMEOUT_MS,
                reset_device: true,
            })
            .is_err()
        );
        assert!(
            parse_ndsif_ports(&NdsifSettings {
                port: None,
                ports: Some("/dev/ttyACM0, ".to_owned()),
                baud_rate: DEFAULT_NDSIF_BAUD_RATE,
                timeout_ms: DEFAULT_NDSIF_TIMEOUT_MS,
                reset_device: true,
            })
            .is_err()
        );
    }

    #[test]
    fn uri_handler_round_trips_escaped_file_paths() {
        gst::init().unwrap();
        let path = Path::new("/tmp/chipstream test.vgm");
        let uri = glib::filename_to_uri(path, None).unwrap().replacen(
            "file:",
            &format!("{URI_SCHEME}:"),
            1,
        );
        let source = glib::Object::builder::<ChipstreamSrc>().build();
        let handler: &gst::URIHandler = source.upcast_ref();

        handler.set_uri(&uri).unwrap();

        assert_eq!(
            source.property::<String>("location"),
            path.to_string_lossy()
        );
        assert_eq!(handler.uri().as_deref(), Some(uri.as_str()));
        assert_eq!(handler.protocols(), [URI_SCHEME]);
    }

    #[test]
    fn discard_to_frame_matches_sequential_rendering() {
        let data = include_bytes!("../../soundlog/assets/vgm/sn76489_a4.vgm");
        let mut expected_player =
            Sequencer::Vgm(VgmSequencer::from_vgm(data.to_vec(), 48_000, 512).unwrap());
        let mut expected = Vec::new();
        while let Some(block) = expected_player.render().unwrap() {
            expected.extend_from_slice(block.frames());
        }

        let target_frame = 1_237_u64;
        assert!(expected.len() > target_frame as usize);
        let mut seek_player =
            Sequencer::Vgm(VgmSequencer::from_vgm(data.to_vec(), 48_000, 512).unwrap());
        let cancelled = AtomicBool::new(false);
        let mut actual = discard_to_frame(&mut seek_player, target_frame, &cancelled)
            .unwrap()
            .unwrap_or_default();
        while let Some(block) = seek_player.render().unwrap() {
            actual.extend_from_slice(block.frames());
        }

        assert_eq!(actual, expected[target_frame as usize..]);
    }

    #[test]
    fn checkpoint_seek_matches_sequential_rendering() {
        let data = include_bytes!("../../soundlog/assets/vgm/sn76489_a4.vgm");
        let settings = RenderSettings {
            sample_rate: 48_000,
            chunk_frames: 512,
            play_count: 1,
            fadeout_samples: None,
        };
        let mut expected_player =
            VgmSequencer::from_vgm(data.to_vec(), settings.sample_rate, settings.chunk_frames)
                .unwrap();
        expected_player.set_loop_count(Some(settings.play_count));
        expected_player.set_fadeout_samples(None).unwrap();
        let mut expected = Vec::new();
        while let Some(block) = expected_player.render().unwrap() {
            expected.extend_from_slice(block.frames());
        }

        let mut player = Sequencer::Vgm(
            VgmSequencer::from_vgm(data.to_vec(), settings.sample_rate, settings.chunk_frames)
                .unwrap(),
        );
        if let Sequencer::Vgm(player) = &mut player {
            player.set_loop_count(Some(settings.play_count));
            player.set_fadeout_samples(None).unwrap();
        }
        let mut checkpoints = VecDeque::new();
        capture_checkpoint(&mut player, &mut checkpoints).unwrap();
        let cancelled = AtomicBool::new(false);
        let (output, _receiver) = mpsc::sync_channel(1);
        let mut checkpointing_enabled = true;
        let mut next_checkpoint_frame = settings.sample_rate as usize * CHECKPOINT_INTERVAL_SECONDS;
        let target_frame = 1_237_u64;
        let (position, pending, reached_end) = seek_player(
            &mut player,
            &mut checkpoints,
            Path::new("unused.vgm"),
            None,
            settings,
            target_frame,
            &cancelled,
            &mut checkpointing_enabled,
            &mut next_checkpoint_frame,
            &output,
        )
        .unwrap();

        assert_eq!(position, target_frame);
        assert!(!reached_end);
        let mut actual = pending.unwrap_or_default();
        while let Some(block) = player.render().unwrap() {
            actual.extend_from_slice(block.frames());
        }
        assert_eq!(actual, expected[target_frame as usize..]);
    }

    #[test]
    fn source_accepts_flushing_time_seek() {
        gst::init().unwrap();
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../soundlog/assets/vgm/sn76489_a4.vgm");
        let source = glib::Object::builder::<ChipstreamSrc>()
            .property("location", path.to_string_lossy().as_ref())
            .build();
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
            .unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.add(&source).unwrap();
        pipeline.add(&sink).unwrap();
        source.link(&sink).unwrap();
        let bus = pipeline.bus().unwrap();

        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_mseconds(100),
            )
            .unwrap();
        pipeline
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_mseconds(50),
            )
            .unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(10),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("pipeline should finish after seeking");
        assert_eq!(message.type_(), gst::MessageType::Eos, "{message:?}");
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn source_answers_time_duration_query() {
        gst::init().unwrap();
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../soundlog/assets/vgm/sn76489_a4.vgm");
        let data = fs::read(&path).unwrap();
        let expected_frames =
            measure_vgm_duration_frames(&data, DEFAULT_SAMPLE_RATE, Some(1), None)
                .unwrap()
                .unwrap();
        let expected = checked_frame_time(u64::from(expected_frames), DEFAULT_SAMPLE_RATE).unwrap();
        let source = glib::Object::builder::<ChipstreamSrc>()
            .property("location", path.to_string_lossy().as_ref())
            .property("play-count", 1_u32)
            .property("fadeout-seconds", 0.0_f64)
            .build();
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("async", false)
            .build()
            .unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.add(&source).unwrap();
        pipeline.add(&sink).unwrap();
        source.link(&sink).unwrap();
        let bus = pipeline.bus().unwrap();

        pipeline.set_state(gst::State::Paused).unwrap();
        pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
        bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::DurationChanged],
        )
        .expect("source should report its measured duration");

        let mut query = gst::query::Duration::new(gst::Format::Time);
        assert!(source.query(&mut query));
        assert_eq!(
            query.result(),
            gst::GenericFormattedValue::Time(Some(expected))
        );
        pipeline.set_state(gst::State::Null).unwrap();
    }

    #[test]
    fn loads_adjacent_ym2608_adpcm_rom_for_vgm_playback() {
        let directory = std::env::temp_dir().join(format!(
            "chipstream-gstreamer-rom-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();

        let input_path = directory.join("song.vgm");
        let rom_path = directory.join("ym2608_adpcm_rom.bin");
        let mut builder = VgmBuilder::new();
        builder.register_chip(SoundlogChip::Ym2608, SoundlogInstance::Primary, 8_000_000);
        for (register, value) in [(0x11, 0x3f), (0x18, 0xdf), (0x10, 0x01)] {
            builder.add_chip_write(
                SoundlogInstance::Primary,
                Ym2608Spec {
                    port: 0,
                    register,
                    value,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(4410));
        let bytes: Vec<u8> = builder.finalize().into();
        fs::write(&input_path, bytes).unwrap();
        fs::write(&rom_path, [0x77; 8192]).unwrap();

        let settings = RenderSettings {
            sample_rate: 44_100,
            chunk_frames: 256,
            play_count: 1,
            fadeout_samples: None,
        };
        let (mut sequencer, _, _, _) = load_sequencer(&input_path, None, settings).unwrap();
        let mut has_audio = false;
        while let Some(block) = sequencer.render().unwrap() {
            has_audio |= block
                .frames()
                .iter()
                .any(|frame| frame[0].abs() > 0.001 && frame[1].abs() > 0.001);
        }

        fs::remove_dir_all(&directory).unwrap();
        assert!(has_audio);
    }

    #[test]
    fn playbin_and_playbin3_play_chipstream_uri() {
        gst::init().unwrap();
        gst::Element::register(
            None,
            "chipstreamsrc",
            gst::Rank::PRIMARY,
            ChipstreamSrc::static_type(),
        )
        .unwrap();
        let fixtures = [
            "../soundlog/assets/vgm/sn76489_a4.vgm",
            "../soundlog/assets/mdx/mdx_fm_loop.mdx",
        ];
        for factory in ["playbin", "playbin3"] {
            for fixture in fixtures {
                let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
                let uri = glib::filename_to_uri(path, None).unwrap().replacen(
                    "file:",
                    &format!("{URI_SCHEME}:"),
                    1,
                );
                let sink = gst::ElementFactory::make("fakesink")
                    .property("sync", false)
                    .property("async", false)
                    .build()
                    .unwrap();
                let player = gst::ElementFactory::make(factory)
                    .property("uri", &uri)
                    .property("audio-sink", &sink)
                    .build()
                    .unwrap_or_else(|error| panic!("failed to create {factory}: {error}"));
                let pipeline = gst::Pipeline::new();
                pipeline.add(&player).unwrap();
                let bus = pipeline.bus().unwrap();

                pipeline.set_state(gst::State::Playing).unwrap();
                let message = bus
                    .timed_pop_filtered(
                        gst::ClockTime::from_seconds(10),
                        &[gst::MessageType::Eos, gst::MessageType::Error],
                    )
                    .unwrap_or_else(|| panic!("{factory} should finish playback: {fixture}"));
                assert_eq!(message.type_(), gst::MessageType::Eos, "{message:?}");
                pipeline.set_state(gst::State::Null).unwrap();
            }
        }
    }
}
