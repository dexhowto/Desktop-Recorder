// No console window next to the app in release builds on Windows
// (build with `--features console` to keep it for log output)
#![cfg_attr(all(windows, not(debug_assertions), not(feature = "console")), windows_subsystem = "windows")]

mod app;
mod arrows;
mod audio_merge;
mod clicks;
mod config;
mod editor;
mod encoding;
mod focus;
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
    return run_windows(native_options);
    #[cfg(not(windows))]
    run(native_options)
}

/// Opens the window on Windows, where graphics setup can crash the whole app
/// in a VM or with a broken driver.
///
/// wgpu is limited to Direct3D 12 (with its software fallback, WARP), because
/// probing Vulkan drivers crashed under x64 emulation on Windows for ARM.
/// A start that crashes before its first frame is remembered, and the next
/// start uses OpenGL. `--glow` forces OpenGL; WGPU_BACKEND=dx12|vulkan|gl
/// overrides the wgpu backend.
#[cfg(windows)]
fn run_windows(native_options: eframe::NativeOptions) -> eframe::Result<()> {
    use platform::windows::{log_line, report_fatal, startup};

    let crashed = startup::previous_crash();
    let use_glow = std::env::args().any(|a| a == "--glow") || crashed == Some(startup::Renderer::Wgpu);
    if crashed == Some(startup::Renderer::Glow) {
        report_fatal(
            "DexHowTo Recorder closed while opening its window, with Direct3D and with OpenGL.\n\n\
             It will try Direct3D again now.",
        );
    }

    if use_glow {
        if crashed == Some(startup::Renderer::Wgpu) {
            log_line("The last start crashed with Direct3D, using OpenGL");
        }
        let mut options = native_options;
        options.renderer = eframe::Renderer::Glow;
        log_line("Opening the window with OpenGL");
        startup::attempt(startup::Renderer::Glow);
        let result = run(options);
        if let Err(e) = &result {
            startup::clear();
            report_fatal(&format!("DexHowTo Recorder could not open its window.\n\nOpenGL: {e}"));
        }
        return result;
    }

    let mut options = native_options;
    options.renderer = eframe::Renderer::Wgpu;
    let backend = std::env::var("WGPU_BACKEND").ok();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        if backend.is_none() {
            setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        }
    }
    log_line(&format!(
        "Opening the window with wgpu ({})",
        backend.as_deref().unwrap_or("dx12")
    ));
    startup::attempt(startup::Renderer::Wgpu);
    let first = run(options);
    let Err(wgpu_error) = first else {
        return first;
    };
    log_line(&format!("wgpu renderer failed: {wgpu_error}"));

    let mut options = self::native_options();
    options.renderer = eframe::Renderer::Glow;
    log_line("Opening the window with OpenGL");
    startup::attempt(startup::Renderer::Glow);
    let second = run(options);
    if let Err(glow_error) = &second {
        startup::clear();
        report_fatal(&format!(
            "DexHowTo Recorder could not open its window.\n\n\
             Direct3D: {wgpu_error}\nOpenGL: {glow_error}"
        ));
    }
    second
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
            // Windows: transparent wgpu windows show white or black there, so the
            // dock is opaque and the overlay is a native window (see `window`)
            .with_transparent(crate::window::OVERLAY_IN_DOCK)
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
