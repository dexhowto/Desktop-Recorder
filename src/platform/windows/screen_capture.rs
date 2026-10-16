//! Screen recording on Windows: Windows Graphics Capture delivers the monitor's
//! frames and Media Foundation encodes them to H.264 / HEVC in an MP4 file
//! (both through the `windows-capture` crate).
//!
//! This app's own windows are left out of the video with
//! `SetWindowDisplayAffinity` (see `window.rs`), not here.
//!
//! TODO(windows): system audio (WASAPI loopback) and the camera.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::camera_capture::{unix_now, CameraCapture};
use crate::config::{RecorderConfig, VideoCodec};
use crate::mic_capture::MicCapture;
use crate::recorder::started_unix_millis;

/// Settings handed to the capture thread.
struct HandlerFlags {
    width: u32,
    height: u32,
    fps: u32,
    codec: VideoCodec,
    path: PathBuf,
    first_frame_unix: Arc<Mutex<Option<f64>>>,
}

/// Receives frames on the capture thread and feeds them to the encoder.
struct FrameHandler {
    encoder: Option<VideoEncoder>,
    first_frame_unix: Arc<Mutex<Option<f64>>>,
}

impl GraphicsCaptureApiHandler for FrameHandler {
    type Flags = HandlerFlags;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let f = ctx.flags;
        let sub_type = match f.codec {
            VideoCodec::H264 => VideoSettingsSubType::H264,
            VideoCodec::HEVC => VideoSettingsSubType::HEVC,
        };
        let video = VideoSettingsBuilder::new(f.width, f.height)
            .sub_type(sub_type)
            .frame_rate(f.fps)
            .bitrate(bitrate_for(f.width, f.height, f.fps));
        // The microphone is merged in afterwards, like on macOS
        let encoder = VideoEncoder::new(
            video,
            AudioSettingsBuilder::default().disabled(true),
            ContainerSettingsBuilder::default(),
            &f.path,
        )
        .map_err(|e| format!("Failed to start the video encoder: {e}"))?;
        Ok(Self {
            encoder: Some(encoder),
            first_frame_unix: f.first_frame_unix,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if let Ok(mut t) = self.first_frame_unix.lock() {
            if t.is_none() {
                *t = Some(unix_now());
            }
        }
        if let Some(encoder) = self.encoder.as_mut() {
            encoder
                .send_frame(frame)
                .map_err(|e| format!("Failed to encode a frame: {e}"))?;
        }
        Ok(())
    }
}

/// Bits per second for the recording: about 0.1 bit per pixel per frame,
/// 8–40 Mbit/s.
fn bitrate_for(width: u32, height: u32, fps: u32) -> u32 {
    let bits = width as f64 * height as f64 * fps.max(1) as f64 * 0.1;
    bits.clamp(8_000_000.0, 40_000_000.0) as u32
}

/// A running screen recording.
pub struct ScreenCapture {
    control: Option<CaptureControl<FrameHandler, String>>,
    pub mic: Option<MicCapture>,
    pub camera: Option<CameraCapture>,
    /// Wall-clock time (Unix seconds) the first screen frame arrived
    pub first_frame_unix: Arc<Mutex<Option<f64>>>,
    /// The video file already has a system audio track (never on Windows yet)
    pub captures_system_audio: bool,
}

/// What `start` reports about the recording it started.
pub struct StartedCapture {
    pub capture: ScreenCapture,
    pub display_label: String,
    pub display_id: u32,
    /// Size of the recorded video in pixels
    pub width: u32,
    pub height: u32,
}

/// Starts recording the display chosen in `config` to `output_path`.
pub fn start(config: &RecorderConfig, output_path: &PathBuf) -> Result<StartedCapture, String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create destination folder: {e}"))?;
    }
    if output_path.exists() {
        let _ = std::fs::remove_file(output_path);
    }

    if !GraphicsCaptureApi::is_supported().unwrap_or(false) {
        return Err("Screen capture needs Windows 10 version 1903 or later".to_string());
    }

    let monitors = super::display::monitors();
    if monitors.is_empty() {
        return Err("No displays detected".to_string());
    }
    let (chosen_index, monitor) = config
        .selected_display_id
        .and_then(|id| monitors.iter().enumerate().find(|(_, m)| m.id == id))
        .or_else(|| monitors.iter().enumerate().find(|(_, m)| m.is_primary))
        .unwrap_or((0, &monitors[0]));

    // Windows Graphics Capture always delivers the monitor's physical pixels.
    // H.264 needs even sizes.
    // TODO(windows): honour "best quality" off by scaling down to points.
    let width = monitor.width() & !1;
    let height = monitor.height() & !1;
    let display_label = format!("Display {} ({} × {})", chosen_index + 1, width, height);

    // The microphone starts first so it already runs when the first frame arrives
    let first_frame_unix = Arc::new(Mutex::new(None::<f64>));
    let mic = if config.capture_microphone {
        let raw_path = std::env::temp_dir().join(format!(
            "dexhowto_mic_{}_{}.f32",
            std::process::id(),
            started_unix_millis()
        ));
        match MicCapture::start(raw_path, first_frame_unix.clone()) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("Warning: {e}");
                None
            }
        }
    } else {
        None
    };

    let cursor = match (config.shows_cursor, GraphicsCaptureApi::is_cursor_settings_supported()) {
        (true, Ok(true)) => CursorCaptureSettings::WithCursor,
        (false, Ok(true)) => CursorCaptureSettings::WithoutCursor,
        _ => CursorCaptureSettings::Default,
    };
    // The yellow "being captured" border can only be turned off on Windows 11
    let border = if GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    };
    let interval = if GraphicsCaptureApi::is_minimum_update_interval_supported().unwrap_or(false) {
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / config.fps.max(1) as f64))
    } else {
        MinimumUpdateIntervalSettings::Default
    };

    let settings = Settings::new(
        Monitor::from_raw_hmonitor(monitor.handle.0),
        cursor,
        border,
        SecondaryWindowSettings::Default,
        interval,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        HandlerFlags {
            width,
            height,
            fps: config.fps,
            codec: config.codec,
            // The encoder always writes an MP4 container, also for a `.mov` name
            path: output_path.clone(),
            first_frame_unix: first_frame_unix.clone(),
        },
    );
    let control = FrameHandler::start_free_threaded(settings)
        .map_err(|e| format!("Failed to start screen capture: {e:?}"))?;

    Ok(StartedCapture {
        capture: ScreenCapture {
            control: Some(control),
            mic,
            camera: None,
            first_frame_unix,
            captures_system_audio: false,
        },
        display_label,
        display_id: monitor.id,
        width,
        height,
    })
}

impl ScreenCapture {
    /// Stops capturing and waits until the video file is completely written.
    pub fn stop(&mut self) {
        let Some(control) = self.control.take() else {
            return;
        };
        let handler = control.callback();
        if let Err(e) = control.stop() {
            eprintln!("Warning: screen capture did not stop cleanly: {e:?}");
        }
        let encoder = handler.lock().encoder.take();
        if let Some(encoder) = encoder {
            if let Err(e) = encoder.finish() {
                eprintln!("Warning: failed to finish the video file: {e}");
            }
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
