use std::array;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender, bounded};
use crossbeam_queue::ArrayQueue;
use parking_lot::Mutex;
use sonema_core::{Project, TrackId};
use thiserror::Error;

use crate::{MediaPool, RealtimeSession};

pub const MAX_METER_TRACKS: usize = 128;
const RETIRED_SESSION_CAPACITY: usize = 128;

#[derive(Debug, Clone)]
pub struct DeviceStatus {
    pub output_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, Error)]
pub enum AudioEngineError {
    #[error("기본 오디오 출력 장치를 찾을 수 없습니다")]
    NoOutputDevice,
    #[error("출력 장치 설정을 읽지 못했습니다: {0}")]
    OutputConfig(String),
    #[error("지원하지 않는 출력 샘플 형식입니다: {0}")]
    UnsupportedSampleFormat(String),
    #[error("오디오 출력 스트림을 만들지 못했습니다: {0}")]
    BuildStream(String),
    #[error("오디오 출력을 시작하지 못했습니다: {0}")]
    StartStream(String),
    #[error("오디오 세션을 준비하지 못했습니다: {0}")]
    CompileSession(String),
    #[error("오디오 엔진 연결이 끊겼습니다")]
    Disconnected,
}

#[derive(Debug)]
struct SharedState {
    playing: AtomicBool,
    playhead: AtomicU64,
    track_peaks: [AtomicU32; MAX_METER_TRACKS],
    master_peak: AtomicU32,
    last_error: Mutex<Option<String>>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            playing: AtomicBool::new(false),
            playhead: AtomicU64::new(0),
            track_peaks: array::from_fn(|_| AtomicU32::new(0.0_f32.to_bits())),
            master_peak: AtomicU32::new(0.0_f32.to_bits()),
            last_error: Mutex::new(None),
        }
    }
}

/// Preallocated one-way input monitoring path. Producers never allocate or
/// block; overflowing samples are dropped instead of stalling an audio callback.
#[derive(Debug, Clone)]
pub struct MonitorBus {
    queue: Arc<ArrayQueue<f32>>,
    enabled: Arc<AtomicBool>,
    input_channels: Arc<AtomicUsize>,
    input_rate: Arc<AtomicU32>,
    output_rate: u32,
}

impl MonitorBus {
    fn new(output_rate: u32) -> Self {
        Self {
            queue: Arc::new(ArrayQueue::new(output_rate as usize * 8)),
            enabled: Arc::new(AtomicBool::new(false)),
            input_channels: Arc::new(AtomicUsize::new(1)),
            input_rate: Arc::new(AtomicU32::new(0)),
            output_rate,
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> bool {
        while self.queue.pop().is_some() {}
        let compatible = self.input_rate.load(Ordering::Relaxed) == self.output_rate;
        let actual = enabled && compatible;
        self.enabled.store(actual, Ordering::Release);
        actual
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn rates_match(&self) -> bool {
        self.input_rate.load(Ordering::Relaxed) == self.output_rate
    }

    pub(crate) fn configure_input(&self, sample_rate: u32, channels: usize) {
        self.input_rate.store(sample_rate, Ordering::Release);
        self.input_channels
            .store(channels.max(1), Ordering::Release);
        if sample_rate != self.output_rate {
            self.enabled.store(false, Ordering::Release);
        }
    }

    pub(crate) fn push(&self, value: f32) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.queue.push(value);
        }
    }

    fn pop_frame(&self) -> [f32; 2] {
        if !self.enabled.load(Ordering::Acquire) {
            return [0.0, 0.0];
        }
        let channels = self.input_channels.load(Ordering::Relaxed).max(1);
        let left = self.queue.pop().unwrap_or(0.0);
        let right = if channels > 1 {
            self.queue.pop().unwrap_or(left)
        } else {
            left
        };
        for _ in 2..channels {
            let _ = self.queue.pop();
        }
        [left, right]
    }
}

enum EngineCommand {
    SetSession(Box<RealtimeSession>),
    Play,
    Pause,
    Stop,
    Seek(u64),
    SetLoopEnabled(bool),
}

struct AudioThread {
    receiver: Receiver<EngineCommand>,
    shared: Arc<SharedState>,
    monitor: MonitorBus,
    retired_sessions: Arc<ArrayQueue<Box<RealtimeSession>>>,
    session: Option<Box<RealtimeSession>>,
    playhead: f64,
    loop_enabled: bool,
}

impl AudioThread {
    fn new(
        receiver: Receiver<EngineCommand>,
        shared: Arc<SharedState>,
        monitor: MonitorBus,
        retired_sessions: Arc<ArrayQueue<Box<RealtimeSession>>>,
    ) -> Self {
        Self {
            receiver,
            shared,
            monitor,
            retired_sessions,
            session: None,
            playhead: 0.0,
            loop_enabled: false,
        }
    }

    fn begin_buffer(&mut self) {
        while let Ok(command) = self.receiver.try_recv() {
            match command {
                EngineCommand::SetSession(mut session) => {
                    session.reset_dsp();
                    if let Some(retired) = self.session.replace(session)
                        && let Err(retired) = self.retired_sessions.push(retired)
                    {
                        // This queue is larger than the bounded command queue and is drained
                        // before every UI-side update. If that invariant is ever broken, leaking
                        // one graph is safer than deallocating it on the realtime callback.
                        std::mem::forget(retired);
                    }
                }
                EngineCommand::Play => {
                    if self.session.is_some() {
                        self.shared.playing.store(true, Ordering::Release);
                    }
                }
                EngineCommand::Pause => {
                    self.shared.playing.store(false, Ordering::Release);
                }
                EngineCommand::Stop => {
                    self.shared.playing.store(false, Ordering::Release);
                    self.playhead = 0.0;
                    if let Some(session) = &mut self.session {
                        session.reset_dsp();
                    }
                    self.shared.playhead.store(0, Ordering::Release);
                }
                EngineCommand::Seek(frame) => {
                    self.playhead = frame as f64;
                    if let Some(session) = &mut self.session {
                        session.reset_dsp();
                    }
                    self.shared.playhead.store(frame, Ordering::Release);
                }
                EngineCommand::SetLoopEnabled(enabled) => self.loop_enabled = enabled,
            }
        }
    }

    #[inline]
    fn next_frame(&mut self) -> [f32; 2] {
        let mut output = [0.0_f32; 2];
        if self.shared.playing.load(Ordering::Acquire)
            && let Some(session) = &mut self.session
        {
            if self.loop_enabled
                && let Some((start, end)) = session.loop_region()
                && self.playhead >= end
            {
                self.playhead = start;
                session.reset_dsp();
            }
            if self.playhead < session.duration_frames() as f64 {
                output = session.process_frame(self.playhead);
                self.playhead += session.project_frames_per_output_frame();
            } else {
                self.shared.playing.store(false, Ordering::Release);
            }
        }
        let monitor = self.monitor.pop_frame();
        output[0] += monitor[0];
        output[1] += monitor[1];
        output
    }

    fn finish_buffer(&self) {
        self.shared
            .playhead
            .store(self.playhead.max(0.0) as u64, Ordering::Release);
        if self.shared.playing.load(Ordering::Acquire)
            && let Some(session) = &self.session
        {
            for (index, (_, peak)) in session.track_meters().enumerate().take(MAX_METER_TRACKS) {
                self.shared.track_peaks[index].store(peak.to_bits(), Ordering::Relaxed);
            }
            self.shared
                .master_peak
                .store(session.master_meter().to_bits(), Ordering::Relaxed);
            return;
        }
        for meter in &self.shared.track_peaks {
            meter.store(0.0_f32.to_bits(), Ordering::Relaxed);
        }
        self.shared
            .master_peak
            .store(0.0_f32.to_bits(), Ordering::Relaxed);
    }
}

pub struct AudioEngine {
    sender: Sender<EngineCommand>,
    shared: Arc<SharedState>,
    monitor: MonitorBus,
    retired_sessions: Arc<ArrayQueue<Box<RealtimeSession>>>,
    stream: Stream,
    status: DeviceStatus,
}

impl AudioEngine {
    pub fn new() -> Result<Self, AudioEngineError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(AudioEngineError::NoOutputDevice)?;
        let output_name = device.to_string();
        let supported = device
            .default_output_config()
            .map_err(|error| AudioEngineError::OutputConfig(error.to_string()))?;
        let sample_format = supported.sample_format();
        let config = supported.config();
        let status = DeviceStatus {
            output_name,
            sample_rate: config.sample_rate,
            channels: config.channels,
        };
        let (sender, receiver) = bounded(64);
        let shared = Arc::new(SharedState::new());
        let monitor = MonitorBus::new(status.sample_rate);
        let retired_sessions = Arc::new(ArrayQueue::new(RETIRED_SESSION_CAPACITY));
        let thread = AudioThread::new(
            receiver,
            shared.clone(),
            monitor.clone(),
            retired_sessions.clone(),
        );
        let error_shared = shared.clone();

        let channels = config.channels as usize;
        let stream = match sample_format {
            SampleFormat::F32 => {
                build_output_stream::<f32>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::F64 => {
                build_output_stream::<f64>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::I8 => {
                build_output_stream::<i8>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::I16 => {
                build_output_stream::<i16>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::I24 => {
                build_output_stream::<cpal::I24>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::I32 => {
                build_output_stream::<i32>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::I64 => {
                build_output_stream::<i64>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::U8 => {
                build_output_stream::<u8>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::U16 => {
                build_output_stream::<u16>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::U24 => {
                build_output_stream::<cpal::U24>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::U32 => {
                build_output_stream::<u32>(&device, config, channels, thread, error_shared)
            }
            SampleFormat::U64 => {
                build_output_stream::<u64>(&device, config, channels, thread, error_shared)
            }
            other => return Err(AudioEngineError::UnsupportedSampleFormat(other.to_string())),
        }
        .map_err(|error| AudioEngineError::BuildStream(error.to_string()))?;
        stream
            .play()
            .map_err(|error| AudioEngineError::StartStream(error.to_string()))?;
        Ok(Self {
            sender,
            shared,
            monitor,
            retired_sessions,
            stream,
            status,
        })
    }

    pub fn set_project(
        &self,
        project: &Project,
        media: &MediaPool,
        metronome: bool,
    ) -> Result<(), AudioEngineError> {
        self.collect_retired_sessions();
        let session = RealtimeSession::compile(project, media, self.status.sample_rate, metronome)
            .map_err(|error| AudioEngineError::CompileSession(error.to_string()))?;
        self.send(EngineCommand::SetSession(Box::new(session)))
    }

    pub fn play(&self) -> Result<(), AudioEngineError> {
        self.send(EngineCommand::Play)
    }

    pub fn pause(&self) -> Result<(), AudioEngineError> {
        self.send(EngineCommand::Pause)
    }

    pub fn stop(&self) -> Result<(), AudioEngineError> {
        self.send(EngineCommand::Stop)
    }

    pub fn seek(&self, frame: u64) -> Result<(), AudioEngineError> {
        self.send(EngineCommand::Seek(frame))
    }

    pub fn set_loop_enabled(&self, enabled: bool) -> Result<(), AudioEngineError> {
        self.send(EngineCommand::SetLoopEnabled(enabled))
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Acquire)
    }

    pub fn playhead(&self) -> u64 {
        self.shared.playhead.load(Ordering::Acquire)
    }

    pub fn track_meter(&self, index: usize) -> f32 {
        self.shared
            .track_peaks
            .get(index)
            .map(|value| f32::from_bits(value.load(Ordering::Relaxed)))
            .unwrap_or(0.0)
    }

    pub fn master_meter(&self) -> f32 {
        f32::from_bits(self.shared.master_peak.load(Ordering::Relaxed))
    }

    pub fn status(&self) -> &DeviceStatus {
        &self.status
    }

    pub fn monitor_bus(&self) -> MonitorBus {
        self.monitor.clone()
    }

    pub fn take_error(&self) -> Option<String> {
        self.shared.last_error.lock().take()
    }

    pub fn collect_retired_sessions(&self) {
        while self.retired_sessions.pop().is_some() {}
    }

    fn send(&self, command: EngineCommand) -> Result<(), AudioEngineError> {
        self.sender
            .send(command)
            .map_err(|_| AudioEngineError::Disconnected)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.stream.pause();
    }
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    mut thread: AudioThread,
    error_shared: Arc<SharedState>,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    device.build_output_stream::<T, _, _>(
        config,
        move |data, _| fill_output(data, channels, &mut thread),
        move |error: cpal::Error| {
            *error_shared.last_error.lock() = Some(error.to_string());
        },
        None,
    )
}

fn fill_output<T>(data: &mut [T], channels: usize, thread: &mut AudioThread)
where
    T: SizedSample + FromSample<f32>,
{
    thread.begin_buffer();
    for frame in data.chunks_mut(channels.max(1)) {
        write_frame(frame, thread.next_frame(), |sample| {
            T::from_sample(sample.clamp(-1.0, 1.0))
        });
    }
    thread.finish_buffer();
}

fn write_frame<T: Copy>(frame: &mut [T], stereo: [f32; 2], convert: impl Fn(f32) -> T) {
    if frame.len() == 1 {
        frame[0] = convert((stereo[0] + stereo[1]) * 0.5);
        return;
    }
    frame[0] = convert(stereo[0]);
    frame[1] = convert(stereo[1]);
    for sample in &mut frame[2..] {
        *sample = convert(0.0);
    }
}

#[allow(dead_code)]
fn _assert_track_id_send(_: TrackId) {}
