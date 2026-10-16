//! Microphone recording.
//!
//! ScreenCaptureKit delivers microphone audio to the app, but its file writer
//! (`SCRecordingOutput`) leaves the microphone out of the video file. So the
//! microphone samples are saved here with their timestamps, and merged into the
//! finished video with ffmpeg, lined up against the first video frame.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::stream::output_type::SCStreamOutputType;
use screencapturekit::stream::sc_stream::SCStream;

use crate::audio_merge::MicTrack;

/// kAudioFormatFlagIsFloat
const FORMAT_FLAG_IS_FLOAT: u32 = 1;

struct MicState {
    writer: Option<BufWriter<File>>,
    sample_rate: f64,
    /// Timestamp of the first microphone sample, in seconds of the capture clock
    first_mic_pts: Option<f64>,
    /// Timestamp of the first video frame, which is time zero in the video file
    first_video_pts: Option<f64>,
    frames_written: u64,
}

/// Collects microphone audio while a recording runs.
pub struct MicCapture {
    state: Arc<Mutex<MicState>>,
    raw_path: PathBuf,
}

impl MicCapture {
    /// Registers microphone and screen handlers on `stream`. Call before `start_capture`.
    pub fn attach(stream: &mut SCStream, raw_path: PathBuf) -> Result<Self, String> {
        let file = File::create(&raw_path)
            .map_err(|e| format!("Failed to create microphone buffer file: {e}"))?;
        let state = Arc::new(Mutex::new(MicState {
            writer: Some(BufWriter::new(file)),
            sample_rate: 48_000.0,
            first_mic_pts: None,
            first_video_pts: None,
            frames_written: 0,
        }));

        let mic_state = state.clone();
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, _| {
                    if let Ok(mut s) = mic_state.lock() {
                        write_mic_sample(&mut s, &sample);
                    }
                },
                SCStreamOutputType::Microphone,
            )
            .map_err(|e| format!("Failed to receive microphone audio: {e:?}"))?;

        let video_state = state.clone();
        stream
            .add_output_handler(
                move |sample: CMSampleBuffer, _| {
                    if let Ok(mut s) = video_state.lock() {
                        if s.first_video_pts.is_none() {
                            s.first_video_pts = sample.presentation_timestamp().as_seconds();
                        }
                    }
                },
                SCStreamOutputType::Screen,
            )
            .map_err(|e| format!("Failed to receive video timing: {e:?}"))?;

        Ok(Self { state, raw_path })
    }

    /// Stops collecting. Returns `None` when no microphone audio arrived.
    pub fn finish(self) -> Option<MicTrack> {
        let mut s = self.state.lock().ok()?;
        if let Some(mut w) = s.writer.take() {
            let _ = w.flush();
        }
        let (mic, video) = (s.first_mic_pts?, s.first_video_pts?);
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

/// Appends one microphone buffer as mono f32 samples, padding gaps with silence
/// so the audio stays in sync with the capture clock.
fn write_mic_sample(s: &mut MicState, sample: &CMSampleBuffer) {
    let Some(pts) = sample.presentation_timestamp().as_seconds() else {
        return;
    };
    let Some(format) = sample.format_description() else {
        return;
    };
    let rate = format.audio_sample_rate().unwrap_or(48_000.0);
    let is_float = format.audio_format_flags().unwrap_or(FORMAT_FLAG_IS_FLOAT) & FORMAT_FLAG_IS_FLOAT != 0;
    let bits = format.audio_bits_per_channel().unwrap_or(32);
    let frames = sample.num_samples().max(0) as usize;
    let Ok(list) = sample.audio_buffer_list() else {
        return;
    };

    // Mix every channel down to mono
    let mut mono = vec![0.0f32; frames];
    let mut contributing = 0usize;
    for i in 0..list.num_buffers() {
        let Some(buffer) = list.get(i) else { continue };
        let per_buffer = buffer.number_channels.max(1) as usize;
        let data = buffer.data();
        let values: Vec<f32> = if is_float && bits == 32 {
            data.chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        } else if bits == 16 {
            data.chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect()
        } else {
            continue;
        };
        // Interleaved buffers hold `per_buffer` channels per frame
        for (f, out) in mono.iter_mut().enumerate() {
            for c in 0..per_buffer {
                if let Some(v) = values.get(f * per_buffer + c) {
                    *out += *v;
                }
            }
        }
        contributing += per_buffer;
    }
    if contributing == 0 {
        return;
    }
    let scale = 1.0 / contributing as f32;

    let Some(writer) = s.writer.as_mut() else {
        return;
    };
    let first = *s.first_mic_pts.get_or_insert(pts);
    s.sample_rate = rate;

    // Fill dropped time with silence (more than 20ms behind the clock)
    let expected = ((pts - first) * rate).round().max(0.0) as u64;
    if expected > s.frames_written + (rate * 0.02) as u64 {
        let gap = expected - s.frames_written;
        let silence = vec![0u8; gap as usize * 4];
        let _ = writer.write_all(&silence);
        s.frames_written += gap;
    }

    let mut bytes = Vec::with_capacity(frames * 4);
    for v in mono {
        bytes.extend_from_slice(&(v * scale).to_le_bytes());
    }
    let _ = writer.write_all(&bytes);
    s.frames_written += frames as u64;
}
