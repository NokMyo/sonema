use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;
use thiserror::Error;

use crate::MonitorBus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("오디오 입력 장치 목록을 읽지 못했습니다: {0}")]
    Enumerate(String),
    #[error("선택한 오디오 입력 장치를 찾을 수 없습니다")]
    NoInputDevice,
    #[error("입력 장치 설정을 읽지 못했습니다: {0}")]
    InputConfig(String),
    #[error("지원하지 않는 입력 샘플 형식입니다: {0}")]
    UnsupportedSampleFormat(String),
    #[error("녹음 스트림을 만들지 못했습니다: {0}")]
    BuildStream(String),
    #[error("녹음을 시작하지 못했습니다: {0}")]
    StartStream(String),
}

#[derive(Debug)]
pub struct RecordedAudio {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
    pub dropped_samples: u64,
}

pub struct Recorder {
    stream: Stream,
    queue: Arc<ArrayQueue<f32>>,
    channels: usize,
    sample_rate: u32,
    dropped_samples: Arc<AtomicU64>,
    last_error: Arc<Mutex<Option<String>>>,
}

pub fn list_input_devices() -> Result<Vec<InputDevice>, RecorderError> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|device| device.to_string());
    let devices = host
        .input_devices()
        .map_err(|error| RecorderError::Enumerate(error.to_string()))?;
    let mut result = devices
        .map(|device| {
            let name = device.to_string();
            InputDevice {
                is_default: default_name.as_deref() == Some(name.as_str()),
                name,
            }
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then(left.name.cmp(&right.name))
    });
    Ok(result)
}

impl Recorder {
    pub fn start(
        requested_device: Option<&str>,
        monitor: Option<MonitorBus>,
    ) -> Result<Self, RecorderError> {
        let host = cpal::default_host();
        let device = if let Some(requested) = requested_device {
            host.input_devices()
                .map_err(|error| RecorderError::Enumerate(error.to_string()))?
                .find(|device| device.to_string() == requested)
                .ok_or(RecorderError::NoInputDevice)?
        } else {
            host.default_input_device()
                .ok_or(RecorderError::NoInputDevice)?
        };
        let supported = device
            .default_input_config()
            .map_err(|error| RecorderError::InputConfig(error.to_string()))?;
        let sample_format = supported.sample_format();
        let config = supported.config();
        let channels = config.channels as usize;
        let sample_rate = config.sample_rate;
        let queue = Arc::new(ArrayQueue::new(sample_rate as usize * channels.max(1) * 8));
        let dropped_samples = Arc::new(AtomicU64::new(0));
        let last_error = Arc::new(Mutex::new(None));
        if let Some(bus) = &monitor {
            bus.configure_input(sample_rate, channels);
        }

        let capture_queue = queue.clone();
        let capture_dropped = dropped_samples.clone();
        let error_slot = last_error.clone();
        let stream = match sample_format {
            SampleFormat::F32 => build_input_stream::<f32>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::F64 => build_input_stream::<f64>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::I8 => build_input_stream::<i8>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::I16 => build_input_stream::<i16>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::I24 => build_input_stream::<cpal::I24>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::I32 => build_input_stream::<i32>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::I64 => build_input_stream::<i64>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::U8 => build_input_stream::<u8>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::U16 => build_input_stream::<u16>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::U24 => build_input_stream::<cpal::U24>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::U32 => build_input_stream::<u32>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            SampleFormat::U64 => build_input_stream::<u64>(
                &device,
                config,
                channels,
                capture_queue,
                capture_dropped,
                monitor,
                error_slot,
            ),
            other => return Err(RecorderError::UnsupportedSampleFormat(other.to_string())),
        }
        .map_err(|error| RecorderError::BuildStream(error.to_string()))?;
        stream
            .play()
            .map_err(|error| RecorderError::StartStream(error.to_string()))?;
        Ok(Self {
            stream,
            queue,
            channels,
            sample_rate,
            dropped_samples,
            last_error,
        })
    }

    pub fn drain_into(&self, channels: &mut Vec<Vec<f32>>) -> usize {
        if channels.len() != self.channels {
            channels.clear();
            channels.resize_with(self.channels, Vec::new);
        }
        let frames = self.queue.len() / self.channels.max(1);
        for _ in 0..frames {
            for channel in channels.iter_mut() {
                channel.push(self.queue.pop().unwrap_or(0.0));
            }
        }
        frames
    }

    pub fn finish(self, mut channels: Vec<Vec<f32>>) -> RecordedAudio {
        let _ = self.stream.pause();
        self.drain_into(&mut channels);
        RecordedAudio {
            sample_rate: self.sample_rate,
            channels,
            dropped_samples: self.dropped_samples.load(Ordering::Relaxed),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channel_count(&self) -> usize {
        self.channels
    }

    pub fn take_error(&self) -> Option<String> {
        self.last_error.lock().take()
    }
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    queue: Arc<ArrayQueue<f32>>,
    dropped: Arc<AtomicU64>,
    monitor: Option<MonitorBus>,
    error_slot: Arc<Mutex<Option<String>>>,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device.build_input_stream::<T, _, _>(
        config,
        move |data, _| capture(data, channels, &queue, &dropped, monitor.as_ref()),
        move |error: cpal::Error| {
            *error_slot.lock() = Some(error.to_string());
        },
        None,
    )
}

fn capture<T>(
    data: &[T],
    channels: usize,
    queue: &ArrayQueue<f32>,
    dropped: &AtomicU64,
    monitor: Option<&MonitorBus>,
) where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = channels.max(1);
    for frame in data.chunks_exact(channels) {
        if queue.capacity().saturating_sub(queue.len()) < channels {
            dropped.fetch_add(channels as u64, Ordering::Relaxed);
            continue;
        }
        for &sample in frame {
            let sample = f32::from_sample(sample);
            let sample = if sample.is_finite() { sample } else { 0.0 };
            let _ = queue.push(sample);
            if let Some(bus) = monitor {
                bus.push(sample);
            }
        }
    }
}
