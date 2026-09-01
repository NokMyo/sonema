//! Realtime-safe audio backend, decoding, recording, and deterministic mixdown.

mod decode;
mod engine;
mod recorder;
mod render;
mod source;

pub use decode::decode_audio_file;
pub use engine::{AudioEngine, AudioEngineError, DeviceStatus, MAX_METER_TRACKS, MonitorBus};
pub use recorder::{InputDevice, RecordedAudio, Recorder, RecorderError, list_input_devices};
pub use render::{OfflineMix, RealtimeSession, render_offline};
pub use source::{AudioSource, MediaPool, SourceError};
