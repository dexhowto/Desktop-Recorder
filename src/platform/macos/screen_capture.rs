//! Screen recording with ScreenCaptureKit: the display is written straight to
//! the video file by `SCRecordingOutput`. The microphone and camera are started
//! here too, because they hook into the same stream and must start before it.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use screencapturekit::recording_output::{
    RecordingCallbacks, SCRecordingOutput, SCRecordingOutputCodec,
    SCRecordingOutputConfiguration, SCRecordingOutputFileType,
};
use screencapturekit::shareable_content::SCShareableContent;
use screencapturekit::stream::configuration::{color_space, SCCaptureResolutionType, SCStreamConfiguration};
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::stream::sc_stream::SCStream;

type CGDisplayModeRef = *mut std::ffi::c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCopyDisplayMode(display: u32) -> CGDisplayModeRef;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeRelease(mode: CGDisplayModeRef);
}

use crate::config::{OutputFormat, RecorderConfig, VideoCodec};
use crate::camera_capture::CameraCapture;
use crate::mic_capture::MicCapture;
use crate::recorder::started_unix_millis;

/// A running screen recording.
pub struct ScreenCapture {
    stream: SCStream,
    recording_output: SCRecordingOutput,
    /// Fires once the recording file is completely written
    recording_finished: Receiver<()>,
    pub mic: Option<MicCapture>,
    pub camera: Option<CameraCapture>,
    /// Wall-clock time (Unix seconds) the first screen frame arrived
    pub first_frame_unix: std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    /// The video file already has a system audio track
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
    // 1. Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create destination folder: {e}"))?;
    }

    // Remove existing file if it exists so SCRecordingOutput has a fresh file
    if output_path.exists() {
        let _ = std::fs::remove_file(output_path);
    }

    // 2. Query displays from ScreenCaptureKit
    let shareable = SCShareableContent::get()
        .map_err(|e| format!("Failed to query screens (Screen Recording permission may be required): {e:?}"))?;

    let displays = shareable.displays();
    if displays.is_empty() {
        return Err("No displays detected".to_string());
    }

    // Find requested display or fallback to first
    let (chosen_index, display) = if let Some(target_id) = config.selected_display_id {
        displays
            .iter()
            .enumerate()
            .find(|(_, d)| d.display_id() == target_id)
            .unwrap_or((0, &displays[0]))
    } else {
        (0, &displays[0])
    };

    let (target_w, target_h, is_retina) = if config.best_quality {
        let (pw, ph) = unsafe {
            let mode = CGDisplayCopyDisplayMode(display.display_id());
            if !mode.is_null() {
                let w = CGDisplayModeGetPixelWidth(mode) as u32;
                let h = CGDisplayModeGetPixelHeight(mode) as u32;
                CGDisplayModeRelease(mode);
                (w, h)
            } else {
                (display.width() * 2, display.height() * 2)
            }
        };

        if pw > 0 && ph > 0 {
            (pw, ph, pw > display.width())
        } else {
            (display.width() * 2, display.height() * 2, true)
        }
    } else {
        (display.width(), display.height(), false)
    };

    let display_label = if is_retina {
        format!(
            "Display {} ({} × {} Retina • Best)",
            chosen_index + 1,
            target_w,
            target_h
        )
    } else {
        format!(
            "Display {} ({} × {})",
            chosen_index + 1,
            target_w,
            target_h
        )
    };

    // 3. Build Content Filter. Unless the user asked for a full-screen recording,
    // leave this app's own windows out: while recording, the dock shows the
    // countdown and live ⌘ arrows on screen.
    let own_pid = std::process::id() as i32;
    let own_app = shareable
        .applications()
        .into_iter()
        .find(|a| a.process_id() == own_pid)
        .filter(|_| !config.record_own_windows);
    let builder = SCContentFilter::create().with_display(display);
    let builder = match &own_app {
        Some(app) => builder.with_excluding_applications(&[app], &[]),
        None => builder.with_excluding_windows(&[]),
    };
    let filter = builder
        .build()
        .map_err(|e| format!("Failed to build content filter: {e:?}"))?;

    // 4. Configure Stream
    let mut stream_config = SCStreamConfiguration::new();
    stream_config.set_width(target_w);
    stream_config.set_height(target_h);
    stream_config.set_fps(config.fps);
    stream_config.set_shows_cursor(config.shows_cursor);
    stream_config.set_captures_audio(config.capture_system_audio);

    if config.best_quality {
        stream_config.set_scales_to_fit(false);
        stream_config.set_queue_depth(8);
        if let Err(e) = stream_config.set_capture_resolution_type(SCCaptureResolutionType::Best) {
            eprintln!("Warning: could not set capture resolution type Best: {e:?}");
        }
        if let Err(e) = stream_config.set_color_space_name(color_space::DISPLAY_P3) {
            eprintln!("Warning: could not set Display P3 color space: {e:?}");
        }
    }

    if config.capture_microphone {
        if let Err(e) = stream_config.set_captures_microphone(true) {
            eprintln!("Warning: could not enable microphone capture: {e:?}");
        }
    }

    // 5. Configure Recording Output
    let sc_codec = match config.codec {
        VideoCodec::H264 => SCRecordingOutputCodec::H264,
        VideoCodec::HEVC => SCRecordingOutputCodec::HEVC,
    };

    let sc_file_type = match config.format {
        OutputFormat::MP4 => SCRecordingOutputFileType::MP4,
        OutputFormat::MOV => SCRecordingOutputFileType::MOV,
    };

    let rec_config = SCRecordingOutputConfiguration::new()
        .map_err(|e| format!("Failed to create recording configuration: {e:?}"))?
        .with_output_url(output_path)
        .map_err(|e| format!("Invalid output file path: {e:?}"))?
        .with_video_codec(sc_codec)
        .with_output_file_type(sc_file_type);

    let (finished_tx, recording_finished) = channel::<()>();
    let finished_tx = std::sync::Mutex::new(finished_tx);
    let callbacks = RecordingCallbacks::new()
        .on_finish(move || {
            if let Ok(tx) = finished_tx.lock() {
                let _ = tx.send(());
            }
        })
        .on_fail(|error| eprintln!("Recording output failed: {error}"));
    let recording_output = SCRecordingOutput::new_with_delegate(&rec_config, callbacks)
        .ok_or_else(|| "Failed to initialize SCRecordingOutput (requires macOS 15.0+)".to_string())?;

    // 6. Create Stream and start capture
    let mut stream = SCStream::new(&filter, &stream_config)
        .map_err(|e| format!("Failed to initialize capture stream: {e:?}"))?;

    // Note when the first screen frame arrives, to line the camera clip up with it
    let first_frame_unix = std::sync::Arc::new(std::sync::Mutex::new(None::<f64>));
    let first_frame = first_frame_unix.clone();
    stream
        .add_output_handler(
            move |_sample: screencapturekit::cm::CMSampleBuffer, _| {
                if let Ok(mut t) = first_frame.lock() {
                    if t.is_none() {
                        *t = Some(crate::camera_capture::unix_now());
                    }
                }
            },
            screencapturekit::stream::output_type::SCStreamOutputType::Screen,
        )
        .map_err(|e| format!("Failed to receive video frames: {e:?}"))?;

    // The camera is recorded by ffmpeg; start it first so it is already running
    // when the first screen frame arrives (the lead is trimmed off later)
    let camera = if config.capture_camera
        && crate::av_access::status(crate::av_access::Device::Camera)
            == crate::av_access::Access::Allowed
    {
        let temp = std::env::temp_dir().join(format!(
            "dexhowto_camera_{}_{}.mp4",
            std::process::id(),
            started_unix_millis()
        ));
        match CameraCapture::start(temp) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("Warning: {e}");
                None
            }
        }
    } else {
        None
    };

    // The recording file never contains the microphone, so collect it separately
    let mic = if config.capture_microphone
        && crate::av_access::status(crate::av_access::Device::Microphone)
            == crate::av_access::Access::Allowed
    {
        let raw_path = std::env::temp_dir().join(format!(
            "dexhowto_mic_{}_{}.f32",
            std::process::id(),
            started_unix_millis()
        ));
        match MicCapture::attach(&mut stream, raw_path) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("Warning: {e}");
                None
            }
        }
    } else {
        None
    };

    stream
        .add_recording_output(&recording_output)
        .map_err(|e| format!("Failed to attach recording output: {e:?}"))?;

    stream
        .start_capture()
        .map_err(|e| format!("Failed to start screen capture: {e:?}"))?;

    Ok(StartedCapture {
        capture: ScreenCapture {
            stream,
            recording_output,
            recording_finished,
            mic,
            camera,
            first_frame_unix,
            captures_system_audio: config.capture_system_audio,
        },
        display_label,
        display_id: display.display_id(),
        width: target_w,
        height: target_h,
    })
}

impl ScreenCapture {
    /// Stops capturing and waits until the video file is completely written.
    pub fn stop(&mut self) {
        let _ = self.stream.stop_capture();
        let _ = self.stream.remove_recording_output(&self.recording_output);
        // Wait until the file is completely written before touching it
        let _ = self.recording_finished.recv_timeout(Duration::from_secs(10));
    }
}
