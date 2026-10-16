//! Camera recording on Windows: not available yet.
//!
//! TODO(windows): record the camera with ffmpeg (`-f dshow -i video="…"`) the
//! way the macOS version uses AVFoundation, and align it the same way.

use std::path::{Path, PathBuf};

/// Seconds since the Unix epoch, the clock both the camera and screen are measured with.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Never created on Windows yet; the camera tile is disabled.
pub struct CameraCapture {
    _private: (),
}

impl CameraCapture {
    pub fn finish(
        self,
        _out_path: &Path,
        _screen_start_unix: Option<f64>,
        _duration_secs: f64,
    ) -> Result<PathBuf, String> {
        Err("Camera recording is not available on Windows yet".to_string())
    }
}
