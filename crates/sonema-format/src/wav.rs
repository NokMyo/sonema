use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};
use sonema_audio::OfflineMix;
use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavBitDepth {
    Pcm16,
    Pcm24,
    Float32,
}

#[derive(Debug, Clone, Copy)]
pub struct WavExportOptions {
    pub bit_depth: WavBitDepth,
    pub dither: bool,
}

impl Default for WavExportOptions {
    fn default() -> Self {
        Self { bit_depth: WavBitDepth::Pcm24, dither: true }
    }
}

#[derive(Debug, Error)]
pub enum WavError {
    #[error("WAV 파일을 쓸 수 없습니다: {0}")]
    Io(#[from] std::io::Error),
    #[error("WAV 인코딩에 실패했습니다: {0}")]
    Hound(#[from] hound::Error),
    #[error("좌우 채널 길이가 다릅니다")]
    UnequalChannels,
    #[error("WAV 파일을 최종 위치에 저장하지 못했습니다: {0}")]
    Persist(String),
}

pub fn write_wav(
    path: &Path,
    mix: &OfflineMix,
    options: WavExportOptions,
) -> Result<(), WavError> {
    if mix.channels[0].len() != mix.channels[1].len() {
        return Err(WavError::UnequalChannels);
    }
    let parent = path.parent().filter(|value| !value.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;
    let spec = match options.bit_depth {
        WavBitDepth::Pcm16 => WavSpec {
            channels: 2,
            sample_rate: mix.sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        },
        WavBitDepth::Pcm24 => WavSpec {
            channels: 2,
            sample_rate: mix.sample_rate,
            bits_per_sample: 24,
            sample_format: SampleFormat::Int,
        },
        WavBitDepth::Float32 => WavSpec {
            channels: 2,
            sample_rate: mix.sample_rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
    };
    {
        let mut writer = WavWriter::new(temporary.as_file_mut(), spec)?;
        let mut dither = Dither::new(0x534f_4e45_4d41);
        for (&left, &right) in mix.channels[0].iter().zip(&mix.channels[1]) {
            match options.bit_depth {
                WavBitDepth::Pcm16 => {
                    writer.write_sample(to_pcm16(left, options.dither, &mut dither))?;
                    writer.write_sample(to_pcm16(right, options.dither, &mut dither))?;
                }
                WavBitDepth::Pcm24 => {
                    writer.write_sample(to_pcm24(left, options.dither, &mut dither))?;
                    writer.write_sample(to_pcm24(right, options.dither, &mut dither))?;
                }
                WavBitDepth::Float32 => {
                    writer.write_sample(sanitize(left))?;
                    writer.write_sample(sanitize(right))?;
                }
            }
        }
        writer.finalize()?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| WavError::Persist(error.error.to_string()))?;
    Ok(())
}

fn to_pcm16(sample: f32, with_dither: bool, dither: &mut Dither) -> i16 {
    let noise = if with_dither { dither.tpdf() / 32_768.0 } else { 0.0 };
    (sanitize(sample + noise) * 32_767.0).round() as i16
}

fn to_pcm24(sample: f32, with_dither: bool, dither: &mut Dither) -> i32 {
    let noise = if with_dither { dither.tpdf() / 8_388_608.0 } else { 0.0 };
    (sanitize(sample + noise) * 8_388_607.0).round() as i32
}

fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 }
}

struct Dither(u64);

impl Dither {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn uniform(&mut self) -> f32 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        (value as u32) as f32 / u32::MAX as f32
    }

    fn tpdf(&mut self) -> f32 {
        self.uniform() - self.uniform()
    }
}
