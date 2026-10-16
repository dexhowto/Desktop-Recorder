//! Windows: Windows Graphics Capture + Media Foundation (through the
//! `windows-capture` crate), WASAPI microphone (through `cpal`) and Win32.
//!
//! Not done yet (phase 1): system audio (WASAPI loopback), camera (ffmpeg
//! dshow) and an installer.

pub mod av_access;
pub mod camera_capture;
pub mod display;
pub mod mic_capture;
pub mod screen_capture;
pub mod window;

use std::io::Write;
use std::sync::OnceLock;

/// Log file for errors, since release builds have no console:
/// `%APPDATA%\DexHowTo Recorder\log.txt`.
pub fn log_path() -> std::path::PathBuf {
    crate::platform::app_support_dir().join("log.txt")
}

/// Appends a line to the log file (and to the console, when there is one).
pub fn log_line(message: &str) {
    eprintln!("{message}");
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "[{}] {message}", chrono::Local::now().to_rfc3339());
    }
}

/// Logs `message` and shows it in a message box.
pub fn report_fatal(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    use windows::core::HSTRING;
    log_line(message);
    let text = format!("{message}\n\nDetails are in {}", log_path().display());
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("DexHowTo Recorder"), MB_OK | MB_ICONERROR);
    }
}

/// Sends warnings and errors from eframe, winit and wgpu to the log file.
struct FileLogger;

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            log_line(&format!("{} {}: {}", record.level(), record.target(), record.args()));
        }
    }

    fn flush(&self) {}
}

/// Starts the log: a first line, and warnings from the libraries.
pub fn init_log() {
    static LOGGER: FileLogger = FileLogger;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
    log_line(&format!("Starting DexHowTo Recorder {}", env!("CARGO_PKG_VERSION")));
}

/// Shows a crash in a message box and the log file instead of closing silently.
pub fn install_panic_report() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let message = format!("DexHowTo Recorder crashed (thread {thread}): {info}");
        // Only the UI thread ends the app; other threads are logged
        if thread == "main" {
            report_fatal(&message);
        } else {
            log_line(&message);
        }
    }));
}

/// ffmpeg arguments for H.264 at export: the Media Foundation hardware encoder
/// when this ffmpeg has it, otherwise libx264.
pub fn ffmpeg_h264_encoder_args() -> Vec<&'static str> {
    static HAS_MF: OnceLock<bool> = OnceLock::new();
    let has_mf = *HAS_MF.get_or_init(|| {
        crate::platform::command(crate::editor::find_ffmpeg_bin())
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains("h264_mf"))
            .unwrap_or(false)
    });
    if has_mf {
        vec!["-c:v", "h264_mf", "-b:v", "15M"]
    } else {
        vec!["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"]
    }
}
