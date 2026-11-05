use std::fs::{File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn filename_title(title: &str) -> String {
    let title: String = title
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .scan(0usize, |length, character| {
            *length += character.len_utf8();
            (*length <= 120).then_some(character)
        })
        .collect();
    let title = title.trim_matches(|character: char| character == '.' || character.is_whitespace());
    if title.is_empty() {
        "Recording".into()
    } else {
        title.into()
    }
}

fn create_file(directory: &Path, title: &str) -> Result<(PathBuf, File)> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let title = filename_title(title);
    for sequence in 0..10_000 {
        let path = directory.join(format!("{title}-{timestamp}-{sequence}.wav"));
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

fn stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sender: mpsc::SyncSender<Vec<i16>>,
    failed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
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
        move |stream_error| {
            if let Ok(mut error) = error.lock() {
                *error = Some(stream_error.to_string());
            }
            input_failed.store(true, Ordering::Release);
        },
        None,
    )?)
}

pub struct Capture {
    stream: Option<cpal::Stream>,
    writer: JoinHandle<Result<u64>>,
    failed: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    path: PathBuf,
}

impl Capture {
    pub fn start(
        settings: &RecordingSettings,
        title: &str,
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
        macro_rules! build {
            ($sample:ty) => {
                stream::<$sample>(&device, &config, sender, failed.clone(), error.clone())?
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
        let (path, file) = create_file(directory, title)?;
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

    pub fn finish(mut self) -> Result<Option<PathBuf>> {
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
            Ok(_) => Ok(Some(self.path)),
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
        let (path, file) = create_file(directory.path(), "../bad: title?").unwrap();
        let (other, _) = create_file(directory.path(), "../bad: title?").unwrap();
        assert_ne!(path, other);
        assert_eq!(path.parent(), Some(directory.path()));
        assert!(!path.file_name().unwrap().to_string_lossy().contains(':'));
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
        let (long_path, _) = create_file(directory.path(), &"音".repeat(200)).unwrap();
        assert!(long_path.file_name().unwrap().to_string_lossy().len() < 255);
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
            let (path, file) = create_file(directory.path(), "track").unwrap();
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
                path: path.clone(),
            };
            let result = capture.finish();
            if failed {
                assert!(result.is_err());
                assert!(!path.exists());
            } else {
                assert_eq!(result.unwrap(), Some(path.clone()));
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
}
