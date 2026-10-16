// No console window next to the app in release builds on Windows
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod arrows;
mod audio_merge;
mod config;
mod editor;
mod keys;
mod overlay;
mod platform;
mod prefs;
mod export;
mod recorder;
mod recording_files;
mod telemetry;
mod tray;
mod ui_kit;

// The platform modules, under the same names on every system
use platform::current::{av_access, camera_capture, display, mic_capture, screen_capture, window};

use app::DexHowToApp;

fn main() -> eframe::Result<()> {
    platform::init();
    let (origin_x, origin_y, screen_w, screen_h) = crate::display::get_main_display_bounds_points();
    let dock_w = 940.0;
    let dock_h = 210.0;
    let dock_x = origin_x + (screen_w - dock_w) / 2.0;
    let dock_y = origin_y + (screen_h - dock_h - 115.0).max(20.0);

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([dock_w, dock_h])
            .with_min_inner_size([860.0, 200.0])
            .with_position([dock_x, dock_y])
            .with_decorations(false)
            .with_transparent(true)
            .with_title("DexHowTo Recorder")
            .with_icon(ui_kit::app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "DexHowTo Recorder",
        native_options,
        Box::new(|cc| Ok(Box::new(DexHowToApp::new(cc)))),
    )
}
