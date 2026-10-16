//! Adds a separately recorded microphone track to a finished recording.
//!
//! The platform microphone capture saves mono f32 samples to a raw file while
//! the screen records; ffmpeg then merges them into the video, lined up against
//! the first video frame.

use std::path::{Path, PathBuf};

/// Microphone audio ready to be merged into the video.
pub struct MicTrack {
    pub raw_path: PathBuf,
    pub sample_rate: f64,
    /// Seconds after the start of the video at which the microphone audio begins
    /// (negative when it began before the first frame)
    pub offset_secs: f64,
}

/// Merges the microphone track into `video` in place. The video stream is copied;
/// when the video already has system audio, both are mixed into one track.
pub fn merge_into_video(video: &Path, track: &MicTrack, video_has_audio: bool) -> Result<(), String> {
    let ffmpeg = crate::editor::find_ffmpeg_bin();
    let tmp = video.with_extension(format!(
        "withmic.{}",
        video.extension().and_then(|e| e.to_str()).unwrap_or("mp4")
    ));

    // Line the microphone up with the first video frame
    let align = if track.offset_secs >= 0.0 {
        let ms = (track.offset_secs * 1000.0).round() as i64;
        format!("adelay={ms}:all=1")
    } else {
        format!("atrim=start={:.4},asetpts=PTS-STARTPTS", -track.offset_secs)
    };
    let filter = if video_has_audio {
        format!(
            "[1:a]{align},aformat=channel_layouts=stereo[mic];\
             [0:a][mic]amix=inputs=2:duration=first:normalize=0[a]"
        )
    } else {
        format!("[1:a]{align},aformat=channel_layouts=stereo,apad[a]")
    };

    let out = crate::platform::command(&ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(video)
        .arg("-f")
        .arg("f32le")
        .arg("-ar")
        .arg(format!("{}", track.sample_rate.round() as u32))
        .arg("-ac")
        .arg("1")
        .arg("-i")
        .arg(&track.raw_path)
        .arg("-filter_complex")
        .arg(&filter)
        .arg("-map")
        .arg("0:v")
        .arg("-map")
        .arg("[a]")
        .arg("-c:v")
        .arg("copy")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("192k")
        .arg("-shortest")
        .arg(&tmp)
        .output()
        .map_err(|e| format!("Failed to run ffmpeg to add the microphone: {e}"))?;

    let _ = std::fs::remove_file(&track.raw_path);
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Could not add the microphone to the video: {}",
            err.lines().last().unwrap_or("ffmpeg failed")
        ));
    }
    std::fs::rename(&tmp, video).map_err(|e| format!("Failed to save the video with microphone: {e}"))
}

impl Drop for MicTrack {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.raw_path);
    }
}
