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

use std::sync::OnceLock;

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
