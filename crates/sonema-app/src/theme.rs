use std::path::Path;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(15, 18, 21);
pub const PANEL: Color32 = Color32::from_rgb(22, 26, 30);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(29, 34, 39);
pub const BORDER: Color32 = Color32::from_rgb(47, 54, 60);
pub const TEXT: Color32 = Color32::from_rgb(226, 231, 234);
pub const MUTED: Color32 = Color32::from_rgb(139, 151, 158);
pub const ACCENT: Color32 = Color32::from_rgb(67, 213, 164);
pub const RED: Color32 = Color32::from_rgb(239, 87, 103);
pub const AMBER: Color32 = Color32::from_rgb(239, 177, 73);

pub fn install(context: &egui::Context) {
    install_system_font(context);
    context.set_theme(egui::Theme::Dark);
    let mut visuals = Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(11, 14, 16);
    visuals.faint_bg_color = PANEL_RAISED;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.bg_fill = PANEL_RAISED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(40, 47, 52);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_fill = Color32::from_rgb(47, 59, 60);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.selection.bg_fill = Color32::from_rgb(31, 91, 74);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.override_text_color = Some(TEXT);
    context.set_visuals(visuals);
    context.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(8.0, 7.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.slider_width = 140.0;
        style.visuals.window_corner_radius = egui::CornerRadius::same(5);
    });
}

fn install_system_font(context: &egui::Context) {
    let candidates = [
        "C:/Windows/Fonts/malgun.ttf",
        "C:/Windows/Fonts/malgunsl.ttf",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
    ];
    let Some(bytes) = candidates
        .iter()
        .find_map(|path| std::fs::read(Path::new(path)).ok())
    else {
        return;
    };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("sonema-system".into(), FontData::from_owned(bytes).into());
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "sonema-system".into());
    }
    context.set_fonts(fonts);
}
