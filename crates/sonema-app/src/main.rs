#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod theme;
mod timeline;

use std::sync::Arc;

use app::SonemaApp;
use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Febius Sonema")
            .with_inner_size([1_440.0, 900.0])
            .with_min_inner_size([1_024.0, 680.0])
            .with_icon(Arc::new(app_icon()))
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

fn app_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = vec![0_u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let edge_x = x.min(SIZE - 1 - x);
            let edge_y = y.min(SIZE - 1 - y);
            let corner_x = 9_u32.saturating_sub(edge_x);
            let corner_y = 9_u32.saturating_sub(edge_y);
            let outside_corner = corner_x * corner_x + corner_y * corner_y > 81;
            let offset = ((y * SIZE + x) * 4) as usize;
            if outside_corner {
                continue;
            }

            rgba[offset..offset + 4].copy_from_slice(&[21, 25, 29, 255]);
            let phase = x as f32 / (SIZE - 1) as f32 * std::f32::consts::TAU * 1.5;
            let wave_y = 31.5 + phase.sin() * 13.0;
            if (y as f32 - wave_y).abs() <= 2.2 && (6..=57).contains(&x) {
                rgba[offset..offset + 4].copy_from_slice(&[72, 224, 173, 255]);
            }
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}
