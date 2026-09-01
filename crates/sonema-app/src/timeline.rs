use eframe::egui::{
    self, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2,
};
use sonema_audio::MediaPool;
use sonema_core::{ClipId, Project, SnapMode, TrackColor, TrackId};

use crate::theme;

const HEADER_WIDTH: f32 = 210.0;
const RULER_HEIGHT: f32 = 34.0;
const TRACK_HEIGHT: f32 = 88.0;
const HANDLE_WIDTH: f32 = 7.0;

#[derive(Debug, Clone)]
pub enum TimelineAction {
    SelectTrack(TrackId),
    SelectClip(ClipId),
    Seek(u64),
    ToggleMute(TrackId),
    ToggleSolo(TrackId),
    ToggleArm(TrackId),
    MoveClip { clip: ClipId, track: TrackId, start: u64 },
    TrimStart { clip: ClipId, start: u64 },
    TrimEnd { clip: ClipId, end: u64 },
    Split(ClipId),
    Duplicate(ClipId),
    Delete(ClipId),
}

#[derive(Debug, Clone, Copy)]
enum DragKind {
    Move,
    TrimStart,
    TrimEnd,
}

#[derive(Debug, Clone)]
struct DragPreview {
    clip: ClipId,
    kind: DragKind,
    original_start: u64,
    original_end: u64,
    preview_track: TrackId,
    preview_start: u64,
    preview_end: u64,
}

#[derive(Debug)]
pub struct TimelineState {
    pub pixels_per_second: f32,
    drag: Option<DragPreview>,
}

#[derive(Debug, Clone, Copy)]
pub struct TimelineView {
    pub playhead: u64,
    pub selected_track: Option<TrackId>,
    pub selected_clip: Option<ClipId>,
    pub snap: SnapMode,
}

impl Default for TimelineState {
    fn default() -> Self {
        Self { pixels_per_second: 92.0, drag: None }
    }
}

impl TimelineState {
    pub fn zoom_in(&mut self) {
        self.pixels_per_second = (self.pixels_per_second * 1.2).min(260.0);
    }

    pub fn zoom_out(&mut self) {
        self.pixels_per_second = (self.pixels_per_second / 1.2).max(24.0);
    }
}

pub fn show(
    ui: &mut egui::Ui,
    project: &Project,
    media: &MediaPool,
    state: &mut TimelineState,
    view: TimelineView,
) -> Vec<TimelineAction> {
    let TimelineView { playhead, selected_track, selected_clip, snap } = view;
    let mut actions = Vec::new();
    let duration_seconds = project.frames_to_seconds(project.duration_frames()) + 30.0;
    let timeline_seconds = duration_seconds.max(180.0);
    let world_size = vec2(
        HEADER_WIDTH + timeline_seconds as f32 * state.pixels_per_second,
        RULER_HEIGHT + TRACK_HEIGHT * project.tracks.len().max(1) as f32,
    );

    egui::ScrollArea::both()
        .id_salt("arrangement-scroll")
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            let (world_rect, world_response) = ui.allocate_exact_size(world_size, Sense::click());
            let painter = ui.painter_at(world_rect);
            painter.rect_filled(world_rect, 0.0, theme::BG);
            draw_grid(&painter, world_rect, viewport, project, state.pixels_per_second);

            for (track_index, track) in project.tracks.iter().enumerate() {
                let top = world_rect.top() + RULER_HEIGHT + track_index as f32 * TRACK_HEIGHT;
                let row_rect = Rect::from_min_max(
                    pos2(world_rect.left() + HEADER_WIDTH, top),
                    pos2(world_rect.right(), top + TRACK_HEIGHT),
                );
                let fill = if track_index % 2 == 0 {
                    Color32::from_rgb(18, 22, 25)
                } else {
                    Color32::from_rgb(20, 24, 27)
                };
                painter.rect_filled(row_rect, 0.0, fill);
                painter.line_segment(
                    [pos2(row_rect.left(), row_rect.bottom()), row_rect.right_bottom()],
                    Stroke::new(1.0, theme::BORDER),
                );

                for clip in &track.clips {
                    let Some(duration) = project.clip_duration_frames(clip) else { continue };
                    let left = world_rect.left()
                        + HEADER_WIDTH
                        + project.frames_to_seconds(clip.start_frame) as f32 * state.pixels_per_second;
                    let width = (project.frames_to_seconds(duration) as f32 * state.pixels_per_second)
                        .max(HANDLE_WIDTH * 2.0 + 2.0);
                    let clip_rect = Rect::from_min_size(
                        pos2(left, top + 7.0),
                        vec2(width, TRACK_HEIGHT - 14.0),
                    );
                    if clip_rect.right() < world_rect.left() + viewport.min.x
                        || clip_rect.left() > world_rect.left() + viewport.max.x
                    {
                        continue;
                    }
                    draw_clip(
                        &painter,
                        clip_rect,
                        track.color,
                        &clip.name,
                        media.get(&clip.media_id).map(AsRef::as_ref),
                        clip.source_in,
                        clip.source_out,
                        selected_clip == Some(clip.id),
                    );

                    let center_rect = Rect::from_min_max(
                        pos2(clip_rect.left() + HANDLE_WIDTH, clip_rect.top()),
                        pos2(clip_rect.right() - HANDLE_WIDTH, clip_rect.bottom()),
                    );
                    let move_response = ui.interact(center_rect, Id::new(("clip", clip.id)), Sense::click_and_drag());
                    let left_response = ui.interact(
                        Rect::from_min_max(
                            clip_rect.left_top(),
                            pos2(clip_rect.left() + HANDLE_WIDTH, clip_rect.bottom()),
                        ),
                        Id::new(("trim-left", clip.id)),
                        Sense::click_and_drag(),
                    );
                    let right_response = ui.interact(
                        Rect::from_min_max(
                            pos2(clip_rect.right() - HANDLE_WIDTH, clip_rect.top()),
                            clip_rect.right_bottom(),
                        ),
                        Id::new(("trim-right", clip.id)),
                        Sense::click_and_drag(),
                    );

                    if move_response.clicked() || left_response.clicked() || right_response.clicked() {
                        actions.push(TimelineAction::SelectTrack(track.id));
                        actions.push(TimelineAction::SelectClip(clip.id));
                    }
                    begin_or_update_drag(
                        ui,
                        project,
                        state,
                        &mut actions,
                        clip.id,
                        track.id,
                        clip.start_frame,
                        clip.start_frame + duration,
                        DragKind::Move,
                        &move_response,
                        snap,
                        world_rect,
                    );
                    begin_or_update_drag(
                        ui,
                        project,
                        state,
                        &mut actions,
                        clip.id,
                        track.id,
                        clip.start_frame,
                        clip.start_frame + duration,
                        DragKind::TrimStart,
                        &left_response,
                        snap,
                        world_rect,
                    );
                    begin_or_update_drag(
                        ui,
                        project,
                        state,
                        &mut actions,
                        clip.id,
                        track.id,
                        clip.start_frame,
                        clip.start_frame + duration,
                        DragKind::TrimEnd,
                        &right_response,
                        snap,
                        world_rect,
                    );
                    move_response.context_menu(|ui| {
                        if ui.button("재생 헤드에서 분할").clicked() {
                            actions.push(TimelineAction::Split(clip.id));
                            ui.close();
                        }
                        if ui.button("복제").clicked() {
                            actions.push(TimelineAction::Duplicate(clip.id));
                            ui.close();
                        }
                        if ui.button("삭제").clicked() {
                            actions.push(TimelineAction::Delete(clip.id));
                            ui.close();
                        }
                    });
                }
            }

            if let Some(drag) = &state.drag {
                draw_drag_preview(&painter, world_rect, project, state.pixels_per_second, drag);
            }

            let playhead_x = world_rect.left()
                + HEADER_WIDTH
                + project.frames_to_seconds(playhead) as f32 * state.pixels_per_second;
            painter.line_segment(
                [pos2(playhead_x, world_rect.top()), pos2(playhead_x, world_rect.bottom())],
                Stroke::new(1.5, theme::ACCENT),
            );
            painter.circle_filled(pos2(playhead_x, world_rect.top() + RULER_HEIGHT - 5.0), 4.0, theme::ACCENT);

            draw_sticky_headers(
                ui,
                &painter,
                world_rect,
                viewport,
                project,
                selected_track,
                &mut actions,
            );

            if world_response.clicked()
                && let Some(pointer) = world_response.interact_pointer_pos()
            {
                let timeline_left = world_rect.left() + HEADER_WIDTH;
                if pointer.x >= timeline_left {
                    let seconds = ((pointer.x - timeline_left) / state.pixels_per_second).max(0.0);
                    let frame = project.seconds_to_frames(seconds as f64);
                    actions.push(TimelineAction::Seek(snap.snap(project, frame)));
                }
            }
        });
    actions
}

#[allow(clippy::too_many_arguments)]
fn begin_or_update_drag(
    ui: &egui::Ui,
    project: &Project,
    state: &mut TimelineState,
    actions: &mut Vec<TimelineAction>,
    clip: ClipId,
    track: TrackId,
    original_start: u64,
    original_end: u64,
    kind: DragKind,
    response: &egui::Response,
    snap: SnapMode,
    world_rect: Rect,
) {
    if response.drag_started() {
        state.drag = Some(DragPreview {
            clip,
            kind,
            original_start,
            original_end,
            preview_track: track,
            preview_start: original_start,
            preview_end: original_end,
        });
    }
    let Some(drag) = state.drag.as_mut().filter(|drag| drag.clip == clip) else { return };
    if response.dragged() {
        let delta_seconds = response.drag_delta().x / state.pixels_per_second;
        let delta_frames = (delta_seconds * project.sample_rate as f32).round() as i64;
        let no_snap = ui.input(|input| input.modifiers.alt);
        let apply_snap = |frame| if no_snap { frame } else { snap.snap(project, frame) };
        match drag.kind {
            DragKind::Move => {
                let start = add_signed(drag.original_start, delta_frames);
                let duration = drag.original_end - drag.original_start;
                drag.preview_start = apply_snap(start);
                drag.preview_end = drag.preview_start.saturating_add(duration);
                if let Some(pointer) = response.interact_pointer_pos() {
                    let index = ((pointer.y - world_rect.top() - RULER_HEIGHT) / TRACK_HEIGHT)
                        .floor()
                        .max(0.0) as usize;
                    if let Some(target) = project.tracks.get(index) {
                        drag.preview_track = target.id;
                    }
                }
            }
            DragKind::TrimStart => {
                let candidate = apply_snap(add_signed(drag.original_start, delta_frames));
                drag.preview_start = candidate.min(drag.original_end.saturating_sub(16));
            }
            DragKind::TrimEnd => {
                let candidate = apply_snap(add_signed(drag.original_end, delta_frames));
                drag.preview_end = candidate.max(drag.original_start.saturating_add(16));
            }
        }
    }
    if response.drag_stopped() {
        match drag.kind {
            DragKind::Move => actions.push(TimelineAction::MoveClip {
                clip,
                track: drag.preview_track,
                start: drag.preview_start,
            }),
            DragKind::TrimStart => actions.push(TimelineAction::TrimStart {
                clip,
                start: drag.preview_start,
            }),
            DragKind::TrimEnd => actions.push(TimelineAction::TrimEnd {
                clip,
                end: drag.preview_end,
            }),
        }
        state.drag = None;
    }
}

fn draw_grid(
    painter: &egui::Painter,
    rect: Rect,
    viewport: Rect,
    project: &Project,
    pixels_per_second: f32,
) {
    let beat_seconds = 60.0 / project.bpm;
    let beat_width = beat_seconds as f32 * pixels_per_second;
    let first_timeline_x = (viewport.min.x - HEADER_WIDTH).max(0.0);
    let first_beat = (first_timeline_x / beat_width).floor().max(0.0) as u64;
    let last_beat = ((viewport.max.x - HEADER_WIDTH).max(0.0) / beat_width).ceil() as u64 + 1;
    for beat in first_beat..=last_beat {
        let x = rect.left() + HEADER_WIDTH + beat as f32 * beat_width;
        let downbeat = beat.is_multiple_of(project.time_signature.numerator.max(1) as u64);
        let color = if downbeat { theme::BORDER } else { Color32::from_rgb(34, 39, 43) };
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(if downbeat { 1.0 } else { 0.5 }, color),
        );
        if downbeat {
            let bar = beat / project.time_signature.numerator.max(1) as u64 + 1;
            painter.text(
                pos2(x + 5.0, rect.top() + 17.0),
                Align2::LEFT_CENTER,
                format!("{bar}"),
                FontId::monospace(11.0),
                theme::MUTED,
            );
        }
    }
    painter.line_segment(
        [
            pos2(rect.left() + HEADER_WIDTH, rect.top() + RULER_HEIGHT),
            pos2(rect.right(), rect.top() + RULER_HEIGHT),
        ],
        Stroke::new(1.0, theme::BORDER),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_clip(
    painter: &egui::Painter,
    rect: Rect,
    color: TrackColor,
    name: &str,
    source: Option<&sonema_audio::AudioSource>,
    source_in: u64,
    source_out: u64,
    selected: bool,
) {
    let base = Color32::from_rgb(color.0, color.1, color.2);
    painter.rect_filled(rect, 4.0, base.gamma_multiply(0.48));
    painter.rect_filled(
        Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 22.0)),
        4.0,
        base.gamma_multiply(0.72),
    );
    painter.text(
        pos2(rect.left() + 9.0, rect.top() + 11.0),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(12.0),
        Color32::WHITE,
    );
    if let Some(source) = source {
        let center = rect.center().y + 10.0;
        let half_height = (rect.height() - 31.0) * 0.48;
        let width = rect.width().max(1.0) as usize;
        let step = (width / 220).max(2);
        let source_span = source_out.saturating_sub(source_in).max(1) as f64;
        for pixel in (0..width).step_by(step) {
            let fraction = pixel as f64 / width as f64;
            let source_frame = source_in as f64 + source_span * fraction;
            let peak_index = ((source_frame / source.frames().max(1) as f64)
                * source.peaks.len() as f64)
                .floor() as usize;
            let (min, max) = source.peaks.get(peak_index).copied().unwrap_or((0.0, 0.0));
            let x = rect.left() + pixel as f32;
            painter.line_segment(
                [pos2(x, center - max * half_height), pos2(x, center - min * half_height)],
                Stroke::new(1.0, Color32::from_white_alpha(155)),
            );
        }
    }
    let border = if selected { theme::ACCENT } else { base.gamma_multiply(1.18) };
    draw_border(painter, rect, Stroke::new(if selected { 2.0 } else { 1.0 }, border));
}

fn draw_sticky_headers(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    rect: Rect,
    viewport: Rect,
    project: &Project,
    selected_track: Option<TrackId>,
    actions: &mut Vec<TimelineAction>,
) {
    let left = rect.left() + viewport.min.x;
    let header_rect = Rect::from_min_size(
        pos2(left, rect.top()),
        vec2(HEADER_WIDTH, rect.height()),
    );
    painter.rect_filled(header_rect, 0.0, theme::PANEL);
    painter.rect_filled(
        Rect::from_min_size(header_rect.min, vec2(HEADER_WIDTH, RULER_HEIGHT)),
        0.0,
        Color32::from_rgb(17, 20, 23),
    );
    painter.text(
        pos2(left + 14.0, rect.top() + RULER_HEIGHT * 0.5),
        Align2::LEFT_CENTER,
        "트랙",
        FontId::proportional(12.0),
        theme::MUTED,
    );

    for (index, track) in project.tracks.iter().enumerate() {
        let top = rect.top() + RULER_HEIGHT + index as f32 * TRACK_HEIGHT;
        let row = Rect::from_min_size(pos2(left, top), vec2(HEADER_WIDTH, TRACK_HEIGHT));
        let fill = if selected_track == Some(track.id) {
            Color32::from_rgb(32, 42, 43)
        } else if index % 2 == 0 {
            theme::PANEL
        } else {
            Color32::from_rgb(24, 29, 33)
        };
        painter.rect_filled(row, 0.0, fill);
        painter.rect_filled(
            Rect::from_min_size(row.min, vec2(4.0, TRACK_HEIGHT)),
            0.0,
            Color32::from_rgb(track.color.0, track.color.1, track.color.2),
        );
        painter.text(
            pos2(left + 15.0, top + 25.0),
            Align2::LEFT_CENTER,
            &track.name,
            FontId::proportional(13.0),
            theme::TEXT,
        );
        painter.text(
            pos2(left + 15.0, top + 48.0),
            Align2::LEFT_CENTER,
            format!("{:+.1} dB   {:+.0}", track.gain_db, track.pan * 100.0),
            FontId::monospace(10.0),
            theme::MUTED,
        );
        let select_response = ui.interact(row, Id::new(("track-row", track.id)), Sense::click());
        if select_response.clicked() {
            actions.push(TimelineAction::SelectTrack(track.id));
        }
        let mut button_left = left + 129.0;
        for (label, active, action) in [
            ("M", track.mute, TimelineAction::ToggleMute(track.id)),
            ("S", track.solo, TimelineAction::ToggleSolo(track.id)),
            ("R", track.armed, TimelineAction::ToggleArm(track.id)),
        ] {
            let button = Rect::from_min_size(pos2(button_left, top + 58.0), vec2(22.0, 20.0));
            let color = if active {
                if label == "R" { theme::RED } else { theme::AMBER }
            } else {
                Color32::from_rgb(43, 49, 54)
            };
            painter.rect_filled(button, 3.0, color);
            painter.text(
                button.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::monospace(10.0),
                if active { Color32::BLACK } else { theme::MUTED },
            );
            if ui.interact(button, Id::new((label, track.id)), Sense::click()).clicked() {
                actions.push(action);
            }
            button_left += 25.0;
        }
        painter.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, theme::BORDER));
    }
    painter.line_segment(
        [pos2(left + HEADER_WIDTH, rect.top()), pos2(left + HEADER_WIDTH, rect.bottom())],
        Stroke::new(1.0, theme::BORDER),
    );
}

fn draw_drag_preview(
    painter: &egui::Painter,
    rect: Rect,
    project: &Project,
    zoom: f32,
    drag: &DragPreview,
) {
    let Some(track_index) = project.tracks.iter().position(|track| track.id == drag.preview_track) else {
        return;
    };
    let left = rect.left()
        + HEADER_WIDTH
        + project.frames_to_seconds(drag.preview_start) as f32 * zoom;
    let width = project.frames_to_seconds(drag.preview_end.saturating_sub(drag.preview_start)) as f32 * zoom;
    let top = rect.top() + RULER_HEIGHT + track_index as f32 * TRACK_HEIGHT + 7.0;
    let preview = Rect::from_min_size(pos2(left, top), vec2(width.max(4.0), TRACK_HEIGHT - 14.0));
    painter.rect_filled(preview, 4.0, Color32::from_rgba_unmultiplied(67, 213, 164, 48));
    draw_border(painter, preview, Stroke::new(1.5, theme::ACCENT));
}

fn draw_border(painter: &egui::Painter, rect: Rect, stroke: Stroke) {
    painter.line_segment([rect.left_top(), rect.right_top()], stroke);
    painter.line_segment([rect.right_top(), rect.right_bottom()], stroke);
    painter.line_segment([rect.right_bottom(), rect.left_bottom()], stroke);
    painter.line_segment([rect.left_bottom(), rect.left_top()], stroke);
}

fn add_signed(value: u64, delta: i64) -> u64 {
    if delta < 0 { value.saturating_sub(delta.unsigned_abs()) } else { value.saturating_add(delta as u64) }
}

#[allow(dead_code)]
fn _keep_vec2(_: Vec2, _: Pos2) {}
