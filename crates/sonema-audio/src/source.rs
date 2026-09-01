use std::collections::BTreeMap;
use std::sync::Arc;

use sonema_core::{MediaId, MediaInfo};
use thiserror::Error;
use uuid::Uuid;

pub type MediaPool = BTreeMap<MediaId, Arc<AudioSource>>;

#[derive(Debug, Error)]
pub enum SourceError {
    #[error("오디오에 채널이 없습니다")]
    NoChannels,
    #[error("오디오 채널 길이가 서로 다릅니다")]
    UnequalChannelLengths,
    #[error("오디오 샘플레이트가 올바르지 않습니다")]
    InvalidSampleRate,
    #[error("오디오 파일이 비어 있습니다")]
    Empty,
}

#[derive(Debug)]
pub struct AudioSource {
    pub id: MediaId,
    pub name: String,
    pub sample_rate: u32,
    /// Planar normalized PCM in the range approximately -1.0..1.0.
    pub channels: Vec<Vec<f32>>,
    /// Fixed-resolution overview used for waveform painting.
    pub peaks: Vec<(f32, f32)>,
}

impl AudioSource {
    pub fn new(
        id: MediaId,
        name: impl Into<String>,
        sample_rate: u32,
        channels: Vec<Vec<f32>>,
    ) -> Result<Self, SourceError> {
        if !(8_000..=768_000).contains(&sample_rate) {
            return Err(SourceError::InvalidSampleRate);
        }
        let Some(frames) = channels.first().map(Vec::len) else {
            return Err(SourceError::NoChannels);
        };
        if frames == 0 {
            return Err(SourceError::Empty);
        }
        if channels.len() > 64 || channels.iter().any(|channel| channel.len() != frames) {
            return Err(SourceError::UnequalChannelLengths);
        }
        let peaks = calculate_peaks(&channels, 4_096);
        Ok(Self { id, name: name.into(), sample_rate, channels, peaks })
    }

    pub fn from_recording(
        name: impl Into<String>,
        sample_rate: u32,
        channels: Vec<Vec<f32>>,
    ) -> Result<Self, SourceError> {
        Self::new(Uuid::new_v4(), name, sample_rate, channels)
    }

    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn info(&self, original_path: Option<String>) -> MediaInfo {
        MediaInfo {
            id: self.id,
            name: self.name.clone(),
            sample_rate: self.sample_rate,
            channels: self.channels.len().min(u16::MAX as usize) as u16,
            frames: self.frames() as u64,
            original_path,
        }
    }

    #[inline]
    pub fn sample_linear(&self, channel: usize, position: f64) -> f32 {
        let Some(samples) = self.channels.get(channel) else { return 0.0 };
        if position < 0.0 || position >= samples.len() as f64 {
            return 0.0;
        }
        let index = position as usize;
        let fraction = (position - index as f64) as f32;
        let current = samples[index];
        let next = samples.get(index + 1).copied().unwrap_or(current);
        current + (next - current) * fraction
    }
}

fn calculate_peaks(channels: &[Vec<f32>], bins: usize) -> Vec<(f32, f32)> {
    let frames = channels[0].len();
    let bins = bins.min(frames).max(1);
    let frames_per_bin = frames.div_ceil(bins);
    (0..bins)
        .map(|bin| {
            let start = bin * frames_per_bin;
            let end = ((bin + 1) * frames_per_bin).min(frames);
            let mut min = 0.0_f32;
            let mut max = 0.0_f32;
            for channel in channels {
                for &sample in &channel[start..end] {
                    min = min.min(sample);
                    max = max.max(sample);
                }
            }
            (min, max)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_sampling_interpolates() {
        let source = AudioSource::new(
            Uuid::new_v4(),
            "test",
            48_000,
            vec![vec![0.0, 1.0, 0.0]],
        )
        .unwrap();
        assert!((source.sample_linear(0, 0.5) - 0.5).abs() < 1.0e-6);
        assert!((source.sample_linear(0, 1.5) - 0.5).abs() < 1.0e-6);
    }
}
