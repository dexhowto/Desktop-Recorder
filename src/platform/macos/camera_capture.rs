//! Camera recording, saved as its own file next to the screen recording.
//!
//! ScreenCaptureKit cannot capture cameras, so ffmpeg records the camera through
//! AVFoundation. Frames are stamped with the wall clock as they arrive; when the
//! recording stops, the camera clip is trimmed (or padded) so it starts exactly
//! with the first screen frame, and saved as `<recording>.camera.mp4`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Sizes tried in order; the first one the camera accepts is used.
const CAMERA_SIZES: [&str; 2] = ["1920x1080", "1280x720"];

pub struct CameraCapture {
    child: Child,
    temp_path: PathBuf,
}

/// Seconds since the Unix epoch, the clock both the camera and screen are measured with.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl CameraCapture {
    /// Starts recording the default camera (AVFoundation device 0).
    pub fn start(temp_path: PathBuf) -> Result<Self, String> {
        let ffmpeg = crate::editor::find_ffmpeg_bin();
        let mut last_error = String::new();
        for size in CAMERA_SIZES {
            let _ = std::fs::remove_file(&temp_path);
            let mut child = Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-f", "avfoundation", "-framerate", "30", "-video_size", size])
                .args(["-use_wallclock_as_timestamps", "1"])
                .args(["-i", "0:none"])
                .args(["-copyts", "-c:v", "h264_videotoolbox", "-b:v", "8M"])
                .arg(&temp_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to start the camera recorder: {e}"))?;

            // An unsupported size makes ffmpeg exit right away; otherwise it keeps running
            let deadline = Instant::now() + Duration::from_millis(900);
            let mut exited = None;
            while Instant::now() < deadline {
                if let Ok(Some(status)) = child.try_wait() {
                    exited = Some(status);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            match exited {
                None => return Ok(Self { child, temp_path }),
                Some(_) => {
                    let mut err = String::new();
                    if let Some(mut e) = child.stderr.take() {
                        let _ = std::io::Read::read_to_string(&mut e, &mut err);
                    }
                    last_error = err.trim().lines().last().unwrap_or("").to_string();
                }
            }
        }
        Err(format!("The camera could not be started: {last_error}"))
    }

    /// Stops the camera and writes the clip, aligned to the screen video, to `out_path`.
    ///
    /// `screen_start_unix` is when the first screen frame arrived and
    /// `duration_secs` how long the screen video is.
    pub fn finish(
        mut self,
        out_path: &Path,
        screen_start_unix: Option<f64>,
        duration_secs: f64,
    ) -> Result<PathBuf, String> {
        // "q" makes ffmpeg finish the file cleanly
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = stdin.write_all(b"q");
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() > deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }

        let result = align_clip(&self.temp_path, out_path, screen_start_unix, duration_secs);
        let _ = std::fs::remove_file(&self.temp_path);
        result.map(|_| out_path.to_path_buf())
    }
}

impl Drop for CameraCapture {
    /// Never leave the camera running, e.g. when the screen recording fails to start.
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_file(&self.temp_path);
    }
}

/// Reads when the first camera frame arrived (the clip keeps wall-clock timestamps).
fn clip_start_unix(path: &Path) -> Option<f64> {
    let ffprobe = crate::editor::find_ffmpeg_bin().replace("ffmpeg", "ffprobe");
    let out = Command::new(ffprobe)
        .args(["-v", "error", "-show_entries", "format=start_time", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn align_clip(
    temp: &Path,
    out_path: &Path,
    screen_start_unix: Option<f64>,
    duration_secs: f64,
) -> Result<(), String> {
    if !temp.exists() {
        return Err("The camera did not record anything".to_string());
    }
    let camera_start = clip_start_unix(temp);
    // Positive: the camera started before the screen, so cut that part off
    let lead = match (camera_start, screen_start_unix) {
        (Some(c), Some(s)) if c > 1_000_000_000.0 => s - c,
        _ => 0.0,
    };

    let mut cmd = Command::new(crate::editor::find_ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if lead > 0.0 {
        cmd.args(["-ss", &format!("{lead:.3}")]);
    }
    cmd.arg("-i").arg(temp);
    if lead < 0.0 {
        // The camera started late: hold its first frame until it catches up
        cmd.args([
            "-vf",
            &format!("tpad=start_duration={:.3}:start_mode=clone", -lead),
        ]);
    }
    cmd.args(["-t", &format!("{duration_secs:.3}")])
        .args(["-c:v", "h264_videotoolbox", "-b:v", "8M"])
        .args(["-fps_mode", "cfr", "-r", "30"])
        .args(["-movflags", "+faststart"])
        .arg(out_path);

    let out = cmd
        .output()
        .map_err(|e| format!("Failed to save the camera clip: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(out_path);
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Failed to save the camera clip: {}",
            err.lines().last().unwrap_or("ffmpeg failed")
        ));
    }
    Ok(())
}
