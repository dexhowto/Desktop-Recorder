//! Microphone recording on Windows (WASAPI through `cpal`).
//!
//! Like on macOS, the samples are saved as mono f32 with wall-clock timing and
//! merged into the finished video with ffmpeg, lined up against the first
//! video frame (`crate::audio_merge`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

use crate::audio_merge::MicTrack;
use crate::camera_capture::unix_now;

struct MicState {
    writer: Option<BufWriter<File>>,
    sample_rate: f64,
    /// Wall-clock time (Unix seconds) of the first microphone sample
    first_mic_unix: Option<f64>,
    frames_written: u64,
}

/// Collects microphone audio while a recording runs.
pub struct MicCapture {
    stream: Stream,
    state: Arc<Mutex<MicState>>,
    raw_path: PathBuf,
    /// Wall-clock time the first screen frame arrived (time zero in the video)
    first_video_unix: Arc<Mutex<Option<f64>>>,
}

impl MicCapture {
    /// Starts recording the default microphone into `raw_path`.
    pub fn start(raw_path: PathBuf, first_video_unix: Arc<Mutex<Option<f64>>>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "No microphone found".to_string())?;
        let supported = device
            .default_input_config()
            .map_err(|e| format!("Failed to read the microphone format: {e}"))?;
        let format = supported.sample_format();
        let config = supported.config();
        let channels = config.channels.max(1) as usize;
        let rate = config.sample_rate as f64;

        let file = File::create(&raw_path)
            .map_err(|e| format!("Failed to create microphone buffer file: {e}"))?;
        let state = Arc::new(Mutex::new(MicState {
            writer: Some(BufWriter::new(file)),
            sample_rate: rate,
            first_mic_unix: None,
            frames_written: 0,
        }));

        let on_error = |e| eprintln!("Microphone error: {e}");
        let s = state.clone();
        let stream = match format {
            SampleFormat::F32 => device.build_input_stream(
                config,
                move |data: &[f32], _: &_| write_samples(&s, data.iter().copied(), data.len(), channels),
                on_error,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                config,
                move |data: &[i16], _: &_| {
                    write_samples(&s, data.iter().map(|v| *v as f32 / 32768.0), data.len(), channels)
                },
                on_error,
                None,
            ),
            SampleFormat::I32 => device.build_input_stream(
                config,
                move |data: &[i32], _: &_| {
                    write_samples(&s, data.iter().map(|v| *v as f32 / 2_147_483_648.0), data.len(), channels)
                },
                on_error,
                None,
            ),
            other => return Err(format!("Unsupported microphone format: {other:?}")),
        }
        .map_err(|e| format!("Failed to open the microphone: {e}"))?;
        stream
            .play()
            .map_err(|e| format!("Failed to start the microphone: {e}"))?;

        Ok(Self {
            stream,
            state,
            raw_path,
            first_video_unix,
        })
    }

    /// Stops collecting. Returns `None` when no microphone audio arrived.
    pub fn finish(self) -> Option<MicTrack> {
        let _ = self.stream.pause();
        drop(self.stream);
        let mut s = self.state.lock().ok()?;
        if let Some(mut w) = s.writer.take() {
            let _ = w.flush();
        }
        let video = self.first_video_unix.lock().ok().and_then(|t| *t)?;
        let mic = s.first_mic_unix?;
        if s.frames_written == 0 {
            return None;
        }
        Some(MicTrack {
            raw_path: self.raw_path.clone(),
            sample_rate: s.sample_rate,
            offset_secs: mic - video,
        })
    }
}

/// Appends one buffer of interleaved samples as mono f32, padding gaps with
/// silence so the audio stays in sync with the wall clock.
fn write_samples(
    state: &Mutex<MicState>,
    samples: impl Iterator<Item = f32>,
    len: usize,
    channels: usize,
) {
    let Ok(mut s) = state.lock() else {
        return;
    };
    let frames = len / channels;
    if frames == 0 {
        return;
    }
    let rate = s.sample_rate;
    // The buffer was captured over the time it covers, ending about now
    let pts = unix_now() - frames as f64 / rate;
    let first = *s.first_mic_unix.get_or_insert(pts);

    let mut mono = vec![0.0f32; frames];
    for (i, v) in samples.enumerate() {
        if let Some(out) = mono.get_mut(i / channels) {
            *out += v;
        }
    }
    let scale = 1.0 / channels as f32;

    let frames_written = s.frames_written;
    let Some(writer) = s.writer.as_mut() else {
        return;
    };
    // Fill dropped time with silence (more than 50ms behind the clock; the
    // callback timing is less exact than the capture clock on macOS)
    let expected = ((pts - first) * rate).round().max(0.0) as u64;
    let mut written = frames_written;
    if expected > written + (rate * 0.05) as u64 {
        let gap = expected - written;
        let _ = writer.write_all(&vec![0u8; gap as usize * 4]);
        written += gap;
    }

    let mut bytes = Vec::with_capacity(frames * 4);
    for v in mono {
        bytes.extend_from_slice(&(v * scale).to_le_bytes());
    }
    let _ = writer.write_all(&bytes);
    s.frames_written = written + frames as u64;
}
