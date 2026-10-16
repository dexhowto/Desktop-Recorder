use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::Local;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonState {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub origin_x: f64,
    pub origin_y: f64,
    #[serde(default)]
    pub point_width: f64,
    #[serde(default)]
    pub point_height: f64,
    #[serde(default)]
    pub scale_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TelemetryRecord {
    #[serde(rename = "session_start")]
    SessionStart {
        version: String,
        created_at: String,
        video_file: String,
        fps: u32,
        display: DisplayInfo,
    },
    #[serde(rename = "mouse")]
    Mouse {
        t: f64,
        time_ms: u64,
        frame: u64,
        x: f64,
        y: f64,
        norm_x: f64,
        norm_y: f64,
        is_inside: bool,
        global_x: f64,
        global_y: f64,
        event: String,
        button: Option<String>,
        buttons: ButtonState,
        modifiers: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        click_duration_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        click_count: Option<u32>,
    },
    #[serde(rename = "session_end")]
    SessionEnd {
        t: f64,
        duration_secs: f64,
        total_records: u64,
    },
}

#[derive(Debug, Clone)]
pub struct TelemetrySummary {
    pub total_records: u64,
    pub duration_secs: f64,
    pub path: PathBuf,
}

pub struct TelemetryHandle {
    stop_tx: Option<Sender<()>>,
    join_handle: Option<JoinHandle<Result<TelemetrySummary, String>>>,
    pub jsonl_path: PathBuf,
}

impl TelemetryHandle {
    pub fn start(
        jsonl_path: &Path,
        video_filename: &str,
        fps: u32,
        display_id: u32,
        display_width: u32,
        display_height: u32,
        started_at: Instant,
    ) -> Result<Self, String> {
        let file = File::create(jsonl_path)
            .map_err(|e| format!("Failed to create telemetry JSONL file at {jsonl_path:?}: {e}"))?;

        let mut writer = BufWriter::new(file);

        // Query display bounds to properly offset multi-monitor coordinates
        // macOS: global points; Windows: physical pixels (see `crate::display`)
        let (origin_x, origin_y, bounds_w, bounds_h) = crate::display::telemetry_display_bounds(display_id);
        let point_w = if bounds_w > 0.0 { bounds_w } else { display_width as f64 };
        let point_h = if bounds_h > 0.0 { bounds_h } else { display_height as f64 };
        let scale_factor = if point_w > 0.0 { display_width as f64 / point_w } else { 1.0 };

        let display_info = DisplayInfo {
            id: display_id,
            width: display_width,
            height: display_height,
            origin_x,
            origin_y,
            point_width: point_w,
            point_height: point_h,
            scale_factor,
        };

        // Emit initial session_start metadata line
        let start_record = TelemetryRecord::SessionStart {
            version: "1.0".to_string(),
            created_at: Local::now().to_rfc3339(),
            video_file: video_filename.to_string(),
            fps,
            display: display_info.clone(),
        };

        let start_json = serde_json::to_string(&start_record)
            .map_err(|e| format!("Failed to serialize session_start record: {e}"))?;
        writeln!(writer, "{start_json}")
            .map_err(|e| format!("Failed to write session_start: {e}"))?;
        let _ = writer.flush();

        let (stop_tx, stop_rx) = channel::<()>();
        let path_buf = jsonl_path.to_path_buf();
        let path_buf_clone = path_buf.clone();

        let join_handle = thread::Builder::new()
            .name("dexhowto-mouse-telemetry".to_string())
            .spawn(move || {
                run_telemetry_loop(
                    writer,
                    stop_rx,
                    display_info,
                    fps,
                    started_at,
                    path_buf_clone,
                )
            })
            .map_err(|e| format!("Failed to spawn telemetry thread: {e}"))?;

        Ok(Self {
            stop_tx: Some(stop_tx),
            join_handle: Some(join_handle),
            jsonl_path: path_buf,
        })
    }

    pub fn stop(mut self) -> Result<TelemetrySummary, String> {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }

        if let Some(handle) = self.join_handle.take() {
            match handle.join() {
                Ok(summary_res) => summary_res,
                Err(err) => Err(format!("Telemetry thread panicked: {err:?}")),
            }
        } else {
            Err("Telemetry thread already terminated".to_string())
        }
    }
}

fn parse_modifiers(flags: u64) -> Vec<String> {
    let mut mods = Vec::with_capacity(4);
    if flags & 0x00020000 != 0 {
        mods.push("shift".to_string());
    }
    if flags & 0x00040000 != 0 {
        mods.push("ctrl".to_string());
    }
    if flags & 0x00080000 != 0 {
        mods.push("alt".to_string());
    }
    if flags & 0x00100000 != 0 {
        mods.push("cmd".to_string());
    }
    if flags & 0x00010000 != 0 {
        mods.push("caps_lock".to_string());
    }
    if flags & 0x00800000 != 0 {
        mods.push("fn".to_string());
    }
    mods
}

fn run_telemetry_loop(
    mut writer: BufWriter<File>,
    stop_rx: Receiver<()>,
    display: DisplayInfo,
    fps: u32,
    started_at: Instant,
    path: PathBuf,
) -> Result<TelemetrySummary, String> {
    let sample_interval = Duration::from_millis(8); // ~120 Hz sampling rate
    let stationary_heartbeat_interval = Duration::from_millis(150);

    let mut prev_x = -99999.0;
    let mut prev_y = -99999.0;
    let mut prev_left = false;
    let mut prev_right = false;
    let mut prev_middle = false;
    let mut prev_flags: u64 = 0;
    let mut last_emitted = Instant::now();
    let mut record_count: u64 = 1; // 1 for session_start

    // Click tracking states
    let mut pending_left: Option<(Instant, f64, f64, bool)> = None; // (time, x, y, is_ctrl_right)
    let mut pending_right: Option<(Instant, f64, f64)> = None;
    let mut pending_middle: Option<(Instant, f64, f64)> = None;
    let mut last_click_instant: Option<Instant> = None;
    let mut last_click_pos = (-999.0, -999.0);
    let mut last_click_btn = "";

    loop {
        // Check for stop request
        if stop_rx.try_recv().is_ok() {
            break;
        }

        let elapsed = started_at.elapsed();
        let t_secs = elapsed.as_secs_f64();
        let time_ms = elapsed.as_millis() as u64;
        let frame = (t_secs * fps as f64).floor() as u64;

        // Cursor position and system button state
        let (loc_x, loc_y) = crate::display::telemetry_cursor_position();
        // Modifier keys from the hardware state, as macOS event flags on every system
        let flags = crate::display::modifier_flags();

        let left_down = crate::display::mouse_button_down(0);
        let right_down = crate::display::mouse_button_down(1);
        let middle_down = crate::display::mouse_button_down(2);

        // Relative coordinates on the captured display
        let rel_x = loc_x - display.origin_x;
        let rel_y = loc_y - display.origin_y;

        let is_inside = rel_x >= 0.0 && rel_x <= display.point_width && rel_y >= 0.0 && rel_y <= display.point_height;

        let norm_x = if display.point_width > 0.0 {
            (rel_x / display.point_width).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let norm_y = if display.point_height > 0.0 {
            (rel_y / display.point_height).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let pixel_x = norm_x * display.width as f64;
        let pixel_y = norm_y * display.height as f64;

        // Detect state changes
        let pos_changed = (rel_x - prev_x).abs() > 0.25 || (rel_y - prev_y).abs() > 0.25;
        let left_changed = left_down != prev_left;
        let right_changed = right_down != prev_right;
        let middle_changed = middle_down != prev_middle;
        let flags_changed = flags != prev_flags;

        let mut event_type: Option<(&'static str, Option<&'static str>)> = None;
        let mut completed_click: Option<(&'static str, &'static str, u64, u32)> = None;

        // Ctrl + left click is a right click on macOS only
        let is_ctrl_pressed = cfg!(target_os = "macos") && flags & 0x00040000 != 0;

        if left_changed {
            if left_down {
                // macOS Ctrl + Left Click represents a Right Click
                if is_ctrl_pressed {
                    pending_left = Some((Instant::now(), rel_x, rel_y, true));
                    event_type = Some(("right_down", Some("right")));
                } else {
                    pending_left = Some((Instant::now(), rel_x, rel_y, false));
                    event_type = Some(("left_down", Some("left")));
                }
            } else {
                if let Some((down_instant, dx, dy, is_ctrl_right)) = pending_left.take() {
                    let dur_ms = down_instant.elapsed().as_millis() as u64;
                    let dist = ((rel_x - dx).powi(2) + (rel_y - dy).powi(2)).sqrt();
                    let was_click = dist < 12.0 && dur_ms < 600;

                    if is_ctrl_right {
                        event_type = Some(("right_up", Some("right")));
                        if was_click {
                            last_click_instant = Some(Instant::now());
                            last_click_pos = (rel_x, rel_y);
                            last_click_btn = "right";
                            completed_click = Some(("right_click", "right", dur_ms, 1));
                        }
                    } else {
                        event_type = Some(("left_up", Some("left")));
                        if was_click {
                            let is_double = if let Some(last_t) = last_click_instant {
                                let gap_ms = last_t.elapsed().as_millis();
                                let gap_dist = ((rel_x - last_click_pos.0).powi(2) + (rel_y - last_click_pos.1).powi(2)).sqrt();
                                gap_ms < 400 && gap_dist < 12.0 && last_click_btn == "left"
                            } else {
                                false
                            };

                            let evt = if is_double { "double_click" } else { "left_click" };
                            let cnt = if is_double { 2 } else { 1 };
                            last_click_instant = Some(Instant::now());
                            last_click_pos = (rel_x, rel_y);
                            last_click_btn = "left";
                            completed_click = Some((evt, "left", dur_ms, cnt));
                        }
                    }
                } else {
                    event_type = Some(("left_up", Some("left")));
                }
            }
        } else if right_changed {
            if right_down {
                pending_right = Some((Instant::now(), rel_x, rel_y));
                event_type = Some(("right_down", Some("right")));
            } else {
                event_type = Some(("right_up", Some("right")));
                if let Some((down_instant, dx, dy)) = pending_right.take() {
                    let dur_ms = down_instant.elapsed().as_millis() as u64;
                    let dist = ((rel_x - dx).powi(2) + (rel_y - dy).powi(2)).sqrt();
                    if dist < 12.0 && dur_ms < 600 {
                        last_click_instant = Some(Instant::now());
                        last_click_pos = (rel_x, rel_y);
                        last_click_btn = "right";
                        completed_click = Some(("right_click", "right", dur_ms, 1));
                    }
                }
            }
        } else if middle_changed {
            if middle_down {
                pending_middle = Some((Instant::now(), rel_x, rel_y));
                event_type = Some(("middle_down", Some("middle")));
            } else {
                event_type = Some(("middle_up", Some("middle")));
                if let Some((down_instant, dx, dy)) = pending_middle.take() {
                    let dur_ms = down_instant.elapsed().as_millis() as u64;
                    let dist = ((rel_x - dx).powi(2) + (rel_y - dy).powi(2)).sqrt();
                    if dist < 12.0 && dur_ms < 600 {
                        completed_click = Some(("middle_click", "middle", dur_ms, 1));
                    }
                }
            }
        } else if pos_changed {
            if left_down || right_down || middle_down {
                event_type = Some(("drag", None));
            } else {
                event_type = Some(("move", None));
            }
        } else if flags_changed {
            event_type = Some(("modifier_change", None));
        } else if last_emitted.elapsed() >= stationary_heartbeat_interval {
            event_type = Some(("stationary", None));
        }

        if let Some((evt_name, btn_name)) = event_type {
            let modifiers = parse_modifiers(flags);

            let record = TelemetryRecord::Mouse {
                t: (t_secs * 10_000.0).round() / 10_000.0, // 4 decimal places
                time_ms,
                frame,
                x: (pixel_x * 100.0).round() / 100.0,
                y: (pixel_y * 100.0).round() / 100.0,
                norm_x: (norm_x * 10_000.0).round() / 10_000.0,
                norm_y: (norm_y * 10_000.0).round() / 10_000.0,
                is_inside,
                global_x: (loc_x * 100.0).round() / 100.0,
                global_y: (loc_y * 100.0).round() / 100.0,
                event: evt_name.to_string(),
                button: btn_name.map(|s| s.to_string()),
                buttons: ButtonState {
                    left: left_down,
                    right: right_down,
                    middle: middle_down,
                },
                modifiers: modifiers.clone(),
                click_duration_ms: None,
                click_count: None,
            };

            if let Ok(json_str) = serde_json::to_string(&record) {
                if writeln!(writer, "{json_str}").is_ok() {
                    record_count += 1;
                }
            }

            // If a click completed on this frame, emit the synthesized click record
            if let Some((click_evt, click_btn, dur_ms, cnt)) = completed_click {
                let click_record = TelemetryRecord::Mouse {
                    t: (t_secs * 10_000.0).round() / 10_000.0,
                    time_ms,
                    frame,
                    x: (pixel_x * 100.0).round() / 100.0,
                    y: (pixel_y * 100.0).round() / 100.0,
                    norm_x: (norm_x * 10_000.0).round() / 10_000.0,
                    norm_y: (norm_y * 10_000.0).round() / 10_000.0,
                    is_inside,
                    global_x: (loc_x * 100.0).round() / 100.0,
                    global_y: (loc_y * 100.0).round() / 100.0,
                    event: click_evt.to_string(),
                    button: Some(click_btn.to_string()),
                    buttons: ButtonState {
                        left: left_down,
                        right: right_down,
                        middle: middle_down,
                    },
                    modifiers,
                    click_duration_ms: Some(dur_ms),
                    click_count: Some(cnt),
                };

                if let Ok(json_str) = serde_json::to_string(&click_record) {
                    if writeln!(writer, "{json_str}").is_ok() {
                        record_count += 1;
                    }
                }
            }

            prev_x = rel_x;
            prev_y = rel_y;
            prev_left = left_down;
            prev_right = right_down;
            prev_middle = middle_down;
            prev_flags = flags;
            last_emitted = Instant::now();
        }

        thread::sleep(sample_interval);
    }

    // Emit final session_end record
    let final_elapsed = started_at.elapsed();
    let final_secs = final_elapsed.as_secs_f64();
    let end_record = TelemetryRecord::SessionEnd {
        t: (final_secs * 10_000.0).round() / 10_000.0,
        duration_secs: (final_secs * 100.0).round() / 100.0,
        total_records: record_count + 1,
    };

    if let Ok(end_json) = serde_json::to_string(&end_record) {
        let _ = writeln!(writer, "{end_json}");
        record_count += 1;
    }

    let _ = writer.flush();

    Ok(TelemetrySummary {
        total_records: record_count,
        duration_secs: final_secs,
        path,
    })
}
