// No console window next to the app in release builds on Windows
// (build with `--features console` to keep it for log output)
#![cfg_attr(all(windows, not(debug_assertions), not(feature = "console")), windows_subsystem = "windows")]

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
    // Without a console, a failure would close the app without a word:
    // report it in a message box and in the log file instead.
    #[cfg(windows)]
    {
        platform::windows::init_log();
        platform::windows::install_panic_report();
    }
    platform::init();
    let native_options = native_options();

    #[cfg(windows)]
    {
        // wgpu (Direct3D 12 / Vulkan) first; OpenGL when no GPU adapter works.
        // `--glow` starts with OpenGL; WGPU_BACKEND=dx12|vulkan|gl picks the wgpu backend.
        if std::env::args().any(|a| a == "--glow") {
            let mut options = native_options;
            options.renderer = eframe::Renderer::Glow;
            platform::trace("Opening the window with OpenGL (--glow)");
            let result = run(options);
            if let Err(e) = &result {
                platform::windows::report_fatal(&format!("DexHowTo Recorder could not open its window.\n\nOpenGL: {e}"));
            }
            return result;
        }
        let mut options = native_options;
        options.renderer = eframe::Renderer::Wgpu;
        platform::trace(&format!(
            "Opening the window with wgpu (WGPU_BACKEND={})",
            std::env::var("WGPU_BACKEND").unwrap_or_else(|_| "default".to_string())
        ));
        let first = run(options);
        let Err(wgpu_error) = first else {
            return first;
        };
        platform::windows::log_line(&format!("wgpu renderer failed: {wgpu_error}"));
        let mut options = self::native_options();
        options.renderer = eframe::Renderer::Glow;
        platform::trace("Opening the window with OpenGL");
        let second = run(options);
        if let Err(glow_error) = &second {
            platform::windows::report_fatal(&format!(
                "DexHowTo Recorder could not open its window.\n\n\
                 Direct3D/Vulkan: {wgpu_error}\nOpenGL: {glow_error}"
            ));
        }
        second
    }
    #[cfg(not(windows))]
    run(native_options)
}

fn native_options() -> eframe::NativeOptions {
    platform::trace("Reading the main display");
    let (origin_x, origin_y, screen_w, screen_h) = crate::display::get_main_display_bounds_points();
    platform::trace(&format!("Main display: {screen_w} x {screen_h} points at ({origin_x}, {origin_y})"));
    let dock_w = 940.0;
    let dock_h = 210.0;
    let dock_x = origin_x + (screen_w - dock_w) / 2.0;
    let dock_y = origin_y + (screen_h - dock_h - 115.0).max(20.0);

    eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([dock_w, dock_h])
            .with_min_inner_size([860.0, 200.0])
            .with_position([dock_x, dock_y])
            .with_decorations(false)
            .with_transparent(true)
            .with_title("DexHowTo Recorder")
            .with_icon(ui_kit::app_icon()),
        ..Default::default()
    }
}

fn run(native_options: eframe::NativeOptions) -> eframe::Result<()> {
    eframe::run_native(
        "DexHowTo Recorder",
        native_options,
        Box::new(|cc| Ok(Box::new(DexHowToApp::new(cc)))),
    )
}
