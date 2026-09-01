use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROJECT_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 400.0;

pub type TrackId = Uuid;
pub type ClipId = Uuid;
pub type MediaId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub format_version: u32,
    pub id: Uuid,
    pub name: String,
    pub sample_rate: u32,
    pub bpm: f64,
    pub time_signature: TimeSignature,
    pub tracks: Vec<Track>,
    pub media: BTreeMap<MediaId, MediaInfo>,
    pub master: MasterSettings,
    pub loop_region: Option<FrameRange>,
    pub created_unix_ms: u64,
    pub modified_unix_ms: u64,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl Project {
    pub fn new(name: impl Into<String>, sample_rate: u32) -> Self {
        let now = unix_ms();
        let mut project = Self {
            format_version: PROJECT_FORMAT_VERSION,
            id: Uuid::new_v4(),
            name: clean_name(name.into(), "제목 없는 프로젝트"),
            sample_rate: sample_rate.clamp(8_000, 384_000),
            bpm: 120.0,
            time_signature: TimeSignature::default(),
            tracks: Vec::new(),
            media: BTreeMap::new(),
            master: MasterSettings::default(),
            loop_region: None,
            created_unix_ms: now,
            modified_unix_ms: now,
            extensions: BTreeMap::new(),
        };
        project.add_track("오디오 1");
        project
    }

    pub fn touch(&mut self) {
        self.modified_unix_ms = unix_ms();
    }

    pub fn add_track(&mut self, name: impl Into<String>) -> TrackId {
        let id = Uuid::new_v4();
        let color = TrackColor::palette(self.tracks.len());
        self.tracks.push(Track::new(id, name, color));
        self.touch();
        id
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|track| track.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|track| track.id == id)
    }

    pub fn media_info(&self, id: MediaId) -> Option<&MediaInfo> {
        self.media.get(&id)
    }

    pub fn clip(&self, id: ClipId) -> Option<(&Track, &Clip)> {
        self.tracks
            .iter()
            .find_map(|track| track.clips.iter().find(|clip| clip.id == id).map(|clip| (track, clip)))
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.tracks
            .iter_mut()
            .find_map(|track| track.clips.iter_mut().find(|clip| clip.id == id))
    }

    pub fn clip_location(&self, id: ClipId) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(track_index, track)| {
            track
                .clips
                .iter()
                .position(|clip| clip.id == id)
                .map(|clip_index| (track_index, clip_index))
        })
    }

    pub fn duration_frames(&self) -> u64 {
        self.tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter_map(|clip| self.clip_end_frame(clip))
            .max()
            .unwrap_or(self.sample_rate as u64 * 30)
    }

    pub fn clip_duration_frames(&self, clip: &Clip) -> Option<u64> {
        let media = self.media.get(&clip.media_id)?;
        let source_frames = clip.source_out.saturating_sub(clip.source_in);
        Some(scale_frames(source_frames, media.sample_rate, self.sample_rate).max(1))
    }

    pub fn clip_end_frame(&self, clip: &Clip) -> Option<u64> {
        Some(clip.start_frame.saturating_add(self.clip_duration_frames(clip)?))
    }

    pub fn seconds_to_frames(&self, seconds: f64) -> u64 {
        seconds.max(0.0).mul_add(self.sample_rate as f64, 0.5) as u64
    }

    pub fn frames_to_seconds(&self, frames: u64) -> f64 {
        frames as f64 / self.sample_rate as f64
    }

    pub fn beat_frames(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / self.bpm.clamp(MIN_BPM, MAX_BPM)
    }

    pub fn any_soloed(&self) -> bool {
        self.tracks.iter().any(|track| track.solo)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.format_version == 0 || self.format_version > PROJECT_FORMAT_VERSION {
            return Err(format!("지원하지 않는 프로젝트 형식 {}", self.format_version));
        }
        if !(8_000..=384_000).contains(&self.sample_rate) {
            return Err("프로젝트 샘플레이트가 올바르지 않습니다".into());
        }
        if !(MIN_BPM..=MAX_BPM).contains(&self.bpm) {
            return Err("BPM이 지원 범위를 벗어났습니다".into());
        }
        if self.tracks.is_empty() || self.tracks.len() > 1_024 || self.media.len() > 65_536 {
            return Err("프로젝트 항목 수가 안전 한도를 넘었습니다".into());
        }
        if !(1..=32).contains(&self.time_signature.numerator)
            || !(1..=32).contains(&self.time_signature.denominator)
            || !self.time_signature.denominator.is_power_of_two()
        {
            return Err("박자표가 올바르지 않습니다".into());
        }
        if !finite_in(self.master.gain_db, -90.0, 12.0)
            || !finite_in(self.master.limiter_ceiling_db, -12.0, 0.0)
        {
            return Err("마스터 설정이 올바르지 않습니다".into());
        }
        for (id, media) in &self.media {
            if *id != media.id
                || id.is_nil()
                || !(8_000..=768_000).contains(&media.sample_rate)
                || media.channels == 0
                || media.channels > 64
                || media.frames == 0
            {
                return Err(format!("{} 미디어 정보가 올바르지 않습니다", media.name));
            }
        }
        let mut track_ids = std::collections::BTreeSet::new();
        let mut clip_ids = std::collections::BTreeSet::new();
        for track in &self.tracks {
            if track.id.is_nil() || !track_ids.insert(track.id) {
                return Err("트랙 식별자가 중복되었거나 올바르지 않습니다".into());
            }
            if !finite_in(track.gain_db, -90.0, 12.0)
                || !finite_in(track.pan, -1.0, 1.0)
                || !channel_strip_is_valid(&track.effects)
            {
                return Err(format!("{} 트랙 설정이 올바르지 않습니다", track.name));
            }
            if track.clips.len() > 100_000 {
                return Err(format!("{} 트랙에 클립이 너무 많습니다", track.name));
            }
            for clip in &track.clips {
                if clip.id.is_nil() || !clip_ids.insert(clip.id) {
                    return Err("클립 식별자가 중복되었거나 올바르지 않습니다".into());
                }
                let media = self
                    .media
                    .get(&clip.media_id)
                    .ok_or_else(|| format!("{} 클립의 미디어가 없습니다", clip.name))?;
                if clip.source_in >= clip.source_out || clip.source_out > media.frames {
                    return Err(format!("{} 클립의 소스 범위가 올바르지 않습니다", clip.name));
                }
                let duration = self
                    .clip_duration_frames(clip)
                    .ok_or_else(|| format!("{} 클립 길이를 계산할 수 없습니다", clip.name))?;
                if !finite_in(clip.gain_db, -90.0, 24.0)
                    || clip.fade_in > duration
                    || clip.fade_out > duration
                {
                    return Err(format!("{} 클립 설정이 올바르지 않습니다", clip.name));
                }
            }
        }
        if let Some(range) = self.loop_region
            && (range.is_empty() || range.end > self.duration_frames())
        {
            return Err("루프 구간이 올바르지 않습니다".into());
        }
        Ok(())
    }
}

fn finite_in(value: f32, minimum: f32, maximum: f32) -> bool {
    value.is_finite() && (minimum..=maximum).contains(&value)
}

fn channel_strip_is_valid(strip: &ChannelStrip) -> bool {
    finite_in(strip.high_pass.frequency_hz, 10.0, 2_000.0)
        && eq_band_is_valid(&strip.low_eq)
        && eq_band_is_valid(&strip.mid_eq)
        && eq_band_is_valid(&strip.high_eq)
        && finite_in(strip.compressor.threshold_db, -72.0, 0.0)
        && finite_in(strip.compressor.ratio, 1.0, 30.0)
        && finite_in(strip.compressor.attack_ms, 0.05, 500.0)
        && finite_in(strip.compressor.release_ms, 2.0, 5_000.0)
        && finite_in(strip.compressor.makeup_db, -12.0, 24.0)
}

fn eq_band_is_valid(band: &EqBandSettings) -> bool {
    finite_in(band.frequency_hz, 10.0, 192_000.0)
        && finite_in(band.gain_db, -24.0, 24.0)
        && finite_in(band.q, 0.1, 18.0)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self { numerator: 4, denominator: 4 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameRange {
    pub start: u64,
    pub end: u64,
}

impl FrameRange {
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (start < end).then_some(Self { start, end })
    }

    pub fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub color: TrackColor,
    pub gain_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub armed: bool,
    pub monitor: bool,
    pub effects: ChannelStrip,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(id: TrackId, name: impl Into<String>, color: TrackColor) -> Self {
        Self {
            id,
            name: clean_name(name.into(), "오디오"),
            color,
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            armed: false,
            monitor: false,
            effects: ChannelStrip::default(),
            clips: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackColor(pub u8, pub u8, pub u8);

impl TrackColor {
    pub fn palette(index: usize) -> Self {
        const COLORS: [TrackColor; 10] = [
            TrackColor(66, 197, 156),
            TrackColor(84, 151, 232),
            TrackColor(156, 112, 222),
            TrackColor(229, 118, 144),
            TrackColor(231, 157, 77),
            TrackColor(197, 198, 85),
            TrackColor(69, 183, 204),
            TrackColor(119, 137, 232),
            TrackColor(211, 105, 196),
            TrackColor(109, 190, 105),
        ];
        COLORS[index % COLORS.len()]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Clip {
    pub id: ClipId,
    pub media_id: MediaId,
    pub name: String,
    /// Position on the project timeline in project-rate frames.
    pub start_frame: u64,
    /// Non-destructive in/out points in source-rate frames.
    pub source_in: u64,
    pub source_out: u64,
    pub gain_db: f32,
    /// Fade lengths in project-rate frames.
    pub fade_in: u64,
    pub fade_out: u64,
}

impl Clip {
    pub fn new(media_id: MediaId, name: impl Into<String>, start_frame: u64, source_frames: u64) -> Self {
        Self {
            id: Uuid::new_v4(),
            media_id,
            name: clean_name(name.into(), "오디오 클립"),
            start_frame,
            source_in: 0,
            source_out: source_frames.max(1),
            gain_db: 0.0,
            fade_in: 0,
            fade_out: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaInfo {
    pub id: MediaId,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    #[serde(default)]
    pub original_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MasterSettings {
    pub gain_db: f32,
    pub limiter_enabled: bool,
    pub limiter_ceiling_db: f32,
}

impl Default for MasterSettings {
    fn default() -> Self {
        Self { gain_db: 0.0, limiter_enabled: true, limiter_ceiling_db: -0.3 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelStrip {
    pub high_pass: HighPassSettings,
    pub low_eq: EqBandSettings,
    pub mid_eq: EqBandSettings,
    pub high_eq: EqBandSettings,
    pub compressor: CompressorSettings,
}

impl Default for ChannelStrip {
    fn default() -> Self {
        Self {
            high_pass: HighPassSettings::default(),
            low_eq: EqBandSettings::low_shelf(120.0),
            mid_eq: EqBandSettings::peak(1_500.0),
            high_eq: EqBandSettings::high_shelf(8_000.0),
            compressor: CompressorSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HighPassSettings {
    pub enabled: bool,
    pub frequency_hz: f32,
}

impl Default for HighPassSettings {
    fn default() -> Self {
        Self { enabled: false, frequency_hz: 80.0 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EqKind {
    LowShelf,
    Peak,
    HighShelf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqBandSettings {
    pub enabled: bool,
    pub kind: EqKind,
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl EqBandSettings {
    pub fn low_shelf(frequency_hz: f32) -> Self {
        Self { enabled: false, kind: EqKind::LowShelf, frequency_hz, gain_db: 0.0, q: 0.707 }
    }

    pub fn peak(frequency_hz: f32) -> Self {
        Self { enabled: false, kind: EqKind::Peak, frequency_hz, gain_db: 0.0, q: 1.0 }
    }

    pub fn high_shelf(frequency_hz: f32) -> Self {
        Self { enabled: false, kind: EqKind::HighShelf, frequency_hz, gain_db: 0.0, q: 0.707 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompressorSettings {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

impl Default for CompressorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 12.0,
            release_ms: 120.0,
            makeup_db: 0.0,
        }
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    if db <= -90.0 { 0.0 } else { 10.0_f32.powf(db / 20.0) }
}

pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(0.000_001).log10()
}

pub fn scale_frames(frames: u64, from_rate: u32, to_rate: u32) -> u64 {
    if from_rate == 0 {
        return 0;
    }
    ((frames as u128 * to_rate as u128 + (from_rate as u128 / 2)) / from_rate as u128)
        .min(u64::MAX as u128) as u64
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn clean_name(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() { fallback.into() } else { trimmed.chars().take(160).collect() }
}
