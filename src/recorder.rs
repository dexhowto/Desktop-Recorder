use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

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
use crate::telemetry::TelemetryHandle;

#[derive(Debug, Clone)]
pub enum RecorderCommand {
    StartWithCountdown {
        config: RecorderConfig,
        output_path: PathBuf,
        countdown_secs: u32,
    },
    CancelCountdown,
    Stop,
}

#[derive(Debug, Clone)]
pub enum RecorderEvent {
    CountdownTick {
        remaining_secs: u32,
    },
    CountdownCancelled,
    Started {
        output_path: PathBuf,
        display_label: String,
        telemetry_path: Option<PathBuf>,
    },
    Tick {
        duration_secs: f64,
        file_size_bytes: u64,
    },
    Stopped {
        output_path: PathBuf,
        telemetry_path: Option<PathBuf>,
        /// Camera clip saved next to the recording, when the camera was on
        camera_path: Option<PathBuf>,
        duration_secs: f64,
        file_size_bytes: u64,
    },
    Error(String),
}

pub struct RecorderHandle {
    command_tx: Sender<RecorderCommand>,
    event_rx: Receiver<RecorderEvent>,
}

impl RecorderHandle {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = channel::<RecorderCommand>();
        let (evt_tx, evt_rx) = channel::<RecorderEvent>();

        thread::spawn(move || {
            run_recorder_loop(cmd_rx, evt_tx);
        });

        Self {
            command_tx: cmd_tx,
            event_rx: evt_rx,
        }
    }

    pub fn start_with_countdown(&self, config: RecorderConfig, output_path: PathBuf, countdown_secs: u32) {
        let _ = self.command_tx.send(RecorderCommand::StartWithCountdown {
            config,
            output_path,
            countdown_secs,
        });
    }

    pub fn cancel_countdown(&self) {
        let _ = self.command_tx.send(RecorderCommand::CancelCountdown);
    }

    pub fn stop(&self) {
        let _ = self.command_tx.send(RecorderCommand::Stop);
    }

    pub fn try_recv(&self) -> Option<RecorderEvent> {
        self.event_rx.try_recv().ok()
    }
}

struct ActiveSession {
    stream: SCStream,
    recording_output: SCRecordingOutput,
    /// Fires once the recording file is completely written
    recording_finished: Receiver<()>,
    mic: Option<MicCapture>,
    camera: Option<CameraCapture>,
    /// Wall-clock time (Unix seconds) the first screen frame arrived
    first_frame_unix: std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    captures_system_audio: bool,
    output_path: PathBuf,
    started_at: Instant,
    telemetry: Option<TelemetryHandle>,
    telemetry_path: Option<PathBuf>,
}

fn run_recorder_loop(cmd_rx: Receiver<RecorderCommand>, evt_tx: Sender<RecorderEvent>) {
    let mut current_session: Option<ActiveSession> = None;

    loop {
        // While not recording, wait blocking for command
        if current_session.is_none() {
            match cmd_rx.recv() {
                Ok(RecorderCommand::StartWithCountdown {
                    config,
                    output_path,
                    countdown_secs,
                }) => {
                    let mut cancelled = false;

                    // Execute countdown in the background thread
                    for rem in (1..=countdown_secs).rev() {
                        let _ = evt_tx.send(RecorderEvent::CountdownTick {
                            remaining_secs: rem,
                        });

                        let deadline = Instant::now() + Duration::from_secs(1);
                        while Instant::now() < deadline {
                            let now = Instant::now();
                            let wait_dur = if deadline > now {
                                (deadline - now).min(Duration::from_millis(100))
                            } else {
                                Duration::from_millis(0)
                            };

                            match cmd_rx.recv_timeout(wait_dur) {
                                Ok(RecorderCommand::CancelCountdown | RecorderCommand::Stop) => {
                                    cancelled = true;
                                    break;
                                }
                                Ok(RecorderCommand::StartWithCountdown { .. }) => {}
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                    cancelled = true;
                                    break;
                                }
                            }
                        }

                        if cancelled {
                            break;
                        }
                    }

                    if cancelled {
                        let _ = evt_tx.send(RecorderEvent::CountdownCancelled);
                        continue;
                    }

                    // Start ScreenCaptureKit capture stream
                    match start_recording(&config, &output_path) {
                        Ok((session, display_label, telemetry_path)) => {
                            let _ = evt_tx.send(RecorderEvent::Started {
                                output_path: session.output_path.clone(),
                                display_label,
                                telemetry_path,
                            });
                            current_session = Some(session);
                        }
                        Err(err) => {
                            let _ = evt_tx.send(RecorderEvent::Error(err));
                        }
                    }
                }
                Ok(RecorderCommand::CancelCountdown | RecorderCommand::Stop) => {
                    // Not recording, ignore
                }
                Err(_) => {
                    // GUI closed channel, terminate thread
                    break;
                }
            }
        } else {
            // While recording, check for Stop command or poll recording stats
            match cmd_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(RecorderCommand::Stop | RecorderCommand::CancelCountdown) => {
                    if let Some(session) = current_session.take() {
                        match stop_recording(session) {
                            Ok((path, telemetry_path, camera_path, duration, size)) => {
                                let _ = evt_tx.send(RecorderEvent::Stopped {
                                    output_path: path,
                                    telemetry_path,
                                    camera_path,
                                    duration_secs: duration,
                                    file_size_bytes: size,
                                });
                            }
                            Err(err) => {
                                let _ = evt_tx.send(RecorderEvent::Error(err));
                            }
                        }
                    }
                }
                Ok(RecorderCommand::StartWithCountdown { .. }) => {
                    // Already recording, ignore
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Update stats tick
                    if let Some(ref session) = current_session {
                        let duration_secs = session.started_at.elapsed().as_secs_f64();
                        let file_size_bytes = std::fs::metadata(&session.output_path)
                            .map(|m| m.len())
                            .unwrap_or(0);

                        let _ = evt_tx.send(RecorderEvent::Tick {
                            duration_secs,
                            file_size_bytes,
                        });
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // GUI exited, cleanup recording
                    if let Some(session) = current_session.take() {
                        let _ = stop_recording(session);
                    }
                    break;
                }
            }
        }
    }
}

fn start_recording(
    config: &RecorderConfig,
    output_path: &PathBuf,
) -> Result<(ActiveSession, String, Option<PathBuf>), String> {
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

    let started_at = Instant::now();

    // 7. Start Agentic AI mouse telemetry logger if enabled in config
    let (telemetry, telemetry_path) = if config.export_ai_telemetry {
        let t_path = RecorderConfig::generate_telemetry_file_path(output_path);
        let video_name = output_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("recording");

        match TelemetryHandle::start(
            &t_path,
            video_name,
            config.fps,
            display.display_id(),
            target_w,
            target_h,
            started_at,
        ) {
            Ok(handle) => {
                eprintln!("Agentic AI telemetry logging active at: {:?}", t_path);
                (Some(handle), Some(t_path))
            }
            Err(e) => {
                eprintln!("Warning: Failed to start AI telemetry logger: {e}");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    Ok((
        ActiveSession {
            stream,
            recording_output,
            recording_finished,
            mic,
            camera,
            first_frame_unix,
            captures_system_audio: config.capture_system_audio,
            output_path: output_path.clone(),
            started_at,
            telemetry,
            telemetry_path: telemetry_path.clone(),
        },
        display_label,
        telemetry_path,
    ))
}

fn started_unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[allow(clippy::type_complexity)]
fn stop_recording(
    mut session: ActiveSession,
) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>, f64, u64), String> {
    let duration_secs = session.started_at.elapsed().as_secs_f64();

    // Stop stream and flush recording
    let _ = session.stream.stop_capture();
    let _ = session.stream.remove_recording_output(&session.recording_output);
    // Wait until the file is completely written before touching it
    let _ = session.recording_finished.recv_timeout(Duration::from_secs(10));

    // Add the microphone audio collected during the recording. If this fails the
    // video itself is still fine, so keep it and only report the problem.
    if let Some(track) = session.mic.take().and_then(MicCapture::finish) {
        if let Err(e) = crate::mic_capture::merge_into_video(
            &session.output_path,
            &track,
            session.captures_system_audio,
        ) {
            eprintln!("Warning: {e}");
        }
    }

    // Stop telemetry logger and flush JSONL file
    let final_telemetry_path = if let Some(telemetry) = session.telemetry.take() {
        let path = telemetry.jsonl_path.clone();
        match telemetry.stop() {
            Ok(summary) => {
                eprintln!(
                    "Telemetry saved: {} records in {:.2}s to {:?}",
                    summary.total_records, summary.duration_secs, summary.path
                );
                Some(summary.path)
            }
            Err(e) => {
                eprintln!("Warning: Error stopping telemetry: {e}");
                Some(path)
            }
        }
    } else {
        session.telemetry_path
    };

    // Wait slightly for filesystem to flush
    thread::sleep(Duration::from_millis(200));

    // Save the camera clip next to the recording, lined up with its first frame
    let camera_path = session.camera.take().and_then(|camera| {
        let video_secs = crate::editor::probe_video_metadata(&session.output_path).0;
        let first_frame = session.first_frame_unix.lock().ok().and_then(|t| *t);
        let out = camera_clip_path(&session.output_path);
        match camera.finish(&out, first_frame, video_secs) {
            Ok(path) => Some(path),
            Err(e) => {
                eprintln!("Warning: {e}");
                None
            }
        }
    });

    let file_size_bytes = std::fs::metadata(&session.output_path)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok((
        session.output_path,
        final_telemetry_path,
        camera_path,
        duration_secs,
        file_size_bytes,
    ))
}

/// `Screen Recording ….mp4` → `Screen Recording ….camera.mp4`
fn camera_clip_path(video: &std::path::Path) -> PathBuf {
    video.with_extension("camera.mp4")
}
