//! Background video export: cuts the recording, burns the telemetry cursor into
//! the frames with ffmpeg overlays, reports progress, and re-times the telemetry.

use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use crate::editor::TelemetrySession;

// ─────────────────────────────────────────────────────────────────────────────
// Cursor Style & Geometry (shared by the editor preview and the export)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CursorIcon {
    /// Black arrow with a white border, like the macOS pointer
    Mac,
    /// White arrow with a black border
    Classic,
    /// Rounded arrowhead without a tail
    Modern,
    /// Round dot, hotspot in the center
    Dot,
}

/// How one cursor icon is drawn. Lengths are in cursor units (see `UNIT_PX_AT_1080P`).
struct CursorShape {
    /// Outline of the body; the hotspot is at the origin
    points: &'static [(f32, f32)],
    /// Circle radius, used instead of `points` when > 0
    circle: f32,
    /// Corner rounding added around `points`
    round: f32,
    /// Border width drawn outside the body
    border: f32,
    fill: [u8; 4],
    border_color: [u8; 4],
}

const MAC_ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 16.2),
    (3.9, 12.6),
    (6.3, 18.3),
    (8.7, 17.3),
    (6.4, 11.8),
    (11.5, 11.5),
];

const MODERN_POINTER: [(f32, f32); 4] = [(0.0, 0.0), (1.4, 15.0), (5.4, 10.4), (11.4, 9.6)];

impl CursorIcon {
    pub const ALL: [CursorIcon; 4] = [
        CursorIcon::Mac,
        CursorIcon::Classic,
        CursorIcon::Modern,
        CursorIcon::Dot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CursorIcon::Mac => "macOS",
            CursorIcon::Classic => "Classic",
            CursorIcon::Modern => "Modern",
            CursorIcon::Dot => "Dot",
        }
    }

    fn shape(self) -> CursorShape {
        const BLACK: [u8; 4] = [12, 12, 14, 255];
        const WHITE: [u8; 4] = [255, 255, 255, 255];
        match self {
            CursorIcon::Mac => CursorShape {
                points: &MAC_ARROW,
                circle: 0.0,
                round: 0.25,
                border: 1.15,
                fill: BLACK,
                border_color: WHITE,
            },
            CursorIcon::Classic => CursorShape {
                points: &MAC_ARROW,
                circle: 0.0,
                round: 0.25,
                border: 1.0,
                fill: WHITE,
                border_color: BLACK,
            },
            CursorIcon::Modern => CursorShape {
                points: &MODERN_POINTER,
                circle: 0.0,
                round: 0.8,
                border: 1.3,
                fill: [24, 24, 30, 255],
                border_color: WHITE,
            },
            CursorIcon::Dot => CursorShape {
                points: &[],
                circle: 5.0,
                round: 0.0,
                border: 1.5,
                fill: [24, 24, 30, 255],
                border_color: WHITE,
            },
        }
    }
}

/// Pixels per cursor unit at 1080p and 100% size (the arrow ends up ~24px tall).
pub const UNIT_PX_AT_1080P: f32 = 24.0 / 19.0;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CursorStyle {
    /// Draw the cursor into the exported video
    pub burn_in: bool,
    pub icon: CursorIcon,
    /// 1.0 = natural size
    pub size: f32,
    /// Soft circle behind the cursor
    pub highlight: bool,
    /// Unmultiplied sRGBA
    pub highlight_rgba: [u8; 4],
    /// Highlight radius in pixels at 1080p
    pub highlight_radius: f32,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            burn_in: true,
            icon: CursorIcon::Mac,
            size: 1.0,
            highlight: false,
            highlight_rgba: [255, 210, 60, 120],
            highlight_radius: 28.0,
        }
    }
}

impl CursorStyle {
    /// Pixels per cursor unit for a frame `frame_height` pixels tall.
    pub fn unit_px(&self, frame_height: f32) -> f32 {
        UNIT_PX_AT_1080P * self.size * frame_height / 1080.0
    }

    pub fn highlight_px(&self, frame_height: f32) -> f32 {
        self.highlight_radius * frame_height / 1080.0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sprite Rasterization (RGBA images fed to ffmpeg as overlay inputs)
// ─────────────────────────────────────────────────────────────────────────────

pub struct Sprite {
    pub width: usize,
    pub height: usize,
    /// Unmultiplied RGBA8
    pub rgba: Vec<u8>,
    /// Pixel inside the sprite that sits on the pointer position
    pub hotspot: (f32, f32),
}

fn point_in_polygon(x: f32, y: f32, pts: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (xi, yi) = pts[i];
        let (xj, yj) = pts[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Signed distance to a polygon: negative inside, positive outside.
pub(crate) fn signed_distance_polygon(x: f32, y: f32, pts: &[(f32, f32)]) -> f32 {
    let mut best = f32::MAX;
    for i in 0..pts.len() {
        let (ax, ay) = pts[i];
        let (bx, by) = pts[(i + 1) % pts.len()];
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, py) = (ax + t * dx - x, ay + t * dy - y);
        best = best.min((px * px + py * py).sqrt());
    }
    if point_in_polygon(x, y, pts) { -best } else { best }
}

/// Premultiplied "over" compositing of `top` onto `base`.
pub(crate) fn over(base: [f32; 4], top: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - top[3];
    [
        top[0] + base[0] * k,
        top[1] + base[1] * k,
        top[2] + base[2] * k,
        top[3] + base[3] * k,
    ]
}

pub(crate) fn premul(c: [u8; 4], coverage: f32) -> [f32; 4] {
    let a = c[3] as f32 / 255.0 * coverage.clamp(0.0, 1.0);
    [
        c[0] as f32 / 255.0 * a,
        c[1] as f32 / 255.0 * a,
        c[2] as f32 / 255.0 * a,
        a,
    ]
}

pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Renders a sprite from a signed-distance description, one sample per pixel with
/// analytic anti-aliasing. `shade` gets the pixel center and returns premultiplied RGBA.
pub(crate) fn rasterize(width: usize, height: usize, shade: impl Fn(f32, f32) -> [f32; 4]) -> Vec<u8> {
    let mut out = vec![0u8; width * height * 4];
    for py in 0..height {
        for px in 0..width {
            let c = shade(px as f32 + 0.5, py as f32 + 0.5);
            let i = (py * width + px) * 4;
            if c[3] > 0.0 {
                for k in 0..3 {
                    out[i + k] = (c[k] / c[3] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                out[i + 3] = (c[3] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Cursor icon at `unit_px` pixels per cursor unit: crisp border, slightly rounded
/// corners and a soft drop shadow.
pub fn cursor_sprite(icon: CursorIcon, unit_px: f32) -> Sprite {
    let s = unit_px.max(0.2);
    let shape = icon.shape();
    let border = shape.border * s;
    let round = shape.round * s;
    // Soft shadow: a little below the cursor, blurred
    let shadow_dy = 0.9 * s;
    let shadow_blur = (1.8 * s).max(1.0);
    let shadow_rgba = [0, 0, 0, 115];

    // Bounds of the body in pixels, relative to the hotspot
    let (min_x, min_y, max_x, max_y) = if shape.circle > 0.0 {
        let r = shape.circle * s;
        (-r, -r, r, r)
    } else {
        shape.points.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(a, b, c, d), &(x, y)| (a.min(x * s), b.min(y * s), c.max(x * s), d.max(y * s)),
        )
    };
    let margin = round + border + shadow_blur + 2.0;
    let ox = margin - min_x;
    let oy = margin - min_y;
    let width = (max_x - min_x + margin * 2.0).ceil() as usize;
    let height = (max_y - min_y + margin * 2.0 + shadow_dy).ceil() as usize;

    let pts: Vec<(f32, f32)> = shape.points.iter().map(|&(x, y)| (ox + x * s, oy + y * s)).collect();
    let circle_r = shape.circle * s;
    // Distance to the body edge in pixels (negative inside)
    let sd = |x: f32, y: f32| -> f32 {
        if circle_r > 0.0 {
            ((x - ox).powi(2) + (y - oy).powi(2)).sqrt() - circle_r
        } else {
            signed_distance_polygon(x, y, &pts) - round
        }
    };

    let rgba = rasterize(width, height, |x, y| {
        let d = sd(x, y);
        let outer = d - border;
        let shadow_d = sd(x, y - shadow_dy) - border;
        let shadow = 1.0 - smoothstep(-shadow_blur, shadow_blur, shadow_d);
        let mut c = premul(shadow_rgba, shadow);
        c = over(c, premul(shape.border_color, 0.5 - outer));
        over(c, premul(shape.fill, 0.5 - d))
    });

    Sprite {
        width,
        height,
        rgba,
        hotspot: (ox, oy),
    }
}

/// Filled circle of `radius` pixels with a soft edge, centered on the hotspot.
pub fn highlight_sprite(rgba: [u8; 4], radius: f32) -> Sprite {
    let r = radius.max(1.0);
    let c = r + 2.0;
    let size = (c * 2.0).ceil() as usize;
    let data = rasterize(size, size, |x, y| {
        let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
        // 1.5px feathered rim
        premul(rgba, (r - d) / 1.5 + 0.5)
    });
    Sprite {
        width: size,
        height: size,
        rgba: data,
        hotspot: (c, c),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Background Export Job
// ─────────────────────────────────────────────────────────────────────────────

pub struct ExportRequest {
    pub video_path: PathBuf,
    pub kept_segments: Vec<(f64, f64)>,
    pub has_audio: bool,
    /// Cursor path in the time of the cut video (cuts already applied, and
    /// cleaned when Clean Move is on)
    pub telemetry: Option<TelemetrySession>,
    pub fps: u32,
    pub video_width: u32,
    pub video_height: u32,
    pub style: CursorStyle,
    /// Pointer arrows to draw (already filtered by whether arrows are enabled)
    pub arrows: Vec<crate::arrows::Arrow>,
    pub arrow_style: crate::arrows::ArrowStyle,
    /// Clicks to show as expanding rings, in cut-video time (empty when click
    /// effects are off)
    pub clicks: Vec<crate::clicks::Click>,
    pub click_style: crate::clicks::ClickStyle,
    pub out_video: PathBuf,
    pub out_telemetry: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ExportResult {
    pub output_video: PathBuf,
    pub output_telemetry: Option<PathBuf>,
    pub duration: f64,
    pub synced_records: u64,
}

pub enum ExportEvent {
    /// Fraction done, 0..=1
    Progress(f32),
    Finished(Result<ExportResult, String>),
}

pub struct ExportJob {
    events: Receiver<ExportEvent>,
    cancel: Arc<AtomicBool>,
}

impl ExportJob {
    pub fn start(request: ExportRequest) -> Self {
        let (tx, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancel.clone();
        thread::Builder::new()
            .name("dexhowto-export".to_string())
            .spawn(move || {
                let result = run_export(&request, &tx, &cancel_flag);
                let _ = tx.send(ExportEvent::Finished(result));
            })
            .expect("Failed to spawn export thread");
        Self { events, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drains pending events without blocking.
    pub fn poll(&self) -> Vec<ExportEvent> {
        self.events.try_iter().collect()
    }
}

/// Maps a source recording time to the output time after cuts, or `None` when cut out.
fn output_time(kept_segments: &[(f64, f64)], src_t: f64) -> Option<f64> {
    let mut acc = 0.0;
    for &(s, e) in kept_segments {
        if src_t >= s && src_t <= e {
            return Some(acc + (src_t - s));
        }
        acc += e - s;
    }
    None
}

/// Writes an ffmpeg `sendcmd` script that moves the cursor (`overlay@cur`) and the
/// highlight (`overlay@hl`) every output frame. Positions only change when needed.
fn write_cursor_commands(
    path: &Path,
    req: &ExportRequest,
    telemetry: &TelemetrySession,
    cursor: &Sprite,
    highlight: Option<&Sprite>,
    out_duration: f64,
) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("Failed to write cursor path: {e}"))?;
    let mut w = BufWriter::new(file);
    let fps = req.fps.max(1) as f64;
    let frames = (out_duration * fps).ceil() as u64 + 1;
    let hidden = -100_000i64;
    let mut last: Option<(i64, i64)> = None;

    for i in 0..frames {
        let out_t = i as f64 / fps;
        // The telemetry is already in cut-video time
        let pos = telemetry.sample_at(out_t).and_then(|s| {
            s.is_inside.then(|| {
                (
                    s.norm_x * req.video_width as f64,
                    s.norm_y * req.video_height as f64,
                )
            })
        });
        let key = pos.map_or((hidden, hidden), |(x, y)| (x.round() as i64, y.round() as i64));
        if last == Some(key) {
            continue;
        }
        last = Some(key);

        let place = |sprite: &Sprite| match pos {
            Some((x, y)) => (
                (x - sprite.hotspot.0 as f64).round() as i64,
                (y - sprite.hotspot.1 as f64).round() as i64,
            ),
            None => (hidden, hidden),
        };
        let (cx, cy) = place(cursor);
        let mut line = format!("{out_t:.4} overlay@cur x {cx}, overlay@cur y {cy}");
        if let Some(h) = highlight {
            let (hx, hy) = place(h);
            line.push_str(&format!(", overlay@hl x {hx}, overlay@hl y {hy}"));
        }
        writeln!(w, "{line};").map_err(|e| format!("Failed to write cursor path: {e}"))?;
    }
    w.flush().map_err(|e| format!("Failed to write cursor path: {e}"))
}

/// Writes an ffmpeg `sendcmd` script for the click rings. Every slot is a
/// `crop@ck{n}` that picks the animation frame out of the sprite sheet and an
/// `overlay@ck{n}` that places it. An idle slot shows the blank cell off screen.
fn write_click_commands(
    path: &Path,
    req: &ExportRequest,
    sheet: &crate::clicks::ClickSheet,
    slots: &[Option<usize>],
    slot_count: usize,
    out_duration: f64,
) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("Failed to write click effects: {e}"))?;
    let mut w = BufWriter::new(file);
    let frames = (out_duration * req.fps.max(1) as f64).ceil() as u64 + 1;
    let hidden = -100_000i64;
    let blank = sheet.cell_xy(crate::clicks::ClickKind::Left, None);
    let mut last: Vec<crate::clicks::SlotState> = vec![None; slot_count];

    for i in 0..frames {
        let states = crate::clicks::slot_states(
            &req.clicks,
            slots,
            slot_count,
            sheet,
            req.click_style.duration(),
            req.fps,
            (req.video_width, req.video_height),
            i,
        );
        let mut parts = Vec::new();
        for (n, (state, prev)) in states.iter().zip(&last).enumerate() {
            if i > 0 && state == prev {
                continue;
            }
            let ((cx, cy), (x, y)) = state.unwrap_or((blank, (hidden, hidden)));
            parts.push(format!(
                "crop@ck{n} x {cx}, crop@ck{n} y {cy}, overlay@ck{n} x {x}, overlay@ck{n} y {y}"
            ));
        }
        last = states;
        if !parts.is_empty() {
            let t = i as f64 / req.fps.max(1) as f64;
            writeln!(w, "{t:.4} {};", parts.join(", ")).map_err(|e| format!("Failed to write click effects: {e}"))?;
        }
    }
    w.flush().map_err(|e| format!("Failed to write click effects: {e}"))
}

/// Path of a temporary file as it must be written inside a filter graph: ':'
/// separates options, so a Windows drive letter ("C:/…") must be escaped.
fn filter_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "")
        .replace(':', "\\:")
}

/// A rendered arrow animation and where to place it in the video.
struct ArrowClip {
    path: PathBuf,
    x: i64,
    y: i64,
}

/// Frames per second of the arrow growing animation.
const ARROW_ANIM_FPS: f64 = 30.0;

/// Renders the arrow growing from the ⌘ press to the release into a transparent
/// clip covering just the area the arrow sweeps over.
fn render_arrow_growth(
    tmp: &Path,
    index: usize,
    arrow: &crate::arrows::Arrow,
    req: &ExportRequest,
    thickness: f32,
    aspect: f64,
) -> Result<ArrowClip, String> {
    let (vw, vh) = (req.video_width as f32, req.video_height as f32);
    let px = |p: (f64, f64)| (p.0 as f32 * vw, p.1 as f32 * vh);

    // Area swept by the arrow: the tail plus every tip position, with room for the border
    let margin = thickness * 2.5 + (thickness * 0.35).max(1.5) + thickness * 0.9 + 2.0;
    let mut points: Vec<(f32, f32)> = arrow.path.iter().map(|(_, p)| px(*p)).collect();
    points.push(px(arrow.from));
    points.push(px(arrow.to));
    let x0 = (points.iter().map(|p| p.0).fold(f32::MAX, f32::min) - margin).floor().max(0.0);
    let y0 = (points.iter().map(|p| p.1).fold(f32::MAX, f32::min) - margin).floor().max(0.0);
    let x1 = (points.iter().map(|p| p.0).fold(f32::MIN, f32::max) + margin).ceil().min(vw);
    let y1 = (points.iter().map(|p| p.1).fold(f32::MIN, f32::max) + margin).ceil().min(vh);
    let (width, height) = ((x1 - x0).max(2.0) as usize, (y1 - y0).max(2.0) as usize);
    let local = |p: (f32, f32)| (p.0 - x0, p.1 - y0);

    let duration = (arrow.released_t - arrow.pressed_t).max(1.0 / ARROW_ANIM_FPS);
    let frames = (duration * ARROW_ANIM_FPS).ceil() as usize;
    let path = tmp.join(format!("arrow{index}_grow.mov"));

    let mut encoder = crate::platform::command(crate::editor::find_ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{width}x{height}")])
        .args(["-framerate", &format!("{ARROW_ANIM_FPS}")])
        .args(["-i", "-", "-c:v", "png", "-pix_fmt", "rgba"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start the arrow renderer: {e}"))?;
    let mut stdin = encoder.stdin.take().ok_or("Arrow renderer has no input")?;

    // Draw frames on all cores, a batch at a time, and send them in order
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let from = local(px(arrow.from));
    let mut next = 0;
    while next < frames {
        let batch: Vec<usize> = (next..(next + threads).min(frames)).collect();
        next += batch.len();
        let rendered: Vec<Vec<u8>> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|&k| {
                    scope.spawn(move || {
                        let mut canvas = vec![0u8; width * height * 4];
                        let t = arrow.pressed_t + k as f64 / ARROW_ANIM_FPS;
                        let tip = arrow.tip_at(t);
                        if crate::arrows::long_enough(arrow.from, tip, aspect) {
                            crate::arrows::draw_arrow_into(
                                &mut canvas,
                                width,
                                height,
                                from,
                                local(px(tip)),
                                thickness,
                                req.arrow_style.rgba,
                            );
                        }
                        canvas
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        for frame in rendered {
            stdin
                .write_all(&frame)
                .map_err(|e| format!("Failed to render the arrow animation: {e}"))?;
        }
    }
    drop(stdin);
    let out = encoder
        .wait_with_output()
        .map_err(|e| format!("Arrow renderer failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Arrow renderer failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(ArrowClip {
        path,
        x: x0 as i64,
        y: y0 as i64,
    })
}

fn write_sprite(path: &Path, sprite: &Sprite) -> Result<(), String> {
    std::fs::write(path, &sprite.rgba).map_err(|e| format!("Failed to write cursor image: {e}"))
}

fn run_export(
    req: &ExportRequest,
    tx: &Sender<ExportEvent>,
    cancel: &AtomicBool,
) -> Result<ExportResult, String> {
    if req.kept_segments.is_empty() {
        return Err("No scenes kept to export".to_string());
    }
    let out_duration: f64 = req.kept_segments.iter().map(|&(s, e)| (e - s).max(0.0)).sum();

    let tmp = std::env::temp_dir().join(format!(
        "dexhowto_export_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("Failed to create temp folder: {e}"))?;
    let result = encode(req, tx, cancel, &tmp, out_duration);
    let _ = std::fs::remove_dir_all(&tmp);
    result?;

    // Re-time the (possibly cleaned) telemetry to match the cut video
    let (output_telemetry, synced_records) = match &req.telemetry {
        Some(tel) => {
            let name = req
                .out_video
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("video.mp4");
            // Already in cut-video time: keep every record as it is
            let count = tel
                .sync_with_cut_ranges(&[(0.0, out_duration)], name, &req.out_telemetry, req.fps)
                .unwrap_or(0);
            (Some(req.out_telemetry.clone()), count)
        }
        None => (None, 0),
    };

    Ok(ExportResult {
        output_video: req.out_video.clone(),
        output_telemetry,
        duration: out_duration,
        synced_records,
    })
}

fn encode(
    req: &ExportRequest,
    tx: &Sender<ExportEvent>,
    cancel: &AtomicBool,
    tmp: &Path,
    out_duration: f64,
) -> Result<(), String> {
    let mut cmd = crate::platform::command(crate::editor::find_ffmpeg_bin());
    cmd.arg("-y").arg("-i").arg(&req.video_path);

    // 1. Cut: trim every kept segment and join them.
    // Screen recordings only get a new frame when the screen changes, so a still
    // screen can go a third of a second without one. Repeating frames to a constant
    // rate first makes cuts land exactly and lets overlays move on every frame.
    let fps = req.fps.max(1);
    let count = req.kept_segments.len();
    let mut filter = String::new();
    let mut concat_inputs = String::new();
    for (i, &(start, end)) in req.kept_segments.iter().enumerate() {
        filter.push_str(&format!(
            "[0:v]fps={fps},trim=start={start:.3}:end={end:.3},setpts=PTS-STARTPTS[v{i}];"
        ));
        concat_inputs.push_str(&format!("[v{i}]"));
        if req.has_audio {
            filter.push_str(&format!(
                "[0:a]atrim=start={start:.3}:end={end:.3},asetpts=PTS-STARTPTS[a{i}];"
            ));
            concat_inputs.push_str(&format!("[a{i}]"));
        }
    }
    let audio_flag = if req.has_audio { 1 } else { 0 };
    let audio_out = if req.has_audio { "[a]" } else { "" };
    filter.push_str(&format!(
        "{concat_inputs}concat=n={count}:v=1:a={audio_flag}[vcut]{audio_out}"
    ));

    // 2. Overlays: arrows, then the cursor highlight and icon on top
    let burn = req
        .telemetry
        .as_ref()
        .filter(|_| req.style.burn_in);
    let frame_h = req.video_height as f32;

    // Arrows that are not cut out, with their times in the output video. The growing
    // part is only animated when the press and release are in the same kept part.
    struct PlacedArrow<'a> {
        arrow: &'a crate::arrows::Arrow,
        out_press: Option<f64>,
        out_release: f64,
    }
    let arrows: Vec<PlacedArrow> = req
        .arrows
        .iter()
        .filter_map(|a| {
            let out_release = output_time(&req.kept_segments, a.released_t)?;
            let out_press = output_time(&req.kept_segments, a.pressed_t).filter(|p| {
                ((out_release - p) - (a.released_t - a.pressed_t)).abs() < 0.05
            });
            Some(PlacedArrow { arrow: a, out_press, out_release })
        })
        .collect();

    let mut video_out = "[vcut]".to_string();
    if burn.is_some() || !arrows.is_empty() || !req.clicks.is_empty() {
        let next_input = std::cell::Cell::new(1usize);
        let take_input = || {
            let idx = next_input.get();
            next_input.set(idx + 1);
            idx
        };
        let add_sprite = |cmd: &mut Command, name: &str, sprite: &Sprite, looped_secs: Option<f64>| -> Result<usize, String> {
            let p = tmp.join(format!("{name}.rgba"));
            write_sprite(&p, sprite)?;
            if let Some(secs) = looped_secs {
                // Repeat the single image as a short clip, so it can fade in and out
                cmd.args(["-stream_loop", "-1", "-framerate", &fps.to_string(), "-t", &format!("{secs:.3}")]);
            }
            cmd.arg("-f")
                .arg("rawvideo")
                .arg("-pix_fmt")
                .arg("rgba")
                .arg("-video_size")
                .arg(format!("{}x{}", sprite.width, sprite.height))
                .arg("-i")
                .arg(&p);
            Ok(take_input())
        };

        let mut base = "[vcut]".to_string();

        let style = &req.arrow_style;
        let hold = style.hold_secs.max(0.5) as f64;
        let thickness = style.thickness_px(frame_h);
        let aspect = req.video_width as f64 / req.video_height.max(1) as f64;
        let px = |p: (f64, f64)| {
            (
                (p.0 * req.video_width as f64) as f32,
                (p.1 * req.video_height as f64) as f32,
            )
        };
        for (i, placed) in arrows.iter().enumerate() {
            let arrow = placed.arrow;
            let release = placed.out_release;

            // While ⌘ is held: the arrow grows, its tip following the mouse
            if let Some(press) = placed.out_press {
                let clip = render_arrow_growth(tmp, i, arrow, req, thickness, aspect)?;
                cmd.arg("-i").arg(&clip.path);
                let idx = take_input();
                filter.push_str(&format!(
                    ";[{idx}:v]format=rgba,setpts=PTS-STARTPTS+{press:.3}/TB[ag{i}];\
                     {base}[ag{i}]overlay=x={x}:y={y}:eof_action=pass:enable='between(t,{press:.3},{release:.3})'[vg{i}]",
                    x = clip.x,
                    y = clip.y,
                ));
                base = format!("[vg{i}]");
            }

            // After release: the same arrow stays, then fades out
            let (from, to) = (px(arrow.from), px(arrow.to));
            let sprite = crate::arrows::arrow_sprite(from, to, thickness, style.rgba);
            let x = (from.0 - sprite.hotspot.0).round() as i64;
            let y = (from.1 - sprite.hotspot.1).round() as i64;
            let idx = add_sprite(&mut cmd, &format!("arrow{i}"), &sprite, Some(hold))?;
            let end = release + hold;
            filter.push_str(&format!(
                ";[{idx}:v]format=rgba,fade=t=out:st={fade_out:.3}:d=0.4:alpha=1,\
                 setpts=PTS-STARTPTS+{release:.3}/TB[ar{i}];\
                 {base}[ar{i}]overlay=x={x}:y={y}:eof_action=pass:enable='between(t,{release:.3},{end:.3})'[va{i}]",
                fade_out = (hold - 0.4).max(0.0),
            ));
            base = format!("[va{i}]");
        }

        // Click rings, under the cursor: one sprite sheet with every animation
        // frame, looped, and a few crop + overlay slots moved by sendcmd
        if !req.clicks.is_empty() {
            let sheet = crate::clicks::ClickSheet::render(&req.click_style, frame_h, fps);
            let duration = req.click_style.duration();
            // Leave a slot empty for two frames before reusing it, so a new ring
            // never shows for a frame with the previous one's position
            let gap = 2.0 / fps as f64;
            let (slots, slot_count) =
                crate::clicks::assign_slots(&req.clicks, duration, gap, crate::clicks::MAX_SLOTS);
            let cmd_path = tmp.join("clicks.cmd");
            write_click_commands(&cmd_path, req, &sheet, &slots, slot_count, out_duration)?;
            let idx = add_sprite(&mut cmd, "clicks", &sheet.sprite, None)?;
            let (bx, by) = sheet.cell_xy(crate::clicks::ClickKind::Left, None);
            let outputs: String = (0..slot_count).map(|n| format!("[cks{n}]")).collect();
            filter.push_str(&format!(
                ";[{idx}:v]loop=loop=-1:size=1:start=0,setpts=N/({fps}*TB),\
                 trim=duration={len:.3},format=rgba,sendcmd=f='{cmd}',split={slot_count}{outputs}",
                len = out_duration + 1.0,
                cmd = filter_path(&cmd_path),
            ));
            for n in 0..slot_count {
                filter.push_str(&format!(
                    ";[cks{n}]crop@ck{n}=w={cell}:h={cell}:x={bx}:y={by}[ckc{n}];\
                     {base}[ckc{n}]overlay@ck{n}=x=-100000:y=-100000:eof_action=pass[vck{n}]",
                    cell = sheet.cell,
                ));
                base = format!("[vck{n}]");
            }
        }

        if let Some(tel) = burn {
            let cursor = cursor_sprite(req.style.icon, req.style.unit_px(frame_h));
            let highlight = req.style.highlight.then(|| {
                highlight_sprite(req.style.highlight_rgba, req.style.highlight_px(frame_h))
            });
            let cmd_path = tmp.join("cursor.cmd");
            write_cursor_commands(&cmd_path, req, tel, &cursor, highlight.as_ref(), out_duration)?;
            let cmd_arg = filter_path(&cmd_path);

            filter.push_str(&format!(";{base}sendcmd=f='{cmd_arg}'[vcmd]"));
            base = "[vcmd]".to_string();
            if let Some(h) = &highlight {
                let idx = add_sprite(&mut cmd, "highlight", h, None)?;
                filter.push_str(&format!(
                    ";{base}[{idx}:v]overlay@hl=x=-100000:y=-100000:eof_action=repeat[vhl]"
                ));
                base = "[vhl]".to_string();
            }
            let idx = add_sprite(&mut cmd, "cursor", &cursor, None)?;
            filter.push_str(&format!(
                ";{base}[{idx}:v]overlay@cur=x=-100000:y=-100000:eof_action=repeat[vcur]"
            ));
            base = "[vcur]".to_string();
        }

        filter.push_str(&format!(";{base}format=yuv420p[vout]"));
        video_out = "[vout]".to_string();
    }

    cmd.arg("-filter_complex").arg(&filter).arg("-map").arg(&video_out);
    if req.has_audio {
        cmd.arg("-map").arg("[a]").arg("-c:a").arg("aac");
    }
    cmd.args(crate::platform::h264_encoder_args())
        .arg("-progress")
        .arg("pipe:1")
        .arg("-nostats")
        .arg(&req.out_video)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run ffmpeg: {e}"))?;

    // Collect stderr on its own thread so a full pipe never blocks ffmpeg
    let mut stderr = child.stderr.take();
    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();
        if let Some(ref mut err) = stderr {
            let _ = err.read_to_string(&mut text);
        }
        text
    });

    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            if let Some(us) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = us.trim().parse::<f64>() {
                    let frac = (us / 1_000_000.0 / out_duration.max(0.001)).clamp(0.0, 1.0);
                    let _ = tx.send(ExportEvent::Progress(frac as f32));
                }
            }
        }
    }

    let status = child.wait().map_err(|e| format!("ffmpeg failed: {e}"))?;
    let err_text = stderr_reader.join().unwrap_or_default();

    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&req.out_video);
        return Err("Export cancelled".to_string());
    }
    if !status.success() {
        crate::platform::trace(&format!("Export failed: ffmpeg said: {}", err_text.trim()));
        return Err(format!("ffmpeg error: {}", err_text.trim()));
    }
    let _ = tx.send(ExportEvent::Progress(1.0));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_sprite_tip_is_on_hotspot() {
        let sprite = cursor_sprite(CursorIcon::Mac, 2.0);
        assert_eq!(sprite.rgba.len(), sprite.width * sprite.height * 4);
        // The pixel just inside the tip is opaque, the corner is transparent
        let (hx, hy) = (sprite.hotspot.0 as usize, sprite.hotspot.1 as usize);
        let alpha = |x: usize, y: usize| sprite.rgba[(y * sprite.width + x) * 4 + 3];
        assert!(alpha(hx + 1, hy + 3) > 200);
        assert_eq!(alpha(sprite.width - 1, 0), 0);
    }

    #[test]
    fn test_highlight_sprite_is_round() {
        let sprite = highlight_sprite([255, 0, 0, 128], 20.0);
        let alpha = |x: usize, y: usize| sprite.rgba[(y * sprite.width + x) * 4 + 3];
        let c = sprite.hotspot.0 as usize;
        assert!(alpha(c, c) > 100);
        assert_eq!(alpha(0, 0), 0);
    }

    /// Runs a real export with ffmpeg and checks the ring is in the right frames
    /// at the right place. Needs ffmpeg: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn test_export_burns_click_rings_into_the_video() {
        use crate::clicks::{Click, ClickKind, ClickStyle};
        let dir = std::env::temp_dir().join(format!("dexhowto_click_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let ffmpeg = crate::editor::find_ffmpeg_bin();
        let ok = crate::platform::command(&ffmpeg)
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=1280x720:r=30:d=3"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(ok.success());

        let out = dir.join("out.mp4");
        let style = ClickStyle::default();
        let req = ExportRequest {
            video_path: src,
            // Cut out 0.5..1.0: a click recorded at 1.5 s plays at 1.0 s
            kept_segments: vec![(0.0, 0.5), (1.0, 3.0)],
            has_audio: false,
            telemetry: None,
            fps: 30,
            video_width: 1280,
            video_height: 720,
            style: CursorStyle::default(),
            arrows: Vec::new(),
            arrow_style: crate::arrows::ArrowStyle::default(),
            clicks: vec![
                Click { t: 1.0, norm: (0.25, 0.5), kind: ClickKind::Left },
                Click { t: 1.1, norm: (0.75, 0.5), kind: ClickKind::Right },
            ],
            click_style: style.clone(),
            out_video: out.clone(),
            out_telemetry: dir.join("out.mouse.jsonl"),
        };
        let (tx, _rx) = channel();
        run_export(&req, &tx, &AtomicBool::new(false)).unwrap();

        // Brightest pixel (RGB) of the 16 px wide strip starting at (x, y) in the
        // frame at time t. A strip, because the thin ring is softened by encoding.
        let brightest = |t: f64, x: u32, y: u32| -> [u8; 3] {
            let o = crate::platform::command(&ffmpeg)
                .args(["-loglevel", "error", "-ss", &format!("{t:.3}"), "-i"])
                .arg(&out)
                .args(["-frames:v", "1", "-vf", &format!("format=rgb24,crop=16:1:{x}:{y}")])
                .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
                .output()
                .unwrap();
            assert_eq!(o.stdout.len(), 48, "{}", String::from_utf8_lossy(&o.stderr));
            let px = o.stdout.chunks(3).max_by_key(|p| p.iter().map(|&c| c as u32).sum::<u32>()).unwrap();
            [px[0], px[1], px[2]]
        };
        // 0.2 s after each click its ring is about this far from the click
        let r = style.radius_px(720.0);
        let p = 0.2 / style.duration() as f32;
        let ring_r = r * (0.2 + 0.8 * (1.0 - (1.0 - p).powi(3)));
        let strip_x = |cx: f32| (cx + ring_r - 8.0) as u32;
        let left = brightest(1.2, strip_x(320.0), 360);
        let right = brightest(1.3, strip_x(960.0), 360);
        assert!(left[0] > 100 && left[0] > left[2] + 40, "left ring should be yellow, got {left:?}");
        assert!(right[2] > 100 && right[2] > right[0] + 40, "right ring should be blue, got {right:?}");
        // Nothing before the click or after the ring has faded
        assert!(brightest(0.8, strip_x(320.0), 360).iter().all(|&c| c < 30));
        assert!(brightest(2.2, strip_x(320.0), 360).iter().all(|&c| c < 30));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
