#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod theme;
mod timeline;

use app::SonemaApp;
use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Febius Sonema")
            .with_inner_size([1_440.0, 900.0])
            .with_min_inner_size([1_024.0, 680.0])
            .with_app_id("com.febius.sonema"),
        renderer: eframe::Renderer::Glow,
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native(
        "com.febius.sonema",
        options,
        Box::new(|creation_context| Ok(Box::new(SonemaApp::new(creation_context)))),
    )
}
