use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Instant;

use eframe::egui::{
    self, Color32, ColorImage, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use serde_json::Value;

use crate::ui_kit::{
    badge, button, card, icon_button, kbd, logo_texture, section_label, stat_row,
    theme, toggle_row, truncate_middle, vertical_divider, ButtonKind,
};
use crate::arrows::{detect_arrows, Arrow, ArrowStyle};
use crate::keys::KeyBindings;
use crate::export::{CursorIcon, CursorStyle, ExportEvent, ExportJob, ExportRequest};

// ─────────────────────────────────────────────────────────────────────────────
// Telemetry Data Model & Synchronization
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TelemetrySample {
    pub t: f64,
    pub time_ms: u64,
    pub frame: u64,
    pub norm_x: f64,
    pub norm_y: f64,
    pub x: f64,
    pub y: f64,
    pub is_inside: bool,
    pub event: String,
    pub button: Option<String>,
    pub left_down: bool,
    pub right_down: bool,
    pub click_duration_ms: Option<u64>,
    pub click_count: Option<u32>,
    pub raw_json: Value,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TelemetrySession {
    pub session_start: Value,
    pub samples: Vec<TelemetrySample>,
    pub session_end: Option<Value>,
    pub jsonl_path: PathBuf,
}

/// Ctrl/⌘-held samples closer together than this belong to the same protected stretch.
const CTRL_RUN_MAX_GAP_SECS: f64 = 0.25;

impl TelemetrySample {
    /// True when Control was held: the user marked this mouse movement to keep.
    #[cfg(test)]
    pub fn ctrl_held(&self) -> bool {
        self.modifier_held("ctrl")
    }

    /// Mouse movement the user marked to keep exactly as recorded: made while
    /// holding the keep-movement key, or the arrow key (arrows keep their path).
    pub fn protected_move(&self, keys: &KeyBindings) -> bool {
        self.modifier_held(keys.keep_movement.telemetry_name())
            || self.modifier_held(keys.arrow.telemetry_name())
    }

    /// True when the modifier key `name` ("cmd", "ctrl", "shift", "alt") was held.
    pub fn modifier_held(&self, name: &str) -> bool {
        self.raw_json
            .get("modifiers")
            .and_then(|m| m.as_array())
            .is_some_and(|mods| mods.iter().any(|m| m.as_str() == Some(name)))
    }
}

/// Number of separate Ctrl/⌘-held stretches in `samples`.
fn count_ctrl_runs(samples: &[TelemetrySample], keys: &KeyBindings) -> usize {
    let mut runs = 0;
    let mut last: Option<f64> = None;
    for s in samples.iter().filter(|s| s.protected_move(keys) && s.is_inside) {
        if last.is_none_or(|t| s.t - t >= CTRL_RUN_MAX_GAP_SECS) {
            runs += 1;
        }
        last = Some(s.t);
    }
    runs
}

impl TelemetrySession {
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("Failed to open telemetry file: {e}"))?;
        let reader = BufReader::new(file);

        let mut session_start: Option<Value> = None;
        let mut session_end: Option<Value> = None;
        let mut samples = Vec::new();

        for line_res in reader.lines() {
            let line = match line_res {
                Ok(l) => l,
                Err(_) => continue,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let val: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let rec_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match rec_type {
                "session_start" => {
                    session_start = Some(val);
                }
                "session_end" => {
                    session_end = Some(val);
                }
                "mouse" => {
                    let t = val.get("t").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let time_ms = val.get("time_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                    let frame = val.get("frame").and_then(|v| v.as_u64()).unwrap_or(0);
                    let norm_x = val.get("norm_x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let norm_y = val.get("norm_y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let x = val.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let y = val.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let is_inside = val.get("is_inside").and_then(|v| v.as_bool()).unwrap_or(true);
                    let event = val
                        .get("event")
                        .and_then(|v| v.as_str())
                        .unwrap_or("move")
                        .to_string();
                    let button = val.get("button").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let left_down = val
                        .get("buttons")
                        .and_then(|b| b.get("left"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let right_down = val
                        .get("buttons")
                        .and_then(|b| b.get("right"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let click_duration_ms =
                        val.get("click_duration_ms").and_then(|v| v.as_u64());
                    let click_count =
                        val.get("click_count").and_then(|v| v.as_u64()).map(|c| c as u32);

                    samples.push(TelemetrySample {
                        t,
                        time_ms,
                        frame,
                        norm_x,
                        norm_y,
                        x,
                        y,
                        is_inside,
                        event,
                        button,
                        left_down,
                        right_down,
                        click_duration_ms,
                        click_count,
                        raw_json: val,
                    });
                }
                _ => {}
            }
        }

        let start_val = session_start
            .ok_or_else(|| "Telemetry file missing session_start header".to_string())?;

        Ok(Self {
            session_start: start_val,
            samples,
            session_end,
            jsonl_path: path.to_path_buf(),
        })
    }

    /// Linear interpolation of mouse coordinates and nearest state for a given timestamp
    pub fn sample_at(&self, t: f64) -> Option<TelemetrySample> {
        if self.samples.is_empty() {
            return None;
        }

        // Fast boundary checks
        if t <= self.samples[0].t {
            return Some(self.samples[0].clone());
        }
        let last_idx = self.samples.len() - 1;
        if t >= self.samples[last_idx].t {
            return Some(self.samples[last_idx].clone());
        }

        // Binary search to find bracket [idx - 1, idx]
        let idx = match self.samples.binary_search_by(|s| {
            s.t.partial_cmp(&t).unwrap_or(std::cmp::Ordering::Equal)
        }) {
            Ok(exact) => return Some(self.samples[exact].clone()),
            Err(insertion_idx) => insertion_idx,
        };

        let prev = &self.samples[idx - 1];
        let next = &self.samples[idx];

        let dt = next.t - prev.t;
        let alpha = if dt > 0.0001 {
            ((t - prev.t) / dt).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let norm_x = prev.norm_x + alpha * (next.norm_x - prev.norm_x);
        let norm_y = prev.norm_y + alpha * (next.norm_y - prev.norm_y);
        let x = prev.x + alpha * (next.x - prev.x);
        let y = prev.y + alpha * (next.y - prev.y);

        // Pick dominant active state from nearest point
        let nearest = if alpha < 0.5 { prev } else { next };

        Some(TelemetrySample {
            t,
            time_ms: (t * 1000.0) as u64,
            frame: nearest.frame,
            norm_x,
            norm_y,
            x,
            y,
            is_inside: nearest.is_inside,
            event: nearest.event.clone(),
            button: nearest.button.clone(),
            left_down: nearest.left_down,
            right_down: nearest.right_down,
            click_duration_ms: nearest.click_duration_ms,
            click_count: nearest.click_count,
            raw_json: nearest.raw_json.clone(),
        })
    }

    /// Check if a click event occurred recently (within `window_secs` before `t`)
    pub fn recent_click_near(&self, t: f64, window_secs: f64) -> Option<(String, f64, f64, f64)> {
        let t_min = t - window_secs;
        // Search backwards from current time
        for s in self.samples.iter().rev() {
            if s.t < t_min {
                break;
            }
            if s.t <= t {
                let is_click = s.event == "left_click"
                    || s.event == "right_click"
                    || s.event == "double_click"
                    || s.event == "middle_click";
                if is_click {
                    let age = t - s.t;
                    return Some((s.event.clone(), age, s.norm_x, s.norm_y));
                }
            }
        }
        None
    }

    /// "Clean Move": drops plain pointer moves and keeps only the clicks (plus the
    /// button down/up and drag records that make up each click or drag) made inside
    /// the recorded screen; clicks outside it are removed too. Moves recorded while
    /// Control was held are protected and kept exactly as recorded. Between the kept
    /// records it generates a smooth, eased, slightly curved cursor path sampled at
    /// `fps`. Generated records carry `"synthetic": true`.
    pub fn clean_moves(&self, fps: u32, keys: &KeyBindings) -> Result<TelemetrySession, String> {
        let is_button_event = |s: &TelemetrySample| {
            s.left_down
                || s.right_down
                || s.raw_json
                    .get("buttons")
                    .and_then(|b| b.get("middle"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                || s.event.ends_with("_down")
                || s.event.ends_with("_up")
                || s.event.ends_with("_click")
                || s.event == "drag"
        };

        // Outside the recorded screen the position is clamped to its edge, so those
        // records would only drag the cursor to the border; drop them entirely
        let anchors: Vec<&TelemetrySample> = self
            .samples
            .iter()
            .filter(|s| s.is_inside && (is_button_event(s) || s.protected_move(keys)))
            .collect();
        if !anchors
            .iter()
            .any(|s| s.event.ends_with("_click") || s.protected_move(keys))
        {
            return Err("No clicks or key-held moves inside the recorded screen".to_string());
        }

        let fps = fps.max(1) as f64;
        // Recorded pixel size of the display, so x/y and norm_x/norm_y stay consistent
        let dim = |key: &str| {
            self.session_start
                .get("display")
                .and_then(|d| d.get(key))
                .and_then(|v| v.as_f64())
                .filter(|v| *v > 0.0)
        };
        let display = dim("width").zip(dim("height"));
        let mut out: Vec<TelemetrySample> = Vec::new();

        // Glide from where the cursor was at the start of the recording to the first click
        if let Some(first) = self.samples.first().filter(|s| s.is_inside) {
            if first.t < anchors[0].t {
                out.push(synthetic_sample(first, first, 0.0, first.t, fps, display));
                push_glide(&mut out, first, first.t, anchors[0], fps, display);
            }
        }

        for (i, anchor) in anchors.iter().enumerate() {
            out.push((*anchor).clone());
            let Some(next) = anchors.get(i + 1) else {
                break;
            };
            // Only travel once every button is released; drags keep their recorded path
            let released = !anchor.left_down
                && !anchor.right_down
                && !anchor.event.ends_with("_down")
                && anchor.event != "drag";
            // Inside one Ctrl-held stretch the recorded path is kept as is
            let protected_run = anchor.protected_move(keys)
                && next.protected_move(keys)
                && next.t - anchor.t < CTRL_RUN_MAX_GAP_SECS;
            if released && !protected_run {
                push_glide(&mut out, anchor, anchor.t, next, fps, display);
            }
        }

        out.sort_by(|a, b| a.t.total_cmp(&b.t));

        let mut session_start = self.session_start.clone();
        if let Some(obj) = session_start.as_object_mut() {
            obj.insert("clean_moves".to_string(), Value::Bool(true));
        }

        Ok(TelemetrySession {
            session_start,
            samples: out,
            session_end: self.session_end.clone(),
            jsonl_path: self.jsonl_path.clone(),
        })
    }

    /// Synchronizes telemetry records with kept segments (drops cuts and re-indexes continuous time)
    /// The telemetry as it plays in the cut video: records inside cut-out parts are
    /// dropped and the rest is shifted so time runs on continuously.
    pub fn retimed(&self, kept_segments: &[(f64, f64)], fps: u32) -> TelemetrySession {
        let mut offsets = Vec::with_capacity(kept_segments.len());
        let mut total = 0.0;
        for &(start, end) in kept_segments {
            offsets.push(total);
            total += (end - start).max(0.0);
        }
        let samples = self
            .samples
            .iter()
            .filter_map(|sample| {
                let (idx, &(seg_start, _)) = kept_segments
                    .iter()
                    .enumerate()
                    .find(|(_, seg)| sample.t >= seg.0 && sample.t <= seg.1)?;
                let new_t = offsets[idx] + (sample.t - seg_start);
                let mut moved = sample.clone();
                moved.t = new_t;
                moved.time_ms = (new_t * 1000.0).round() as u64;
                moved.frame = (new_t * fps as f64).floor() as u64;
                if let Some(obj) = moved.raw_json.as_object_mut() {
                    obj.insert("t".into(), serde_json::json!((new_t * 10_000.0).round() / 10_000.0));
                    obj.insert("time_ms".into(), serde_json::json!(moved.time_ms));
                    obj.insert("frame".into(), serde_json::json!(moved.frame));
                }
                Some(moved)
            })
            .collect();
        TelemetrySession {
            session_start: self.session_start.clone(),
            samples,
            session_end: None,
            jsonl_path: self.jsonl_path.clone(),
        }
    }

    pub fn sync_with_cut_ranges(
        &self,
        kept_segments: &[(f64, f64)],
        new_video_filename: &str,
        output_path: &Path,
        fps: u32,
    ) -> Result<u64, String> {
        let file = File::create(output_path)
            .map_err(|e| format!("Failed to create synchronized telemetry file: {e}"))?;
        let mut writer = BufWriter::new(file);

        // 1. Write updated session_start with new video filename
        let mut start_val = self.session_start.clone();
        if let Some(obj) = start_val.as_object_mut() {
            obj.insert(
                "video_file".to_string(),
                Value::String(new_video_filename.to_string()),
            );
        }
        let start_str = serde_json::to_string(&start_val)
            .map_err(|e| format!("Failed to serialize session_start: {e}"))?;
        writeln!(writer, "{start_str}")
            .map_err(|e| format!("Failed to write session_start: {e}"))?;

        let mut kept_records_count: u64 = 0;

        // Precompute accumulated start offsets for each kept segment
        let mut seg_offsets = Vec::with_capacity(kept_segments.len());
        let mut total_kept_duration = 0.0;
        for &(start, end) in kept_segments {
            seg_offsets.push(total_kept_duration);
            total_kept_duration += (end - start).max(0.0);
        }

        // 2. Filter and shift mouse records
        for sample in &self.samples {
            // Find which kept segment contains sample.t
            let mut matching_seg: Option<(usize, f64)> = None;
            for (idx, &(seg_start, seg_end)) in kept_segments.iter().enumerate() {
                if sample.t >= seg_start && sample.t <= seg_end {
                    matching_seg = Some((idx, seg_start));
                    break;
                }
            }

            if let Some((seg_idx, seg_start)) = matching_seg {
                let offset = seg_offsets[seg_idx];
                let new_t = offset + (sample.t - seg_start);
                let new_time_ms = (new_t * 1000.0).round() as u64;
                let new_frame = (new_t * fps as f64).floor() as u64;

                let mut record_val = sample.raw_json.clone();
                if let Some(obj) = record_val.as_object_mut() {
                    let rounded_t = (new_t * 10_000.0).round() / 10_000.0;
                    obj.insert("t".to_string(), serde_json::json!(rounded_t));
                    obj.insert("time_ms".to_string(), serde_json::json!(new_time_ms));
                    obj.insert("frame".to_string(), serde_json::json!(new_frame));
                }

                if let Ok(json_str) = serde_json::to_string(&record_val) {
                    if writeln!(writer, "{json_str}").is_ok() {
                        kept_records_count += 1;
                    }
                }
            }
        }

        // 3. Write updated session_end
        let end_record = serde_json::json!({
            "type": "session_end",
            "t": (total_kept_duration * 10_000.0).round() / 10_000.0,
            "duration_secs": (total_kept_duration * 10_000.0).round() / 10_000.0,
            "total_records": kept_records_count + 2
        });
        if let Ok(end_str) = serde_json::to_string(&end_record) {
            let _ = writeln!(writer, "{end_str}");
        }

        let _ = writer.flush();
        Ok(kept_records_count)
    }
}

/// Appends an eased, gently curved cursor path from `from` (at time `t_from`) to `to`.
/// The cursor rests at `from`, travels, and settles on `to` shortly before its time.
fn push_glide(
    out: &mut Vec<TelemetrySample>,
    from: &TelemetrySample,
    t_from: f64,
    to: &TelemetrySample,
    fps: f64,
    display: Option<(f64, f64)>,
) {
    let gap = to.t - t_from;
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let dist = (dx * dx + dy * dy).sqrt();
    if gap <= 1.0 / fps || dist < 1.0 {
        return;
    }

    // Longer moves take a little longer, but never more than the time available
    let settle = (gap * 0.15).min(0.12);
    let travel = (0.3 + dist / 3000.0).clamp(0.3, 1.2).min(gap - settle);
    let arrive = to.t - settle;
    let depart = (arrive - travel).max(t_from);
    if arrive - depart < 1.0 / fps {
        return;
    }

    // Hold still until departure
    if depart > t_from {
        out.push(synthetic_sample(from, to, 0.0, depart, fps, display));
    }

    let steps = ((arrive - depart) * fps).ceil().max(1.0) as usize;
    for step in 1..=steps {
        let u = step as f64 / steps as f64;
        let t = depart + (arrive - depart) * u;
        out.push(synthetic_sample(from, to, ease_in_out_cubic(u), t, fps, display));
    }
}

fn ease_in_out_cubic(u: f64) -> f64 {
    if u < 0.5 {
        4.0 * u * u * u
    } else {
        1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
    }
}

/// Builds a "move" record at fraction `k` (0..=1) along a quadratic curve from `from` to `to`.
/// The curve bows sideways by 8% of the distance so the motion looks hand-made.
fn synthetic_sample(
    from: &TelemetrySample,
    to: &TelemetrySample,
    k: f64,
    t: f64,
    fps: f64,
    display: Option<(f64, f64)>,
) -> TelemetrySample {
    let bow = 0.08;
    // Control point: midpoint pushed along the perpendicular, in each coordinate space
    let curve = |a: f64, b: f64, pa: f64, pb: f64| {
        let ctrl = (a + b) / 2.0 + (pb - pa) * bow;
        (1.0 - k) * (1.0 - k) * a + 2.0 * (1.0 - k) * k * ctrl + k * k * b
    };
    let x = curve(from.x, to.x, -from.y, -to.y);
    let y = curve(from.y, to.y, from.x, to.x);
    let (norm_x, norm_y) = match display {
        Some((w, h)) => (x / w, y / h),
        None => (
            curve(from.norm_x, to.norm_x, -from.norm_y, -to.norm_y),
            curve(from.norm_y, to.norm_y, from.norm_x, to.norm_x),
        ),
    };

    let get = |s: &TelemetrySample, key: &str| s.raw_json.get(key).and_then(|v| v.as_f64());
    let global = |key: &str, a: f64, b: f64, p: f64| match (get(from, key), get(to, key)) {
        (Some(ga), Some(gb)) if (b - a).abs() > 1e-9 => Some(ga + (gb - ga) * (p - a) / (b - a)),
        (Some(ga), _) => Some(ga),
        _ => None,
    };
    let global_x = global("global_x", from.x, to.x, x);
    let global_y = global("global_y", from.y, to.y, y);

    let round = |v: f64, scale: f64| (v * scale).round() / scale;
    let time_ms = (t * 1000.0).round() as u64;
    let frame = (t * fps).floor() as u64;

    let mut raw = from.raw_json.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.remove("click_duration_ms");
        obj.remove("click_count");
        obj.insert("t".into(), serde_json::json!(round(t, 10_000.0)));
        obj.insert("time_ms".into(), serde_json::json!(time_ms));
        obj.insert("frame".into(), serde_json::json!(frame));
        obj.insert("x".into(), serde_json::json!(round(x, 100.0)));
        obj.insert("y".into(), serde_json::json!(round(y, 100.0)));
        obj.insert("norm_x".into(), serde_json::json!(round(norm_x, 10_000.0)));
        obj.insert("norm_y".into(), serde_json::json!(round(norm_y, 10_000.0)));
        if let Some(gx) = global_x {
            obj.insert("global_x".into(), serde_json::json!(round(gx, 100.0)));
        }
        if let Some(gy) = global_y {
            obj.insert("global_y".into(), serde_json::json!(round(gy, 100.0)));
        }
        obj.insert("event".into(), Value::String("move".into()));
        obj.insert("button".into(), Value::Null);
        obj.insert(
            "buttons".into(),
            serde_json::json!({"left": false, "right": false, "middle": false}),
        );
        // Generated motion was not made with any keys held
        obj.insert("modifiers".into(), serde_json::json!([]));
        obj.insert("synthetic".into(), Value::Bool(true));
    }

    TelemetrySample {
        t,
        time_ms,
        frame,
        norm_x,
        norm_y,
        x,
        y,
        is_inside: to.is_inside,
        event: "move".to_string(),
        button: None,
        left_down: false,
        right_down: false,
        click_duration_ms: None,
        click_count: None,
        raw_json: raw,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cut Scenes & Segments Management
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct CutScene {
    pub id: usize,
    pub start_secs: f64,
    pub end_secs: f64,
}

impl CutScene {
    pub fn duration(&self) -> f64 {
        (self.end_secs - self.start_secs).max(0.0)
    }
}

/// Maps final-video time `out` back to recording time, given the kept parts.
pub fn source_time_in(kept: &[(f64, f64)], out: f64) -> f64 {
    let mut acc = 0.0;
    for &(s, e) in kept {
        if out <= acc + (e - s) {
            return s + (out - acc).max(0.0);
        }
        acc += e - s;
    }
    kept.last().map_or(0.0, |&(_, e)| e)
}

/// Maps recording time `t` to final-video time. Times inside a cut land where the
/// video continues after it.
pub fn output_time_in(kept: &[(f64, f64)], t: f64) -> f64 {
    let mut acc = 0.0;
    for &(s, e) in kept {
        if t < s {
            return acc;
        }
        if t <= e {
            return acc + (t - s);
        }
        acc += e - s;
    }
    acc
}

pub fn compute_kept_segments(cuts: &[CutScene], total_duration: f64) -> Vec<(f64, f64)> {
    if cuts.is_empty() {
        return if total_duration > 0.0 {
            vec![(0.0, total_duration)]
        } else {
            vec![]
        };
    }

    // Sort cuts by start time
    let mut sorted_cuts = cuts.to_vec();
    sorted_cuts.sort_by(|a, b| a.start_secs.partial_cmp(&b.start_secs).unwrap());

    // Merge overlapping or adjacent cuts
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for cut in sorted_cuts {
        let s = cut.start_secs.clamp(0.0, total_duration);
        let e = cut.end_secs.clamp(0.0, total_duration);
        if e <= s {
            continue;
        }

        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                // Overlaps with previous cut
                last.1 = last.1.max(e);
            } else {
                merged.push((s, e));
            }
        } else {
            merged.push((s, e));
        }
    }

    // Invert merged cuts to get kept segments
    let mut kept = Vec::new();
    let mut cur_time = 0.0;

    for (cut_start, cut_end) in merged {
        if cut_start > cur_time {
            kept.push((cur_time, cut_start));
        }
        cur_time = cut_end;
    }

    if cur_time < total_duration {
        kept.push((cur_time, total_duration));
    }

    kept
}

// ─────────────────────────────────────────────────────────────────────────────
// Asynchronous Video Frame Loader via ffmpeg
// ─────────────────────────────────────────────────────────────────────────────

struct FrameRequest {
    time: f64,
}

struct FrameResponse {
    time: f64,
    rgba_bytes: Vec<u8>,
    width: usize,
    height: usize,
}

pub struct AsyncFrameLoader {
    req_tx: Sender<FrameRequest>,
    resp_rx: Receiver<FrameResponse>,
    pub current_texture: Option<TextureHandle>,
    pub current_frame_time: Option<f64>,
    pub is_loading: bool,
    last_requested_time: f64,
}

impl AsyncFrameLoader {
    pub fn new(video_path: PathBuf) -> Self {
        let (req_tx, req_rx) = channel::<FrameRequest>();
        let (resp_tx, resp_rx) = channel::<FrameResponse>();

        thread::Builder::new()
            .name("dexhowto-frame-decoder".to_string())
            .spawn(move || {
                let ffmpeg_bin = find_ffmpeg_bin();
                let width = 854;
                let height = 480;
                let expected_bytes = width * height * 4;

                while let Ok(req) = req_rx.recv() {
                    // Drain any newer pending requests to avoid lag during fast scrubbing
                    let mut latest_time = req.time;
                    while let Ok(newer) = req_rx.try_recv() {
                        latest_time = newer.time;
                    }

                    let t_str = format!("{:.3}", latest_time.max(0.0));
                    let child = crate::platform::command(&ffmpeg_bin)
                        .arg("-ss")
                        .arg(&t_str)
                        .arg("-i")
                        .arg(&video_path)
                        .arg("-vframes")
                        .arg("1")
                        .arg("-s")
                        .arg(format!("{width}x{height}"))
                        .arg("-f")
                        .arg("rawvideo")
                        .arg("-pix_fmt")
                        .arg("rgba")
                        .arg("-")
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .spawn();

                    if let Ok(mut proc) = child {
                        let mut buf = Vec::with_capacity(expected_bytes);
                        if let Some(ref mut stdout) = proc.stdout {
                            let _ = stdout.read_to_end(&mut buf);
                        }
                        let _ = proc.wait();

                        if buf.len() >= expected_bytes {
                            buf.truncate(expected_bytes);
                            let _ = resp_tx.send(FrameResponse {
                                time: latest_time,
                                rgba_bytes: buf,
                                width,
                                height,
                            });
                        }
                    }
                }
            })
            .expect("Failed to spawn frame decoder thread");

        Self {
            req_tx,
            resp_rx,
            current_texture: None,
            current_frame_time: None,
            is_loading: false,
            last_requested_time: -999.0,
        }
    }

    pub fn request_frame(&mut self, time: f64) {
        if (self.last_requested_time - time).abs() > 0.03 {
            self.last_requested_time = time;
            self.is_loading = true;
            let _ = self.req_tx.send(FrameRequest { time });
        }
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        // Collect latest decoded frame
        let mut latest_resp = None;
        while let Ok(resp) = self.resp_rx.try_recv() {
            latest_resp = Some(resp);
        }

        if let Some(resp) = latest_resp {
            let color_image = ColorImage::from_rgba_unmultiplied(
                [resp.width, resp.height],
                &resp.rgba_bytes,
            );
            self.current_texture = Some(ctx.load_texture(
                "dexhowto_editor_preview",
                color_image,
                TextureOptions::LINEAR,
            ));
            self.current_frame_time = Some(resp.time);
            self.is_loading = false;
            ctx.request_repaint();
        }

        if self.is_loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

pub fn find_ffmpeg_bin() -> String {
    #[cfg(target_os = "macos")]
    let candidates = [
        "/opt/homebrew/bin/ffmpeg".to_string(),
        "/usr/local/bin/ffmpeg".to_string(),
        "ffmpeg".to_string(),
    ];
    // An ffmpeg.exe next to the app first, then the one on PATH, then a common install folder
    #[cfg(windows)]
    let candidates = [
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("ffmpeg.exe")))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "ffmpeg.exe".to_string()),
        "ffmpeg".to_string(),
        r"C:\ffmpeg\bin\ffmpeg.exe".to_string(),
    ];
    for c in candidates {
        if crate::platform::command(&c).arg("-version").output().is_ok() {
            return c;
        }
    }
    "ffmpeg".to_string()
}

/// ffprobe from the same place as ffmpeg.
pub fn find_ffprobe_bin() -> String {
    #[cfg(target_os = "macos")]
    {
        if crate::platform::command("/opt/homebrew/bin/ffprobe")
            .arg("-version")
            .output()
            .is_ok()
        {
            "/opt/homebrew/bin/ffprobe".to_string()
        } else {
            "ffprobe".to_string()
        }
    }
    #[cfg(windows)]
    {
        let ffmpeg = PathBuf::from(find_ffmpeg_bin());
        match ffmpeg.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            Some(dir) => dir.join("ffprobe.exe").to_string_lossy().into_owned(),
            None => "ffprobe".to_string(),
        }
    }
}

pub(crate) fn probe_video_metadata(video_path: &Path) -> (f64, u32, u32, u32, bool) {
    let ffprobe_bin = find_ffprobe_bin();

    let output = crate::platform::command(ffprobe_bin)
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("stream=width,height,codec_type,r_frame_rate")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("json")
        .arg(video_path)
        .output();

    if let Ok(out) = output {
        if let Ok(text) = String::from_utf8(out.stdout) {
            if let Ok(val) = serde_json::from_str::<Value>(&text) {
                let duration = val
                    .get("format")
                    .and_then(|f| f.get("duration"))
                    .and_then(|d| d.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(10.0);

                let mut width = 1920;
                let mut height = 1080;
                let mut fps = 60;
                let mut has_audio = false;

                if let Some(streams) = val.get("streams").and_then(|s| s.as_array()) {
                    for stream in streams {
                        let c_type = stream
                            .get("codec_type")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        if c_type == "video" {
                            width = stream.get("width").and_then(|w| w.as_u64()).unwrap_or(1920) as u32;
                            height =
                                stream.get("height").and_then(|h| h.as_u64()).unwrap_or(1080) as u32;
                            if let Some(r_fps) = stream
                                .get("r_frame_rate")
                                .and_then(|r| r.as_str())
                            {
                                if let Some((num, den)) = r_fps.split_once('/') {
                                    if let (Ok(n), Ok(d)) = (num.parse::<f64>(), den.parse::<f64>()) {
                                        if d > 0.0 {
                                            fps = (n / d).round() as u32;
                                        }
                                    }
                                }
                            }
                        } else if c_type == "audio" {
                            has_audio = true;
                        }
                    }
                }
                return (duration, width, height, fps, has_audio);
            }
        }
    }

    (10.0, 1920, 1080, 60, false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Video Editor Window State & UI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragTarget {
    Playhead,
    SelectionIn,           // dragging green handle / mark in
    SelectionOut,          // dragging red handle / mark out
    NewSelection(f64),     // rubber-band selection on empty track, anchor time
    Pan,                   // middle-drag / alt-drag panning of a zoomed timeline
    Overview,              // dragging the visible window in the overview bar
}

#[allow(dead_code)]
pub struct VideoEditor {
    pub video_path: PathBuf,
    pub telemetry_path: Option<PathBuf>,
    pub telemetry: Option<TelemetrySession>,
    /// "Clean Move" is on: the cursor glides from click to click in the cut video
    clean_moves_on: bool,
    /// Cursor path in the cut video, rebuilt when the cuts or Clean Move change
    cursor_track_cache: Option<(Vec<CutScene>, bool, TelemetrySession)>,
    /// Why the last "Clean Move" attempt failed, shown next to the button
    pub clean_move_message: Option<String>,
    /// What the active "Clean Move" kept and removed, shown on hover
    pub clean_move_summary: Option<String>,
    pub total_duration: f64,
    pub video_width: u32,
    pub video_height: u32,
    pub fps: u32,
    pub has_audio: bool,

    // Playback state
    pub current_time: f64,
    pub is_playing: bool,
    pub playback_speed: f64,
    pub last_play_instant: Instant,
    pub skip_cut_scenes: bool,

    // Mouse overlay rendering toggles
    pub show_mouse_cursor: bool,
    pub show_click_ripples: bool,

    // Frame decoder
    frame_loader: AsyncFrameLoader,

    // Cut scenes
    pub cuts: Vec<CutScene>,
    next_cut_id: usize,
    pub selected_cut_id: Option<usize>,

    // Interactive dragging & Mark In/Out selection
    pub active_drag: Option<DragTarget>,
    pub mark_in: Option<f64>,
    pub mark_out: Option<f64>,
    pub cut_duration_preset: f64,

    // Timeline view (zoom & scroll) and snapping
    pub timeline_zoom: f64,        // 1.0 = whole video fits the track
    pub timeline_view_start: f64,  // seconds at the left edge of the track
    pub snapping_enabled: bool,
    snap_guide: Option<f64>,       // time of the active snap guide (drawn while dragging)

    // Undo / redo history of cut lists
    undo_stack: Vec<Vec<CutScene>>,
    redo_stack: Vec<Vec<CutScene>>,

    // Export progress / notification
    pub export_status: Option<ExportStatus>,
    export_job: Option<ExportJob>,
    pub export_dialog_open: bool,

    // Cursor look, used by the preview and burned into the export
    pub cursor_style: CursorStyle,

    // Pointer arrows drawn with ⌘ while recording
    pub arrows: Vec<Arrow>,
    pub arrow_style: ArrowStyle,
    /// Keys held while recording to draw arrows / keep exact movement
    pub keys: KeyBindings,
}

#[derive(Clone, Debug)]
pub enum ExportStatus {
    /// Fraction done, 0..=1
    Exporting(f32),
    Success {
        output_video: PathBuf,
        output_telemetry: Option<PathBuf>,
        saved_duration: f64,
        synced_records: u64,
    },
    Error(String),
}

/// Output paths for exporting `video`: `<name>_edited.<ext>` and its `.mouse.jsonl`.
/// Earlier exports are never overwritten: when either file exists, the next free
/// number is added (`<name>_edited_2.<ext>`, `_3`, …).
pub fn free_export_paths(video: &Path) -> (PathBuf, PathBuf) {
    let stem = video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    let ext = video.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let parent = video.parent().unwrap_or_else(|| Path::new("."));
    let candidate = |n: u32| {
        let name = if n <= 1 {
            format!("{stem}_edited")
        } else {
            format!("{stem}_edited_{n}")
        };
        (
            parent.join(format!("{name}.{ext}")),
            parent.join(format!("{name}.mouse.jsonl")),
        )
    };
    (1..10_000)
        .map(candidate)
        .find(|(v, t)| !v.exists() && !t.exists())
        .unwrap_or_else(|| candidate(1))
}

/// Given a selected file (either video or jsonl), resolves the (video_path, telemetry_path) pair.
pub fn resolve_recording_pair(path: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

    if ext == "jsonl" {
        let tel_path = path.to_path_buf();
        let base_name = if filename.ends_with(".mouse.jsonl") {
            &filename[..filename.len() - 12]
        } else {
            &filename[..filename.len() - 6]
        };

        if let Some(parent) = path.parent() {
            for v_ext in &["mp4", "mov", "mkv", "avi"] {
                let candidate = parent.join(format!("{base_name}.{v_ext}"));
                if candidate.exists() {
                    return (Some(candidate), Some(tel_path));
                }
                let candidate_edited = parent.join(format!("{base_name}_edited.{v_ext}"));
                if candidate_edited.exists() {
                    return (Some(candidate_edited), Some(tel_path));
                }
            }
        }
        (None, Some(tel_path))
    } else {
        // Video file
        let video_path = path.to_path_buf();
        let mut telemetry_path: Option<PathBuf> = None;

        // 1. Direct match: {basename}.mouse.jsonl (how DexHowTo Recorder saves it)
        let direct_mouse = path.with_extension("mouse.jsonl");
        if direct_mouse.exists() {
            telemetry_path = Some(direct_mouse);
        } else {
            // 2. Direct match: {basename}.jsonl
            let direct_jsonl = path.with_extension("jsonl");
            if direct_jsonl.exists() {
                telemetry_path = Some(direct_jsonl);
            } else if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                // 3. If edited video, check base recording without _edited
                if let Some(base) = stem.strip_suffix("_edited") {
                    if let Some(parent) = path.parent() {
                        let base_mouse = parent.join(format!("{base}.mouse.jsonl"));
                        if base_mouse.exists() {
                            telemetry_path = Some(base_mouse);
                        } else {
                            let base_jsonl = parent.join(format!("{base}.jsonl"));
                            if base_jsonl.exists() {
                                telemetry_path = Some(base_jsonl);
                            }
                        }
                    }
                }
            }
        }

        (Some(video_path), telemetry_path)
    }
}

impl VideoEditor {
    pub fn reload(&mut self, video_path: PathBuf, telemetry_path: Option<PathBuf>) {
        *self = Self::open(video_path, telemetry_path);
    }

    pub fn attach_telemetry(&mut self, tel_path: PathBuf) -> Result<usize, String> {
        let session = TelemetrySession::load_from_file(&tel_path)?;
        let count = session.samples.len();
        let aspect = self.video_width as f64 / self.video_height.max(1) as f64;
        self.arrows = detect_arrows(&session, aspect, self.keys.arrow);
        self.telemetry = Some(session);
        self.clean_moves_on = false;
        self.cursor_track_cache = None;
        self.clean_move_message = None;
        self.telemetry_path = Some(tel_path);
        Ok(count)
    }

    pub fn open(video_path: PathBuf, telemetry_path: Option<PathBuf>) -> Self {
        let (total_duration, video_width, video_height, fps, has_audio) =
            probe_video_metadata(&video_path);

        let telemetry = telemetry_path
            .as_ref()
            .and_then(|p| TelemetrySession::load_from_file(p).ok());
        let aspect = video_width as f64 / video_height.max(1) as f64;
        let arrows = telemetry
            .as_ref()
            .map(|t| detect_arrows(t, aspect, KeyBindings::default().arrow))
            .unwrap_or_default();

        let mut frame_loader = AsyncFrameLoader::new(video_path.clone());
        frame_loader.request_frame(0.0);

        Self {
            video_path,
            telemetry_path,
            telemetry,
            clean_moves_on: false,
            cursor_track_cache: None,
            clean_move_message: None,
            clean_move_summary: None,
            total_duration,
            video_width,
            video_height,
            fps,
            has_audio,
            current_time: 0.0,
            is_playing: false,
            playback_speed: 1.0,
            last_play_instant: Instant::now(),
            skip_cut_scenes: true,
            show_mouse_cursor: true,
            show_click_ripples: true,
            frame_loader,
            cuts: Vec::new(),
            next_cut_id: 1,
            selected_cut_id: None,
            active_drag: None,
            mark_in: None,
            mark_out: None,
            cut_duration_preset: 2.0,
            timeline_zoom: 1.0,
            timeline_view_start: 0.0,
            snapping_enabled: true,
            snap_guide: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            export_status: None,
            export_job: None,
            export_dialog_open: false,
            cursor_style: CursorStyle::default(),
            arrows,
            arrow_style: ArrowStyle::default(),
            keys: KeyBindings::default(),
        }
    }

    // ── Clean Move & cursor path ─────────────────────────────────────────────
    pub fn clean_moves_active(&self) -> bool {
        self.clean_moves_on
    }

    /// Builds the cursor path as it plays in the cut video. Cuts are applied first,
    /// so with Clean Move the cursor glides straight from the last click before a
    /// cut to the next click after it, instead of finishing a glide that was cut.
    fn build_cursor_track(&self, clean: bool) -> Result<Option<TelemetrySession>, String> {
        let Some(tel) = self.telemetry.as_ref() else {
            return Ok(None);
        };
        let kept = compute_kept_segments(&self.cuts, self.total_duration);
        let track = tel.retimed(&kept, self.fps);
        if clean {
            track.clean_moves(self.fps, &self.keys).map(Some)
        } else {
            Ok(Some(track))
        }
    }

    /// The cursor path in the cut video (output time), cleaned when Clean Move is on.
    pub fn cursor_track(&mut self) -> Option<&TelemetrySession> {
        let fresh = matches!(
            &self.cursor_track_cache,
            Some((cuts, clean, _)) if *cuts == self.cuts && *clean == self.clean_moves_on
        );
        if !fresh {
            let track = self
                .build_cursor_track(self.clean_moves_on)
                .ok()
                .flatten()
                .or_else(|| self.build_cursor_track(false).ok().flatten());
            self.cursor_track_cache = track.map(|t| (self.cuts.clone(), self.clean_moves_on, t));
        }
        self.cursor_track_cache.as_ref().map(|(_, _, t)| t)
    }

    /// Where recording time `t` lands in the cut video. Times inside a cut map to
    /// the point where the video continues after it.
    pub fn output_time_of(&self, t: f64) -> f64 {
        output_time_in(&compute_kept_segments(&self.cuts, self.total_duration), t)
    }

    /// Uses `keys` to read the telemetry: finds the arrows again and rebuilds the
    /// cursor path.
    pub fn set_key_bindings(&mut self, keys: KeyBindings) {
        self.keys = keys;
        let aspect = self.video_width as f64 / self.video_height.max(1) as f64;
        self.arrows = self
            .telemetry
            .as_ref()
            .map(|t| detect_arrows(t, aspect, self.keys.arrow))
            .unwrap_or_default();
        self.cursor_track_cache = None;
    }

    /// Turns on "Clean Move". Returns the number of clicks kept.
    pub fn apply_clean_moves(&mut self) -> Result<usize, String> {
        let tel = self
            .telemetry
            .as_ref()
            .ok_or_else(|| "No telemetry loaded".to_string())?;
        let cleaned = self
            .build_cursor_track(true)?
            .ok_or_else(|| "No telemetry loaded".to_string())?;
        let kept_track = self.build_cursor_track(false)?.unwrap_or_else(|| tel.clone());
        let count_clicks = |t: &TelemetrySession| {
            t.samples.iter().filter(|s| s.event.ends_with("_click")).count()
        };
        let clicks = count_clicks(&cleaned);
        let outside = count_clicks(&kept_track).saturating_sub(clicks);
        let ctrl_runs = count_ctrl_runs(&kept_track.samples, &self.keys);
        self.clean_move_summary = Some(format!(
            "Kept {clicks} click{} and {ctrl_runs} key-held move{}, removed {outside} click{} outside the recorded screen",
            if clicks == 1 { "" } else { "s" },
            if ctrl_runs == 1 { "" } else { "s" },
            if outside == 1 { "" } else { "s" },
        ));
        self.clean_moves_on = true;
        Ok(clicks)
    }

    /// Puts the recorded mouse moves back.
    pub fn restore_original_moves(&mut self) {
        self.clean_moves_on = false;
    }

    // ── Undo / Redo ──────────────────────────────────────────────────────────
    /// Snapshot the current cut list before a mutation so it can be undone.
    pub fn push_history(&mut self) {
        if self.undo_stack.last() != Some(&self.cuts) {
            self.undo_stack.push(self.cuts.clone());
            if self.undo_stack.len() > 100 {
                self.undo_stack.remove(0);
            }
        }
        self.redo_stack.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(std::mem::replace(&mut self.cuts, prev));
            self.fix_selection_after_history();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(std::mem::replace(&mut self.cuts, next));
            self.fix_selection_after_history();
        }
    }

    fn fix_selection_after_history(&mut self) {
        if let Some(id) = self.selected_cut_id {
            if !self.cuts.iter().any(|c| c.id == id) {
                self.selected_cut_id = self.cuts.last().map(|c| c.id);
            }
        }
        self.next_cut_id = self
            .next_cut_id
            .max(self.cuts.iter().map(|c| c.id + 1).max().unwrap_or(1));
    }

    // ── Timeline view helpers ────────────────────────────────────────────────
    // The timeline shows the final video: cut-out parts take no space, so the
    // view (zoom and scroll) is measured in final-video seconds.

    /// Length of the final video, after cuts.
    pub fn output_duration(&self) -> f64 {
        self.kept_duration().max(0.05)
    }

    /// Visible duration of the timeline at the current zoom level.
    pub fn view_duration(&self) -> f64 {
        (self.output_duration() / self.timeline_zoom.max(1.0)).max(0.05)
    }

    pub fn clamp_view(&mut self) {
        self.timeline_zoom = self.timeline_zoom.clamp(1.0, 200.0);
        let max_start = (self.output_duration() - self.view_duration()).max(0.0);
        self.timeline_view_start = self.timeline_view_start.clamp(0.0, max_start);
    }

    /// Zoom by `factor`, keeping recording time `anchor_t` fixed under the pointer.
    pub fn zoom_around(&mut self, factor: f64, anchor_t: f64) {
        let anchor = self.output_time_of(anchor_t);
        let old_vd = self.view_duration();
        let frac = ((anchor - self.timeline_view_start) / old_vd).clamp(0.0, 1.0);
        self.timeline_zoom = (self.timeline_zoom * factor).clamp(1.0, 200.0);
        let new_vd = self.view_duration();
        self.timeline_view_start = anchor - frac * new_vd;
        self.clamp_view();
    }

    /// Scrolls the view so that recording time `t` is visible (while playing / scrubbing).
    pub fn ensure_visible(&mut self, t: f64) {
        let t = self.output_time_of(t);
        let vd = self.view_duration();
        let margin = vd * 0.05;
        if t < self.timeline_view_start + margin {
            self.timeline_view_start = t - margin;
        } else if t > self.timeline_view_start + vd - margin {
            self.timeline_view_start = t - vd + margin;
        }
        self.clamp_view();
    }

    /// Undoes the cuts `ids` and selects the part they removed, so it can be
    /// adjusted and cut again.
    pub fn restore_cuts(&mut self, ids: &[usize]) {
        let restored: Vec<&CutScene> = self.cuts.iter().filter(|c| ids.contains(&c.id)).collect();
        let Some(start) = restored.iter().map(|c| c.start_secs).reduce(f64::min) else {
            return;
        };
        let end = restored.iter().map(|c| c.end_secs).fold(start, f64::max);
        self.push_history();
        self.cuts.retain(|c| !ids.contains(&c.id));
        self.selected_cut_id = None;
        self.mark_in = Some(start);
        self.mark_out = Some(end);
        self.seek(start);
        self.ensure_visible(start);
    }

    pub fn seek(&mut self, t: f64) {
        self.current_time = t.clamp(0.0, self.total_duration);
        self.frame_loader.request_frame(self.current_time);
    }

    pub fn add_cut_scene_at(&mut self, start: f64, duration: f64) {
        let s = start.clamp(0.0, self.total_duration);
        let e = (s + duration).clamp(0.0, self.total_duration);
        if e > s {
            self.push_history();
            let id = self.next_cut_id;
            self.next_cut_id += 1;
            self.cuts.push(CutScene {
                id,
                start_secs: s,
                end_secs: e,
            });
            self.selected_cut_id = Some(id);
        }
    }

    pub fn cut_selection(&mut self) -> bool {
        let (s, e) = match (self.mark_in, self.mark_out) {
            (Some(i), Some(o)) => {
                let s = i.min(o);
                let e = i.max(o);
                (s, e)
            }
            (Some(i), None) => {
                let s = i.min(self.current_time);
                let e = i.max(self.current_time);
                (s, e)
            }
            (None, Some(o)) => {
                let s = self.current_time.min(o);
                let e = self.current_time.max(o);
                (s, e)
            }
            (None, None) => return false,
        };

        if e - s >= 0.05 {
            self.push_history();
            let id = self.next_cut_id;
            self.next_cut_id += 1;
            self.cuts.push(CutScene {
                id,
                start_secs: s.clamp(0.0, self.total_duration),
                end_secs: e.clamp(0.0, self.total_duration),
            });
            self.selected_cut_id = Some(id);
            self.mark_in = None;
            self.mark_out = None;
            true
        } else {
            false
        }
    }

    pub fn clear_selection(&mut self) {
        self.mark_in = None;
        self.mark_out = None;
    }

    pub fn set_mark_in_at_playhead(&mut self) {
        self.mark_in = Some(self.current_time);
        if let Some(out) = self.mark_out {
            if out <= self.current_time {
                self.mark_out = None;
            }
        }
    }

    pub fn set_mark_out_at_playhead(&mut self) {
        self.mark_out = Some(self.current_time);
        if let Some(i) = self.mark_in {
            if i >= self.current_time {
                self.mark_in = None;
            }
        }
    }

    pub fn remove_cut(&mut self, id: usize) {
        if !self.cuts.iter().any(|c| c.id == id) {
            return;
        }
        self.push_history();
        self.cuts.retain(|c| c.id != id);
        if self.selected_cut_id == Some(id) {
            self.selected_cut_id = self.cuts.last().map(|c| c.id);
        }
    }

    pub fn clear_all_cuts(&mut self) {
        if !self.cuts.is_empty() {
            self.push_history();
        }
        self.cuts.clear();
        self.selected_cut_id = None;
        self.mark_in = None;
        self.mark_out = None;
    }

    pub fn is_inside_cut(&self, t: f64) -> Option<&CutScene> {
        self.cuts
            .iter()
            .find(|c| t >= c.start_secs && t <= c.end_secs)
    }

    pub fn kept_duration(&self) -> f64 {
        let kept_segments = compute_kept_segments(&self.cuts, self.total_duration);
        kept_segments.iter().map(|&(s, e)| (e - s).max(0.0)).sum()
    }

    pub fn is_exporting(&self) -> bool {
        self.export_job.is_some()
    }

    /// Output paths next to the source, numbered so earlier exports are kept
    /// (see `free_export_paths`).
    pub fn export_paths(&self) -> (PathBuf, PathBuf) {
        free_export_paths(&self.video_path)
    }

    /// Starts the export on a background thread; progress arrives via `poll_export`.
    pub fn start_export(&mut self) {
        if self.is_exporting() {
            return;
        }
        let (out_video, out_telemetry) = self.export_paths();
        self.export_job = Some(ExportJob::start(ExportRequest {
            video_path: self.video_path.clone(),
            kept_segments: compute_kept_segments(&self.cuts, self.total_duration),
            has_audio: self.has_audio,
            // Cursor path already in cut-video time (and cleaned when Clean Move is on)
            telemetry: self.cursor_track().cloned(),
            fps: self.fps,
            video_width: self.video_width,
            video_height: self.video_height,
            style: self.cursor_style.clone(),
            arrows: if self.arrow_style.enabled { self.arrows.clone() } else { Vec::new() },
            arrow_style: self.arrow_style.clone(),
            out_video,
            out_telemetry,
        }));
        self.export_status = Some(ExportStatus::Exporting(0.0));
    }

    pub fn cancel_export(&self) {
        if let Some(job) = &self.export_job {
            job.cancel();
        }
    }

    /// Applies progress / completion events from the running export.
    pub fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.export_job else {
            return;
        };
        for event in job.poll() {
            match event {
                ExportEvent::Progress(p) => self.export_status = Some(ExportStatus::Exporting(p)),
                ExportEvent::Finished(result) => {
                    self.export_status = Some(match result {
                        Ok(r) => ExportStatus::Success {
                            output_video: r.output_video,
                            output_telemetry: r.output_telemetry,
                            saved_duration: r.duration,
                            synced_records: r.synced_records,
                        },
                        Err(e) => ExportStatus::Error(e),
                    });
                    self.export_job = None;
                    break;
                }
            }
        }
        if self.export_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Editor-only Widgets
// ─────────────────────────────────────────────────────────────────────────────

fn play_button(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let mut fill = theme::ACCENT;
        if resp.hovered() {
            fill = fill.lerp_to_gamma(Color32::WHITE, 0.1);
        }
        if resp.is_pointer_button_down_on() {
            fill = fill.lerp_to_gamma(Color32::BLACK, 0.15);
        }
        ui.painter().circle_filled(rect.center(), 20.0, fill);
        let c = rect.center();
        if playing {
            for dx in [-4.0, 4.0] {
                ui.painter().rect_filled(
                    Rect::from_center_size(Pos2::new(c.x + dx, c.y), Vec2::new(4.0, 14.0)),
                    1.0,
                    Color32::WHITE,
                );
            }
        } else {
            let pts = vec![
                Pos2::new(c.x - 5.0, c.y - 8.0),
                Pos2::new(c.x + 9.0, c.y),
                Pos2::new(c.x - 5.0, c.y + 8.0),
            ];
            ui.painter()
                .add(egui::Shape::convex_polygon(pts, Color32::WHITE, Stroke::NONE));
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Editable mm:ss.xx field with fine nudge buttons. Returns true when the value changed.
fn time_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    min: f64,
    max: f64,
    playhead: f64,
) -> bool {
    let mut changed = false;
    let max = max.max(min);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_sized(
            [40.0, 28.0],
            egui::Label::new(RichText::new(label).size(12.0).color(theme::TEXT_DIM)),
        );
        let resp = ui.add_sized(
            [84.0, 26.0],
            egui::DragValue::new(value)
                .speed(0.01)
                .range(min..=max)
                .custom_formatter(|v, _| format_timecode(v))
                .custom_parser(parse_timecode),
        );
        changed |= resp.changed();
        if button(ui, ButtonKind::Ghost, "−", true)
            .on_hover_text("−0.1s")
            .clicked()
        {
            *value = (*value - 0.1).clamp(min, max);
            changed = true;
        }
        if button(ui, ButtonKind::Ghost, "+", true)
            .on_hover_text("+0.1s")
            .clicked()
        {
            *value = (*value + 0.1).clamp(min, max);
            changed = true;
        }
        if button(ui, ButtonKind::Ghost, "⌖", true)
            .on_hover_text("Set to playhead")
            .clicked()
        {
            *value = playhead.clamp(min, max);
            changed = true;
        }
    });
    changed
}

// ─────────────────────────────────────────────────────────────────────────────
// UI Drawing Functions
// ─────────────────────────────────────────────────────────────────────────────

pub fn render_editor_view(ui: &mut egui::Ui, editor: &mut VideoEditor, close_flag: &mut bool) {
    editor.frame_loader.update(ui.ctx());
    editor.poll_export(ui.ctx());
    advance_playback(ui.ctx(), editor);
    if !editor.export_dialog_open {
        handle_shortcuts(ui, editor);
    }

    let panel_frame = |margin: Margin| egui::Frame::new().fill(theme::PANEL).inner_margin(margin);

    egui::Panel::top("editor_header")
        .frame(panel_frame(Margin::symmetric(16, 10)))
        .show(ui, |ui| render_header(ui, editor, close_flag));

    egui::Panel::bottom("editor_timeline")
        .frame(panel_frame(Margin::symmetric(16, 12)))
        .show(ui, |ui| {
            render_edit_toolbar(ui, editor);
            ui.add_space(10.0);
            render_timeline_track(ui, editor);
        });

    egui::Panel::right("editor_inspector")
        .resizable(false)
        .exact_size(300.0)
        .frame(panel_frame(Margin::symmetric(14, 14)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| render_inspector(ui, editor));
        });

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin::same(16)))
        .show(ui, |ui| {
            render_export_status(ui, editor);
            let transport_h = 44.0;
            let preview_h = (ui.available_height() - transport_h - 10.0).max(160.0);
            render_preview(ui, editor, preview_h);
            ui.add_space(10.0);
            render_transport(ui, editor);
        });

    if editor.export_dialog_open {
        render_export_dialog(ui.ctx(), editor);
    }
}

fn advance_playback(ctx: &egui::Context, editor: &mut VideoEditor) {
    if !editor.is_playing {
        return;
    }
    let now = Instant::now();
    let delta = now.duration_since(editor.last_play_instant).as_secs_f64() * editor.playback_speed;
    editor.last_play_instant = now;
    editor.current_time += delta;

    // Skip cut scenes during preview playback
    if editor.skip_cut_scenes {
        if let Some(cut) = editor.is_inside_cut(editor.current_time) {
            editor.current_time = cut.end_secs + 0.01;
        }
    }

    if editor.current_time >= editor.total_duration {
        editor.current_time = editor.total_duration;
        editor.is_playing = false;
    }

    editor.frame_loader.request_frame(editor.current_time);
    let t = editor.current_time;
    editor.ensure_visible(t);
    ctx.request_repaint();
}

fn toggle_playback(editor: &mut VideoEditor) {
    if !editor.is_playing && editor.current_time >= editor.total_duration - 0.01 {
        editor.seek(0.0);
    }
    editor.is_playing = !editor.is_playing;
    editor.last_play_instant = Instant::now();
}

fn handle_shortcuts(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    // Keyboard shortcuts — ignored while the user is typing into a numeric field
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let mut undo = false;
    let mut redo = false;
    ui.input(|i| {
        let cmd = i.modifiers.command;
        if cmd && i.key_pressed(egui::Key::Z) {
            if i.modifiers.shift {
                redo = true;
            } else {
                undo = true;
            }
            return;
        }
        if cmd && i.key_pressed(egui::Key::Y) {
            redo = true;
            return;
        }
        if i.key_pressed(egui::Key::Space) {
            toggle_playback(editor);
        }
        let step = if i.modifiers.shift { 1.0 } else { 1.0 / editor.fps.max(1) as f64 };

        // Alt + Arrows nudges the selected cut; plain arrows step the playhead
        let nudge_dir = if i.key_pressed(egui::Key::ArrowLeft) {
            -1.0
        } else if i.key_pressed(egui::Key::ArrowRight) {
            1.0
        } else {
            0.0
        };
        if nudge_dir != 0.0 {
            if i.modifiers.alt {
                if let Some(id) = editor.selected_cut_id {
                    let total = editor.total_duration;
                    editor.push_history();
                    if let Some(cut) = editor.cuts.iter_mut().find(|c| c.id == id) {
                        let dur = cut.duration();
                        let ns = (cut.start_secs + nudge_dir * step).clamp(0.0, total - dur);
                        cut.start_secs = ns;
                        cut.end_secs = ns + dur;
                    }
                }
            } else {
                let t = editor.current_time + nudge_dir * step;
                editor.seek(t);
            }
        }
        if i.key_pressed(egui::Key::Home) {
            editor.seek(0.0);
        }
        if i.key_pressed(egui::Key::End) {
            let t = editor.total_duration;
            editor.seek(t);
        }
        // Mark In (I) & Mark Out (O) shortcuts
        if i.key_pressed(egui::Key::I) {
            editor.set_mark_in_at_playhead();
        }
        if i.key_pressed(egui::Key::O) {
            editor.set_mark_out_at_playhead();
        }
        // Zoom in / out / fit
        let anchor = editor.current_time;
        if i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus) {
            editor.zoom_around(1.5, anchor);
        }
        if i.key_pressed(egui::Key::Minus) {
            editor.zoom_around(1.0 / 1.5, anchor);
        }
        if i.key_pressed(egui::Key::Num0) {
            editor.timeline_zoom = 1.0;
            editor.clamp_view();
        }
        // Toggle magnetic snapping
        if i.key_pressed(egui::Key::S) && !cmd {
            editor.snapping_enabled = !editor.snapping_enabled;
        }
        // Delete or Backspace cuts current selection or deletes selected cut
        if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
            if !editor.cut_selection() {
                if let Some(sel_id) = editor.selected_cut_id {
                    editor.remove_cut(sel_id);
                }
            }
        }
        // Escape clears selection or deselects cut
        if i.key_pressed(egui::Key::Escape) {
            editor.clear_selection();
            editor.selected_cut_id = None;
        }
    });
    if undo {
        editor.undo();
    }
    if redo {
        editor.redo();
    }
}

// ── Header ───────────────────────────────────────────────────────────────────

fn render_header(ui: &mut egui::Ui, editor: &mut VideoEditor, close_flag: &mut bool) {
    let filename = editor
        .video_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("Recording")
        .to_string();

    ui.horizontal(|ui| {
        // The icon has a transparent margin, so draw it a little larger than the 32px row
        let (logo, _) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::hover());
        ui.painter().image(
            logo_texture(ui.ctx()).id(),
            logo,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.add_space(4.0);

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(
                RichText::new(truncate_middle(&filename, 44))
                    .size(13.5)
                    .strong()
                    .color(theme::TEXT),
            )
            .on_hover_text(editor.video_path.display().to_string());
            ui.label(
                RichText::new(format!(
                    "{}×{}  ·  {} fps  ·  {}",
                    editor.video_width,
                    editor.video_height,
                    editor.fps,
                    format_timecode(editor.total_duration)
                ))
                .size(11.0)
                .color(theme::TEXT_DIM),
            );
        });

        ui.add_space(10.0);

        // With Clean Move on, count the cleaned path the video will use
        let cleaned_count = if editor.clean_moves_active() {
            editor.cursor_track().map(|t| t.samples.len())
        } else {
            None
        };
        if let Some(ref tel) = editor.telemetry {
            let recorded = tel.samples.len();
            let (label, hint) = match cleaned_count {
                Some(cleaned) => (
                    format!("•  Telemetry  {cleaned} events  (Clean Move)"),
                    format!(
                        "Recorded: {recorded} events. With Clean Move and cuts: {cleaned} events in the final video.\nFrom {}",
                        tel.jsonl_path.display()
                    ),
                ),
                None => (
                    format!("•  Telemetry  {recorded} events"),
                    format!("Mouse telemetry synced from {}", tel.jsonl_path.display()),
                ),
            };
            badge(ui, &label, theme::SUCCESS).on_hover_text(hint);
        } else if button(ui, ButtonKind::Tinted(theme::WARN), "⚠ Attach telemetry…", true)
            .on_hover_text("No mouse telemetry found. Pick a .jsonl file recorded with this video.")
            .clicked()
        {
            let mut dialog = rfd::FileDialog::new()
                .add_filter("AI Telemetry JSONL (*.jsonl)", &["jsonl"])
                .add_filter("All Files", &["*"]);
            if let Some(dir) = editor.video_path.parent() {
                dialog = dialog.set_directory(dir);
            }
            if let Some(picked_tel) = dialog.pick_file() {
                let _ = editor.attach_telemetry(picked_tel);
            }
        }

        if editor.telemetry.is_some() {
            render_clean_move_button(ui, editor);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if button(ui, ButtonKind::Ghost, "×", true)
                .on_hover_text("Close editor")
                .clicked()
            {
                *close_flag = true;
            }

            let exporting = editor.is_exporting();
            if button(ui, ButtonKind::Primary, "Export…", !exporting)
                .on_hover_text(
                    "Choose the cursor look, then render the cut video (_edited) and a re-timed telemetry .jsonl next to the original",
                )
                .clicked()
            {
                editor.export_dialog_open = true;
            }

            if button(ui, ButtonKind::Subtle, "Open…", true)
                .on_hover_text("Open another recording (.mp4/.mov) or its telemetry (.jsonl)")
                .clicked()
            {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter(
                        "Recorded Video & Telemetry (*.mp4, *.mov, *.jsonl)",
                        &["mp4", "mov", "jsonl"],
                    )
                    .add_filter("All Files", &["*"]);
                if let Some(dir) = editor.video_path.parent() {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(picked) = dialog.pick_file() {
                    let (vid_opt, tel_opt) = resolve_recording_pair(&picked);
                    if let Some(vid) = vid_opt {
                        editor.reload(vid, tel_opt);
                        ui.ctx().request_repaint();
                    }
                }
            }
        });
    });
}

/// "Clean Move" toggle: keeps only the clicks and replaces the recorded mouse
/// movement with smooth glides between them.
fn render_clean_move_button(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    if editor.clean_moves_active() {
        if button(ui, ButtonKind::Tinted(theme::ACCENT), "Clean Move: On", true)
            .on_hover_text(format!(
                "{}.\nSmooth paths between clicks are active and will be exported. Click to restore the recorded mouse moves.",
                editor.clean_move_summary.as_deref().unwrap_or("Clean Move active")
            ))
            .clicked()
        {
            editor.restore_original_moves();
            editor.clean_move_message = None;
        }
    } else if button(ui, ButtonKind::Subtle, "Clean Move", true)
        .on_hover_text(format!(
            "Remove the recorded mouse moves and any clicks outside the recorded screen, keep the clicks, and draw a smooth path from one click to the next. Moves recorded while holding {} or {} are kept as recorded.",
            editor.keys.keep_movement.label(),
            editor.keys.arrow.label()
        ))
        .clicked()
    {
        editor.clean_move_message = editor.apply_clean_moves().err();
    }

    if let Some(msg) = &editor.clean_move_message {
        badge(ui, msg, theme::WARN);
    }
}

// ── Export Status Banner ─────────────────────────────────────────────────────

fn render_export_status(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    let Some(status) = editor.export_status.clone() else {
        return;
    };
    let accent = match status {
        ExportStatus::Exporting(_) => theme::ACCENT,
        ExportStatus::Success { .. } => theme::SUCCESS,
        ExportStatus::Error(_) => theme::CUT,
    };
    let mut dismiss = false;

    egui::Frame::new()
        .fill(accent.gamma_multiply(0.1))
        .stroke(Stroke::new(1.0, accent.gamma_multiply(0.5)))
        .corner_radius(8.0)
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| match &status {
                ExportStatus::Exporting(progress) => {
                    ui.label(RichText::new("Exporting").strong().color(theme::TEXT));
                    let cancel_w = 80.0;
                    ui.add(
                        egui::ProgressBar::new(*progress)
                            .desired_width((ui.available_width() - cancel_w).max(80.0))
                            .desired_height(10.0)
                            .fill(theme::ACCENT)
                            .corner_radius(5.0),
                    );
                    ui.label(
                        RichText::new(format!("{:>3.0}%", progress * 100.0))
                            .monospace()
                            .color(theme::TEXT_DIM),
                    );
                    if button(ui, ButtonKind::Ghost, "Cancel", true).clicked() {
                        editor.cancel_export();
                    }
                }
                ExportStatus::Success {
                    output_video,
                    output_telemetry,
                    saved_duration,
                    synced_records,
                } => {
                    ui.label(RichText::new("✔").size(14.0).color(accent));
                    ui.label(RichText::new("Export complete").strong().color(theme::TEXT));
                    ui.label(
                        RichText::new(format!(
                            "{} long  ·  {} telemetry events",
                            format_timecode(*saved_duration),
                            synced_records
                        ))
                        .color(theme::TEXT_DIM),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if button(ui, ButtonKind::Ghost, "×", true).clicked() {
                            dismiss = true;
                        }
                        if button(ui, ButtonKind::Subtle, "Play", true).clicked() {
                            crate::platform::open_path(output_video);
                        }
                        let show = format!("Show in {}", crate::platform::FILE_MANAGER);
                        if button(ui, ButtonKind::Subtle, &show, true).clicked() {
                            crate::platform::reveal_in_file_manager(output_video);
                        }
                        if let Some(tp) = output_telemetry {
                            if button(ui, ButtonKind::Ghost, "Telemetry", true)
                                .on_hover_text(format!(
                                    "Reveal the re-timed .jsonl in {}",
                                    crate::platform::FILE_MANAGER
                                ))
                                .clicked()
                            {
                                crate::platform::reveal_in_file_manager(tp);
                            }
                        }
                    });
                }
                ExportStatus::Error(err) => {
                    ui.label(RichText::new("Export failed").strong().color(accent));
                    ui.add(
                        egui::Label::new(
                            RichText::new(err.lines().last().unwrap_or(err)).color(theme::TEXT_DIM),
                        )
                        .truncate(),
                    )
                    .on_hover_text(err);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if button(ui, ButtonKind::Ghost, "×", true).clicked() {
                            dismiss = true;
                        }
                    });
                }
            });
        });
    ui.add_space(10.0);

    if dismiss {
        editor.export_status = None;
    }
}

// ── Export Dialog ────────────────────────────────────────────────────────────

fn render_export_dialog(ctx: &egui::Context, editor: &mut VideoEditor) {
    let has_telemetry = editor.telemetry.is_some();
    let (out_video, _) = editor.export_paths();
    let out_name = out_video
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("video.mp4")
        .to_string();
    let kept = editor.kept_duration();
    let arrow_count = editor.arrows.len();
    let (video_w, video_h) = (editor.video_width, editor.video_height);
    let mut start = false;
    let mut close = false;

    let modal = egui::Modal::new(egui::Id::new("editor_export_dialog"))
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .corner_radius(12.0)
                .inner_margin(Margin::same(20)),
        )
        .show(ctx, |ui| {
            ui.set_width(460.0);
            let style = &mut editor.cursor_style;

            ui.label(RichText::new("Export video").size(16.0).strong().color(theme::TEXT));
            ui.label(
                RichText::new(format!(
                    "{out_name}  ·  {}  ·  {}×{}",
                    format_timecode(kept),
                    video_w,
                    video_h
                ))
                .size(11.5)
                .color(theme::TEXT_DIM),
            );
            ui.add_space(16.0);

            // Scroll when the options are taller than the window
            let max_body = (ctx.content_rect().height() - 230.0).max(200.0);
            egui::ScrollArea::vertical().max_height(max_body).show(ui, |ui| {
            section_label(ui, "CURSOR");
            card(ui, |ui| {
                if !has_telemetry {
                    ui.label(
                        RichText::new("No mouse telemetry is attached, so no cursor can be drawn.")
                            .size(12.0)
                            .color(theme::WARN),
                    );
                    ui.add_space(6.0);
                }
                let mut burn = style.burn_in && has_telemetry;
                ui.add_enabled_ui(has_telemetry, |ui| {
                    toggle_row(
                        ui,
                        "Burn cursor into video",
                        "Draw the telemetry cursor (including Clean Move paths) into the exported frames",
                        &mut burn,
                    );
                });
                if has_telemetry {
                    style.burn_in = burn;
                }

                ui.add_enabled_ui(burn, |ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Icon").size(12.0).color(theme::TEXT_DIM));
                    ui.horizontal(|ui| {
                        for icon in CursorIcon::ALL {
                            if cursor_tile(ui, icon, style.icon == icon).clicked() {
                                style.icon = icon;
                            }
                        }
                    });
                    ui.add_space(8.0);
                    slider_row(ui, "Size", &mut style.size, 0.5..=3.0, |v| format!("{:.0}%", v * 100.0));

                    ui.add_space(10.0);
                    toggle_row(
                        ui,
                        "Highlight",
                        "Soft circle behind the cursor so viewers can follow it",
                        &mut style.highlight,
                    );
                    ui.add_enabled_ui(style.highlight, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [64.0, 24.0],
                                egui::Label::new(RichText::new("Color").size(12.0).color(theme::TEXT_DIM)),
                            );
                            let [r, g, b, a] = style.highlight_rgba;
                            let mut color = Color32::from_rgba_unmultiplied(r, g, b, a);
                            if egui::color_picker::color_edit_button_srgba(
                                ui,
                                &mut color,
                                egui::color_picker::Alpha::OnlyBlend,
                            )
                            .changed()
                            {
                                style.highlight_rgba = color.to_srgba_unmultiplied();
                            }
                            for preset in [
                                [255, 210, 60, 120],
                                [88, 136, 255, 110],
                                [236, 72, 90, 110],
                                [64, 206, 128, 110],
                            ] {
                                let (rect, resp) =
                                    ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                                let [pr, pg, pb, _] = preset;
                                ui.painter().circle_filled(rect.center(), 8.0, Color32::from_rgb(pr, pg, pb));
                                if style.highlight_rgba == preset {
                                    ui.painter().circle_stroke(rect.center(), 9.5, Stroke::new(1.5, theme::TEXT));
                                }
                                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    style.highlight_rgba = preset;
                                }
                            }
                        });
                        slider_row(ui, "Size", &mut style.highlight_radius, 10.0..=90.0, |v| {
                            format!("{v:.0}px")
                        });
                    });

                    ui.add_space(12.0);
                    cursor_preview(ui, style, video_h);
                });
            });

            ui.add_space(16.0);
            section_label(ui, "ARROWS");
            let arrow_key = editor.keys.arrow.label();
            card(ui, |ui| {
                let arrows = &mut editor.arrow_style;
                if arrow_count == 0 {
                    ui.label(
                        RichText::new(
                            &format!("No arrows in this recording. While recording, hold {arrow_key} and move the mouse to draw an arrow to what you want to point at."),
                        )
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                    );
                } else {
                    toggle_row(
                        ui,
                        &format!("Draw arrows ({arrow_count})"),
                        &format!("Arrows drawn by holding {arrow_key} while moving the mouse"),
                        &mut arrows.enabled,
                    );
                    ui.add_enabled_ui(arrows.enabled, |ui| {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [64.0, 24.0],
                                egui::Label::new(RichText::new("Color").size(12.0).color(theme::TEXT_DIM)),
                            );
                            for preset in [
                                [236, 52, 68, 255],
                                [255, 196, 40, 255],
                                [88, 136, 255, 255],
                                [52, 190, 120, 255],
                                [250, 250, 252, 255],
                            ] {
                                let (rect, resp) =
                                    ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                                let [r, g, b, _] = preset;
                                ui.painter().circle_filled(rect.center(), 8.0, Color32::from_rgb(r, g, b));
                                ui.painter().circle_stroke(rect.center(), 8.0, Stroke::new(1.0, theme::BORDER));
                                if arrows.rgba == preset {
                                    ui.painter().circle_stroke(rect.center(), 10.0, Stroke::new(1.5, theme::TEXT));
                                }
                                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    arrows.rgba = preset;
                                }
                            }
                        });
                        slider_row(ui, "Stays", &mut arrows.hold_secs, 1.0..=8.0, |v| format!("{v:.1}s"));
                        slider_row(ui, "Thickness", &mut arrows.thickness, 3.0..=14.0, |v| format!("{v:.0}px"));
                    });
                }
            });
            });

            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Telemetry is re-timed and saved as .mouse.jsonl next to the video.")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
            });
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if button(ui, ButtonKind::Primary, "Export", true).clicked() {
                    start = true;
                }
                if button(ui, ButtonKind::Ghost, "Cancel", true).clicked() {
                    close = true;
                }
            });
        });

    if modal.should_close() {
        close = true;
    }
    if start {
        editor.export_dialog_open = false;
        editor.start_export();
    } else if close {
        editor.export_dialog_open = false;
    }
}

fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    fmt: impl Fn(f64) -> String + 'static,
) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [64.0, 24.0],
            egui::Label::new(RichText::new(label).size(12.0).color(theme::TEXT_DIM)),
        );
        ui.spacing_mut().slider_width = 260.0;
        ui.add(egui::Slider::new(value, range).custom_formatter(move |v, _| fmt(v)));
    });
}

/// Selectable tile showing a cursor icon.
fn cursor_tile(ui: &mut egui::Ui, icon: CursorIcon, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(98.0, 72.0), Sense::click());
    // Mid-gray so both dark and white cursors stand out
    let base = Color32::from_rgb(78, 86, 104);
    let fill = if selected {
        base.lerp_to_gamma(theme::ACCENT, 0.35)
    } else if resp.hovered() {
        base.lerp_to_gamma(Color32::WHITE, 0.08)
    } else {
        base
    };
    let stroke = if selected { theme::ACCENT } else { theme::BORDER };
    ui.painter()
        .rect(rect, 8.0, fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let tip = match icon {
        CursorIcon::Dot => Pos2::new(rect.center().x, rect.top() + 28.0),
        _ => Pos2::new(rect.center().x - 10.0, rect.top() + 9.0),
    };
    draw_cursor(ui.painter(), tip, icon, 1.8);
    ui.painter().text(
        Pos2::new(rect.center().x, rect.bottom() - 9.0),
        egui::Align2::CENTER_CENTER,
        icon.label(),
        FontId::proportional(11.0),
        if selected { Color32::WHITE } else { Color32::from_white_alpha(200) },
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Shows the cursor at its real exported size relative to a 1080p-tall strip.
fn cursor_preview(ui: &mut egui::Ui, style: &CursorStyle, video_height: u32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 110.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 8.0, Color32::from_rgb(58, 66, 84));
    // Light half so dark cursors can be judged too
    painter.rect_filled(
        Rect::from_min_max(rect.center_top(), rect.right_bottom()),
        egui::CornerRadius { nw: 0, ne: 8, sw: 0, se: 8 },
        Color32::from_rgb(232, 234, 240),
    );
    // Preview at 1080p scale so sizes look like a typical full-screen playback
    let frame_h = 1080.0;
    for x in [rect.left() + rect.width() * 0.25, rect.left() + rect.width() * 0.75] {
        let tip = Pos2::new(x, rect.center().y - 8.0);
        if style.highlight {
            let [r, g, b, a] = style.highlight_rgba;
            painter.circle_filled(
                tip,
                style.highlight_px(frame_h),
                Color32::from_rgba_unmultiplied(r, g, b, a),
            );
        }
        draw_cursor(&painter, tip, style.icon, style.unit_px(frame_h));
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "Shown at 1080p size  ·  scaled to {video_height}p in the export"
        ))
        .size(10.5)
        .color(theme::TEXT_FAINT),
    );
}

// ── Video Preview with Telemetry Mouse Overlay ───────────────────────────────

fn render_preview(ui: &mut egui::Ui, editor: &mut VideoEditor, height: f32) {
    let (outer_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(outer_rect);
    painter.rect_filled(outer_rect, 10.0, Color32::from_rgb(5, 6, 8));

    // Compute fitted video aspect rectangle
    let aspect = (editor.video_width as f32) / (editor.video_height as f32).max(1.0);
    let inner = outer_rect.shrink(12.0);
    let mut fit_w = inner.width();
    let mut fit_h = fit_w / aspect;
    if fit_h > inner.height() {
        fit_h = inner.height();
        fit_w = fit_h * aspect;
    }
    let video_rect = Rect::from_center_size(inner.center(), Vec2::new(fit_w, fit_h));

    if let Some(ref tex) = editor.frame_loader.current_texture {
        painter.image(
            tex.id(),
            video_rect,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.rect_filled(video_rect, 4.0, theme::SURFACE);
        painter.text(
            video_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Loading frame…",
            FontId::proportional(13.0),
            theme::TEXT_FAINT,
        );
    }

    // Pointer arrows (⌘ gestures), under the cursor
    if editor.arrow_style.enabled {
        let style = &editor.arrow_style;
        let thickness = style.thickness_px(video_rect.height());
        let aspect = editor.video_width as f64 / editor.video_height.max(1) as f64;
        for arrow in &editor.arrows {
            // Grows with the mouse while ⌘ is held, then stays and fades out
            let Some((from, tip, opacity)) = style.state_at(arrow, editor.current_time, aspect) else {
                continue;
            };
            let at = |p: (f64, f64)| {
                Pos2::new(
                    video_rect.left() + p.0 as f32 * video_rect.width(),
                    video_rect.top() + p.1 as f32 * video_rect.height(),
                )
            };
            draw_arrow(&painter, at(from), at(tip), thickness, style.rgba, opacity);
        }
    }

    // Removed-scene overlay: dim the frame and show a pill on top
    if let Some(cut) = editor.is_inside_cut(editor.current_time) {
        painter.rect_filled(video_rect, 0.0, Color32::from_black_alpha(130));
        let text = format!("✂  Removed in export  ·  Cut #{}  ·  {:.2}s", cut.id, cut.duration());
        let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), Color32::WHITE);
        let pill = Rect::from_center_size(
            Pos2::new(video_rect.center().x, video_rect.top() + 24.0),
            galley.size() + Vec2::new(24.0, 10.0),
        );
        painter.rect_filled(pill, pill.height() / 2.0, theme::CUT);
        painter.galley(pill.center() - galley.size() / 2.0, galley, Color32::WHITE);
    }

    // ── Render Telemetry Mouse Pointer & Click Ripples ───────────────────────
    // The cursor follows the path of the cut video, exactly as it will be exported
    let out_t = editor.output_time_of(editor.current_time);
    let Some(sample) = editor.cursor_track().and_then(|t| t.sample_at(out_t)) else {
        return;
    };

    let mouse_pos = Pos2::new(
        video_rect.left() + (sample.norm_x as f32) * video_rect.width(),
        video_rect.top() + (sample.norm_y as f32) * video_rect.height(),
    );

    // Click Ripple Animation (if a click occurred within 350ms of current_time)
    if editor.show_click_ripples {
        if let Some(tel) = editor.cursor_track() {
            if let Some((click_type, age, rx, ry)) = tel.recent_click_near(out_t, 0.35)
            {
                let r_center = Pos2::new(
                    video_rect.left() + (rx as f32) * video_rect.width(),
                    video_rect.top() + (ry as f32) * video_rect.height(),
                );
                draw_click_ripple(&painter, r_center, age, &click_type);
            }
        }
    }

    // Cursor in the chosen export style, scaled like it will be in the video
    if editor.show_mouse_cursor && sample.is_inside {
        let style = &editor.cursor_style;
        let frame_h = video_rect.height();
        if style.highlight {
            let [r, g, b, a] = style.highlight_rgba;
            painter.circle_filled(
                mouse_pos,
                style.highlight_px(frame_h),
                Color32::from_rgba_unmultiplied(r, g, b, a),
            );
        }
        draw_cursor(&painter, mouse_pos, style.icon, style.unit_px(frame_h));

        // Drag dot indicator
        if sample.event == "drag" || (sample.left_down && sample.event == "move") {
            painter.circle_filled(mouse_pos + Vec2::new(14.0, 14.0), 3.5, theme::WARN);
        }
    }

    // Mouse coordinate HUD (bottom-left of video)
    let is_click = matches!(
        sample.event.as_str(),
        "left_click" | "right_click" | "double_click"
    );
    let hud_text = if is_click {
        format!("{}  ·  {:.0}, {:.0}", sample.event.replace('_', " "), sample.x, sample.y)
    } else {
        format!("{:.0}, {:.0}", sample.x, sample.y)
    };
    let galley = painter.layout_no_wrap(hud_text, FontId::monospace(10.5), theme::TEXT);
    let hud_rect = Rect::from_min_size(
        Pos2::new(video_rect.left() + 10.0, video_rect.bottom() - 10.0 - 20.0),
        Vec2::new(galley.size().x + 16.0, 20.0),
    );
    painter.rect_filled(hud_rect, 10.0, Color32::from_black_alpha(170));
    let text_color = if is_click { theme::WARN } else { theme::TEXT };
    painter.galley(hud_rect.center() - galley.size() / 2.0, galley, text_color);
}

// ── Transport Bar ────────────────────────────────────────────────────────────

fn render_transport(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());

    // Left: timecode
    let painter = ui.painter();
    // Time in the final video (after cuts), like the timeline ruler
    painter.text(
        Pos2::new(bar.left() + 2.0, bar.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        format_timecode(editor.output_time_of(editor.current_time)),
        FontId::monospace(17.0),
        theme::TEXT,
    );
    painter.text(
        Pos2::new(bar.left() + 2.0, bar.center().y + 11.0),
        egui::Align2::LEFT_CENTER,
        format!("of {}", format_timecode(editor.kept_duration())),
        FontId::monospace(11.0),
        theme::TEXT_FAINT,
    );

    // Center: transport buttons
    let center_w = 6.0 * 30.0 + 40.0 + 6.0 * 4.0;
    let center_rect = Rect::from_center_size(bar.center(), Vec2::new(center_w, bar.height()));
    let frame = 1.0 / editor.fps.max(1) as f64;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(center_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let t = editor.current_time;
            if icon_button(ui, "⏮", true).on_hover_text("Go to start (Home)").clicked() {
                editor.seek(0.0);
            }
            if icon_button(ui, "⏪", true).on_hover_text("Back 1 second (Shift ←)").clicked() {
                editor.seek(t - 1.0);
            }
            if icon_button(ui, "◀", true).on_hover_text("Previous frame (←)").clicked() {
                editor.seek(t - frame);
            }
            let play_hint = if editor.is_playing { "Pause (Space)" } else { "Play (Space)" };
            if play_button(ui, editor.is_playing).on_hover_text(play_hint).clicked() {
                toggle_playback(editor);
            }
            if icon_button(ui, "▶", true).on_hover_text("Next frame (→)").clicked() {
                editor.seek(t + frame);
            }
            if icon_button(ui, "⏩", true).on_hover_text("Forward 1 second (Shift →)").clicked() {
                editor.seek(t + 1.0);
            }
            if icon_button(ui, "⏭", true).on_hover_text("Go to end (End)").clicked() {
                let end = editor.total_duration;
                editor.seek(end);
            }
        },
    );

    // Right: playback speed
    let right_rect = Rect::from_min_max(
        Pos2::new(center_rect.right() + 8.0, bar.top()),
        bar.right_bottom(),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            egui::ComboBox::from_id_salt("editor_speed_combo")
                .selected_text(format!("{}×", editor.playback_speed))
                .width(64.0)
                .show_ui(ui, |ui| {
                    for speed in [0.5, 1.0, 1.5, 2.0] {
                        ui.selectable_value(&mut editor.playback_speed, speed, format!("{speed}×"));
                    }
                });
            ui.label(RichText::new("Speed").size(11.5).color(theme::TEXT_FAINT));
        },
    );
}

// ── Edit Toolbar (above the timeline) ────────────────────────────────────────

fn render_edit_toolbar(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;

        if button(ui, ButtonKind::Ghost, "Undo", editor.can_undo()).on_hover_text("Undo (⌘Z)").clicked() {
            editor.undo();
        }
        if button(ui, ButtonKind::Ghost, "Redo", editor.can_redo()).on_hover_text("Redo (Shift ⌘Z)").clicked() {
            editor.redo();
        }

        vertical_divider(ui);

        // Mark In / Out
        let (in_label, in_kind) = match editor.mark_in {
            Some(t) => (format!("[  {}", format_timecode(t)), ButtonKind::Tinted(theme::MARK_IN)),
            None => ("[  Mark In".to_string(), ButtonKind::Subtle),
        };
        if button(ui, in_kind, &in_label, true)
            .on_hover_text("Set the selection start at the playhead (I)")
            .clicked()
        {
            editor.set_mark_in_at_playhead();
        }

        let (out_label, out_kind) = match editor.mark_out {
            Some(t) => (format!("{}  ]", format_timecode(t)), ButtonKind::Tinted(theme::MARK_OUT)),
            None => ("Mark Out  ]".to_string(), ButtonKind::Subtle),
        };
        if button(ui, out_kind, &out_label, true)
            .on_hover_text("Set the selection end at the playhead (O)")
            .clicked()
        {
            editor.set_mark_out_at_playhead();
        }

        let has_selection = editor.mark_in.is_some() || editor.mark_out.is_some();
        if button(ui, ButtonKind::Danger, "✂  Cut selection", has_selection)
            .on_hover_text("Remove the marked range from the export (Delete)")
            .clicked()
        {
            editor.cut_selection();
        }
        if has_selection
            && button(ui, ButtonKind::Ghost, "Clear", true)
                .on_hover_text("Clear the In/Out marks (Esc)")
                .clicked()
        {
            editor.clear_selection();
        }

        vertical_divider(ui);

        if button(
            ui,
            ButtonKind::Subtle,
            &format!("✂  Quick cut {:.1}s", editor.cut_duration_preset),
            true,
        )
        .on_hover_text("Cut a fixed-length range starting at the playhead")
        .clicked()
        {
            editor.add_cut_scene_at(editor.current_time, editor.cut_duration_preset);
        }
        ui.add_sized(
            [54.0, 26.0],
            egui::DragValue::new(&mut editor.cut_duration_preset)
                .speed(0.1)
                .range(0.2..=60.0)
                .custom_formatter(|v, _| format!("{v:.1}s")),
        )
        .on_hover_text("Quick cut length");

        // Right side: zoom & snapping
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let anchor = editor.current_time;
            if icon_button(ui, "+", editor.timeline_zoom < 200.0)
                .on_hover_text("Zoom in (+ or ⌘ scroll)")
                .clicked()
            {
                editor.zoom_around(1.5, anchor);
            }
            ui.label(
                RichText::new(format!("{:.0}%", editor.timeline_zoom * 100.0))
                    .size(11.5)
                    .monospace()
                    .color(theme::TEXT_DIM),
            );
            if icon_button(ui, "−", editor.timeline_zoom > 1.0)
                .on_hover_text("Zoom out (−)")
                .clicked()
            {
                editor.zoom_around(1.0 / 1.5, anchor);
            }
            if button(ui, ButtonKind::Ghost, "Fit", editor.timeline_zoom > 1.0)
                .on_hover_text("Fit the whole video (0)")
                .clicked()
            {
                editor.timeline_zoom = 1.0;
                editor.clamp_view();
            }

            vertical_divider(ui);

            let snap_kind = if editor.snapping_enabled {
                ButtonKind::Tinted(theme::ACCENT)
            } else {
                ButtonKind::Subtle
            };
            if button(ui, snap_kind, "Snap", true)
                .on_hover_text("Snap edges to the playhead, marks and other cuts (S)")
                .clicked()
            {
                editor.snapping_enabled = !editor.snapping_enabled;
            }
        });
    });
}

// ── Inspector (right sidebar) ────────────────────────────────────────────────

fn render_inspector(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    // Summary
    section_label(ui, "SUMMARY");
    card(ui, |ui| {
        let kept = editor.kept_duration();
        let removed = (editor.total_duration - kept).max(0.0);
        stat_row(ui, "Original", format_timecode(editor.total_duration), theme::TEXT);
        stat_row(ui, "Removed", format!("−{}", format_timecode(removed)), theme::CUT);
        stat_row(ui, "Final length", format_timecode(kept), theme::SUCCESS);
        ui.add_space(8.0);
        let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 6.0), Sense::hover());
        ui.painter().rect_filled(bar, 3.0, theme::CUT.gamma_multiply(0.45));
        let frac = (kept / editor.total_duration.max(0.001)).clamp(0.0, 1.0) as f32;
        let kept_bar = Rect::from_min_size(bar.min, Vec2::new(bar.width() * frac, bar.height()));
        ui.painter().rect_filled(kept_bar, 3.0, theme::SUCCESS);
    });
    ui.add_space(16.0);

    // Selected cut
    if let Some(cut_idx) = editor
        .selected_cut_id
        .and_then(|id| editor.cuts.iter().position(|c| c.id == id))
    {
        render_cut_inspector(ui, editor, cut_idx);
        ui.add_space(16.0);
    }

    // Cut list
    ui.horizontal(|ui| {
        section_label(ui, &format!("CUTS  ·  {}", editor.cuts.len()));
        if !editor.cuts.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui
                    .add(egui::Label::new(RichText::new("Clear all").size(11.0).color(theme::TEXT_FAINT)).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Remove every cut (undo with ⌘Z)")
                    .clicked()
                {
                    editor.clear_all_cuts();
                }
            });
        }
    });
    if editor.cuts.is_empty() {
        ui.label(
            RichText::new(
                "No cuts yet. Press I and O to mark a range, then Delete. \
                 Or Shift-drag on the timeline.",
            )
            .size(12.0)
            .color(theme::TEXT_FAINT),
        );
    } else {
        render_cut_list(ui, editor);
    }
    ui.add_space(16.0);

    // Preview options
    section_label(ui, "PREVIEW");
    card(ui, |ui| {
        toggle_row(
            ui,
            "Mouse cursor",
            "Draw the recorded cursor from telemetry",
            &mut editor.show_mouse_cursor,
        );
        toggle_row(
            ui,
            "Click halos",
            "Show a ripple where clicks happened",
            &mut editor.show_click_ripples,
        );
        toggle_row(
            ui,
            "Skip cuts on playback",
            "Jump over removed ranges while previewing",
            &mut editor.skip_cut_scenes,
        );
        let arrow_label = format!("Arrows ({})", editor.arrows.len());
        toggle_row(
            ui,
            &arrow_label,
            &format!(
                "Arrows drawn by holding {} while moving the mouse during recording",
                editor.keys.arrow.label()
            ),
            &mut editor.arrow_style.enabled,
        );
    });
    ui.add_space(16.0);

    // Shortcuts
    section_label(ui, "SHORTCUTS");
    egui::Grid::new("editor_shortcuts")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            for (keys, what) in [
                ("Space", "Play / pause"),
                ("I  O", "Mark in / out"),
                ("Del", "Cut selection or delete cut"),
                ("← →", "Step frame (Shift: 1 second)"),
                ("Opt ← →", "Nudge selected cut"),
                ("⌘Z", "Undo  (Shift ⌘Z redo)"),
                ("+ − 0", "Zoom in / out / fit"),
                ("S", "Toggle snapping"),
                ("Esc", "Clear selection"),
            ] {
                kbd(ui, keys);
                ui.label(RichText::new(what).size(11.5).color(theme::TEXT_DIM));
                ui.end_row();
            }
        });
}

fn render_cut_inspector(ui: &mut egui::Ui, editor: &mut VideoEditor, cut_idx: usize) {
    let total_d = editor.total_duration.max(0.1);
    let cut_id = editor.cuts[cut_idx].id;
    let mut cut_start = editor.cuts[cut_idx].start_secs;
    let mut cut_end = editor.cuts[cut_idx].end_secs;
    let playhead = editor.current_time;
    let mut modified_time: Option<f64> = None;
    let mut remove_requested = false;
    let mut preview_requested = false;

    section_label(ui, "SELECTED CUT");
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::CUT.gamma_multiply(0.6)))
        .corner_radius(8.0)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Cut #{cut_id}"))
                        .size(13.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    badge(ui, &format!("{:.2}s", cut_end - cut_start), theme::CUT);
                });
            });
            ui.add_space(6.0);

            if time_field(ui, "Start", &mut cut_start, 0.0, cut_end - 0.05, playhead) {
                modified_time = Some(cut_start);
            }
            if time_field(ui, "End", &mut cut_end, cut_start + 0.05, total_d, playhead) {
                modified_time = Some(cut_end);
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if button(ui, ButtonKind::Subtle, "▶  Preview", true)
                    .on_hover_text("Play from the start of this cut")
                    .clicked()
                {
                    preview_requested = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(ui, ButtonKind::Tinted(theme::CUT), "Delete cut", true).clicked() {
                        remove_requested = true;
                    }
                });
            });
        });

    if remove_requested {
        editor.remove_cut(cut_id);
    } else if preview_requested {
        editor.seek(cut_start);
        editor.is_playing = true;
        editor.last_play_instant = Instant::now();
    } else if let Some(new_t) = modified_time {
        editor.push_history();
        if let Some(cut) = editor.cuts.get_mut(cut_idx) {
            cut.start_secs = cut_start;
            cut.end_secs = cut_end;
        }
        editor.seek(new_t);
        ui.ctx().request_repaint();
    }
}

fn render_cut_list(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    let mut rows: Vec<CutScene> = editor.cuts.clone();
    rows.sort_by(|a, b| a.start_secs.total_cmp(&b.start_secs));

    let mut select: Option<(usize, f64)> = None;
    let mut remove: Option<usize> = None;

    for (n, cut) in rows.iter().enumerate() {
        let is_sel = editor.selected_cut_id == Some(cut.id);
        let (row, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
        let close_rect = Rect::from_center_size(
            Pos2::new(row.right() - 16.0, row.center().y),
            Vec2::splat(22.0),
        );
        let over_close = resp.hover_pos().is_some_and(|p| close_rect.contains(p));

        let painter = ui.painter();
        let fill = if is_sel {
            theme::SURFACE_HI
        } else if resp.hovered() {
            theme::SURFACE
        } else {
            Color32::TRANSPARENT
        };
        let stroke = if is_sel {
            Stroke::new(1.0, theme::CUT.gamma_multiply(0.6))
        } else {
            Stroke::NONE
        };
        painter.rect(row, 6.0, fill, stroke, egui::StrokeKind::Inside);
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(row.left() + 8.0, row.top() + 9.0),
                Vec2::new(3.0, row.height() - 18.0),
            ),
            1.5,
            theme::CUT,
        );
        painter.text(
            Pos2::new(row.left() + 20.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}", n + 1),
            FontId::monospace(11.0),
            theme::TEXT_FAINT,
        );
        painter.text(
            Pos2::new(row.left() + 40.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            format!(
                "{} → {}",
                format_timecode(cut.start_secs),
                format_timecode(cut.end_secs)
            ),
            FontId::monospace(11.5),
            theme::TEXT,
        );
        painter.text(
            Pos2::new(close_rect.left() - 6.0, row.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{:.2}s", cut.duration()),
            FontId::monospace(11.0),
            theme::TEXT_DIM,
        );
        if resp.hovered() || is_sel {
            if over_close {
                painter.rect_filled(close_rect, 5.0, theme::CUT.gamma_multiply(0.25));
            }
            painter.text(
                close_rect.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                FontId::proportional(11.0),
                if over_close { theme::CUT } else { theme::TEXT_FAINT },
            );
        }

        if resp.clicked() {
            if over_close {
                remove = Some(cut.id);
            } else {
                select = Some((cut.id, cut.start_secs));
            }
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.add_space(2.0);
    }

    if let Some(id) = remove {
        editor.remove_cut(id);
    } else if let Some((id, t)) = select {
        editor.selected_cut_id = Some(id);
        editor.seek(t);
        editor.ensure_visible(t);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Timeline Track Rendering
// ─────────────────────────────────────────────────────────────────────────────

/// Maps between recording time and screen x on the timeline. The timeline shows
/// the final video: cut-out parts take no space, so x follows final-video time.
struct TimeMap {
    left: f32,
    /// Final-video second at the left edge
    start: f64,
    px_per_sec: f64,
    kept: Vec<(f64, f64)>,
    total_out: f64,
}

impl TimeMap {
    fn new(editor: &VideoEditor, rect: Rect) -> Self {
        let kept = compute_kept_segments(&editor.cuts, editor.total_duration);
        let total_out = kept.iter().map(|&(s, e)| e - s).sum::<f64>().max(0.05);
        Self {
            left: rect.left(),
            start: editor.timeline_view_start,
            px_per_sec: rect.width() as f64 / editor.view_duration(),
            kept,
            total_out,
        }
    }

    fn out_of(&self, t: f64) -> f64 {
        output_time_in(&self.kept, t)
    }

    /// Screen x of final-video time `out`.
    fn x_out(&self, out: f64) -> f32 {
        self.left + ((out - self.start) * self.px_per_sec) as f32
    }

    /// Screen x of recording time `t`.
    fn x(&self, t: f64) -> f32 {
        self.x_out(self.out_of(t))
    }

    /// Final-video time under screen x.
    fn out_at(&self, x: f32) -> f64 {
        (self.start + (x - self.left) as f64 / self.px_per_sec).clamp(0.0, self.total_out)
    }

    /// Recording time under screen x (never inside a cut).
    fn t(&self, x: f32) -> f64 {
        source_time_in(&self.kept, self.out_at(x))
    }

    fn is_kept(&self, t: f64) -> bool {
        self.kept.iter().any(|&(s, e)| t >= s && t <= e)
    }
}

/// A place where one or more touching cuts were removed from the timeline.
struct CutMarker {
    ids: Vec<usize>,
    /// Final-video time where the removed part was
    out: f64,
    removed_secs: f64,
}

fn cut_markers(editor: &VideoEditor, map: &TimeMap) -> Vec<CutMarker> {
    // The removed ranges are the gaps between the kept parts
    let mut gaps = Vec::new();
    let mut cursor = 0.0;
    for &(s, e) in &map.kept {
        if s > cursor + 1e-6 {
            gaps.push((cursor, s));
        }
        cursor = e;
    }
    if editor.total_duration > cursor + 1e-6 {
        gaps.push((cursor, editor.total_duration));
    }
    gaps.into_iter()
        .map(|(s, e)| CutMarker {
            ids: editor
                .cuts
                .iter()
                .filter(|c| c.end_secs > s - 1e-6 && c.start_secs < e + 1e-6)
                .map(|c| c.id)
                .collect(),
            out: map.out_of(s),
            removed_secs: e - s,
        })
        .filter(|m| !m.ids.is_empty())
        .collect()
}

/// The undo pill drawn above a cut marker.
fn cut_pill_rect(map: &TimeMap, marker: &CutMarker, ruler: Rect) -> Rect {
    Rect::from_center_size(
        Pos2::new(map.x_out(marker.out), ruler.top() + 10.0),
        Vec2::new(56.0, 18.0),
    )
}

/// Small curved "undo" arrow drawn with lines (the ↶ glyph is not in the font).
fn draw_undo_icon(painter: &egui::Painter, center: Pos2, color: Color32) {
    let r = 4.5;
    let points: Vec<Pos2> = (0..=10)
        .map(|i| {
            let a = std::f32::consts::PI * (1.15 + 1.25 * i as f32 / 10.0);
            center + Vec2::new(a.cos() * r, a.sin() * r + 1.0)
        })
        .collect();
    let tip = points[0];
    painter.add(egui::Shape::line(points, Stroke::new(1.4, color)));
    painter.add(egui::Shape::convex_polygon(
        vec![tip + Vec2::new(-3.0, -2.5), tip + Vec2::new(2.5, -1.5), tip + Vec2::new(-0.5, 3.0)],
        color,
        Stroke::NONE,
    ));
}

/// Snaps `t` to the nearest candidate within 8px on screen. Returns the snapped
/// time and the guide to draw.
fn snap_time(editor: &VideoEditor, t: f64, map: &TimeMap, dragging: DragTarget) -> (f64, Option<f64>) {
    if !editor.snapping_enabled {
        return (t, None);
    }
    let mut candidates = vec![0.0, editor.total_duration];
    if !matches!(
        dragging,
        DragTarget::Playhead | DragTarget::SelectionIn | DragTarget::SelectionOut
    ) {
        candidates.push(editor.current_time);
    }
    if !matches!(dragging, DragTarget::SelectionIn | DragTarget::NewSelection(_)) {
        candidates.extend(editor.mark_in);
    }
    if !matches!(dragging, DragTarget::SelectionOut | DragTarget::NewSelection(_)) {
        candidates.extend(editor.mark_out);
    }
    for c in &editor.cuts {
        candidates.push(c.start_secs);
        candidates.push(c.end_secs);
    }

    let x = map.x(t);
    candidates
        .into_iter()
        .filter(|c| (map.x(*c) - x).abs() <= 8.0)
        .min_by(|a, b| (map.x(*a) - x).abs().total_cmp(&(map.x(*b) - x).abs()))
        .map_or((t, None), |c| (c, Some(c)))
}

fn format_ruler(t: f64, step: f64) -> String {
    let mins = (t / 60.0).floor() as u32;
    let secs = t - mins as f64 * 60.0;
    if step < 1.0 {
        format!("{mins}:{secs:04.1}")
    } else {
        format!("{mins}:{:02}", secs.round() as u32)
    }
}

fn render_timeline_track(ui: &mut egui::Ui, editor: &mut VideoEditor) {
    const RULER_H: f32 = 24.0;
    const LANE_H: f32 = 56.0;
    const OVERVIEW_H: f32 = 8.0;

    let avail_w = ui.available_width();
    let total_h = RULER_H + LANE_H + 10.0 + OVERVIEW_H;
    let (track_rect, track_resp) =
        ui.allocate_exact_size(Vec2::new(avail_w, total_h), Sense::click_and_drag());

    let ruler_rect = Rect::from_min_size(track_rect.min, Vec2::new(avail_w, RULER_H));
    let strip_rect = Rect::from_min_size(ruler_rect.left_bottom(), Vec2::new(avail_w, LANE_H));
    let overview_rect = Rect::from_min_max(
        Pos2::new(track_rect.left(), track_rect.bottom() - OVERVIEW_H),
        track_rect.right_bottom(),
    );
    let lane_rect = ruler_rect.union(strip_rect);
    let total_d = editor.total_duration.max(0.1);

    // ── Wheel zoom (⌘ scroll / pinch) and horizontal panning ─────────────────
    if track_resp.hovered() {
        let (scroll, zoom, hover) =
            ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta(), i.pointer.hover_pos()));
        let map = TimeMap::new(editor, strip_rect);
        if zoom != 1.0 {
            let anchor = hover.map_or(editor.current_time, |p| map.t(p.x));
            editor.zoom_around(zoom as f64, anchor);
        }
        let delta = if scroll.x.abs() > scroll.y.abs() { scroll.x } else { scroll.y };
        if delta != 0.0 && editor.timeline_zoom > 1.0 {
            editor.timeline_view_start -= delta as f64 / map.px_per_sec;
            editor.clamp_view();
        }
    }
    editor.clamp_view();
    let map = TimeMap::new(editor, strip_rect);
    let total_out = map.total_out;
    let overview_out = |x: f32| -> f64 {
        (((x - overview_rect.left()) / overview_rect.width()).clamp(0.0, 1.0) as f64) * total_out
    };

    let markers = cut_markers(editor, &map);
    let hit_pill = |pos: Pos2| -> Option<&CutMarker> {
        markers
            .iter()
            .find(|m| cut_pill_rect(&map, m, ruler_rect).expand(2.0).contains(pos))
    };
    let hit_marker_bar = |pos: Pos2| -> Option<&CutMarker> {
        if !strip_rect.contains(pos) {
            return None;
        }
        markers.iter().find(|m| (map.x_out(m.out) - pos.x).abs() <= 5.0)
    };

    let playhead_x = map.x(editor.current_time);
    // In / Out tabs on either side of the playhead head, with a gap so a click on
    // the playhead itself never hits a tab
    let ear_in = Rect::from_min_size(Pos2::new(playhead_x - 23.0, ruler_rect.top() + 2.0), Vec2::new(14.0, 18.0));
    let ear_out = Rect::from_min_size(Pos2::new(playhead_x + 9.0, ruler_rect.top() + 2.0), Vec2::new(14.0, 18.0));
    // Extra click area only away from the playhead (outward, up and down)
    let on_ear_in = |p: Pos2| {
        Rect::from_min_max(ear_in.min - Vec2::new(4.0, 3.0), ear_in.max + Vec2::new(0.0, 4.0)).contains(p)
    };
    let on_ear_out = |p: Pos2| {
        Rect::from_min_max(ear_out.min - Vec2::new(0.0, 3.0), ear_out.max + Vec2::new(4.0, 4.0)).contains(p)
    };

    // Edges of an existing selection can be grabbed in the clip lane and moved
    let selection_edges = match (editor.mark_in, editor.mark_out) {
        (Some(a), Some(b)) if (b - a).abs() >= 0.02 => Some((map.x(a.min(b)), map.x(a.max(b)))),
        _ => None,
    };
    let on_selection_edge = |p: Pos2| -> Option<DragTarget> {
        let (xs, xe) = selection_edges?;
        if !strip_rect.contains(p) {
            return None;
        }
        let (ds, de) = ((p.x - xs).abs(), (p.x - xe).abs());
        if ds <= 7.0 && ds <= de {
            Some(DragTarget::SelectionIn)
        } else if de <= 7.0 {
            Some(DragTarget::SelectionOut)
        } else {
            None
        }
    };

    let any_down = ui.input(|i| i.pointer.any_down());
    if !any_down {
        editor.active_drag = None;
        editor.snap_guide = None;
    }

    // ── Hover cursor feedback ────────────────────────────────────────────────
    let hover_pos = track_resp.hover_pos();
    if let Some(pos) = hover_pos {
        let icon = if editor.active_drag == Some(DragTarget::Pan) {
            egui::CursorIcon::Grabbing
        } else if hit_pill(pos).is_some() || hit_marker_bar(pos).is_some() {
            egui::CursorIcon::PointingHand
        } else if on_ear_in(pos) || on_ear_out(pos) || on_selection_edge(pos).is_some() {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::Default
        };
        ui.ctx().set_cursor_icon(icon);
        if let Some(m) = hit_pill(pos) {
            track_resp.clone().on_hover_text(format!(
                "Undo this cut ({:.2}s): put it back and select it",
                m.removed_secs
            ));
        }
    }

    // ── Drag start hit testing ───────────────────────────────────────────────
    if track_resp.drag_started() {
        // Decide what was grabbed from where the mouse was pressed: a drag only starts
        // after the mouse has moved a little, and by then it may have left a small tab
        let press = ui.input(|i| i.pointer.press_origin());
        if let Some(pos) = press.or_else(|| track_resp.interact_pointer_pos()) {
            let (alt, shift, middle) = ui.input(|i| {
                (
                    i.modifiers.alt,
                    i.modifiers.shift,
                    i.pointer.button_down(egui::PointerButton::Middle),
                )
            });
            let click_t = map.t(pos.x);
            let target = if alt || middle {
                DragTarget::Pan
            } else if overview_rect.expand2(Vec2::new(0.0, 4.0)).contains(pos) {
                DragTarget::Overview
            } else if on_ear_in(pos) {
                // Start a new selection that ends at the playhead; dragging sets its start
                editor.mark_in = Some(editor.current_time);
                editor.mark_out = Some(editor.current_time);
                DragTarget::SelectionIn
            } else if on_ear_out(pos) {
                // Start a new selection that begins at the playhead; dragging sets its end
                editor.mark_in = Some(editor.current_time);
                editor.mark_out = Some(editor.current_time);
                DragTarget::SelectionOut
            } else if let Some(edge) = on_selection_edge(pos) {
                // Move just this edge; the other one stays where it is
                if let (Some(a), Some(b)) = (editor.mark_in, editor.mark_out) {
                    editor.mark_in = Some(a.min(b));
                    editor.mark_out = Some(a.max(b));
                }
                edge
            } else if shift && strip_rect.contains(pos) {
                editor.mark_in = Some(click_t);
                editor.mark_out = None;
                DragTarget::NewSelection(click_t)
            } else {
                editor.seek(click_t);
                DragTarget::Playhead
            };
            editor.active_drag = Some(target);
        }
    }

    // ── Active dragging ──────────────────────────────────────────────────────
    if track_resp.dragged() {
        if let (Some(pos), Some(target)) = (track_resp.interact_pointer_pos(), editor.active_drag) {
            let raw_t = map.t(pos.x);
            let (t, guide) = snap_time(editor, raw_t, &map, target);
            editor.snap_guide = None;
            match target {
                DragTarget::SelectionIn => {
                    let limit = editor.mark_out.unwrap_or(total_d);
                    let v = t.clamp(0.0, limit);
                    editor.mark_in = Some(v);
                    editor.snap_guide = guide;
                    editor.seek(v);
                }
                DragTarget::SelectionOut => {
                    let limit = editor.mark_in.unwrap_or(0.0);
                    let v = t.clamp(limit, total_d);
                    editor.mark_out = Some(v);
                    editor.snap_guide = guide;
                    editor.seek(v);
                }
                DragTarget::NewSelection(anchor) => {
                    editor.mark_in = Some(anchor.min(t));
                    editor.mark_out = Some(anchor.max(t));
                    editor.snap_guide = guide;
                }
                DragTarget::Pan => {
                    let dx = track_resp.drag_delta().x as f64;
                    editor.timeline_view_start -= dx / map.px_per_sec;
                    editor.clamp_view();
                }
                DragTarget::Overview => {
                    let vd = editor.view_duration();
                    editor.timeline_view_start = overview_out(pos.x) - vd / 2.0;
                    editor.clamp_view();
                }
                DragTarget::Playhead => {
                    editor.snap_guide = guide;
                    editor.seek(t);
                }
            }
            ui.ctx().request_repaint();
        }
    }

    // ── Single click: undo a cut, select a cut, jump the view, or seek ───────
    if track_resp.clicked() {
        if let Some(pos) = track_resp.interact_pointer_pos() {
            if overview_rect.expand2(Vec2::new(0.0, 4.0)).contains(pos) {
                let vd = editor.view_duration();
                editor.timeline_view_start = overview_out(pos.x) - vd / 2.0;
                editor.clamp_view();
            } else if on_ear_in(pos) {
                // Click on the green tab: selection starts at the playhead
                editor.set_mark_in_at_playhead();
            } else if on_ear_out(pos) {
                // Click on the red tab: selection ends at the playhead
                editor.set_mark_out_at_playhead();
            } else if let Some(ids) = hit_pill(pos).map(|m| m.ids.clone()) {
                editor.restore_cuts(&ids);
            } else if let Some(id) = hit_marker_bar(pos).and_then(|m| m.ids.first().copied()) {
                editor.selected_cut_id = Some(id);
            } else {
                editor.seek(map.t(pos.x));
            }
            ui.ctx().request_repaint();
        }
    }

    // Re-read the view: dragging, zooming or undoing a cut may have changed it
    let map = TimeMap::new(editor, strip_rect);
    let markers = cut_markers(editor, &map);
    let painter = ui.painter_at(track_rect);
    let view_end = editor.timeline_view_start + editor.view_duration();
    let visible = |x: f32| x >= strip_rect.left() - 2.0 && x <= strip_rect.right() + 2.0;

    // ── 1. Lane backgrounds ──────────────────────────────────────────────────
    painter.rect_filled(strip_rect, 6.0, theme::BG);

    // ── 2. Ruler ticks (final-video time) with adaptive spacing ──────────────
    let steps = [
        0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
    ];
    let major = steps
        .iter()
        .copied()
        .find(|s| s * map.px_per_sec >= 80.0)
        .unwrap_or(3600.0);
    let minor = major / 5.0;
    let first = (editor.timeline_view_start / minor).floor() as i64;
    for i in first.. {
        let t = i as f64 * minor;
        if t > view_end + minor {
            break;
        }
        if t < 0.0 || t > map.total_out {
            continue;
        }
        let x = map.x_out(t);
        let is_major = i.rem_euclid(5) == 0;
        let tick_h = if is_major { 8.0 } else { 4.0 };
        painter.line_segment(
            [
                Pos2::new(x, ruler_rect.bottom() - tick_h),
                Pos2::new(x, ruler_rect.bottom()),
            ],
            Stroke::new(1.0, if is_major { theme::TEXT_FAINT } else { theme::BORDER }),
        );
        if is_major && x + 44.0 < ruler_rect.right() {
            painter.text(
                Pos2::new(x + 4.0, ruler_rect.top() + 3.0),
                egui::Align2::LEFT_TOP,
                format_ruler(t, major),
                FontId::monospace(10.0),
                theme::TEXT_FAINT,
            );
        }
    }

    // ── 3. The final video clip ──────────────────────────────────────────────
    let clip_rect = Rect::from_min_max(
        Pos2::new(map.x_out(0.0), strip_rect.top() + 4.0),
        Pos2::new(map.x_out(map.total_out), strip_rect.bottom() - 4.0),
    );
    painter.rect(
        clip_rect,
        5.0,
        theme::CLIP,
        Stroke::new(1.0, theme::CLIP_EDGE),
        egui::StrokeKind::Inside,
    );
    let clip_name = editor
        .video_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Recording");
    painter.text(
        Pos2::new(clip_rect.left().max(strip_rect.left()) + 10.0, clip_rect.top() + 6.0),
        egui::Align2::LEFT_TOP,
        format!("🎬  {}", truncate_middle(clip_name, 40)),
        FontId::proportional(11.0),
        theme::TEXT.gamma_multiply(0.85),
    );

    // Arrow markers (⌘ gestures) along the top of the clip
    if editor.arrow_style.enabled {
        let [r, g, b, _] = editor.arrow_style.rgba;
        for arrow in &editor.arrows {
            if !map.is_kept(arrow.pressed_t) {
                continue;
            }
            let x = map.x(arrow.pressed_t);
            if !visible(x) {
                continue;
            }
            let top = clip_rect.top() + 2.0;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x - 5.0, top),
                    Pos2::new(x + 5.0, top),
                    Pos2::new(x, top + 7.0),
                ],
                Color32::from_rgb(r, g, b),
                Stroke::new(1.0, Color32::WHITE),
            ));
        }
    }

    // Click markers from telemetry along the bottom of the clip
    if let Some(ref tel) = editor.telemetry {
        let y = clip_rect.bottom() - 9.0;
        for s in &tel.samples {
            if !matches!(
                s.event.as_str(),
                "left_click" | "right_click" | "double_click" | "middle_click"
            ) || !map.is_kept(s.t)
            {
                continue;
            }
            let x = map.x(s.t);
            if visible(x) {
                painter.circle_filled(Pos2::new(x, y), 2.5, theme::WARN.gamma_multiply(0.85));
            }
        }
    }

    // ── 4. Mark In / Out selection range ─────────────────────────────────────
    if editor.mark_in.is_some() || editor.mark_out.is_some() {
        let in_t = editor.mark_in.unwrap_or(editor.current_time);
        let out_t = editor.mark_out.unwrap_or(editor.current_time);
        let (s, e) = (in_t.min(out_t), in_t.max(out_t));
        if e - s >= 0.02 {
            let sel_r = Rect::from_min_max(
                Pos2::new(map.x(s), ruler_rect.top()),
                Pos2::new(map.x(e), strip_rect.bottom()),
            );
            painter.rect_filled(sel_r, 3.0, theme::ACCENT.gamma_multiply(0.18));
            painter.line_segment([sel_r.left_top(), sel_r.left_bottom()], Stroke::new(1.5, theme::MARK_IN));
            painter.line_segment([sel_r.right_top(), sel_r.right_bottom()], Stroke::new(1.5, theme::MARK_OUT));

            // Grab handles on both edges (drag to move that edge)
            if editor.mark_in.is_some() && editor.mark_out.is_some() {
                for (x, color, target) in [
                    (sel_r.left(), theme::MARK_IN, DragTarget::SelectionIn),
                    (sel_r.right(), theme::MARK_OUT, DragTarget::SelectionOut),
                ] {
                    let hot = editor.active_drag == Some(target)
                        || hover_pos.is_some_and(|p| on_selection_edge(p) == Some(target));
                    let handle = Rect::from_center_size(
                        Pos2::new(x, strip_rect.center().y),
                        Vec2::new(if hot { 9.0 } else { 7.0 }, 26.0),
                    );
                    let fill = if hot { color.lerp_to_gamma(Color32::WHITE, 0.2) } else { color };
                    painter.rect(handle, 3.5, fill, Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.7)), egui::StrokeKind::Inside);
                    for dy in [-4.0, 0.0, 4.0] {
                        painter.line_segment(
                            [
                                Pos2::new(handle.center().x - 1.5, handle.center().y + dy),
                                Pos2::new(handle.center().x + 1.5, handle.center().y + dy),
                            ],
                            Stroke::new(1.0, Color32::WHITE),
                        );
                    }
                }
            }
        }
    }

    // ── 5. Removed parts: a red bar with an undo pill on top ─────────────────
    for marker in &markers {
        let x = map.x_out(marker.out);
        if !visible(x) {
            continue;
        }
        let selected = marker
            .ids
            .iter()
            .any(|id| editor.selected_cut_id == Some(*id));
        let bar_hot = hover_pos.is_some_and(|p| strip_rect.contains(p) && (p.x - x).abs() <= 5.0);
        let bar = Rect::from_center_size(
            Pos2::new(x, strip_rect.center().y),
            Vec2::new(if bar_hot || selected { 5.0 } else { 4.0 }, strip_rect.height() - 2.0),
        );
        painter.rect(
            bar,
            2.0,
            theme::CUT,
            Stroke::new(if selected { 1.5 } else { 0.0 }, theme::SELECTED),
            egui::StrokeKind::Outside,
        );

        let pill = cut_pill_rect(&map, marker, ruler_rect);
        let pill_hot = hover_pos.is_some_and(|p| pill.expand(2.0).contains(p));
        let fill = if pill_hot {
            theme::CUT
        } else {
            theme::CUT.lerp_to_gamma(theme::BG, 0.35)
        };
        painter.rect(pill, 9.0, fill, Stroke::new(1.0, theme::CUT), egui::StrokeKind::Inside);
        draw_undo_icon(&painter, Pos2::new(pill.left() + 11.0, pill.center().y), Color32::WHITE);
        painter.text(
            Pos2::new(pill.left() + 20.0, pill.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{:.1}s", marker.removed_secs),
            FontId::proportional(10.5),
            Color32::WHITE,
        );
    }

    // ── 6. Hover time indicator (final-video time) ───────────────────────────
    if let Some(pos) = hover_pos {
        let over_pill = markers
            .iter()
            .any(|m| cut_pill_rect(&map, m, ruler_rect).expand(2.0).contains(pos));
        if editor.active_drag.is_none() && lane_rect.contains(pos) && !over_pill {
            let out = map.out_at(pos.x);
            let x = map.x_out(out);
            painter.line_segment(
                [Pos2::new(x, ruler_rect.top()), Pos2::new(x, strip_rect.bottom())],
                Stroke::new(1.0, theme::TEXT.gamma_multiply(0.25)),
            );
            let label = format_timecode(out);
            let galley = painter.layout_no_wrap(label, FontId::monospace(10.0), theme::TEXT);
            let r = Rect::from_min_size(
                Pos2::new(x + 6.0, ruler_rect.top() + 2.0),
                galley.size() + Vec2::new(8.0, 4.0),
            );
            painter.rect_filled(r, 3.0, theme::SURFACE_HI);
            painter.galley(r.min + Vec2::new(4.0, 2.0), galley, theme::TEXT);
        }
    }

    // ── 7. Snap guide ────────────────────────────────────────────────────────
    if let Some(g) = editor.snap_guide {
        let x = map.x(g);
        painter.line_segment(
            [Pos2::new(x, ruler_rect.top()), Pos2::new(x, strip_rect.bottom())],
            Stroke::new(1.0, theme::SELECTED),
        );
    }

    // ── 8. Playhead with Mark In / Out ears ──────────────────────────────────
    let px = map.x(editor.current_time);
    if visible(px) {
        painter.line_segment(
            [Pos2::new(px, ruler_rect.top() + 14.0), Pos2::new(px, strip_rect.bottom())],
            Stroke::new(2.0, theme::PLAYHEAD),
        );
        let head = vec![
            Pos2::new(px - 6.0, ruler_rect.top() + 2.0),
            Pos2::new(px + 6.0, ruler_rect.top() + 2.0),
            Pos2::new(px + 6.0, ruler_rect.top() + 11.0),
            Pos2::new(px, ruler_rect.top() + 17.0),
            Pos2::new(px - 6.0, ruler_rect.top() + 11.0),
        ];
        painter.add(egui::Shape::convex_polygon(head, theme::PLAYHEAD, Stroke::NONE));

        for (ear, color, label, hot) in [
            (ear_in, theme::MARK_IN, "[", hover_pos.is_some_and(|p| on_ear_in(p))),
            (ear_out, theme::MARK_OUT, "]", hover_pos.is_some_and(|p| on_ear_out(p))),
        ] {
            let fill = if hot { color.lerp_to_gamma(Color32::WHITE, 0.2) } else { color.gamma_multiply(0.85) };
            painter.rect(ear, 4.0, fill, Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            painter.text(ear.center(), egui::Align2::CENTER_CENTER, label, FontId::monospace(12.0), Color32::WHITE);
        }
    }

    // ── 9. Overview bar: the whole final video, cut points and the visible window
    painter.rect_filled(overview_rect, 3.0, theme::SURFACE);
    let ov_x = |out: f64| overview_rect.left() + (out / map.total_out) as f32 * overview_rect.width();
    for marker in &markers {
        let x = ov_x(marker.out);
        painter.rect_filled(
            Rect::from_center_size(Pos2::new(x, overview_rect.center().y), Vec2::new(2.0, overview_rect.height())),
            1.0,
            theme::CUT,
        );
    }
    if editor.timeline_zoom > 1.0 {
        let win = Rect::from_min_max(
            Pos2::new(ov_x(editor.timeline_view_start), overview_rect.top()),
            Pos2::new(ov_x(view_end), overview_rect.bottom()),
        );
        painter.rect(
            win,
            3.0,
            theme::ACCENT.gamma_multiply(0.25),
            Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    let opx = ov_x(map.out_of(editor.current_time));
    painter.line_segment(
        [Pos2::new(opx, overview_rect.top()), Pos2::new(opx, overview_rect.bottom())],
        Stroke::new(1.5, theme::PLAYHEAD),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Vector Cursor & Ripple Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Cursor images are rendered once per icon at this many pixels per cursor unit,
/// then scaled down with mipmapping wherever they are drawn.
const CURSOR_TEXTURE_UNIT_PX: f32 = 8.0;

#[derive(Clone)]
struct CursorTexture {
    texture: TextureHandle,
    hotspot: Vec2,
    size: Vec2,
}

fn cursor_texture(ctx: &egui::Context, icon: CursorIcon) -> CursorTexture {
    let id = egui::Id::new(("dexhowto_cursor_texture", icon));
    if let Some(cached) = ctx.data(|d| d.get_temp::<CursorTexture>(id)) {
        return cached;
    }
    // Same renderer as the export, so the preview matches the video exactly
    let sprite = crate::export::cursor_sprite(icon, CURSOR_TEXTURE_UNIT_PX);
    let image = ColorImage::from_rgba_unmultiplied([sprite.width, sprite.height], &sprite.rgba);
    let texture = ctx.load_texture(
        format!("cursor_{icon:?}"),
        image,
        TextureOptions {
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::LINEAR
        },
    );
    let cached = CursorTexture {
        texture,
        hotspot: Vec2::new(sprite.hotspot.0, sprite.hotspot.1),
        size: Vec2::new(sprite.width as f32, sprite.height as f32),
    };
    ctx.data_mut(|d| d.insert_temp(id, cached.clone()));
    cached
}

/// Draws a cursor icon with its hotspot at `tip`, `unit_px` pixels per cursor unit.
fn draw_cursor(painter: &egui::Painter, tip: Pos2, icon: CursorIcon, unit_px: f32) {
    let cursor = cursor_texture(painter.ctx(), icon);
    let k = unit_px / CURSOR_TEXTURE_UNIT_PX;
    let rect = Rect::from_min_size(tip - cursor.hotspot * k, cursor.size * k);
    painter.image(
        cursor.texture.id(),
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// Draws a pointer arrow from `from` to `to` (same shape as the exported arrows).
pub(crate) fn draw_arrow(painter: &egui::Painter, from: Pos2, to: Pos2, thickness: f32, rgba: [u8; 4], opacity: f32) {
    let t = thickness.max(1.0);
    let border = (t * 0.35).max(1.5);
    let ((sx, sy), head) = crate::arrows::arrow_geometry((from.x, from.y), (to.x, to.y), t);
    let shaft_end = Pos2::new(sx, sy);
    let head: Vec<Pos2> = head.iter().map(|&(x, y)| Pos2::new(x, y)).collect();
    let [r, g, b, a] = rgba;
    let fill = Color32::from_rgba_unmultiplied(r, g, b, a).gamma_multiply(opacity);
    let white = Color32::WHITE.gamma_multiply(opacity);

    // White border underneath, then the colored arrow
    painter.circle_filled(from, t / 2.0 + border, white);
    painter.line_segment([from, shaft_end], Stroke::new(t + border * 2.0, white));
    painter.add(egui::Shape::convex_polygon(head.clone(), white, Stroke::new(border * 2.0, white)));
    painter.circle_filled(from, t / 2.0, fill);
    painter.line_segment([from, shaft_end], Stroke::new(t, fill));
    painter.add(egui::Shape::convex_polygon(head, fill, Stroke::NONE));
}

fn draw_click_ripple(painter: &egui::Painter, center: Pos2, age_secs: f64, click_type: &str) {
    let progress = (age_secs / 0.35).clamp(0.0, 1.0) as f32;
    let radius = 6.0 + (progress * 22.0);
    let alpha = ((1.0 - progress) * 230.0) as u8;

    let (ring_col, fill_col) = if click_type == "right_click" || click_type == "right_down" {
        (
            Color32::from_rgba_unmultiplied(40, 180, 255, alpha),
            Color32::from_rgba_unmultiplied(20, 100, 200, alpha / 3),
        )
    } else {
        (
            Color32::from_rgba_unmultiplied(255, 200, 40, alpha),
            Color32::from_rgba_unmultiplied(255, 160, 20, alpha / 3),
        )
    };

    painter.circle_filled(center, radius, fill_col);
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(2.0 * (1.0 - progress * 0.5), ring_col),
    );

    if progress < 0.6 {
        let dot_alpha = ((1.0 - (progress / 0.6)) * 255.0) as u8;
        painter.circle_filled(
            center,
            3.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, dot_alpha),
        );
    }
}

fn format_timecode(secs: f64) -> String {
    let total_secs = secs.max(0.0);
    let mins = (total_secs / 60.0).floor() as u32;
    let rem_secs = total_secs % 60.0;
    format!("{mins:02}:{:05.2}", rem_secs)
}

/// Parses "mm:ss.xx", "ss.xx" or "h:mm:ss" back into seconds.
fn parse_timecode(text: &str) -> Option<f64> {
    text.trim()
        .split(':')
        .try_fold(0.0, |acc, part| Some(acc * 60.0 + part.trim().parse::<f64>().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_compute_kept_segments() {
        let cuts = vec![
            CutScene {
                id: 1,
                start_secs: 2.0,
                end_secs: 5.0,
            },
            CutScene {
                id: 2,
                start_secs: 7.0,
                end_secs: 8.5,
            },
        ];

        let kept = compute_kept_segments(&cuts, 10.0);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0], (0.0, 2.0));
        assert_eq!(kept[1], (5.0, 7.0));
        assert_eq!(kept[2], (8.5, 10.0));

        let total_kept: f64 = kept.iter().map(|&(s, e)| e - s).sum();
        assert!((total_kept - 5.5).abs() < 1e-6);
    }

    #[test]
    fn test_telemetry_sync_with_cut_ranges() {
        let temp_dir = std::env::temp_dir();
        let in_path = temp_dir.join("test_telemetry_input.jsonl");
        let out_path = temp_dir.join("test_telemetry_output.jsonl");

        {
            let mut file = File::create(&in_path).unwrap();
            writeln!(
                file,
                r#"{{"type":"session_start","version":"1.0","created_at":"2026-10-15T12:00:00Z","video_file":"raw.mp4","fps":60,"display":{{"id":1,"width":3840,"height":2160,"origin_x":0.0,"origin_y":0.0}}}}"#
            )
            .unwrap();

            // 10 samples from t = 0.5 to t = 5.0
            for i in 1..=10 {
                let t = (i as f64) * 0.5;
                let ms = (t * 1000.0) as u64;
                let frame = (t * 60.0) as u64;
                let evt = if i == 5 { "left_click" } else { "move" };
                writeln!(
                    file,
                    r#"{{"type":"mouse","t":{t},"time_ms":{ms},"frame":{frame},"x":100.0,"y":200.0,"norm_x":0.25,"norm_y":0.5,"is_inside":true,"global_x":100.0,"global_y":200.0,"event":"{evt}","button":null,"buttons":{{"left":false,"right":false,"middle":false}},"modifiers":[]}}"#
                )
                .unwrap();
            }

            writeln!(
                file,
                r#"{{"type":"session_end","t":5.5,"duration_secs":5.5,"total_records":12}}"#
            )
            .unwrap();
        }

        let session = TelemetrySession::load_from_file(&in_path).unwrap();
        assert_eq!(session.samples.len(), 10);

        // Cut out [1.2 .. 3.2] (removes samples at t=1.5, 2.0, 2.5, 3.0)
        // Kept segments: [0.0 .. 1.2] (duration 1.2), [3.2 .. 5.5] (duration 2.3)
        let kept_segments = vec![(0.0, 1.2), (3.2, 5.5)];
        let count = session
            .sync_with_cut_ranges(&kept_segments, "cut_video.mp4", &out_path, 60)
            .unwrap();

        // Kept samples: t=0.5, 1.0 (from seg 1) and t=3.5, 4.0, 4.5, 5.0 (from seg 2) = 6 samples
        assert_eq!(count, 6);

        let synced_session = TelemetrySession::load_from_file(&out_path).unwrap();
        assert_eq!(synced_session.samples.len(), 6);

        // Check video filename updated in session_start
        let vid_name = synced_session
            .session_start
            .get("video_file")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(vid_name, "cut_video.mp4");

        // Verify continuous monotonic timestamps
        let mut prev_t = -1.0;
        for s in &synced_session.samples {
            assert!(s.t > prev_t, "Timestamps must be strictly monotonic");
            assert!(s.t <= 3.51, "Final timestamp must fit inside new duration (1.2 + 2.3 = 3.5)");
            prev_t = s.t;
        }

        // Clean up
        let _ = std::fs::remove_file(in_path);
        let _ = std::fs::remove_file(out_path);
    }

    #[test]
    fn test_mark_in_out_and_cut_selection() {
        let temp_dir = std::env::temp_dir();
        let dummy_vid = temp_dir.join("dummy.mp4");
        let _ = File::create(&dummy_vid);

        let mut editor = VideoEditor::open(dummy_vid.clone(), None);
        editor.total_duration = 30.0;
        editor.current_time = 5.0;

        // Test Mark In
        editor.set_mark_in_at_playhead();
        assert_eq!(editor.mark_in, Some(5.0));
        assert_eq!(editor.mark_out, None);

        // Move playhead to 12.0 and Mark Out
        editor.current_time = 12.0;
        editor.set_mark_out_at_playhead();
        assert_eq!(editor.mark_in, Some(5.0));
        assert_eq!(editor.mark_out, Some(12.0));

        // Cut selection
        let cut_res = editor.cut_selection();
        assert!(cut_res);
        assert_eq!(editor.cuts.len(), 1);
        assert_eq!(editor.cuts[0].start_secs, 5.0);
        assert_eq!(editor.cuts[0].end_secs, 12.0);
        assert_eq!(editor.mark_in, None);
        assert_eq!(editor.mark_out, None);
        assert_eq!(editor.selected_cut_id, Some(1));

        // Test cut duration and kept duration
        assert!((editor.cuts[0].duration() - 7.0).abs() < 1e-6);
        assert!((editor.kept_duration() - 23.0).abs() < 1e-6);

        // Test Mark In with playhead as end
        editor.current_time = 15.0;
        editor.set_mark_in_at_playhead();
        editor.current_time = 18.5;
        let cut_res2 = editor.cut_selection();
        assert!(cut_res2);
        assert_eq!(editor.cuts.len(), 2);
        assert_eq!(editor.cuts[1].start_secs, 15.0);
        assert_eq!(editor.cuts[1].end_secs, 18.5);

        // Clean up
        let _ = std::fs::remove_file(dummy_vid);
    }

    #[test]
    fn test_resolve_recording_pair() {
        let temp_dir = std::env::temp_dir();
        let base_name = format!("test_rec_pair_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let vid_path = temp_dir.join(format!("{base_name}.mp4"));
        let tel_path = temp_dir.join(format!("{base_name}.mouse.jsonl"));
        let _ = File::create(&vid_path);
        let _ = File::create(&tel_path);

        // Given video path -> resolves both
        let (v1, t1) = resolve_recording_pair(&vid_path);
        assert_eq!(v1, Some(vid_path.clone()));
        assert_eq!(t1, Some(tel_path.clone()));

        // Given telemetry path -> resolves both
        let (v2, t2) = resolve_recording_pair(&tel_path);
        assert_eq!(v2, Some(vid_path.clone()));
        assert_eq!(t2, Some(tel_path.clone()));

        // Clean up
        let _ = std::fs::remove_file(&vid_path);
        let _ = std::fs::remove_file(&tel_path);
    }
    #[test]
    fn test_clean_moves_keeps_clicks_and_glides_between_them() {
        let path = std::env::temp_dir().join(format!(
            "test_clean_moves_{}.jsonl",
            std::process::id()
        ));
        {
            let mut file = File::create(&path).unwrap();
            writeln!(
                file,
                r#"{{"type":"session_start","fps":60,"display":{{"id":1,"width":1000,"height":500}}}}"#
            )
            .unwrap();
            let rec = |t: f64, x: f64, y: f64, evt: &str, left: bool| {
                format!(
                    r#"{{"type":"mouse","t":{t},"time_ms":{},"frame":0,"x":{x},"y":{y},"norm_x":{},"norm_y":{},"is_inside":true,"global_x":{},"global_y":{},"event":"{evt}","button":null,"buttons":{{"left":{left},"right":false,"middle":false}},"modifiers":[]}}"#,
                    (t * 1000.0) as u64,
                    x / 1000.0,
                    y / 500.0,
                    x / 2.0,
                    y / 2.0
                )
            };
            // Jittery recorded moves, a click at (100,100), more moves, a click at (900,400)
            for i in 0..20 {
                let t = i as f64 * 0.05;
                writeln!(file, "{}", rec(t, 50.0 + i as f64 * 7.0, 300.0 - i as f64 * 3.0, "move", false)).unwrap();
            }
            writeln!(file, "{}", rec(1.0, 100.0, 100.0, "left_down", true)).unwrap();
            writeln!(file, "{}", rec(1.08, 100.0, 100.0, "left_up", false)).unwrap();
            writeln!(file, "{}", rec(1.08, 100.0, 100.0, "left_click", false)).unwrap();
            for i in 0..40 {
                let t = 1.1 + i as f64 * 0.05;
                writeln!(file, "{}", rec(t, 300.0 + (i % 5) as f64 * 40.0, 50.0 + i as f64 * 9.0, "move", false)).unwrap();
            }
            // A click on another monitor: clamped to the edge and flagged as outside
            for (t, evt, left) in [(2.5, "left_down", true), (2.55, "left_up", false), (2.55, "left_click", false)] {
                let line = rec(t, 1000.0, 0.0, evt, left).replace(r#""is_inside":true"#, r#""is_inside":false"#);
                writeln!(file, "{line}").unwrap();
            }
            writeln!(file, "{}", rec(3.5, 900.0, 400.0, "left_down", true)).unwrap();
            writeln!(file, "{}", rec(3.6, 900.0, 400.0, "left_up", false)).unwrap();
            writeln!(file, "{}", rec(3.6, 900.0, 400.0, "left_click", false)).unwrap();
        }

        let session = TelemetrySession::load_from_file(&path).unwrap();
        let cleaned = session.clean_moves(60, &KeyBindings::default()).unwrap();
        let _ = std::fs::remove_file(&path);

        // Clicks inside the recorded screen survive, the outside one is gone,
        // and every remaining move is generated
        assert_eq!(cleaned.samples.iter().filter(|s| s.event == "left_click").count(), 2);
        assert!(cleaned.samples.iter().all(|s| s.is_inside));
        assert!(cleaned
            .samples
            .iter()
            .filter(|s| s.event == "move")
            .all(|s| s.raw_json.get("synthetic") == Some(&Value::Bool(true))));

        // Time never goes backwards
        assert!(cleaned.samples.windows(2).all(|w| w[0].t <= w[1].t));

        // The cursor sits exactly on each click when it happens
        let at_first = cleaned.sample_at(1.08).unwrap();
        assert!((at_first.x - 100.0).abs() < 0.01 && (at_first.y - 100.0).abs() < 0.01);
        let at_second = cleaned.sample_at(3.5).unwrap();
        assert!((at_second.x - 900.0).abs() < 0.01 && (at_second.y - 400.0).abs() < 0.01);

        // Normalized coordinates follow the pixel path
        for s in cleaned.samples.iter().filter(|s| s.event == "move") {
            assert!((s.norm_x - s.x / 1000.0).abs() < 1e-3);
            assert!((s.norm_y - s.y / 500.0).abs() < 1e-3);
        }

        // Smooth: no single frame jumps more than a fifth of the trip
        let trip = ((800.0f64).powi(2) + (300.0f64).powi(2)).sqrt();
        let glide: Vec<_> = cleaned
            .samples
            .iter()
            .filter(|s| s.t > 1.08 && s.t < 3.5)
            .collect();
        assert!(glide.len() > 20);
        for w in glide.windows(2) {
            let step = ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
            assert!(step < trip / 5.0, "jump of {step}px");
        }

        // Without clicks there is nothing to anchor to
        let no_clicks = TelemetrySession {
            samples: session.samples.iter().filter(|s| s.event == "move").cloned().collect(),
            ..session.clone()
        };
        assert!(no_clicks.clean_moves(60, &KeyBindings::default()).is_err());
    }
    #[test]
    fn test_clean_moves_keeps_ctrl_held_moves() {
        let path = std::env::temp_dir().join(format!(
            "test_clean_moves_ctrl_{}.jsonl",
            std::process::id()
        ));
        {
            let mut file = File::create(&path).unwrap();
            writeln!(
                file,
                r#"{{"type":"session_start","fps":60,"display":{{"id":1,"width":1000,"height":500}}}}"#
            )
            .unwrap();
            let rec = |t: f64, x: f64, y: f64, evt: &str, mods: &str| {
                format!(
                    r#"{{"type":"mouse","t":{t},"time_ms":{},"frame":0,"x":{x},"y":{y},"norm_x":{},"norm_y":{},"is_inside":true,"event":"{evt}","button":null,"buttons":{{"left":false,"right":false,"middle":false}},"modifiers":[{mods}]}}"#,
                    (t * 1000.0) as u64,
                    x / 1000.0,
                    y / 500.0
                )
            };
            writeln!(file, "{}", rec(0.5, 100.0, 100.0, "left_click", "")).unwrap();
            // Plain moves: removed
            for i in 0..10 {
                writeln!(file, "{}", rec(0.6 + i as f64 * 0.05, 150.0 + i as f64 * 20.0, 120.0, "move", "")).unwrap();
            }
            // A wiggly path drawn while holding Control: kept as recorded
            for i in 0..30 {
                let t = 1.5 + i as f64 * 0.02;
                let y = 200.0 + if i % 2 == 0 { 15.0 } else { -15.0 };
                writeln!(file, "{}", rec(t, 400.0 + i as f64 * 10.0, y, "move", r#""ctrl""#)).unwrap();
            }
            // More plain moves after releasing Control: removed
            for i in 0..10 {
                writeln!(file, "{}", rec(2.3 + i as f64 * 0.05, 700.0, 250.0 + i as f64 * 10.0, "move", "")).unwrap();
            }
            writeln!(file, "{}", rec(3.5, 900.0, 400.0, "left_click", "")).unwrap();
        }

        let session = TelemetrySession::load_from_file(&path).unwrap();
        let cleaned = session.clean_moves(60, &KeyBindings::default()).unwrap();
        let _ = std::fs::remove_file(&path);

        // Every Ctrl-held record survives untouched
        let ctrl_before: Vec<_> = session.samples.iter().filter(|s| s.ctrl_held()).collect();
        let ctrl_after: Vec<_> = cleaned.samples.iter().filter(|s| s.ctrl_held()).collect();
        assert_eq!(ctrl_before.len(), 30);
        assert_eq!(ctrl_after.len(), 30);
        for (a, b) in ctrl_before.iter().zip(&ctrl_after) {
            assert_eq!((a.t, a.x, a.y), (b.t, b.x, b.y));
            assert!(b.raw_json.get("synthetic").is_none());
        }

        // Nothing is generated inside the protected stretch
        let (first_t, last_t) = (ctrl_after[0].t, ctrl_after[29].t);
        assert!(!cleaned
            .samples
            .iter()
            .any(|s| s.t > first_t && s.t < last_t && !s.ctrl_held()));

        // Plain moves are gone; the gaps are bridged by generated glides
        assert!(cleaned
            .samples
            .iter()
            .filter(|s| s.event == "move" && !s.ctrl_held())
            .all(|s| s.raw_json.get("synthetic") == Some(&Value::Bool(true))));
        assert!(cleaned.samples.iter().any(|s| s.t > 0.5 && s.t < first_t && s.raw_json.get("synthetic").is_some()));
        assert!(cleaned.samples.iter().any(|s| s.t > last_t && s.t < 3.5 && s.raw_json.get("synthetic").is_some()));
        assert_eq!(count_ctrl_runs(&session.samples, &KeyBindings::default()), 1);
    }

    #[test]
    fn test_clean_move_after_cuts_glides_straight_between_kept_clicks() {
        let sample = |t: f64, x: f64, y: f64, event: &str| {
            let raw = serde_json::json!({
                "type": "mouse", "t": t, "x": x, "y": y,
                "norm_x": x / 1000.0, "norm_y": y / 1000.0, "event": event, "modifiers": []
            });
            TelemetrySample {
                t, time_ms: (t * 1000.0) as u64, frame: 0,
                norm_x: x / 1000.0, norm_y: y / 1000.0, x, y,
                is_inside: true, event: event.to_string(), button: None,
                left_down: false, right_down: false,
                click_duration_ms: None, click_count: None, raw_json: raw,
            }
        };
        let tel = TelemetrySession {
            session_start: serde_json::json!({"type": "session_start", "display": {"width": 1000, "height": 1000}}),
            samples: vec![
                sample(0.0, 100.0, 100.0, "move"),
                sample(1.0, 100.0, 100.0, "left_click"),
                // A click on the far side, inside the part that gets cut out
                sample(4.0, 900.0, 100.0, "left_click"),
                sample(8.0, 100.0, 900.0, "left_click"),
            ],
            session_end: None,
            jsonl_path: PathBuf::new(),
        };
        let cuts = vec![CutScene { id: 1, start_secs: 2.0, end_secs: 6.0 }];
        let kept = compute_kept_segments(&cuts, 9.0);
        let track = tel.retimed(&kept, 60).clean_moves(60, &KeyBindings::default()).unwrap();

        // The cut-out click is gone and the cursor never heads towards it
        assert_eq!(track.samples.iter().filter(|s| s.event == "left_click").count(), 2);
        assert!(track.samples.iter().all(|s| s.x < 150.0));
        // The second click lands at its new time in the cut video (8.0 - 4.0)
        let at_click = track.sample_at(4.0).unwrap();
        assert!((at_click.x - 100.0).abs() < 0.5 && (at_click.y - 900.0).abs() < 0.5);
    }

    #[test]
    fn test_restore_cut_brings_it_back_and_selects_it() {
        let dummy = std::env::temp_dir().join("dummy_restore.mp4");
        let _ = File::create(&dummy);
        let mut editor = VideoEditor::open(dummy.clone(), None);
        editor.total_duration = 20.0;
        editor.current_time = 4.0;
        editor.set_mark_in_at_playhead();
        editor.current_time = 9.0;
        editor.set_mark_out_at_playhead();
        assert!(editor.cut_selection());
        let id = editor.cuts[0].id;

        // The timeline is in final-video time: 5 seconds shorter, 9s now sits at 4s
        assert!((editor.output_duration() - 15.0).abs() < 1e-9);
        assert!((editor.output_time_of(9.0) - 4.0).abs() < 1e-9);
        assert!((editor.output_time_of(6.0) - 4.0).abs() < 1e-9);

        editor.restore_cuts(&[id]);
        assert!(editor.cuts.is_empty());
        assert_eq!((editor.mark_in, editor.mark_out), (Some(4.0), Some(9.0)));
        assert_eq!(editor.current_time, 4.0);
        assert!((editor.output_duration() - 20.0).abs() < 1e-9);

        // Undo puts the cut back
        editor.undo();
        assert_eq!(editor.cuts.len(), 1);
        let _ = std::fs::remove_file(dummy);
    }

    #[test]
    fn test_export_never_overwrites_earlier_exports() {
        let dir = std::env::temp_dir().join(format!("dexhowto_export_names_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("talk.mp4");

        assert_eq!(free_export_paths(&video).0, dir.join("talk_edited.mp4"));
        File::create(dir.join("talk_edited.mp4")).unwrap();
        assert_eq!(free_export_paths(&video).0, dir.join("talk_edited_2.mp4"));
        // A leftover telemetry file alone also counts as taken
        File::create(dir.join("talk_edited_2.mouse.jsonl")).unwrap();
        let (v, t) = free_export_paths(&video);
        assert_eq!(v, dir.join("talk_edited_3.mp4"));
        assert_eq!(t, dir.join("talk_edited_3.mouse.jsonl"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

