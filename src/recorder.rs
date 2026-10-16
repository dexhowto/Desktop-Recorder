use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::RecorderConfig;
use crate::screen_capture::ScreenCapture;
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
    /// The platform screen recording, with its microphone and camera
    capture: ScreenCapture,
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

                    // Start the platform screen capture
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
    let started = crate::screen_capture::start(config, output_path)?;
    let display_label = started.display_label;
    let (target_w, target_h) = (started.width, started.height);

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
            started.display_id,
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
            capture: started.capture,
            output_path: output_path.clone(),
            started_at,
            telemetry,
            telemetry_path: telemetry_path.clone(),
        },
        display_label,
        telemetry_path,
    ))
}

pub(crate) fn started_unix_millis() -> u128 {
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

    // Stop capture and wait until the file is completely written
    session.capture.stop();

    // Add the microphone audio collected during the recording. If this fails the
    // video itself is still fine, so keep it and only report the problem.
    if let Some(track) = session.capture.mic.take().and_then(|mic| mic.finish()) {
        if let Err(e) = crate::audio_merge::merge_into_video(
            &session.output_path,
            &track,
            session.capture.captures_system_audio,
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
    let camera_path = session.capture.camera.take().and_then(|camera| {
        let video_secs = crate::editor::probe_video_metadata(&session.output_path).0;
        let first_frame = session.capture.first_frame_unix.lock().ok().and_then(|t| *t);
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
