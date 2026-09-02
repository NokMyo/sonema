use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use directories::ProjectDirs;
use eframe::egui::{self, Align, Color32, Id, Key, Layout, RichText, Sense, Stroke, Vec2, vec2};
use sonema_audio::{
    AudioEngine, AudioSource, InputDevice, MediaPool, Recorder, decode_audio_file,
    list_input_devices, render_offline,
};
use sonema_core::{ClipId, History, Project, SnapMode, TrackId, db_to_gain, gain_to_db};
use sonema_format::{WavBitDepth, WavExportOptions, load_project, save_project, write_wav};

use crate::theme;
use crate::timeline::{self, TimelineAction, TimelineState, TimelineView};

const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
enum PendingAction {
    New,
    OpenDialog,
    OpenPath(PathBuf),
    Exit,
}

enum TaskResult {
    Decoded {
        path: PathBuf,
        result: Result<AudioSource, String>,
    },
    Loaded {
        path: PathBuf,
        recovered: bool,
        result: Result<(Project, MediaPool), String>,
    },
    Saved {
        path: PathBuf,
        generation: u64,
        autosave: bool,
        result: Result<(), String>,
    },
    Exported {
        path: PathBuf,
        result: Result<f32, String>,
    },
}

#[derive(Debug, Default)]
struct EditSignals {
    changed: bool,
    drag_started: bool,
    drag_stopped: bool,
}

impl EditSignals {
    fn add(&mut self, response: &egui::Response) {
        self.changed |= response.changed();
        self.drag_started |= response.drag_started();
        self.drag_stopped |= response.drag_stopped();
    }
}

pub struct SonemaApp {
    project: Project,
    media: MediaPool,
    history: History,
    engine: Option<AudioEngine>,
    engine_problem: Option<String>,
    timeline: TimelineState,
    selected_track: Option<TrackId>,
    selected_clip: Option<ClipId>,
    fallback_playhead: u64,
    current_path: Option<PathBuf>,
    dirty: bool,
    generation: u64,
    snap: SnapMode,
    snap_enabled: bool,
    metronome: bool,
    loop_enabled: bool,
    mixer_open: bool,
    inspector_open: bool,
    input_devices: Vec<InputDevice>,
    selected_input: Option<String>,
    recorder: Option<Recorder>,
    recording_channels: Vec<Vec<f32>>,
    recording_track: Option<TrackId>,
    recording_start: u64,
    recording_number: u32,
    task_sender: Sender<TaskResult>,
    task_receiver: Receiver<TaskResult>,
    importing: usize,
    saving: bool,
    autosaving: bool,
    exporting: bool,
    pending_action: Option<PendingAction>,
    action_after_save: Option<PendingAction>,
    show_export: bool,
    export_rate: u32,
    export_depth: WavBitDepth,
    export_normalize: bool,
    show_settings: bool,
    show_about: bool,
    recovery_path: Option<PathBuf>,
    recovery_available: bool,
    last_autosave: Instant,
    status: String,
    toast: Option<(String, bool, Instant)>,
    should_close: bool,
}

impl SonemaApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        theme::install(&creation_context.egui_ctx);
        let project = Project::new("제목 없는 프로젝트", 48_000);
        let media = MediaPool::new();
        let (engine, engine_problem) = match AudioEngine::new() {
            Ok(engine) => (Some(engine), None),
            Err(error) => (None, Some(error.to_string())),
        };
        if let Some(engine) = &engine {
            let _ = engine.set_project(&project, &media, false);
        }
        let input_devices = list_input_devices().unwrap_or_default();
        let selected_input = input_devices
            .iter()
            .find(|device| device.is_default)
            .or(input_devices.first())
            .map(|device| device.name.clone());
        let recovery_path = ProjectDirs::from("com", "Febius", "Sonema").map(|directories| {
            let directory = directories.data_local_dir().join("Recovery");
            let _ = std::fs::create_dir_all(&directory);
            directory.join("Last Session.sonema")
        });
        let recovery_available = recovery_path.as_ref().is_some_and(|path| path.exists());
        let (task_sender, task_receiver) = unbounded();
        let selected_track = project.tracks.first().map(|track| track.id);
        Self {
            project,
            media,
            history: History::default(),
            engine,
            engine_problem,
            timeline: TimelineState::default(),
            selected_track,
            selected_clip: None,
            fallback_playhead: 0,
            current_path: None,
            dirty: false,
            generation: 0,
            snap: SnapMode::default(),
            snap_enabled: true,
            metronome: false,
            loop_enabled: false,
            mixer_open: true,
            inspector_open: true,
            input_devices,
            selected_input,
            recorder: None,
            recording_channels: Vec::new(),
            recording_track: None,
            recording_start: 0,
            recording_number: 1,
            task_sender,
            task_receiver,
            importing: 0,
            saving: false,
            autosaving: false,
            exporting: false,
            pending_action: None,
            action_after_save: None,
            show_export: false,
            export_rate: 48_000,
            export_depth: WavBitDepth::Pcm24,
            export_normalize: false,
            show_settings: false,
            show_about: false,
            recovery_path,
            recovery_available,
            last_autosave: Instant::now(),
            status: "준비".into(),
            toast: None,
            should_close: false,
        }
    }

    fn ui_body(&mut self, ui: &mut egui::Ui) {
        let command = self.menu_bar(ui);
        self.execute_ui_command(command);
        ui.separator();
        self.transport_bar(ui);
        ui.separator();

        if self.recovery_available {
            self.recovery_banner(ui);
        }

        let status_height = 24.0;
        let mixer_height = if self.mixer_open { 188.0 } else { 0.0 };
        let separators = if self.mixer_open { 7.0 } else { 0.0 };
        let main_height =
            (ui.available_height() - status_height - mixer_height - separators).max(220.0);
        ui.allocate_ui(vec2(ui.available_width(), main_height), |ui| {
            self.workspace(ui)
        });
        if self.mixer_open {
            ui.separator();
            ui.allocate_ui(vec2(ui.available_width(), mixer_height), |ui| {
                self.mixer(ui)
            });
        }
        ui.separator();
        self.status_bar(ui);
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) -> Option<UiCommand> {
        let mut command = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SONEMA")
                    .strong()
                    .color(theme::ACCENT)
                    .size(14.0),
            );
            ui.label(RichText::new("Febius").color(theme::MUTED).size(11.0));
            ui.separator();
            ui.menu_button("파일", |ui| {
                menu_item(ui, "새 프로젝트", "Ctrl+N", &mut command, UiCommand::New);
                menu_item(ui, "열기", "Ctrl+O", &mut command, UiCommand::Open);
                ui.separator();
                menu_item(ui, "저장", "Ctrl+S", &mut command, UiCommand::Save);
                menu_item(
                    ui,
                    "다른 이름으로 저장",
                    "Ctrl+Shift+S",
                    &mut command,
                    UiCommand::SaveAs,
                );
                ui.separator();
                menu_item(
                    ui,
                    "오디오 가져오기",
                    "Ctrl+I",
                    &mut command,
                    UiCommand::Import,
                );
                menu_item(ui, "믹스 WAV 출력", "", &mut command, UiCommand::Export);
            });
            ui.menu_button("편집", |ui| {
                menu_item_enabled(
                    ui,
                    "실행 취소",
                    "Ctrl+Z",
                    self.history.can_undo(),
                    &mut command,
                    UiCommand::Undo,
                );
                menu_item_enabled(
                    ui,
                    "다시 실행",
                    "Ctrl+Shift+Z",
                    self.history.can_redo(),
                    &mut command,
                    UiCommand::Redo,
                );
                ui.separator();
                menu_item(ui, "클립 분할", "S", &mut command, UiCommand::Split);
                menu_item(
                    ui,
                    "클립 복제",
                    "Ctrl+D",
                    &mut command,
                    UiCommand::Duplicate,
                );
                menu_item(ui, "클립 삭제", "Delete", &mut command, UiCommand::Delete);
            });
            ui.menu_button("트랙", |ui| {
                menu_item(
                    ui,
                    "오디오 트랙 추가",
                    "Ctrl+T",
                    &mut command,
                    UiCommand::AddTrack,
                );
                menu_item(
                    ui,
                    "선택 트랙 삭제",
                    "",
                    &mut command,
                    UiCommand::DeleteTrack,
                );
            });
            ui.menu_button("보기", |ui| {
                if ui
                    .checkbox(&mut self.inspector_open, "채널 스트립")
                    .clicked()
                {
                    ui.close();
                }
                if ui.checkbox(&mut self.mixer_open, "믹서").clicked() {
                    ui.close();
                }
            });
            ui.menu_button("도움말", |ui| {
                if ui.button("오디오 설정").clicked() {
                    command = Some(UiCommand::Settings);
                    ui.close();
                }
                if ui.button("Sonema 정보").clicked() {
                    command = Some(UiCommand::About);
                    ui.close();
                }
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dirty = if self.dirty { "  • 수정됨" } else { "" };
                ui.label(
                    RichText::new(format!("{}{}", self.project.name, dirty)).color(if self.dirty {
                        theme::TEXT
                    } else {
                        theme::MUTED
                    }),
                );
            });
        });
        command
    }

    fn transport_bar(&mut self, ui: &mut egui::Ui) {
        let mut action = None;
        ui.horizontal(|ui| {
            if ui.button("＋ 트랙").clicked() {
                action = Some(UiCommand::AddTrack);
            }
            if ui.button("가져오기").clicked() {
                action = Some(UiCommand::Import);
            }
            ui.separator();
            let recording = self.recorder.is_some();
            if ui
                .add(egui::Button::new("●").fill(if recording {
                    theme::RED
                } else {
                    theme::PANEL_RAISED
                }))
                .on_hover_text("녹음 (R)")
                .clicked()
            {
                action = Some(UiCommand::Record);
            }
            let playing = self.engine.as_ref().is_some_and(AudioEngine::is_playing);
            if ui
                .button(if playing { "Ⅱ" } else { "▶" })
                .on_hover_text("재생/일시정지 (Space)")
                .clicked()
            {
                action = Some(UiCommand::PlayPause);
            }
            if ui.button("■").on_hover_text("정지").clicked() {
                action = Some(UiCommand::Stop);
            }
            ui.label(
                RichText::new(format_time(self.project.frames_to_seconds(self.playhead())))
                    .monospace()
                    .size(18.0)
                    .color(theme::TEXT),
            );
            ui.separator();

            let before = self.project.clone();
            let bpm_response = ui.add(
                egui::DragValue::new(&mut self.project.bpm)
                    .range(20.0..=400.0)
                    .speed(0.25)
                    .suffix(" BPM"),
            );
            if bpm_response.changed() {
                self.history.checkpoint("BPM 변경", before, &self.project);
                self.mark_changed();
            }
            if ui.selectable_label(self.metronome, "메트로놈").clicked() {
                self.metronome = !self.metronome;
                self.rebuild_engine();
            }
            if ui.selectable_label(self.loop_enabled, "루프").clicked() {
                self.loop_enabled = !self.loop_enabled;
                if let Some(engine) = &self.engine {
                    let _ = engine.set_loop_enabled(self.loop_enabled);
                }
            }
            ui.separator();
            ui.checkbox(&mut self.snap_enabled, "스냅");
            if ui
                .small_button("－")
                .on_hover_text("타임라인 축소")
                .clicked()
            {
                self.timeline.zoom_out();
            }
            if ui
                .small_button("＋")
                .on_hover_text("타임라인 확대")
                .clicked()
            {
                self.timeline.zoom_in();
            }
            if ui.selectable_label(self.mixer_open, "믹서").clicked() {
                self.mixer_open = !self.mixer_open;
            }
        });
        self.execute_ui_command(action);
    }

    fn workspace(&mut self, ui: &mut egui::Ui) {
        let playhead = self.playhead();
        let inspector_width = if self.inspector_open { 286.0 } else { 0.0 };
        ui.horizontal(|ui| {
            let timeline_width = (ui.available_width() - inspector_width - 7.0).max(300.0);
            ui.allocate_ui(vec2(timeline_width, ui.available_height()), |ui| {
                let snap = if self.snap_enabled {
                    self.snap
                } else {
                    SnapMode::Off
                };
                let actions = timeline::show(
                    ui,
                    &self.project,
                    &self.media,
                    &mut self.timeline,
                    TimelineView {
                        playhead,
                        selected_track: self.selected_track,
                        selected_clip: self.selected_clip,
                        snap,
                    },
                );
                self.apply_timeline_actions(actions);
            });
            if self.inspector_open {
                ui.separator();
                ui.allocate_ui(vec2(inspector_width, ui.available_height()), |ui| {
                    self.inspector(ui)
                });
            }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("채널 스트립");
        ui.separator();
        let Some(track_id) = self.selected_track else {
            ui.label(RichText::new("트랙을 선택하십시오.").color(theme::MUTED));
            return;
        };
        let Some(index) = self
            .project
            .tracks
            .iter()
            .position(|track| track.id == track_id)
        else {
            return;
        };
        let before = self.project.clone();
        let mut signals = EditSignals::default();
        let mut arm_changed = false;
        {
            let track = &mut self.project.tracks[index];
            signals.add(&ui.text_edit_singleline(&mut track.name));
            ui.label(RichText::new("음량").color(theme::MUTED));
            signals.add(&ui.add(egui::Slider::new(&mut track.gain_db, -60.0..=12.0).suffix(" dB")));
            ui.label(RichText::new("팬").color(theme::MUTED));
            signals.add(&ui.add(egui::Slider::new(&mut track.pan, -1.0..=1.0)));
            ui.horizontal(|ui| {
                signals.add(&ui.checkbox(&mut track.mute, "뮤트"));
                signals.add(&ui.checkbox(&mut track.solo, "솔로"));
                let response = ui.checkbox(&mut track.armed, "녹음 대기");
                arm_changed = response.changed();
                signals.add(&response);
            });
            signals.add(&ui.checkbox(&mut track.monitor, "입력 모니터링"));
            ui.separator();
            ui.collapsing("하이패스", |ui| {
                signals.add(&ui.checkbox(&mut track.effects.high_pass.enabled, "사용"));
                signals.add(
                    &ui.add(
                        egui::Slider::new(&mut track.effects.high_pass.frequency_hz, 20.0..=500.0)
                            .logarithmic(true)
                            .suffix(" Hz"),
                    ),
                );
            });
            ui.collapsing("3밴드 EQ", |ui| {
                eq_band(ui, "저역", &mut track.effects.low_eq, &mut signals);
                eq_band(ui, "중역", &mut track.effects.mid_eq, &mut signals);
                eq_band(ui, "고역", &mut track.effects.high_eq, &mut signals);
            });
            ui.collapsing("컴프레서", |ui| {
                let compressor = &mut track.effects.compressor;
                signals.add(&ui.checkbox(&mut compressor.enabled, "사용"));
                signals.add(&ui.add(
                    egui::Slider::new(&mut compressor.threshold_db, -60.0..=0.0).suffix(" dB"),
                ));
                signals.add(
                    &ui.add(egui::Slider::new(&mut compressor.ratio, 1.0..=20.0).suffix(":1")),
                );
                signals.add(
                    &ui.add(
                        egui::Slider::new(&mut compressor.attack_ms, 0.1..=200.0)
                            .logarithmic(true)
                            .suffix(" ms"),
                    ),
                );
                signals.add(
                    &ui.add(
                        egui::Slider::new(&mut compressor.release_ms, 10.0..=2_000.0)
                            .logarithmic(true)
                            .suffix(" ms"),
                    ),
                );
                signals.add(
                    &ui.add(
                        egui::Slider::new(&mut compressor.makeup_db, -6.0..=18.0).suffix(" dB"),
                    ),
                );
            });
        }
        if arm_changed && self.project.tracks[index].armed {
            for (track_index, track) in self.project.tracks.iter_mut().enumerate() {
                track.armed = track_index == index;
            }
        }
        self.finish_continuous_edit("채널 스트립 변경", before, signals);
        if self.recorder.is_some() {
            self.update_monitoring();
        }
    }

    fn mixer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("MIXER").strong().color(theme::MUTED));
            ui.label(
                RichText::new("트랙별 레벨과 팬")
                    .small()
                    .color(theme::MUTED),
            );
        });
        let before = self.project.clone();
        let mut signals = EditSignals::default();
        let mut selected = None;
        let mut arm_clicked = None;
        egui::ScrollArea::horizontal()
            .id_salt("mixer-scroll")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, track) in self.project.tracks.iter_mut().enumerate() {
                        let meter = self
                            .engine
                            .as_ref()
                            .map_or(0.0, |engine| engine.track_meter(index));
                        egui::Frame::group(ui.style())
                            .fill(theme::PANEL)
                            .show(ui, |ui| {
                                ui.set_width(116.0);
                                if ui
                                    .add(egui::Button::new(&track.name).min_size(vec2(108.0, 22.0)))
                                    .clicked()
                                {
                                    selected = Some(track.id);
                                }
                                ui.horizontal(|ui| {
                                    signals.add(&ui.toggle_value(&mut track.mute, "M"));
                                    signals.add(&ui.toggle_value(&mut track.solo, "S"));
                                    let mut armed = track.armed;
                                    let response = ui.toggle_value(&mut armed, "R");
                                    if response.changed() {
                                        arm_clicked = Some((track.id, armed));
                                        signals.add(&response);
                                    }
                                });
                                ui.horizontal(|ui| {
                                    signals.add(
                                        &ui.add(
                                            egui::Slider::new(&mut track.gain_db, -60.0..=12.0)
                                                .vertical()
                                                .show_value(false),
                                        ),
                                    );
                                    meter_widget(ui, meter, vec2(8.0, 72.0));
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(format!("{:+.1}", track.gain_db))
                                                .monospace()
                                                .small(),
                                        );
                                        signals.add(
                                            &ui.add(
                                                egui::Slider::new(&mut track.pan, -1.0..=1.0)
                                                    .show_value(false),
                                            ),
                                        );
                                        ui.label(
                                            RichText::new(format!("P {:+.0}", track.pan * 100.0))
                                                .small(),
                                        );
                                    });
                                });
                            });
                    }
                    ui.separator();
                    let master_meter = self.engine.as_ref().map_or(0.0, AudioEngine::master_meter);
                    egui::Frame::group(ui.style())
                        .fill(Color32::from_rgb(26, 35, 34))
                        .show(ui, |ui| {
                            ui.set_width(116.0);
                            ui.label(RichText::new("MASTER").strong().color(theme::ACCENT));
                            ui.add_space(28.0);
                            ui.horizontal(|ui| {
                                signals.add(
                                    &ui.add(
                                        egui::Slider::new(
                                            &mut self.project.master.gain_db,
                                            -60.0..=12.0,
                                        )
                                        .vertical()
                                        .show_value(false),
                                    ),
                                );
                                meter_widget(ui, master_meter, vec2(8.0, 72.0));
                                ui.label(
                                    RichText::new(format!(
                                        "{:+.1} dB",
                                        self.project.master.gain_db
                                    ))
                                    .monospace(),
                                );
                            });
                        });
                });
            });
        if let Some((id, armed)) = arm_clicked {
            for track in &mut self.project.tracks {
                track.armed = track.id == id && armed;
            }
        }
        if let Some(id) = selected {
            self.selected_track = Some(id);
        }
        self.finish_continuous_edit("믹서 변경", before, signals);
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let busy = if self.exporting {
                "WAV 출력 중"
            } else if self.saving {
                "저장 중"
            } else if self.importing > 0 {
                "오디오 해석 중"
            } else if self.recorder.is_some() {
                "녹음 중"
            } else {
                &self.status
            };
            ui.label(RichText::new(busy).small().color(theme::MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(engine) = &self.engine {
                    let status = engine.status();
                    ui.label(
                        RichText::new(format!(
                            "{}  ·  {} Hz  ·  {} ch",
                            status.output_name, status.sample_rate, status.channels
                        ))
                        .small()
                        .color(theme::MUTED),
                    );
                } else {
                    ui.label(RichText::new("오디오 출력 없음").small().color(theme::RED));
                }
            });
        });
    }

    fn recovery_banner(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::from_rgb(42, 37, 24))
            .stroke(Stroke::new(1.0, theme::AMBER))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("이전 작업의 자동 복구 파일이 있습니다.");
                    if ui.button("복구").clicked()
                        && let Some(path) = self.recovery_path.clone()
                    {
                        self.request_action(PendingAction::OpenPath(path));
                    }
                    if ui.button("닫기").clicked() {
                        self.recovery_available = false;
                    }
                });
            });
    }

    fn apply_timeline_actions(&mut self, actions: Vec<TimelineAction>) {
        for action in actions {
            match action {
                TimelineAction::SelectTrack(id) => self.selected_track = Some(id),
                TimelineAction::SelectClip(id) => self.selected_clip = Some(id),
                TimelineAction::Seek(frame) => self.seek(frame),
                TimelineAction::ToggleMute(id) => self.toggle_track(id, TrackToggle::Mute),
                TimelineAction::ToggleSolo(id) => self.toggle_track(id, TrackToggle::Solo),
                TimelineAction::ToggleArm(id) => self.toggle_track(id, TrackToggle::Arm),
                TimelineAction::MoveClip { clip, track, start } => {
                    let before = self.project.clone();
                    match self.project.move_clip(clip, track, start) {
                        Ok(()) => self.finish_discrete_edit("클립 이동", before),
                        Err(error) => self.error(error.to_string()),
                    }
                }
                TimelineAction::TrimStart { clip, start } => {
                    let before = self.project.clone();
                    match self.project.trim_clip_start(clip, start) {
                        Ok(()) => self.finish_discrete_edit("클립 시작점 조절", before),
                        Err(error) => self.error(error.to_string()),
                    }
                }
                TimelineAction::TrimEnd { clip, end } => {
                    let before = self.project.clone();
                    match self.project.trim_clip_end(clip, end) {
                        Ok(()) => self.finish_discrete_edit("클립 끝점 조절", before),
                        Err(error) => self.error(error.to_string()),
                    }
                }
                TimelineAction::Split(id) => self.split_clip(Some(id)),
                TimelineAction::Duplicate(id) => self.duplicate_clip(Some(id)),
                TimelineAction::Delete(id) => self.delete_clip(Some(id)),
            }
        }
    }

    fn execute_ui_command(&mut self, command: Option<UiCommand>) {
        let Some(command) = command else { return };
        match command {
            UiCommand::New => self.request_action(PendingAction::New),
            UiCommand::Open => self.request_action(PendingAction::OpenDialog),
            UiCommand::Save => {
                self.begin_save(false);
            }
            UiCommand::SaveAs => {
                self.begin_save(true);
            }
            UiCommand::Import => self.choose_audio_files(),
            UiCommand::Export => self.show_export = true,
            UiCommand::Undo => self.undo(),
            UiCommand::Redo => self.redo(),
            UiCommand::Split => self.split_clip(self.selected_clip),
            UiCommand::Duplicate => self.duplicate_clip(self.selected_clip),
            UiCommand::Delete => self.delete_clip(self.selected_clip),
            UiCommand::AddTrack => self.add_track(),
            UiCommand::DeleteTrack => self.delete_selected_track(),
            UiCommand::Record => self.toggle_recording(),
            UiCommand::PlayPause => self.play_pause(),
            UiCommand::Stop => self.stop(),
            UiCommand::Settings => self.show_settings = true,
            UiCommand::About => self.show_about = true,
        }
    }

    fn request_action(&mut self, action: PendingAction) {
        if self.recorder.is_some() {
            self.stop_recording();
        }
        if self.dirty {
            self.pending_action = Some(action);
        } else {
            self.perform_action(action);
        }
    }

    fn perform_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::New => self.new_project(),
            PendingAction::OpenDialog => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Sonema 프로젝트", &["sonema"])
                    .pick_file()
                {
                    self.open_path(path, false);
                }
            }
            PendingAction::OpenPath(path) => {
                let recovered = self.recovery_path.as_deref() == Some(path.as_path());
                self.open_path(path, recovered);
            }
            PendingAction::Exit => {
                self.dirty = false;
                self.should_close = true;
            }
        }
    }

    fn new_project(&mut self) {
        self.stop();
        self.project = Project::new("제목 없는 프로젝트", 48_000);
        self.media.clear();
        self.history.clear();
        self.selected_track = self.project.tracks.first().map(|track| track.id);
        self.selected_clip = None;
        self.current_path = None;
        self.dirty = false;
        self.generation = self.generation.wrapping_add(1);
        self.fallback_playhead = 0;
        self.rebuild_engine();
        self.status = "새 프로젝트".into();
    }

    fn add_track(&mut self) {
        let before = self.project.clone();
        let id = self
            .project
            .add_track(format!("오디오 {}", self.project.tracks.len() + 1));
        self.selected_track = Some(id);
        self.finish_discrete_edit("트랙 추가", before);
    }

    fn delete_selected_track(&mut self) {
        let Some(id) = self.selected_track else {
            return;
        };
        let before = self.project.clone();
        match self.project.remove_track(id) {
            Ok(()) => {
                if self.project.tracks.is_empty() {
                    self.project.add_track("오디오 1");
                }
                self.selected_track = self.project.tracks.first().map(|track| track.id);
                self.selected_clip = None;
                self.finish_discrete_edit("트랙 삭제", before);
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn toggle_track(&mut self, id: TrackId, toggle: TrackToggle) {
        let before = self.project.clone();
        match toggle {
            TrackToggle::Arm => {
                let new_value = self.project.track(id).is_some_and(|track| !track.armed);
                for track in &mut self.project.tracks {
                    track.armed = track.id == id && new_value;
                }
            }
            TrackToggle::Mute => {
                if let Some(track) = self.project.track_mut(id) {
                    track.mute = !track.mute;
                }
            }
            TrackToggle::Solo => {
                if let Some(track) = self.project.track_mut(id) {
                    track.solo = !track.solo;
                }
            }
        }
        self.selected_track = Some(id);
        self.finish_discrete_edit("트랙 상태 변경", before);
    }

    fn split_clip(&mut self, clip: Option<ClipId>) {
        let Some(clip) = clip else { return };
        let before = self.project.clone();
        match self.project.split_clip(clip, self.playhead()) {
            Ok(right) => {
                self.selected_clip = Some(right);
                self.finish_discrete_edit("클립 분할", before);
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn duplicate_clip(&mut self, clip: Option<ClipId>) {
        let Some(clip) = clip else { return };
        let before = self.project.clone();
        match self.project.duplicate_clip(clip) {
            Ok(copy) => {
                self.selected_clip = Some(copy);
                self.finish_discrete_edit("클립 복제", before);
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn delete_clip(&mut self, clip: Option<ClipId>) {
        let Some(clip) = clip else { return };
        let before = self.project.clone();
        match self.project.remove_clip(clip) {
            Ok(_) => {
                self.selected_clip = None;
                self.finish_discrete_edit("클립 삭제", before);
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn choose_audio_files(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "오디오",
                &[
                    "wav", "wave", "mp3", "flac", "ogg", "m4a", "aac", "aif", "aiff",
                ],
            )
            .pick_files()
        else {
            return;
        };
        self.import_paths(paths);
    }

    fn import_paths(&mut self, paths: Vec<PathBuf>) {
        self.importing += paths.len();
        for path in paths {
            let sender = self.task_sender.clone();
            std::thread::spawn(move || {
                let result = decode_audio_file(&path).map_err(|error| error.to_string());
                let _ = sender.send(TaskResult::Decoded { path, result });
            });
        }
    }

    fn open_path(&mut self, path: PathBuf, recovered: bool) {
        let sender = self.task_sender.clone();
        self.status = "프로젝트 여는 중".into();
        std::thread::spawn(move || {
            let result = load_project(&path).map_err(|error| error.to_string());
            let _ = sender.send(TaskResult::Loaded {
                path,
                recovered,
                result,
            });
        });
    }

    fn begin_save(&mut self, save_as: bool) -> bool {
        if self.recorder.is_some() {
            self.stop_recording();
        }
        if self.saving {
            return false;
        }
        let path = if save_as || self.current_path.is_none() {
            let suggested = format!("{}.sonema", safe_filename(&self.project.name));
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Sonema 프로젝트", &["sonema"])
                .set_file_name(&suggested)
                .save_file()
            else {
                return false;
            };
            path
        } else {
            self.current_path.clone().unwrap()
        };
        self.spawn_save(path, false);
        true
    }

    fn spawn_save(&mut self, path: PathBuf, autosave: bool) {
        let project = self.project.clone();
        let media = self.media.clone();
        let generation = self.generation;
        let sender = self.task_sender.clone();
        if autosave {
            self.autosaving = true;
        } else {
            self.saving = true;
        }
        std::thread::spawn(move || {
            let result = save_project(&path, &project, &media).map_err(|error| error.to_string());
            let _ = sender.send(TaskResult::Saved {
                path,
                generation,
                autosave,
                result,
            });
        });
    }

    fn begin_export(&mut self) {
        if self.recorder.is_some() {
            self.stop_recording();
        }
        if self.exporting {
            return;
        }
        let suggested = format!("{} Mix.wav", safe_filename(&self.project.name));
        let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV 오디오", &["wav"])
            .set_file_name(&suggested)
            .save_file()
        else {
            return;
        };
        let project = self.project.clone();
        let media = self.media.clone();
        let rate = self.export_rate;
        let normalize = self.export_normalize.then_some(-1.0);
        let options = WavExportOptions {
            bit_depth: self.export_depth,
            dither: true,
        };
        let sender = self.task_sender.clone();
        self.exporting = true;
        std::thread::spawn(move || {
            let result = (|| {
                let mix = render_offline(&project, &media, rate, normalize)
                    .map_err(|error| error.to_string())?;
                let peak = mix.peak;
                write_wav(&path, &mix, options).map_err(|error| error.to_string())?;
                Ok(peak)
            })();
            let _ = sender.send(TaskResult::Exported { path, result });
        });
        self.show_export = false;
    }

    fn poll_tasks(&mut self) {
        while let Ok(message) = self.task_receiver.try_recv() {
            match message {
                TaskResult::Decoded { path, result } => {
                    self.importing = self.importing.saturating_sub(1);
                    match result {
                        Ok(source) => {
                            let before = self.project.clone();
                            let source = Arc::new(source);
                            let info = source.info(Some(path.to_string_lossy().into_owned()));
                            let id = source.id;
                            let track = self.project.add_track(info.name.clone());
                            if let Err(error) = self.project.register_media(info) {
                                self.error(error.to_string());
                                continue;
                            }
                            self.media.insert(id, source);
                            match self.project.insert_media_clip(track, id, self.playhead()) {
                                Ok(clip) => {
                                    self.selected_track = Some(track);
                                    self.selected_clip = Some(clip);
                                    self.finish_discrete_edit("오디오 가져오기", before);
                                    self.notice(format!("{} 가져옴", path.display()));
                                }
                                Err(error) => self.error(error.to_string()),
                            }
                        }
                        Err(error) => self.error(format!("{}: {error}", path.display())),
                    }
                }
                TaskResult::Loaded {
                    path,
                    recovered,
                    result,
                } => match result {
                    Ok((project, media)) => {
                        self.stop();
                        self.project = project;
                        self.media = media;
                        self.history.clear();
                        self.selected_track = self.project.tracks.first().map(|track| track.id);
                        self.selected_clip = None;
                        self.current_path = if recovered { None } else { Some(path) };
                        self.dirty = recovered;
                        self.generation = self.generation.wrapping_add(1);
                        self.rebuild_engine();
                        self.recovery_available = false;
                        self.status = if recovered {
                            "자동 복구 완료"
                        } else {
                            "프로젝트 열기 완료"
                        }
                        .into();
                    }
                    Err(error) => self.error(error),
                },
                TaskResult::Saved {
                    path,
                    generation,
                    autosave,
                    result,
                } => {
                    if autosave {
                        self.autosaving = false;
                        match result {
                            Ok(()) => self.recovery_available = true,
                            Err(error) => self.error(format!("자동 복구 저장 실패: {error}")),
                        }
                        continue;
                    }
                    self.saving = false;
                    match result {
                        Ok(()) => {
                            self.current_path = Some(path.clone());
                            if generation == self.generation {
                                self.dirty = false;
                            }
                            self.status = format!("{} 저장 완료", path.display());
                            if let Some(recovery) = &self.recovery_path {
                                let _ = std::fs::remove_file(recovery);
                            }
                            self.recovery_available = false;
                            if let Some(action) = self.action_after_save.take() {
                                self.perform_action(action);
                            }
                        }
                        Err(error) => {
                            self.action_after_save = None;
                            self.error(error);
                        }
                    }
                }
                TaskResult::Exported { path, result } => {
                    self.exporting = false;
                    match result {
                        Ok(peak) => self.notice(format!(
                            "WAV 출력 완료: {}  (피크 {:.1} dBFS)",
                            path.display(),
                            gain_to_db(peak)
                        )),
                        Err(error) => self.error(error),
                    }
                }
            }
        }
    }

    fn play_pause(&mut self) {
        if self.recorder.is_some() {
            self.stop_recording();
            return;
        }
        self.rebuild_engine();
        let Some(engine) = &self.engine else {
            self.error(
                self.engine_problem
                    .clone()
                    .unwrap_or_else(|| "오디오 출력이 없습니다".into()),
            );
            return;
        };
        let result = if engine.is_playing() {
            engine.pause()
        } else {
            engine.play()
        };
        if let Err(error) = result {
            self.error(error.to_string());
        }
    }

    fn stop(&mut self) {
        if self.recorder.is_some() {
            self.stop_recording();
        }
        if let Some(engine) = &self.engine {
            let _ = engine.stop();
        }
        self.fallback_playhead = 0;
    }

    fn seek(&mut self, frame: u64) {
        self.fallback_playhead = frame;
        if let Some(engine) = &self.engine {
            let _ = engine.seek(frame);
        }
    }

    fn playhead(&self) -> u64 {
        self.engine
            .as_ref()
            .map_or(self.fallback_playhead, AudioEngine::playhead)
    }

    fn toggle_recording(&mut self) {
        if self.recorder.is_some() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    fn start_recording(&mut self) {
        let track_id = self
            .project
            .tracks
            .iter()
            .find(|track| track.armed)
            .map(|track| track.id)
            .or(self.selected_track)
            .or_else(|| self.project.tracks.first().map(|track| track.id));
        let Some(track_id) = track_id else {
            self.error("녹음할 트랙이 없습니다".into());
            return;
        };
        for track in &mut self.project.tracks {
            track.armed = track.id == track_id;
        }
        let monitor = self.engine.as_ref().map(AudioEngine::monitor_bus);
        match Recorder::start(self.selected_input.as_deref(), monitor.clone()) {
            Ok(recorder) => {
                let monitor_requested = self
                    .project
                    .track(track_id)
                    .is_some_and(|track| track.monitor);
                if let Some(bus) = &monitor
                    && monitor_requested
                    && !bus.set_enabled(true)
                {
                    self.notice("입출력 샘플레이트가 달라 입력 모니터링을 껐습니다".into());
                }
                self.recording_channels.clear();
                self.recording_channels
                    .resize_with(recorder.channel_count(), Vec::new);
                self.recording_start = self.playhead();
                self.recording_track = Some(track_id);
                self.recorder = Some(recorder);
                self.rebuild_engine();
                if let Some(engine) = &self.engine {
                    let _ = engine.play();
                }
                self.status = "녹음 중".into();
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn stop_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        if let Some(engine) = &self.engine {
            engine.monitor_bus().set_enabled(false);
            let _ = engine.pause();
        }
        let recorded = recorder.finish(std::mem::take(&mut self.recording_channels));
        let Some(track_id) = self.recording_track.take() else {
            return;
        };
        if recorded.channels.first().map_or(0, Vec::len) < 32 {
            self.error("녹음된 오디오가 너무 짧습니다".into());
            return;
        }
        let name = format!("녹음 {:02}", self.recording_number);
        self.recording_number += 1;
        let before = self.project.clone();
        match AudioSource::from_recording(name, recorded.sample_rate, recorded.channels) {
            Ok(source) => {
                let source = Arc::new(source);
                let info = source.info(None);
                let id = source.id;
                if let Err(error) = self.project.register_media(info) {
                    self.error(error.to_string());
                    return;
                }
                self.media.insert(id, source);
                match self
                    .project
                    .insert_media_clip(track_id, id, self.recording_start)
                {
                    Ok(clip) => {
                        self.selected_clip = Some(clip);
                        self.finish_discrete_edit("오디오 녹음", before);
                        if recorded.dropped_samples > 0 {
                            self.error(format!(
                                "녹음 중 {}개 샘플이 장치 지연으로 누락되었습니다",
                                recorded.dropped_samples
                            ));
                        } else {
                            self.notice("녹음 완료".into());
                        }
                    }
                    Err(error) => self.error(error.to_string()),
                }
            }
            Err(error) => self.error(error.to_string()),
        }
    }

    fn poll_recorder(&mut self) {
        let error = if let Some(recorder) = &self.recorder {
            recorder.drain_into(&mut self.recording_channels);
            recorder.take_error()
        } else {
            None
        };
        if let Some(error) = error {
            self.error(format!("녹음 장치 오류: {error}"));
        }
    }

    fn update_monitoring(&mut self) {
        let Some(engine) = &self.engine else { return };
        let requested = self
            .recording_track
            .and_then(|id| self.project.track(id))
            .is_some_and(|track| track.monitor);
        engine.monitor_bus().set_enabled(requested);
    }

    fn undo(&mut self) {
        if let Some(label) = self.history.undo(&mut self.project) {
            self.selected_clip = self
                .selected_clip
                .filter(|id| self.project.clip(*id).is_some());
            self.mark_changed();
            self.status = format!("실행 취소: {label}");
        }
    }

    fn redo(&mut self) {
        if let Some(label) = self.history.redo(&mut self.project) {
            self.mark_changed();
            self.status = format!("다시 실행: {label}");
        }
    }

    fn finish_discrete_edit(&mut self, label: &str, before: Project) {
        self.history.checkpoint(label, before, &self.project);
        self.mark_changed();
    }

    fn finish_continuous_edit(&mut self, label: &str, before: Project, signals: EditSignals) {
        if signals.drag_started {
            self.history.begin(label, &before);
        }
        if signals.changed {
            self.project.touch();
            self.mark_changed();
        }
        if signals.drag_stopped {
            self.history.commit(&self.project);
        } else if signals.changed && !signals.drag_started && !self.history.in_transaction() {
            self.history.checkpoint(label, before, &self.project);
        }
    }

    fn mark_changed(&mut self) {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        self.rebuild_engine();
    }

    fn rebuild_engine(&mut self) {
        if let Some(engine) = &self.engine
            && let Err(error) = engine.set_project(&self.project, &self.media, self.metronome)
        {
            self.engine_problem = Some(error.to_string());
        }
    }

    fn autosave(&mut self) {
        if self.dirty && !self.autosaving && self.last_autosave.elapsed() >= AUTOSAVE_INTERVAL {
            if let Some(path) = self.recovery_path.clone() {
                self.spawn_save(path, true);
            }
            self.last_autosave = Instant::now();
        }
    }

    fn error(&mut self, message: String) {
        self.status = message.clone();
        self.toast = Some((message, true, Instant::now() + Duration::from_secs(6)));
    }

    fn notice(&mut self, message: String) {
        self.status = message.clone();
        self.toast = Some((message, false, Instant::now() + Duration::from_secs(4)));
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        let wants_keyboard_input = context.egui_wants_keyboard_input();
        let command = context.input(|input| {
            let modifier = input.modifiers.command;
            let shift = input.modifiers.shift;
            if modifier && input.key_pressed(Key::S) && shift {
                Some(UiCommand::SaveAs)
            } else if modifier && input.key_pressed(Key::S) {
                Some(UiCommand::Save)
            } else if modifier && input.key_pressed(Key::O) {
                Some(UiCommand::Open)
            } else if modifier && input.key_pressed(Key::N) {
                Some(UiCommand::New)
            } else if modifier && input.key_pressed(Key::I) {
                Some(UiCommand::Import)
            } else if modifier && input.key_pressed(Key::Z) && shift {
                Some(UiCommand::Redo)
            } else if modifier && input.key_pressed(Key::Z) {
                Some(UiCommand::Undo)
            } else if modifier && input.key_pressed(Key::D) {
                Some(UiCommand::Duplicate)
            } else if modifier && input.key_pressed(Key::T) {
                Some(UiCommand::AddTrack)
            } else if !wants_keyboard_input && input.key_pressed(Key::Space) {
                Some(UiCommand::PlayPause)
            } else if !wants_keyboard_input && input.key_pressed(Key::R) {
                Some(UiCommand::Record)
            } else if !wants_keyboard_input && input.key_pressed(Key::S) {
                Some(UiCommand::Split)
            } else if !wants_keyboard_input && input.key_pressed(Key::Delete) {
                Some(UiCommand::Delete)
            } else if !wants_keyboard_input && input.key_pressed(Key::Home) {
                Some(UiCommand::Stop)
            } else {
                None
            }
        });
        self.execute_ui_command(command);
    }

    fn handle_dropped_files(&mut self, context: &egui::Context) {
        let paths = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<PathBuf>>()
        });
        if paths.is_empty() {
            return;
        }
        if paths.len() == 1 && extension_is(&paths[0], "sonema") {
            self.request_action(PendingAction::OpenPath(paths[0].clone()));
        } else {
            self.import_paths(
                paths
                    .into_iter()
                    .filter(|path| !extension_is(path, "sonema"))
                    .collect(),
            );
        }
    }

    fn close_guard(&mut self, context: &egui::Context) {
        if context.input(|input| input.viewport().close_requested())
            && (self.dirty || self.recorder.is_some())
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.recorder.is_some() {
                self.stop_recording();
            }
            if self.pending_action.is_none() && self.action_after_save.is_none() {
                self.pending_action = Some(PendingAction::Exit);
            }
        }
    }

    fn dialogs(&mut self, context: &egui::Context) {
        self.unsaved_dialog(context);
        self.export_dialog(context);
        self.settings_dialog(context);
        self.about_dialog(context);
        self.toast_ui(context);
    }

    fn unsaved_dialog(&mut self, context: &egui::Context) {
        if self.pending_action.is_none() {
            return;
        }
        let mut choice = None;
        egui::Window::new("저장하지 않은 변경 사항")
            .id(Id::new("unsaved-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(context, |ui| {
                ui.label("현재 프로젝트의 변경 사항을 저장하시겠습니까?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("저장 후 계속").clicked() {
                        choice = Some(0);
                    }
                    if ui.button("저장 안 함").clicked() {
                        choice = Some(1);
                    }
                    if ui.button("취소").clicked() {
                        choice = Some(2);
                    }
                });
            });
        match choice {
            Some(0) => {
                let action = self.pending_action.take();
                if self.begin_save(false) {
                    self.action_after_save = action;
                } else {
                    self.pending_action = action;
                }
            }
            Some(1) => {
                if let Some(action) = self.pending_action.take() {
                    self.dirty = false;
                    self.perform_action(action);
                }
            }
            Some(2) => self.pending_action = None,
            _ => {}
        }
    }

    fn export_dialog(&mut self, context: &egui::Context) {
        if !self.show_export {
            return;
        }
        let mut start = false;
        let mut open = self.show_export;
        egui::Window::new("믹스 WAV 출력")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                egui::Grid::new("export-grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("샘플레이트");
                        egui::ComboBox::from_id_salt("export-rate")
                            .selected_text(format!("{} Hz", self.export_rate))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_rate, 44_100, "44,100 Hz");
                                ui.selectable_value(&mut self.export_rate, 48_000, "48,000 Hz");
                                ui.selectable_value(&mut self.export_rate, 96_000, "96,000 Hz");
                            });
                        ui.end_row();
                        ui.label("비트 깊이");
                        egui::ComboBox::from_id_salt("export-depth")
                            .selected_text(match self.export_depth {
                                WavBitDepth::Pcm16 => "16-bit PCM",
                                WavBitDepth::Pcm24 => "24-bit PCM",
                                WavBitDepth::Float32 => "32-bit Float",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.export_depth,
                                    WavBitDepth::Pcm16,
                                    "16-bit PCM",
                                );
                                ui.selectable_value(
                                    &mut self.export_depth,
                                    WavBitDepth::Pcm24,
                                    "24-bit PCM",
                                );
                                ui.selectable_value(
                                    &mut self.export_depth,
                                    WavBitDepth::Float32,
                                    "32-bit Float",
                                );
                            });
                        ui.end_row();
                    });
                ui.checkbox(&mut self.export_normalize, "피크를 -1 dBFS로 정규화");
                ui.label(
                    RichText::new("트랙 EQ와 컴프레서, 마스터 리미터가 그대로 적용됩니다.")
                        .small()
                        .color(theme::MUTED),
                );
                ui.add_space(8.0);
                if ui
                    .add_enabled(!self.exporting, egui::Button::new("WAV 출력"))
                    .clicked()
                {
                    start = true;
                }
            });
        self.show_export = open;
        if start {
            self.begin_export();
        }
    }

    fn settings_dialog(&mut self, context: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        egui::Window::new("오디오 설정")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                if let Some(engine) = &self.engine {
                    let status = engine.status();
                    ui.label(format!("출력: {}", status.output_name));
                    ui.label(format!(
                        "{} Hz / {}채널",
                        status.sample_rate, status.channels
                    ));
                } else {
                    ui.label(RichText::new("출력 장치를 열지 못했습니다.").color(theme::RED));
                    if let Some(problem) = &self.engine_problem {
                        ui.label(problem);
                    }
                }
                ui.separator();
                ui.label("입력 장치");
                egui::ComboBox::from_id_salt("input-device")
                    .selected_text(self.selected_input.as_deref().unwrap_or("입력 없음"))
                    .show_ui(ui, |ui| {
                        for device in &self.input_devices {
                            let label = if device.is_default {
                                format!("{} (기본)", device.name)
                            } else {
                                device.name.clone()
                            };
                            ui.selectable_value(
                                &mut self.selected_input,
                                Some(device.name.clone()),
                                label,
                            );
                        }
                    });
                if ui.button("장치 목록 새로 고침").clicked() {
                    match list_input_devices() {
                        Ok(devices) => self.input_devices = devices,
                        Err(error) => self.error(error.to_string()),
                    }
                }
                ui.label(
                    RichText::new("입력 모니터링은 입출력 샘플레이트가 같을 때만 켜집니다.")
                        .small()
                        .color(theme::MUTED),
                );
            });
        self.show_settings = open;
    }

    fn about_dialog(&mut self, context: &egui::Context) {
        if !self.show_about {
            return;
        }
        let mut open = self.show_about;
        egui::Window::new("Febius Sonema")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(context, |ui| {
                ui.label(
                    RichText::new("SONEMA")
                        .size(30.0)
                        .strong()
                        .color(theme::ACCENT),
                );
                ui.label("빠르고 정교한 오디오 작업 도구.");
                ui.separator();
                ui.label("Version 0.1.0");
                ui.label("Febius Creator Series");
                ui.label("Copyright © 2026 Febius. All rights reserved.");
            });
        self.show_about = open;
    }

    fn toast_ui(&mut self, context: &egui::Context) {
        let Some((message, is_error, expires)) = &self.toast else {
            return;
        };
        if Instant::now() >= *expires {
            self.toast = None;
            return;
        }
        egui::Area::new(Id::new("toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-18.0, -38.0))
            .order(egui::Order::Foreground)
            .show(context, |ui| {
                egui::Frame::new()
                    .fill(if *is_error {
                        Color32::from_rgb(66, 29, 35)
                    } else {
                        Color32::from_rgb(27, 55, 47)
                    })
                    .stroke(Stroke::new(
                        1.0,
                        if *is_error { theme::RED } else { theme::ACCENT },
                    ))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.label(message);
                    });
            });
    }
}

impl eframe::App for SonemaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.poll_tasks();
        self.poll_recorder();
        self.handle_shortcuts(&context);
        self.handle_dropped_files(&context);
        self.close_guard(&context);
        self.autosave();
        self.ui_body(ui);
        self.dialogs(&context);

        if let Some(engine) = &self.engine {
            engine.collect_retired_sessions();
            if let Some(error) = engine.take_error() {
                self.error(format!("오디오 장치 오류: {error}"));
            }
        }
        if self.engine.as_ref().is_some_and(AudioEngine::is_playing)
            || self.recorder.is_some()
            || self.importing > 0
            || self.exporting
            || self.saving
        {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
        let title = format!(
            "{}{} — Febius Sonema",
            self.project.name,
            if self.dirty { " *" } else { "" }
        );
        context.send_viewport_cmd(egui::ViewportCommand::Title(title));
        if self.should_close {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty
            && let Some(path) = &self.recovery_path
        {
            let _ = save_project(path, &self.project, &self.media);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TrackToggle {
    Mute,
    Solo,
    Arm,
}

#[derive(Debug, Clone, Copy)]
enum UiCommand {
    New,
    Open,
    Save,
    SaveAs,
    Import,
    Export,
    Undo,
    Redo,
    Split,
    Duplicate,
    Delete,
    AddTrack,
    DeleteTrack,
    Record,
    PlayPause,
    Stop,
    Settings,
    About,
}

fn menu_item(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: &str,
    command: &mut Option<UiCommand>,
    value: UiCommand,
) {
    menu_item_enabled(ui, label, shortcut, true, command, value);
}

fn menu_item_enabled(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: &str,
    enabled: bool,
    command: &mut Option<UiCommand>,
    value: UiCommand,
) {
    let text = if shortcut.is_empty() {
        label.to_owned()
    } else {
        format!("{label}    {shortcut}")
    };
    if ui
        .add_enabled(enabled, egui::Button::new(text).frame(false))
        .clicked()
    {
        *command = Some(value);
        ui.close();
    }
}

fn eq_band(
    ui: &mut egui::Ui,
    label: &str,
    band: &mut sonema_core::EqBandSettings,
    signals: &mut EditSignals,
) {
    ui.horizontal(|ui| {
        signals.add(&ui.checkbox(&mut band.enabled, label));
        signals.add(&ui.add(egui::Slider::new(&mut band.gain_db, -12.0..=12.0).suffix(" dB")));
    });
    signals.add(
        &ui.add(
            egui::Slider::new(&mut band.frequency_hz, 30.0..=18_000.0)
                .logarithmic(true)
                .suffix(" Hz"),
        ),
    );
}

fn meter_widget(ui: &mut egui::Ui, peak: f32, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, 2.0, Color32::from_rgb(10, 13, 15));
    let db = gain_to_db(peak).clamp(-60.0, 3.0);
    let fraction = ((db + 60.0) / 63.0).clamp(0.0, 1.0);
    let fill = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - rect.height() * fraction),
        rect.right_bottom(),
    );
    let color = if db > 0.0 {
        theme::RED
    } else if db > -9.0 {
        theme::AMBER
    } else {
        theme::ACCENT
    };
    ui.painter().rect_filled(fill, 2.0, color);
}

fn format_time(seconds: f64) -> String {
    let milliseconds = (seconds.max(0.0) * 1_000.0).round() as u64;
    let hours = milliseconds / 3_600_000;
    let minutes = milliseconds / 60_000 % 60;
    let seconds = milliseconds / 1_000 % 60;
    let millis = milliseconds % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn safe_filename(name: &str) -> String {
    let value = name
        .chars()
        .map(|character| {
            if "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    let trimmed = value.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        "Sonema Project".into()
    } else {
        trimmed.into()
    }
}

fn extension_is(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[allow(dead_code)]
fn _gain_reference(db: f32) -> f32 {
    db_to_gain(db)
}
