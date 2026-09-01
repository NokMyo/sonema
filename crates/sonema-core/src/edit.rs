use thiserror::Error;
use uuid::Uuid;

use crate::{
    Clip, ClipId, MediaId, MediaInfo, Project, TrackId, scale_frames,
};

const MIN_CLIP_TIMELINE_FRAMES: u64 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapMode {
    Off,
    BeatDivision(u8),
}

impl Default for SnapMode {
    fn default() -> Self {
        Self::BeatDivision(4)
    }
}

impl SnapMode {
    pub fn snap(self, project: &Project, frame: u64) -> u64 {
        match self {
            Self::Off => frame,
            Self::BeatDivision(divisions) => {
                let grid = (project.beat_frames() / divisions.max(1) as f64).max(1.0);
                ((frame as f64 / grid).round() * grid).max(0.0) as u64
            }
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EditError {
    #[error("트랙을 찾을 수 없습니다")]
    TrackNotFound,
    #[error("클립을 찾을 수 없습니다")]
    ClipNotFound,
    #[error("미디어를 찾을 수 없습니다")]
    MediaNotFound,
    #[error("클립 경계 안에서만 나눌 수 있습니다")]
    SplitOutsideClip,
    #[error("클립 길이가 너무 짧습니다")]
    ClipTooShort,
    #[error("미디어 정보가 올바르지 않습니다")]
    InvalidMedia,
}

impl Project {
    pub fn register_media(&mut self, info: MediaInfo) -> Result<MediaId, EditError> {
        if info.sample_rate < 8_000
            || info.sample_rate > 768_000
            || info.channels == 0
            || info.channels > 64
            || info.frames == 0
        {
            return Err(EditError::InvalidMedia);
        }
        let id = info.id;
        self.media.insert(id, info);
        self.touch();
        Ok(id)
    }

    pub fn insert_media_clip(
        &mut self,
        track_id: TrackId,
        media_id: MediaId,
        start_frame: u64,
    ) -> Result<ClipId, EditError> {
        let media = self.media.get(&media_id).ok_or(EditError::MediaNotFound)?.clone();
        let duration = scale_frames(media.frames, media.sample_rate, self.sample_rate).max(1);
        let mut clip = Clip::new(media_id, media.name, start_frame, media.frames);
        let default_fade = 64.min(duration / 2);
        clip.fade_in = default_fade;
        clip.fade_out = default_fade;
        let id = clip.id;
        let track = self.track_mut(track_id).ok_or(EditError::TrackNotFound)?;
        track.clips.push(clip);
        sort_clips(track);
        self.touch();
        Ok(id)
    }

    pub fn remove_track(&mut self, track_id: TrackId) -> Result<(), EditError> {
        let index = self
            .tracks
            .iter()
            .position(|track| track.id == track_id)
            .ok_or(EditError::TrackNotFound)?;
        self.tracks.remove(index);
        self.touch();
        Ok(())
    }

    pub fn remove_clip(&mut self, clip_id: ClipId) -> Result<Clip, EditError> {
        let (track_index, clip_index) = self.clip_location(clip_id).ok_or(EditError::ClipNotFound)?;
        let clip = self.tracks[track_index].clips.remove(clip_index);
        self.touch();
        Ok(clip)
    }

    pub fn duplicate_clip(&mut self, clip_id: ClipId) -> Result<ClipId, EditError> {
        let (track_index, clip_index) = self.clip_location(clip_id).ok_or(EditError::ClipNotFound)?;
        let mut copy = self.tracks[track_index].clips[clip_index].clone();
        let duration = self.clip_duration_frames(&copy).ok_or(EditError::MediaNotFound)?;
        copy.id = Uuid::new_v4();
        copy.start_frame = copy.start_frame.saturating_add(duration);
        let id = copy.id;
        self.tracks[track_index].clips.push(copy);
        sort_clips(&mut self.tracks[track_index]);
        self.touch();
        Ok(id)
    }

    pub fn move_clip(
        &mut self,
        clip_id: ClipId,
        target_track: TrackId,
        start_frame: u64,
    ) -> Result<(), EditError> {
        let (source_track, clip_index) = self.clip_location(clip_id).ok_or(EditError::ClipNotFound)?;
        let target_index = self
            .tracks
            .iter()
            .position(|track| track.id == target_track)
            .ok_or(EditError::TrackNotFound)?;
        let mut clip = self.tracks[source_track].clips.remove(clip_index);
        clip.start_frame = start_frame;
        self.tracks[target_index].clips.push(clip);
        sort_clips(&mut self.tracks[target_index]);
        if source_track != target_index {
            sort_clips(&mut self.tracks[source_track]);
        }
        self.touch();
        Ok(())
    }

    pub fn split_clip(&mut self, clip_id: ClipId, at_frame: u64) -> Result<ClipId, EditError> {
        let (track_index, clip_index) = self.clip_location(clip_id).ok_or(EditError::ClipNotFound)?;
        let original = self.tracks[track_index].clips[clip_index].clone();
        let media = self.media.get(&original.media_id).ok_or(EditError::MediaNotFound)?;
        let end = self.clip_end_frame(&original).ok_or(EditError::MediaNotFound)?;
        if at_frame <= original.start_frame.saturating_add(MIN_CLIP_TIMELINE_FRAMES)
            || at_frame.saturating_add(MIN_CLIP_TIMELINE_FRAMES) >= end
        {
            return Err(EditError::SplitOutsideClip);
        }

        let timeline_delta = at_frame - original.start_frame;
        let source_delta = scale_frames(timeline_delta, self.sample_rate, media.sample_rate);
        let source_split = original
            .source_in
            .saturating_add(source_delta)
            .clamp(original.source_in + 1, original.source_out - 1);

        let mut left = original.clone();
        left.source_out = source_split;
        left.fade_out = left.fade_out.min(at_frame - left.start_frame);

        let mut right = original;
        right.id = Uuid::new_v4();
        right.start_frame = at_frame;
        right.source_in = source_split;
        right.fade_in = right.fade_in.min(end - at_frame);
        let right_id = right.id;

        self.tracks[track_index].clips[clip_index] = left;
        self.tracks[track_index].clips.insert(clip_index + 1, right);
        self.touch();
        Ok(right_id)
    }

    pub fn trim_clip_start(&mut self, clip_id: ClipId, new_start: u64) -> Result<(), EditError> {
        let (track_index, clip_index) = self.clip_location(clip_id).ok_or(EditError::ClipNotFound)?;
        let original = self.tracks[track_index].clips[clip_index].clone();
        let media = self.media.get(&original.media_id).ok_or(EditError::MediaNotFound)?;
        let end = self.clip_end_frame(&original).ok_or(EditError::MediaNotFound)?;
        let available_before = scale_frames(
            original.source_in,
            media.sample_rate,
            self.sample_rate,
        );
        let new_start = new_start.max(original.start_frame.saturating_sub(available_before));
        if new_start.saturating_add(MIN_CLIP_TIMELINE_FRAMES) >= end {
            return Err(EditError::ClipTooShort);
        }

        let timeline_delta = new_start as i128 - original.start_frame as i128;
        let source_delta = (timeline_delta as f64 * media.sample_rate as f64 / self.sample_rate as f64)
            .round() as i128;
        let source_in = (original.source_in as i128 + source_delta)
            .clamp(0, original.source_out.saturating_sub(1) as i128) as u64;

        let clip = &mut self.tracks[track_index].clips[clip_index];
        clip.start_frame = new_start;
        clip.source_in = source_in;
        let duration = end - new_start;
        clip.fade_in = clip.fade_in.min(duration);
        clip.fade_out = clip.fade_out.min(duration);
        self.touch();
        Ok(())
    }

    pub fn trim_clip_end(&mut self, clip_id: ClipId, new_end: u64) -> Result<(), EditError> {
        let (track_index, clip_index) = self.clip_location(clip_id).ok_or(EditError::ClipNotFound)?;
        let original = self.tracks[track_index].clips[clip_index].clone();
        if new_end <= original.start_frame.saturating_add(MIN_CLIP_TIMELINE_FRAMES) {
            return Err(EditError::ClipTooShort);
        }
        let media = self.media.get(&original.media_id).ok_or(EditError::MediaNotFound)?;
        let timeline_duration = new_end - original.start_frame;
        let source_duration = scale_frames(timeline_duration, self.sample_rate, media.sample_rate);
        let source_out = original
            .source_in
            .saturating_add(source_duration)
            .clamp(original.source_in + 1, media.frames);

        let clip = &mut self.tracks[track_index].clips[clip_index];
        clip.source_out = source_out;
        clip.fade_in = clip.fade_in.min(timeline_duration);
        clip.fade_out = clip.fade_out.min(timeline_duration);
        self.touch();
        Ok(())
    }

    pub fn remove_unreferenced_media(&mut self) -> usize {
        let referenced: std::collections::BTreeSet<_> = self
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(|clip| clip.media_id))
            .collect();
        let before = self.media.len();
        self.media.retain(|id, _| referenced.contains(id));
        before - self.media.len()
    }
}

fn sort_clips(track: &mut crate::Track) {
    track.clips.sort_by_key(|clip| (clip.start_frame, clip.id));
}
