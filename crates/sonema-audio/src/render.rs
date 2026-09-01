use std::sync::Arc;

use anyhow::{Result, anyhow};
use sonema_core::{Clip, Project, TrackId, db_to_gain};
use sonema_dsp::{ChannelStripProcessor, SafetyLimiter};

use crate::{AudioSource, MediaPool};

#[derive(Debug)]
struct RuntimeClip {
    source: Arc<AudioSource>,
    start_frame: f64,
    end_frame: f64,
    source_in: f64,
    source_out: f64,
    source_per_project_frame: f64,
    gain: f32,
    fade_in: f64,
    fade_out: f64,
}

impl RuntimeClip {
    fn compile(clip: &Clip, source: Arc<AudioSource>, project_rate: u32) -> Self {
        let source_frames = clip.source_out.saturating_sub(clip.source_in);
        let duration = source_frames as f64 * project_rate as f64 / source.sample_rate as f64;
        Self {
            source,
            start_frame: clip.start_frame as f64,
            end_frame: clip.start_frame as f64 + duration,
            source_in: clip.source_in as f64,
            source_out: clip.source_out as f64,
            source_per_project_frame: 1.0,
            gain: db_to_gain(clip.gain_db.clamp(-90.0, 24.0)),
            fade_in: clip.fade_in as f64,
            fade_out: clip.fade_out as f64,
        }
        .with_ratio(project_rate)
    }

    fn with_ratio(mut self, project_rate: u32) -> Self {
        self.source_per_project_frame = self.source.sample_rate as f64 / project_rate as f64;
        self
    }

    #[inline]
    fn sample(&self, project_frame: f64) -> Option<[f32; 2]> {
        if project_frame < self.start_frame || project_frame >= self.end_frame {
            return None;
        }
        let timeline_offset = project_frame - self.start_frame;
        let source_position = self.source_in + timeline_offset * self.source_per_project_frame;
        if source_position >= self.source_out {
            return None;
        }

        let mut fade = 1.0_f32;
        if self.fade_in > 0.0 && timeline_offset < self.fade_in {
            fade *= (timeline_offset / self.fade_in).clamp(0.0, 1.0) as f32;
        }
        let remaining = self.end_frame - project_frame;
        if self.fade_out > 0.0 && remaining < self.fade_out {
            fade *= (remaining / self.fade_out).clamp(0.0, 1.0) as f32;
        }
        let gain = self.gain * fade;
        let left = self.source.sample_linear(0, source_position);
        let right = if self.source.channels.len() > 1 {
            self.source.sample_linear(1, source_position)
        } else {
            left
        };
        Some([left * gain, right * gain])
    }
}

#[derive(Debug)]
struct RuntimeTrack {
    id: TrackId,
    clips: Vec<RuntimeClip>,
    processor: ChannelStripProcessor,
    gain: f32,
    pan: f32,
    audible: bool,
    peak: f32,
}

impl RuntimeTrack {
    #[inline]
    fn process(&mut self, project_frame: f64) -> [f32; 2] {
        if !self.audible {
            self.peak *= 0.9995;
            return [0.0, 0.0];
        }
        let mut frame = [0.0_f32; 2];
        for clip in &self.clips {
            if let Some(sample) = clip.sample(project_frame) {
                frame[0] += sample[0];
                frame[1] += sample[1];
            }
        }
        frame = self.processor.process(frame);
        let (left_pan, right_pan) = balance_pan(self.pan);
        frame[0] *= self.gain * left_pan;
        frame[1] *= self.gain * right_pan;
        let instantaneous = frame[0].abs().max(frame[1].abs());
        self.peak = instantaneous.max(self.peak * 0.9995);
        frame
    }
}

/// Fully compiled mutable DSP graph. Construction and allocation happen on the
/// UI/export thread; `process_frame` performs no allocation or locking.
#[derive(Debug)]
pub struct RealtimeSession {
    project_rate: u32,
    output_rate: u32,
    duration_frames: u64,
    tracks: Vec<RuntimeTrack>,
    master_gain: f32,
    limiter: Option<SafetyLimiter>,
    metronome: bool,
    bpm: f64,
    beats_per_bar: u8,
    loop_region: Option<(f64, f64)>,
    master_peak: f32,
}

impl RealtimeSession {
    pub fn compile(
        project: &Project,
        media: &MediaPool,
        output_rate: u32,
        metronome: bool,
    ) -> Result<Self> {
        project.validate().map_err(|error| anyhow!(error))?;
        let any_solo = project.any_soloed();
        let mut tracks = Vec::with_capacity(project.tracks.len());
        for track in &project.tracks {
            let mut clips = Vec::with_capacity(track.clips.len());
            for clip in &track.clips {
                let source = media
                    .get(&clip.media_id)
                    .ok_or_else(|| anyhow!("{} 미디어 PCM이 없습니다", clip.name))?
                    .clone();
                clips.push(RuntimeClip::compile(clip, source, project.sample_rate));
            }
            tracks.push(RuntimeTrack {
                id: track.id,
                clips,
                processor: ChannelStripProcessor::new(output_rate as f32, &track.effects),
                gain: db_to_gain(track.gain_db.clamp(-90.0, 12.0)),
                pan: track.pan.clamp(-1.0, 1.0),
                audible: !track.mute && (!any_solo || track.solo),
                peak: 0.0,
            });
        }
        Ok(Self {
            project_rate: project.sample_rate,
            output_rate,
            duration_frames: project.duration_frames(),
            tracks,
            master_gain: db_to_gain(project.master.gain_db.clamp(-90.0, 12.0)),
            limiter: project
                .master
                .limiter_enabled
                .then(|| SafetyLimiter::new(output_rate as f32, project.master.limiter_ceiling_db)),
            metronome,
            bpm: project.bpm,
            beats_per_bar: project.time_signature.numerator.max(1),
            loop_region: project.loop_region.map(|range| (range.start as f64, range.end as f64)),
            master_peak: 0.0,
        })
    }

    #[inline]
    pub fn process_frame(&mut self, project_frame: f64) -> [f32; 2] {
        let mut output = [0.0_f32; 2];
        for track in &mut self.tracks {
            let frame = track.process(project_frame);
            output[0] += frame[0];
            output[1] += frame[1];
        }
        if self.metronome {
            let click = self.metronome_sample(project_frame);
            output[0] += click;
            output[1] += click;
        }
        output[0] *= self.master_gain;
        output[1] *= self.master_gain;
        if let Some(limiter) = &mut self.limiter {
            output = limiter.process(output);
        }
        let peak = output[0].abs().max(output[1].abs());
        self.master_peak = peak.max(self.master_peak * 0.9995);
        output
    }

    #[inline]
    fn metronome_sample(&self, project_frame: f64) -> f32 {
        let beat_frames = self.project_rate as f64 * 60.0 / self.bpm.clamp(20.0, 400.0);
        let beat_index = (project_frame / beat_frames).floor() as u64;
        let within = project_frame - beat_index as f64 * beat_frames;
        let click_duration = self.project_rate as f64 * 0.025;
        if within >= click_duration {
            return 0.0;
        }
        let accent = beat_index.is_multiple_of(self.beats_per_bar as u64);
        let frequency = if accent { 1_320.0 } else { 880.0 };
        let seconds = within / self.project_rate as f64;
        let envelope = (1.0 - within / click_duration).powi(3) as f32;
        (std::f64::consts::TAU * frequency * seconds).sin() as f32 * envelope * 0.18
    }

    pub fn project_frames_per_output_frame(&self) -> f64 {
        self.project_rate as f64 / self.output_rate as f64
    }

    pub fn duration_frames(&self) -> u64 {
        self.duration_frames
    }

    pub fn loop_region(&self) -> Option<(f64, f64)> {
        self.loop_region
    }

    pub fn track_meters(&self) -> impl Iterator<Item = (TrackId, f32)> + '_ {
        self.tracks.iter().map(|track| (track.id, track.peak))
    }

    pub fn master_meter(&self) -> f32 {
        self.master_peak
    }

    pub fn reset_dsp(&mut self) {
        for track in &mut self.tracks {
            track.processor.reset();
            track.peak = 0.0;
        }
        if let Some(limiter) = &mut self.limiter {
            limiter.reset();
        }
        self.master_peak = 0.0;
    }
}

#[derive(Debug)]
pub struct OfflineMix {
    pub sample_rate: u32,
    pub channels: [Vec<f32>; 2],
    pub peak: f32,
}

pub fn render_offline(
    project: &Project,
    media: &MediaPool,
    sample_rate: u32,
    normalize_to_db: Option<f32>,
) -> Result<OfflineMix> {
    let mut session = RealtimeSession::compile(project, media, sample_rate, false)?;
    let project_duration = project.duration_frames();
    let output_frames = ((project_duration as u128 * sample_rate as u128)
        .div_ceil(project.sample_rate as u128))
        .min(usize::MAX as u128) as usize;
    let mut channels = [vec![0.0_f32; output_frames], vec![0.0_f32; output_frames]];
    let project_step = project.sample_rate as f64 / sample_rate as f64;
    let mut project_frame = 0.0_f64;
    let mut peak = 0.0_f32;
    let [left_channel, right_channel] = &mut channels;
    for (left, right) in left_channel.iter_mut().zip(right_channel.iter_mut()) {
        let frame = session.process_frame(project_frame);
        *left = frame[0];
        *right = frame[1];
        peak = peak.max(frame[0].abs()).max(frame[1].abs());
        project_frame += project_step;
    }
    if let Some(target_db) = normalize_to_db.filter(|_| peak > 1.0e-9) {
        let gain = db_to_gain(target_db.clamp(-12.0, 0.0)) / peak;
        for channel in &mut channels {
            for sample in channel {
                *sample *= gain;
            }
        }
        peak *= gain;
    }
    Ok(OfflineMix { sample_rate, channels, peak })
}

fn balance_pan(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if pan < 0.0 { (1.0, (1.0 + pan).sqrt()) } else { ((1.0 - pan).sqrt(), 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonema_core::{MediaInfo, Project};
    use uuid::Uuid;

    #[test]
    fn offline_render_respects_clip_position() {
        let mut project = Project::new("render", 48_000);
        let media_id = Uuid::new_v4();
        let source = Arc::new(
            AudioSource::new(media_id, "tone", 48_000, vec![vec![0.5; 480]]).unwrap(),
        );
        project
            .register_media(MediaInfo {
                id: media_id,
                name: "tone".into(),
                sample_rate: 48_000,
                channels: 1,
                frames: 480,
                original_path: None,
            })
            .unwrap();
        let track = project.tracks[0].id;
        project.insert_media_clip(track, media_id, 240).unwrap();
        let mut pool = MediaPool::new();
        pool.insert(media_id, source);
        let mix = render_offline(&project, &pool, 48_000, None).unwrap();
        assert_eq!(mix.channels[0][100], 0.0);
        assert!(mix.channels[0][300] > 0.1);
    }
}
