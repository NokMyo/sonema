use sonema_core::{History, MediaInfo, Project, SnapMode};
use uuid::Uuid;

fn project_with_clip() -> (Project, Uuid, Uuid) {
    let mut project = Project::new("테스트", 48_000);
    let track = project.tracks[0].id;
    let media_id = Uuid::new_v4();
    project
        .register_media(MediaInfo {
            id: media_id,
            name: "voice.wav".into(),
            sample_rate: 48_000,
            channels: 1,
            frames: 96_000,
            original_path: None,
        })
        .unwrap();
    let clip = project.insert_media_clip(track, media_id, 48_000).unwrap();
    (project, track, clip)
}

#[test]
fn split_is_non_destructive_and_sample_accurate() {
    let (mut project, _, clip_id) = project_with_clip();
    let right_id = project.split_clip(clip_id, 96_000).unwrap();
    let (_, left) = project.clip(clip_id).unwrap();
    let (_, right) = project.clip(right_id).unwrap();
    assert_eq!((left.source_in, left.source_out), (0, 48_000));
    assert_eq!((right.source_in, right.source_out), (48_000, 96_000));
    assert_eq!(right.start_frame, 96_000);
}

#[test]
fn moving_between_tracks_preserves_source_range() {
    let (mut project, _, clip_id) = project_with_clip();
    let second = project.add_track("더블");
    project.move_clip(clip_id, second, 12_000).unwrap();
    let (track, clip) = project.clip(clip_id).unwrap();
    assert_eq!(track.id, second);
    assert_eq!(clip.start_frame, 12_000);
    assert_eq!((clip.source_in, clip.source_out), (0, 96_000));
}

#[test]
fn history_does_not_require_pcm_copies() {
    let (mut project, _, _) = project_with_clip();
    let mut history = History::default();
    history.begin("트랙 추가", &project);
    project.add_track("코러스");
    assert!(history.commit(&project));
    assert_eq!(project.tracks.len(), 2);
    history.undo(&mut project).unwrap();
    assert_eq!(project.tracks.len(), 1);
    history.redo(&mut project).unwrap();
    assert_eq!(project.tracks.len(), 2);
}

#[test]
fn beat_snap_uses_project_tempo() {
    let project = Project::new("스냅", 48_000);
    // At 120 BPM, a quarter beat is 6,000 frames.
    assert_eq!(SnapMode::BeatDivision(4).snap(&project, 5_700), 6_000);
}
