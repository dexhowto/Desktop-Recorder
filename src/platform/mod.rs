//! Operating-system specific code.
//!
//! `macos` and `windows` expose the same modules with the same functions and
//! types; `main.rs` re-exports the one for the current system at the crate
//! root (`crate::display`, `crate::screen_capture`, …), so the rest of the app
//! does not need to know which system it runs on.
//!
//! Small helpers that only differ in a path or a command live here.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as current;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use self::windows as current;

/// Whether the camera tile can record on this system.
// TODO(windows): record the camera with ffmpeg dshow.
pub const CAMERA_SUPPORTED: bool = cfg!(target_os = "macos");

/// Whether "System audio" can record on this system.
// TODO(windows): capture system audio with WASAPI loopback.
pub const SYSTEM_AUDIO_SUPPORTED: bool = cfg!(target_os = "macos");

/// Whether the system asks the user for screen, microphone and camera access
/// (macOS privacy settings). Windows has no such prompt for desktop apps.
pub const HAS_PRIVACY_PERMISSIONS: bool = cfg!(target_os = "macos");

/// Name of the system file manager, for button labels ("Show in Finder").
pub const FILE_MANAGER: &str = if cfg!(windows) { "Explorer" } else { "Finder" };

/// Prepares the process before any window or display is used.
pub fn init() {
    #[cfg(windows)]
    self::windows::display::init_dpi_awareness();
}

/// Writes a line to the Windows log (`log.txt`): startup steps and errors.
pub fn trace(step: &str) {
    #[cfg(windows)]
    self::windows::log_line(step);
    #[cfg(not(windows))]
    let _ = step;
}

/// The first frame is on screen: the window opened without a crash.
pub fn first_frame_drawn() {
    trace("First frame drawn");
    #[cfg(windows)]
    self::windows::startup::clear();
}

/// A command that never opens a console window (ffmpeg, ffprobe…).
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// CREATE_NO_WINDOW
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Opens a file or folder with its default app.
pub fn open_path(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Shows a file selected in the file manager.
pub fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer needs "/select," and the path as one argument
        let _ = Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
}

/// The user's home folder.
pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(std::env::var(var).unwrap_or_else(|_| ".".to_string()))
}

/// The system folder for videos (~/Movies on macOS, ~/Videos on Windows).
pub fn videos_dir() -> PathBuf {
    home_dir().join(if cfg!(windows) { "Videos" } else { "Movies" })
}

/// Folder where the app keeps its settings.
pub fn app_support_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home_dir().join("AppData").join("Roaming"));
        base.join("DexHowTo Recorder")
    }
    #[cfg(not(windows))]
    {
        home_dir().join("Library/Application Support/DexHowTo Recorder")
    }
}

/// ffmpeg arguments for the hardware H.264 encoder used at export.
pub fn h264_encoder_args() -> Vec<&'static str> {
    #[cfg(target_os = "macos")]
    {
        vec!["-c:v", "h264_videotoolbox", "-b:v", "15M"]
    }
    #[cfg(windows)]
    {
        self::windows::ffmpeg_h264_encoder_args()
    }
}
