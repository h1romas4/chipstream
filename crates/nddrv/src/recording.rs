use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::library::RecordingSettings;

pub fn input_devices() -> Result<Vec<(String, String)>> {
    cpal::default_host()
        .input_devices()
        .context("could not enumerate audio input devices")?
        .map(|device| Ok((device.id()?.to_string(), device.to_string())))
        .collect()
}

fn input_device(device_id: Option<&str>) -> Result<cpal::Device> {
    match device_id {
        Some(device_id) => {
            let id = device_id.parse::<cpal::DeviceId>()?;
            cpal::host_from_id(id.host())?
                .device_by_id(&id)
                .with_context(|| format!("audio input device is unavailable: {device_id}"))
        }
        None => cpal::default_host()
            .default_input_device()
            .context("no default audio input device"),
    }
}

fn input_config(
    device: &cpal::Device,
    sample_rate: Option<u32>,
) -> Result<cpal::SupportedStreamConfig> {
    let default = device
        .default_input_config()
        .context("no default audio input configuration")?;
    let Some(rate) = sample_rate else {
        return Ok(default);
    };
    if default.sample_rate() == rate {
        return Ok(default);
    }
    let ranges = device.supported_input_configs()?.collect::<Vec<_>>();
    ranges
        .iter()
        .find(|range| {
            range.channels() == default.channels()
                && range.sample_format() == default.sample_format()
                && range.try_with_sample_rate(rate).is_some()
        })
        .or_else(|| {
            ranges
                .iter()
                .find(|range| range.try_with_sample_rate(rate).is_some())
        })
        .and_then(|range| range.try_with_sample_rate(rate))
        .with_context(|| format!("audio input device does not support {rate} Hz"))
}

fn create_file(directory: &Path, source: &Path) -> Result<(PathBuf, File)> {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    create_file_at(directory, source, &timestamp)
}

fn create_file_at(directory: &Path, source: &Path, timestamp: &str) -> Result<(PathBuf, File)> {
    let stem = source.file_stem().context("track has no filename")?;
    for sequence in 0..10_000 {
        let mut filename = stem.to_os_string();
        if filename.as_encoded_bytes().len() > 230 {
            filename = stem
                .to_string_lossy()
                .chars()
                .scan(0usize, |length, character| {
                    *length += character.len_utf8();
                    (*length <= 230).then_some(character)
                })
                .collect::<String>()
                .into();
        }
        filename.push(format!("-{timestamp}"));
        if sequence > 0 {
            filename.push(format!("-{sequence}"));
        }
        filename.push(".wav");
        let path = directory.join(filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not create WAV in {}", directory.display()));
            }
        }
    }
    bail!("could not find a unique WAV filename")
}

type Writer = hound::WavWriter<BufWriter<File>>;

fn checked_sample_count(current: u64, incoming: usize) -> Result<u64> {
    let count = current
        .checked_add(u64::try_from(incoming)?)
        .context("WAV sample count overflow")?;
    if count > u64::from(u32::MAX - 60) / 2 {
        bail!("recording exceeds the WAV size limit");
    }
    Ok(count)
}

fn write_samples(mut writer: Writer, receiver: mpsc::Receiver<Vec<i16>>) -> Result<u64> {
    let mut count = 0;
    for block in receiver {
        count = checked_sample_count(count, block.len())?;
        for sample in block {
            writer.write_sample(sample)?;
        }
    }
    writer.finalize()?;
    Ok(count)
}

fn handle_input_error(
    stream_error: cpal::Error,
    failed: &AtomicBool,
    error: &Mutex<Option<String>>,
    xruns: &AtomicU64,
) {
    if stream_error.kind() == cpal::ErrorKind::Xrun {
        xruns.fetch_add(1, Ordering::Relaxed);
    } else {
        if let Ok(mut error) = error.lock() {
            *error = Some(stream_error.to_string());
        }
        failed.store(true, Ordering::Release);
    }
}

fn stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sender: mpsc::SyncSender<Vec<i16>>,
    failed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    xruns: Arc<AtomicU64>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    i16: FromSample<T>,
{
    let input_failed = failed.clone();
    Ok(device.build_input_stream(
        *config,
        move |data: &[T], _| {
            let samples = data
                .iter()
                .map(|sample| sample.to_sample::<i16>())
                .collect();
            if sender.try_send(samples).is_err() {
                failed.store(true, Ordering::Release);
            }
        },
        move |stream_error| handle_input_error(stream_error, &input_failed, &error, &xruns),
        None,
    )?)
}

pub struct Capture {
    stream: Option<cpal::Stream>,
    writer: JoinHandle<Result<u64>>,
    failed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    xruns: Arc<AtomicU64>,
    path: PathBuf,
}

pub struct SavedRecording {
    pub path: PathBuf,
    pub xruns: u64,
}

impl Capture {
    pub fn start(
        settings: &RecordingSettings,
        source: &Path,
        failed: Arc<AtomicBool>,
    ) -> Result<Self> {
        let directory = settings
            .output_directory
            .as_deref()
            .context("select a WAV output folder in Recording settings")?;
        let device = input_device(settings.input_device.as_deref())?;
        let supported = input_config(&device, settings.sample_rate)?;
        let config = supported.config();
        let (sender, receiver) = mpsc::sync_channel(256);
        let error = Arc::new(Mutex::new(None));
        let xruns = Arc::new(AtomicU64::new(0));
        macro_rules! build {
            ($sample:ty) => {
                stream::<$sample>(
                    &device,
                    &config,
                    sender,
                    failed.clone(),
                    error.clone(),
                    xruns.clone(),
                )?
            };
        }
        let stream = match supported.sample_format() {
            SampleFormat::I8 => build!(i8),
            SampleFormat::I16 => build!(i16),
            SampleFormat::I24 => build!(cpal::I24),
            SampleFormat::I32 => build!(i32),
            SampleFormat::I64 => build!(i64),
            SampleFormat::U8 => build!(u8),
            SampleFormat::U16 => build!(u16),
            SampleFormat::U24 => build!(cpal::U24),
            SampleFormat::U32 => build!(u32),
            SampleFormat::U64 => build!(u64),
            SampleFormat::F32 => build!(f32),
            SampleFormat::F64 => build!(f64),
            format => bail!("unsupported audio input format: {format:?}"),
        };
        let (path, file) = create_file(directory, source)?;
        let spec = hound::WavSpec {
            channels: config.channels,
            sample_rate: config.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = match hound::WavWriter::new(BufWriter::new(file), spec) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                return Err(error.into());
            }
        };
        let writer = thread::spawn(move || write_samples(writer, receiver));
        let capture = Self {
            stream: Some(stream),
            writer,
            failed,
            error,
            xruns,
            path,
        };
        if let Err(error) = capture.stream.as_ref().unwrap().play() {
            let path = capture.path.clone();
            let _ = capture.finish();
            let _ = std::fs::remove_file(path);
            return Err(error.into());
        }
        Ok(capture)
    }

    pub fn finish(mut self) -> Result<Option<SavedRecording>> {
        drop(self.stream.take());
        let result = self
            .writer
            .join()
            .map_err(|_| anyhow::anyhow!("WAV writer thread failed"))
            .and_then(|result| result)
            .and_then(|samples| {
                if let Some(error) = self
                    .error
                    .lock()
                    .map_err(|_| anyhow::anyhow!("audio input error state unavailable"))?
                    .take()
                {
                    bail!("audio input error: {error}");
                }
                if self.failed.load(Ordering::Acquire) {
                    bail!("audio input buffer overflow");
                }
                Ok(samples)
            });
        match result {
            Ok(0) => {
                let _ = std::fs::remove_file(&self.path);
                Ok(None)
            }
            Ok(_) => Ok(Some(SavedRecording {
                path: self.path,
                xruns: self.xruns.load(Ordering::Acquire),
            })),
            Err(error) => {
                let _ = std::fs::remove_file(&self.path);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_writer_preserves_samples_and_uses_unique_safe_names() {
        let directory = tempfile::tempdir().unwrap();
        let timestamp = "20261105-123456";
        let (path, file) =
            create_file_at(directory.path(), Path::new("../song.vgm"), timestamp).unwrap();
        let (other, _) =
            create_file_at(directory.path(), Path::new("../song.vgm"), timestamp).unwrap();
        assert_eq!(path.file_name().unwrap(), "song-20261105-123456.wav");
        assert_eq!(other.file_name().unwrap(), "song-20261105-123456-1.wav");
        assert_eq!(path.parent(), Some(directory.path()));
        let writer = hound::WavWriter::new(
            BufWriter::new(file),
            hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(vec![1, -2, 3, -4]).unwrap();
        drop(sender);
        assert_eq!(write_samples(writer, receiver).unwrap(), 4);
        let mut reader = hound::WavReader::open(path).unwrap();
        assert_eq!(reader.spec().sample_rate, 48_000);
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![1, -2, 3, -4]
        );
        let long_name = format!("{}.mdx", "音".repeat(80));
        let (long_path, _) =
            create_file_at(directory.path(), Path::new(&long_name), timestamp).unwrap();
        let (long_collision, _) =
            create_file_at(directory.path(), Path::new(&long_name), timestamp).unwrap();
        assert!(
            long_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .ends_with(timestamp)
        );
        assert!(long_collision.file_name().unwrap().to_string_lossy().len() < 255);
    }

    #[test]
    fn recording_filename_includes_local_readable_timestamp() {
        let directory = tempfile::tempdir().unwrap();
        let (path, _) = create_file(directory.path(), Path::new("music.mdx")).unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        let timestamp = filename
            .strip_prefix("music-")
            .unwrap()
            .strip_suffix(".wav")
            .unwrap();
        chrono::NaiveDateTime::parse_from_str(timestamp, "%Y%m%d-%H%M%S").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recording_filename_preserves_non_utf8_source_name() {
        use std::os::unix::ffi::OsStrExt;

        let directory = tempfile::tempdir().unwrap();
        let source = Path::new(std::ffi::OsStr::from_bytes(b"track\xff.mdx"));
        let (path, _) = create_file_at(directory.path(), source, "20261105-123456").unwrap();
        assert_eq!(
            path.file_name().unwrap().as_bytes(),
            b"track\xff-20261105-123456.wav"
        );
    }

    #[test]
    fn wav_size_is_checked_before_writing() {
        let maximum = u64::from(u32::MAX - 60) / 2;
        assert_eq!(checked_sample_count(maximum - 1, 1).unwrap(), maximum);
        assert!(checked_sample_count(maximum, 1).is_err());
    }

    #[test]
    fn capture_finalizes_valid_audio_and_removes_failed_recording() {
        let directory = tempfile::tempdir().unwrap();
        for failed in [false, true] {
            let (path, file) = create_file(directory.path(), Path::new("track.mdx")).unwrap();
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 44_100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let writer = hound::WavWriter::new(BufWriter::new(file), spec).unwrap();
            let (sender, receiver) = mpsc::channel();
            sender.send(vec![12, -34]).unwrap();
            drop(sender);
            let capture = Capture {
                stream: None,
                writer: thread::spawn(move || write_samples(writer, receiver)),
                failed: Arc::new(AtomicBool::new(failed)),
                error: Arc::new(Mutex::new(None)),
                xruns: Arc::new(AtomicU64::new(0)),
                path: path.clone(),
            };
            let result = capture.finish();
            if failed {
                assert!(result.is_err());
                assert!(!path.exists());
            } else {
                let saved = result.unwrap().unwrap();
                assert_eq!(saved.path, path);
                assert_eq!(saved.xruns, 0);
                let mut reader = hound::WavReader::open(path).unwrap();
                assert_eq!(
                    reader
                        .samples::<i16>()
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap(),
                    vec![12, -34]
                );
            }
        }
    }

    #[test]
    fn input_xrun_is_recoverable_but_disconnect_is_fatal() {
        let failed = AtomicBool::new(false);
        let error = Mutex::new(None);
        let xruns = AtomicU64::new(0);
        handle_input_error(cpal::ErrorKind::Xrun.into(), &failed, &error, &xruns);
        assert_eq!(xruns.load(Ordering::Relaxed), 1);
        assert!(!failed.load(Ordering::Acquire));
        assert!(error.lock().unwrap().is_none());
        handle_input_error(
            cpal::ErrorKind::DeviceNotAvailable.into(),
            &failed,
            &error,
            &xruns,
        );
        assert!(failed.load(Ordering::Acquire));
        assert!(error.lock().unwrap().is_some());
    }

    #[test]
    fn capture_saves_samples_and_reports_recovered_xruns() {
        let directory = tempfile::tempdir().unwrap();
        let (path, file) = create_file(directory.path(), Path::new("track.vgm")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::new(BufWriter::new(file), spec).unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(vec![12, -34]).unwrap();
        drop(sender);
        let failed = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let xruns = Arc::new(AtomicU64::new(0));
        handle_input_error(cpal::ErrorKind::Xrun.into(), &failed, &error, &xruns);
        let capture = Capture {
            stream: None,
            writer: thread::spawn(move || write_samples(writer, receiver)),
            failed,
            error,
            xruns,
            path: path.clone(),
        };
        let saved = capture.finish().unwrap().unwrap();
        assert_eq!(saved.path, path);
        assert_eq!(saved.xruns, 1);
        let mut reader = hound::WavReader::open(path).unwrap();
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![12, -34]
        );
    }
}
