use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use uuid::Uuid;

use crate::AudioSource;

pub fn decode_audio_file(path: &Path) -> Result<AudioSource> {
    let file = Box::new(
        File::open(path).with_context(|| format!("{} 파일을 열 수 없습니다", path.display()))?,
    );
    let stream = MediaSourceStream::new(file, Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("지원하는 오디오 형식을 찾지 못했습니다")?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("오디오 트랙이 없는 파일입니다"))?;
    let codec_parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or_else(|| anyhow!("오디오 코덱 정보가 없습니다"))?
        .clone();
    let sample_rate = codec_parameters
        .sample_rate
        .ok_or_else(|| anyhow!("샘플레이트를 확인할 수 없습니다"))?;
    let channel_count = codec_parameters
        .channels
        .map(|channels| channels.count())
        .ok_or_else(|| anyhow!("채널 구성을 확인할 수 없습니다"))?;
    if channel_count == 0 || channel_count > 64 {
        return Err(anyhow!("지원하지 않는 채널 수입니다: {channel_count}"));
    }

    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&codec_parameters, &AudioDecoderOptions::default())
        .context("오디오 디코더를 만들 수 없습니다")?;
    let mut channels = (0..channel_count).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut packet_samples = Vec::<f32>::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => {
                return Err(anyhow!("중간에 오디오 형식이 바뀌는 파일은 지원하지 않습니다"));
            }
            Err(error) => return Err(error).context("오디오 패킷을 읽지 못했습니다"),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(error).context("오디오 데이터를 해석하지 못했습니다"),
        };
        packet_samples.resize(decoded.samples_interleaved(), f32::MID);
        decoded.copy_to_slice_interleaved(&mut packet_samples);
        for frame in packet_samples.chunks_exact(channel_count) {
            for (channel, &sample) in channels.iter_mut().zip(frame) {
                channel.push(if sample.is_finite() { sample } else { 0.0 });
            }
        }
    }

    let name = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("가져온 오디오")
        .to_owned();
    AudioSource::new(Uuid::new_v4(), name, sample_rate, channels).map_err(Into::into)
}
