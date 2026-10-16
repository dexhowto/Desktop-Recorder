use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    HEVC,
}

impl VideoCodec {
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            Self::H264 => "H.264 (AVC) - Maximum Compatibility",
            Self::HEVC => "HEVC (H.265) - Smaller File Size",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputFormat {
    MP4,
    MOV,
}

impl OutputFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::MP4 => "mp4",
            Self::MOV => "mov",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderConfig {
    pub selected_display_id: Option<u32>,
    pub capture_system_audio: bool,
    pub capture_microphone: bool,
    pub capture_camera: bool,
    pub shows_cursor: bool,
    pub export_ai_telemetry: bool,
    pub best_quality: bool,
    pub auto_open_editor: bool,
    /// Show a Save dialog (name and folder) when a recording stops
    pub ask_save_location: bool,
    /// Record this app's own windows too (full screen). Off: the dock, editor,
    /// countdown and live ⌘ arrows are left out of the video.
    pub record_own_windows: bool,
    pub fps: u32,
    pub codec: VideoCodec,
    pub format: OutputFormat,
    pub output_dir: PathBuf,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let movies_dir = PathBuf::from(&home).join("Movies");
        let output_dir = if movies_dir.exists() {
            movies_dir
        } else {
            let desktop_dir = PathBuf::from(&home).join("Desktop");
            if desktop_dir.exists() {
                desktop_dir
            } else {
                PathBuf::from(".")
            }
        };

        Self {
            selected_display_id: None,
            capture_system_audio: false,
            capture_microphone: false,
            capture_camera: false,
            shows_cursor: true,
            export_ai_telemetry: true,
            best_quality: true,
            auto_open_editor: true,
            ask_save_location: true,
            record_own_windows: false,
            fps: 60,
            codec: VideoCodec::H264,
            format: OutputFormat::MP4,
            output_dir,
        }
    }
}

impl RecorderConfig {
    /// `Screen Recording 2026-10-15 at 23.56.19` for a recording starting now.
    pub fn recording_base_name() -> String {
        let now = chrono::Local::now();
        format!(
            "Screen Recording {} at {}",
            now.format("%Y-%m-%d"),
            now.format("%H.%M.%S")
        )
    }

    /// Final name for a recording: `<base>_1.mp4` in the save folder, with the next
    /// free number when that name is taken.
    pub fn final_output_path(&self, base: &str) -> PathBuf {
        numbered_free_path(&self.output_dir, base, self.format.extension())
    }

    /// Hidden file the recording is written to while it runs, in the save folder
    /// (same disk, so moving it to its final name afterwards is instant). Using a
    /// separate name means the Save dialog never collides with the file being written.
    pub fn temporary_recording_path(&self) -> PathBuf {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.output_dir.join(format!(
            ".DexHowTo recording {millis}.{}",
            self.format.extension()
        ))
    }

    pub fn generate_telemetry_file_path(video_path: &std::path::Path) -> PathBuf {
        video_path.with_extension("mouse.jsonl")
    }
}

/// `<dir>/<base>_<n>.<ext>` with the lowest `n` (from 1) not used by a video or its
/// telemetry file.
pub fn numbered_free_path(dir: &std::path::Path, base: &str, ext: &str) -> PathBuf {
    (1..10_000)
        .map(|n| dir.join(format!("{base}_{n}.{ext}")))
        .find(|p| !p.exists() && !p.with_extension("mouse.jsonl").exists())
        .unwrap_or_else(|| dir.join(format!("{base}_1.{ext}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_names_get_the_next_free_number() {
        let dir = std::env::temp_dir().join(format!("dexhowto_rec_names_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = "Screen Recording 2026-10-15 at 23.56.19";
        assert_eq!(numbered_free_path(&dir, base, "mp4"), dir.join(format!("{base}_1.mp4")));
        std::fs::write(dir.join(format!("{base}_1.mp4")), b"x").unwrap();
        assert_eq!(numbered_free_path(&dir, base, "mp4"), dir.join(format!("{base}_2.mp4")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
