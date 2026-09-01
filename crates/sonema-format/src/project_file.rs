use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sonema_audio::{AudioSource, MediaPool, SourceError};
use sonema_core::{MediaId, Project};
use tempfile::NamedTempFile;
use thiserror::Error;

const MAGIC: &[u8; 8] = b"SONEMA01";
const CONTAINER_VERSION: u32 = 1;
const HEADER_SIZE: u64 = 40;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("프로젝트 파일을 읽거나 쓸 수 없습니다: {0}")]
    Io(#[from] std::io::Error),
    #[error("프로젝트 정보가 손상되었습니다: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Sonema 프로젝트 파일이 아닙니다")]
    InvalidMagic,
    #[error("지원하지 않는 Sonema 컨테이너 버전입니다: {0}")]
    UnsupportedVersion(u32),
    #[error("프로젝트 파일 크기 정보가 올바르지 않습니다")]
    InvalidLength,
    #[error("프로젝트 파일 무결성 검사에 실패했습니다")]
    ChecksumMismatch,
    #[error("프로젝트가 올바르지 않습니다: {0}")]
    InvalidProject(String),
    #[error("프로젝트 미디어가 없습니다: {0}")]
    MissingMedia(String),
    #[error("프로젝트 오디오가 올바르지 않습니다: {0}")]
    Source(#[from] SourceError),
    #[error("임시 프로젝트 파일을 저장하지 못했습니다: {0}")]
    Persist(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    project: Project,
    sources: Vec<SourceDescriptor>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SourceDescriptor {
    id: MediaId,
    name: String,
    sample_rate: u32,
    channels: u16,
    frames: u64,
    byte_offset: u64,
    byte_len: u64,
}

pub fn save_project(path: &Path, project: &Project, media: &MediaPool) -> Result<(), FormatError> {
    project.validate().map_err(FormatError::InvalidProject)?;
    let mut offset = 0_u64;
    let mut sources = Vec::with_capacity(project.media.len());
    for (id, info) in &project.media {
        let source = media
            .get(id)
            .ok_or_else(|| FormatError::MissingMedia(info.name.clone()))?;
        if source.sample_rate != info.sample_rate
            || source.channels.len() != info.channels as usize
            || source.frames() as u64 != info.frames
        {
            return Err(FormatError::InvalidProject(format!(
                "{} 미디어 정보와 PCM 데이터가 일치하지 않습니다",
                info.name
            )));
        }
        let byte_len = info
            .frames
            .checked_mul(info.channels as u64)
            .and_then(|samples| samples.checked_mul(4))
            .ok_or(FormatError::InvalidLength)?;
        sources.push(SourceDescriptor {
            id: *id,
            name: info.name.clone(),
            sample_rate: info.sample_rate,
            channels: info.channels,
            frames: info.frames,
            byte_offset: offset,
            byte_len,
        });
        offset = offset.checked_add(byte_len).ok_or(FormatError::InvalidLength)?;
    }

    let manifest = Manifest { project: project.clone(), sources };
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(FormatError::InvalidLength);
    }
    let parent = path.parent().filter(|value| !value.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        write_header(&mut writer, manifest_bytes.len() as u64, offset, 0)?;
        writer.write_all(&manifest_bytes)?;
        let mut checksum = Fnv64::new();
        checksum.update(&manifest_bytes);
        let mut bytes = Vec::with_capacity(64 * 1024);
        for descriptor in &manifest.sources {
            let source = &media[&descriptor.id];
            for channel in &source.channels {
                for &sample in channel {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                    if bytes.len() >= 64 * 1024 {
                        checksum.update(&bytes);
                        writer.write_all(&bytes)?;
                        bytes.clear();
                    }
                }
            }
        }
        if !bytes.is_empty() {
            checksum.update(&bytes);
            writer.write_all(&bytes)?;
        }
        writer.flush()?;
        writer.seek(SeekFrom::Start(32))?;
        writer.write_all(&checksum.finish().to_le_bytes())?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| FormatError::Persist(error.error.to_string()))?;
    Ok(())
}

pub fn load_project(path: &Path) -> Result<(Project, MediaPool), FormatError> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let (manifest_len, audio_len, expected_checksum) = read_header(&mut reader)?;
    if manifest_len > MAX_MANIFEST_BYTES
        || HEADER_SIZE
            .checked_add(manifest_len)
            .and_then(|value| value.checked_add(audio_len))
            != Some(file_len)
    {
        return Err(FormatError::InvalidLength);
    }
    let manifest_size = usize::try_from(manifest_len).map_err(|_| FormatError::InvalidLength)?;
    let mut manifest_bytes = vec![0_u8; manifest_size];
    reader.read_exact(&mut manifest_bytes)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    manifest.project.validate().map_err(FormatError::InvalidProject)?;

    let mut checksum = Fnv64::new();
    checksum.update(&manifest_bytes);
    let audio_start = HEADER_SIZE + manifest_len;
    let mut pool = MediaPool::new();
    let mut scratch = [0_u8; 4];
    for descriptor in &manifest.sources {
        let expected_len = descriptor
            .frames
            .checked_mul(descriptor.channels as u64)
            .and_then(|samples| samples.checked_mul(4))
            .ok_or(FormatError::InvalidLength)?;
        if descriptor.byte_len != expected_len
            || descriptor.byte_offset.checked_add(descriptor.byte_len).is_none()
            || descriptor.byte_offset + descriptor.byte_len > audio_len
            || descriptor.channels == 0
            || descriptor.channels > 64
        {
            return Err(FormatError::InvalidLength);
        }
        reader.seek(SeekFrom::Start(audio_start + descriptor.byte_offset))?;
        let frames = usize::try_from(descriptor.frames).map_err(|_| FormatError::InvalidLength)?;
        let mut channels = Vec::with_capacity(descriptor.channels as usize);
        for _ in 0..descriptor.channels {
            let mut channel = Vec::with_capacity(frames);
            for _ in 0..frames {
                reader.read_exact(&mut scratch)?;
                checksum.update(&scratch);
                let value = f32::from_le_bytes(scratch);
                channel.push(if value.is_finite() { value } else { 0.0 });
            }
            channels.push(channel);
        }
        let source = AudioSource::new(
            descriptor.id,
            descriptor.name.clone(),
            descriptor.sample_rate,
            channels,
        )?;
        pool.insert(descriptor.id, Arc::new(source));
    }
    if checksum.finish() != expected_checksum {
        return Err(FormatError::ChecksumMismatch);
    }
    for (id, info) in &manifest.project.media {
        let source = pool.get(id).ok_or_else(|| FormatError::MissingMedia(info.name.clone()))?;
        if source.frames() as u64 != info.frames
            || source.sample_rate != info.sample_rate
            || source.channels.len() != info.channels as usize
        {
            return Err(FormatError::InvalidProject(format!("{} 미디어가 일치하지 않습니다", info.name)));
        }
    }
    Ok((manifest.project, pool))
}

fn write_header(
    writer: &mut impl Write,
    manifest_len: u64,
    audio_len: u64,
    checksum: u64,
) -> std::io::Result<()> {
    writer.write_all(MAGIC)?;
    writer.write_all(&CONTAINER_VERSION.to_le_bytes())?;
    writer.write_all(&0_u32.to_le_bytes())?;
    writer.write_all(&manifest_len.to_le_bytes())?;
    writer.write_all(&audio_len.to_le_bytes())?;
    writer.write_all(&checksum.to_le_bytes())?;
    Ok(())
}

fn read_header(reader: &mut impl Read) -> Result<(u64, u64, u64), FormatError> {
    let mut magic = [0_u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(FormatError::InvalidMagic);
    }
    let version = read_u32(reader)?;
    if version != CONTAINER_VERSION {
        return Err(FormatError::UnsupportedVersion(version));
    }
    let _reserved = read_u32(reader)?;
    Ok((read_u64(reader)?, read_u64(reader)?, read_u64(reader)?))
}

fn read_u32(reader: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> std::io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

struct Fnv64(u64);

impl Fnv64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= byte as u64;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonema_core::MediaInfo;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn project_round_trip_preserves_pcm() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("roundtrip.sonema");
        let mut project = Project::new("Roundtrip", 48_000);
        let id = Uuid::new_v4();
        let source = Arc::new(
            AudioSource::new(id, "voice", 48_000, vec![vec![0.0, 0.25, -0.25, 1.0]])
                .unwrap(),
        );
        project
            .register_media(MediaInfo {
                id,
                name: "voice".into(),
                sample_rate: 48_000,
                channels: 1,
                frames: 4,
                original_path: None,
            })
            .unwrap();
        let track = project.tracks[0].id;
        project.insert_media_clip(track, id, 12).unwrap();
        let mut pool = MediaPool::new();
        pool.insert(id, source);
        save_project(&path, &project, &pool).unwrap();
        let (loaded, media) = load_project(&path).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(media[&id].channels[0], vec![0.0, 0.25, -0.25, 1.0]);
    }
}
