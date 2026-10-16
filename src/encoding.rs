//! How exported videos are encoded: frame rate, size, data rate, codec and
//! audio. Set in Settings › Defaults and used by every export.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264Profile {
    Baseline,
    Main,
    High,
}

impl H264Profile {
    fn ffmpeg_name(self) -> &'static str {
        match self {
            H264Profile::Baseline => "baseline",
            H264Profile::Main => "main",
            H264Profile::High => "high",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entropy {
    Cabac,
    Cavlc,
}

/// How the video is fitted to a chosen size when its aspect ratio is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FitMode {
    /// Scaled to fit inside the size; the file is only as big as the video
    FitWithin,
    /// Scaled to fit inside the size, with black bars filling the rest
    Letterbox,
    /// Scaled to fill the size; the overflow is cropped
    Crop,
}

/// Output sizes offered in Settings: (width, height, label).
pub const SIZES: [(u32, u32, &str); 5] = [
    (3840, 2160, "3840 × 2160 4K"),
    (2560, 1440, "2560 × 1440"),
    (1920, 1080, "1920 × 1080 HD"),
    (1280, 720, "1280 × 720"),
    (854, 480, "854 × 480"),
];

/// Frame rates offered in Settings.
pub const FRAME_RATES: [u32; 5] = [24, 25, 30, 50, 60];

/// Data rate range for a custom video data rate, in kbit/s.
pub const VIDEO_KBPS_RANGE: std::ops::RangeInclusive<u32> = 64..=60_000;
/// Data rate range for a custom audio data rate, in kbit/s.
pub const AUDIO_KBPS_RANGE: std::ops::RangeInclusive<u32> = 64..=320;

/// Video data rate used when it is set to automatic.
const AUTO_VIDEO_KBPS: u32 = 15_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportEncoding {
    /// Frames per second; `None` keeps the recording's rate
    pub frame_rate: Option<u32>,
    /// Width and height; `None` keeps the recording's size
    pub size: Option<(u32, u32)>,
    pub preserve_aspect: bool,
    pub fit: FitMode,
    /// Video data rate in kbit/s; `None` is automatic
    pub video_kbps: Option<u32>,
    pub codec: VideoCodec,
    /// A full frame every this many frames
    pub keyframe_every: u32,
    /// H.264 only
    pub profile: H264Profile,
    /// H.264 only; Baseline always uses CAVLC
    pub entropy: Entropy,
    pub encode_audio: bool,
    /// Audio data rate in kbit/s; `None` is automatic
    pub audio_kbps: Option<u32>,
}

impl Default for ExportEncoding {
    fn default() -> Self {
        Self {
            frame_rate: None,
            size: None,
            preserve_aspect: true,
            fit: FitMode::FitWithin,
            video_kbps: None,
            codec: VideoCodec::H264,
            keyframe_every: 60,
            profile: H264Profile::High,
            entropy: Entropy::Cabac,
            encode_audio: true,
            audio_kbps: None,
        }
    }
}

impl ExportEncoding {
    /// Filters added after the overlays: new size and frame rate. Empty when
    /// the recording's size and rate are kept.
    pub fn output_filters(&self) -> Vec<String> {
        let mut filters = Vec::new();
        if let Some((w, h)) = self.size {
            match (self.preserve_aspect, self.fit) {
                (false, _) => filters.push(format!("scale={w}:{h}")),
                (true, FitMode::FitWithin) => filters.push(format!(
                    "scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2"
                )),
                (true, FitMode::Letterbox) => {
                    filters.push(format!("scale={w}:{h}:force_original_aspect_ratio=decrease"));
                    filters.push(format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black"));
                }
                (true, FitMode::Crop) => {
                    filters.push(format!("scale={w}:{h}:force_original_aspect_ratio=increase"));
                    filters.push(format!("crop={w}:{h}"));
                }
            }
            filters.push("setsar=1".to_string());
        }
        if let Some(fps) = self.frame_rate {
            filters.push(format!("fps={fps}"));
        }
        filters
    }

    /// ffmpeg video encoder arguments for `encoder` (e.g. "h264_videotoolbox").
    pub fn video_args(&self, encoder: &str) -> Vec<String> {
        let software = encoder.starts_with("lib");
        let mut args: Vec<String> = vec!["-c:v".into(), encoder.into()];
        match self.video_kbps {
            Some(kbps) => args.extend(["-b:v".into(), format!("{kbps}k")]),
            // Software encoders pick their own rate for a set quality
            None if software => args.extend(["-preset", "veryfast", "-crf", "18"].map(String::from)),
            None => args.extend(["-b:v".into(), format!("{AUTO_VIDEO_KBPS}k")]),
        }
        args.extend(["-g".into(), self.keyframe_every.max(1).to_string()]);
        match self.codec {
            VideoCodec::H264 => {
                // Media Foundation takes neither option
                if !encoder.ends_with("_mf") {
                    let entropy = match (self.profile, self.entropy) {
                        (H264Profile::Baseline, _) | (_, Entropy::Cavlc) => "cavlc",
                        _ => "cabac",
                    };
                    args.extend(["-profile:v".into(), self.profile.ffmpeg_name().into()]);
                    args.extend(["-coder".into(), entropy.into()]);
                }
            }
            // QuickTime and Apple devices only play HEVC tagged as hvc1
            VideoCodec::Hevc => args.extend(["-tag:v", "hvc1"].map(String::from)),
        }
        args.extend(["-pix_fmt", "yuv420p"].map(String::from));
        args
    }

    /// ffmpeg audio encoder arguments.
    pub fn audio_args(&self) -> Vec<String> {
        let mut args: Vec<String> = vec!["-c:a".into(), "aac".into()];
        if let Some(kbps) = self.audio_kbps {
            args.extend(["-b:a".into(), format!("{kbps}k")]);
        }
        args
    }

    /// One line describing the settings, for the export dialog.
    pub fn summary(&self, recording: (u32, u32), recording_fps: u32) -> String {
        let codec = match self.codec {
            VideoCodec::H264 => "H.264",
            VideoCodec::Hevc => "HEVC",
        };
        let (w, h) = self.size.unwrap_or(recording);
        let fps = self.frame_rate.unwrap_or(recording_fps);
        let rate = match self.video_kbps {
            Some(k) => format!("{k} kbit/s"),
            None => "automatic data rate".to_string(),
        };
        let audio = if self.encode_audio { "" } else { " · no audio" };
        format!("{codec} · {w} × {h} · {fps} fps · {rate}{audio}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_keeps_size_and_rate() {
        assert!(ExportEncoding::default().output_filters().is_empty());
    }

    #[test]
    fn test_fit_modes_and_frame_rate() {
        let mut enc = ExportEncoding {
            size: Some((1920, 1080)),
            frame_rate: Some(30),
            fit: FitMode::Letterbox,
            ..Default::default()
        };
        let f = enc.output_filters();
        assert!(f[1].starts_with("pad=1920:1080"));
        assert_eq!(f.last().unwrap(), "fps=30");
        enc.preserve_aspect = false;
        assert_eq!(enc.output_filters()[0], "scale=1920:1080");
    }

    #[test]
    fn test_video_args() {
        let enc = ExportEncoding {
            video_kbps: Some(6000),
            profile: H264Profile::Baseline,
            ..Default::default()
        };
        let args = enc.video_args("h264_videotoolbox").join(" ");
        assert!(args.contains("-b:v 6000k"));
        assert!(args.contains("-profile:v baseline -coder cavlc"));
        let auto = ExportEncoding::default().video_args("libx264").join(" ");
        assert!(auto.contains("-crf 18"));
        let hevc = ExportEncoding { codec: VideoCodec::Hevc, ..Default::default() };
        assert!(hevc.video_args("hevc_videotoolbox").join(" ").contains("-tag:v hvc1"));
    }
}
