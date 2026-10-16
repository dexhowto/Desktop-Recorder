//! Windows: Windows Graphics Capture + Media Foundation (through the
//! `windows-capture` crate), WASAPI microphone (through `cpal`) and Win32.
//!
//! Not done yet (phase 1): system audio (WASAPI loopback), camera (ffmpeg
//! dshow) and an installer.

pub mod av_access;
pub mod camera_capture;
pub mod display;
pub mod mic_capture;
pub mod overlay_window;
pub mod screen_capture;
pub mod startup;
pub mod window;

use std::io::Write;
use std::sync::OnceLock;

/// Log file for errors, since release builds have no console: `log.txt` next
/// to the exe, or `%APPDATA%\DexHowTo Recorder\log.txt` when that folder is
/// not writable.
pub fn log_path() -> std::path::PathBuf {
    log_file().0.clone()
}

/// The log, opened once per run and written in order. Appending through a new
/// handle per line breaks on some network and VM shared folders (the writes
/// land at a wrong offset), so the file is started fresh and kept open.
fn log_file() -> &'static (std::path::PathBuf, std::sync::Mutex<Option<std::fs::File>>) {
    static LOG: OnceLock<(std::path::PathBuf, std::sync::Mutex<Option<std::fs::File>>)> = OnceLock::new();
    LOG.get_or_init(|| {
        let create = |path: std::path::PathBuf| std::fs::File::create(&path).ok().map(|file| (path, file));
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("log.txt")));
        let fallback = crate::platform::app_support_dir().join("log.txt");
        // Keep the previous run's log, which may hold why it closed
        let beside_exe = beside_exe.inspect(|path| {
            let _ = std::fs::rename(path, path.with_file_name("log.previous.txt"));
        });
        let opened = beside_exe.and_then(create).or_else(|| {
            let _ = std::fs::create_dir_all(crate::platform::app_support_dir());
            create(fallback.clone())
        });
        match opened {
            Some((path, file)) => (path, std::sync::Mutex::new(Some(file))),
            None => (fallback, std::sync::Mutex::new(None)),
        }
    })
}

/// Writes a line to the log file (and to the console, when there is one).
pub fn log_line(message: &str) {
    eprintln!("{message}");
    if let Ok(mut file) = log_file().1.lock() {
        if let Some(file) = file.as_mut() {
            let _ = writeln!(file, "[{}] {message}", chrono::Local::now().to_rfc3339());
            let _ = file.flush();
        }
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

/// Logs a native crash (access violation…), which closes the app without a
/// Rust panic.
unsafe extern "system" fn log_native_crash(
    info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    let record = unsafe { info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) };
    match record {
        Some(r) => log_line(&format!(
            "Native crash: exception 0x{:08X} at address {:?}",
            r.ExceptionCode.0 as u32, r.ExceptionAddress
        )),
        None => log_line("Native crash (no details)"),
    }
    // EXCEPTION_CONTINUE_SEARCH: let Windows end the app as usual
    0
}

/// Shows a crash in a message box and the log file instead of closing silently.
pub fn install_panic_report() {
    unsafe {
        windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(log_native_crash));
    }
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
