//! Click effects: a ring that expands and fades where the mouse was clicked.
//!
//! Recording never draws clicks; the telemetry only stores them. The editor shows
//! them as timeline markers and in the preview, and the export burns the same
//! rings into the video. The ring geometry here is shared by the preview (egui)
//! and the export (CPU-rendered sprite sheet), so both look the same.

use serde::{Deserialize, Serialize};

use crate::editor::TelemetrySession;
use crate::export::{over, premul, rasterize, Sprite};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickKind {
    Left,
    Double,
    Right,
    Middle,
}

impl ClickKind {
    /// The kind of a synthesized click record (`left_click`, `right_click`, …).
    pub fn from_event(event: &str) -> Option<Self> {
        match event {
            "left_click" => Some(ClickKind::Left),
            "double_click" => Some(ClickKind::Double),
            "right_click" => Some(ClickKind::Right),
            "middle_click" => Some(ClickKind::Middle),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ClickKind::Left => "Click",
            ClickKind::Double => "Double click",
            ClickKind::Right => "Right click",
            ClickKind::Middle => "Middle click",
        }
    }

    /// Column of this kind in the export sprite sheet. A middle click looks like
    /// a left click.
    fn sheet_column(self) -> usize {
        match self {
            ClickKind::Left | ClickKind::Middle => 0,
            ClickKind::Double => 1,
            ClickKind::Right => 2,
        }
    }

    fn uses_right_color(self) -> bool {
        self == ClickKind::Right
    }
}

/// Sheet columns, one per look (see `ClickKind::sheet_column`).
const SHEET_KINDS: [ClickKind; 3] = [ClickKind::Left, ClickKind::Double, ClickKind::Right];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClickStyle {
    /// Show click rings in the preview and burn them into the export
    pub enabled: bool,
    /// Ring color of left, double and middle clicks (unmultiplied sRGBA)
    pub left_rgba: [u8; 4],
    /// Ring color of right clicks (unmultiplied sRGBA)
    pub right_rgba: [u8; 4],
    /// Largest ring radius in pixels at 1080p
    pub radius: f32,
    /// How long a ring takes to expand and fade out
    pub duration_secs: f32,
}

impl Default for ClickStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            left_rgba: [255, 196, 40, 255],
            right_rgba: [88, 136, 255, 255],
            radius: 34.0,
            duration_secs: 0.5,
        }
    }
}

pub const RADIUS_RANGE: std::ops::RangeInclusive<f32> = 12.0..=90.0;
pub const DURATION_RANGE: std::ops::RangeInclusive<f32> = 0.2..=1.2;

impl ClickStyle {
    /// Largest ring radius for a frame `frame_height` pixels tall.
    pub fn radius_px(&self, frame_height: f32) -> f32 {
        self.radius.clamp(*RADIUS_RANGE.start(), *RADIUS_RANGE.end()) * frame_height / 1080.0
    }

    pub fn duration(&self) -> f64 {
        self.duration_secs.clamp(*DURATION_RANGE.start(), *DURATION_RANGE.end()) as f64
    }

    pub fn color(&self, kind: ClickKind) -> [u8; 4] {
        if kind.uses_right_color() { self.right_rgba } else { self.left_rgba }
    }
}

/// One click, in the time of the cut video.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Click {
    pub t: f64,
    /// Position as a fraction of the video size
    pub norm: (f64, f64),
    pub kind: ClickKind,
}

/// The clicks in `track`, which is already in cut-video time. Clicks outside the
/// recorded screen are left out, like the cursor.
pub fn clicks_in(track: &TelemetrySession) -> Vec<Click> {
    let mut clicks: Vec<Click> = track
        .samples
        .iter()
        .filter(|s| s.is_inside)
        .filter_map(|s| {
            Some(Click {
                t: s.t,
                norm: (s.norm_x, s.norm_y),
                kind: ClickKind::from_event(&s.event)?,
            })
        })
        .collect();
    clicks.sort_by(|a, b| a.t.total_cmp(&b.t));
    clicks
}

/// The clicks whose ring shows at time `t`: their index in `clicks` and how long
/// ago each happened. `clicks` must be sorted by time.
pub fn active_at(clicks: &[Click], t: f64, duration: f64) -> impl Iterator<Item = (usize, f64)> + '_ {
    let first = clicks.partition_point(|c| c.t <= t - duration);
    clicks[first..]
        .iter()
        .enumerate()
        .take_while(move |(_, c)| c.t <= t)
        .map(move |(i, c)| (first + i, t - c.t))
}

// ─────────────────────────────────────────────────────────────────────────────
// Ring Geometry (shared by preview and export)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub radius: f32,
    pub width: f32,
    /// 0..=1
    pub alpha: f32,
}

/// What a click looks like at one moment, in pixels around the click position.
#[derive(Clone, Debug, PartialEq)]
pub struct Ripple {
    pub rings: Vec<Ring>,
    /// Soft disc inside the outer ring
    pub fill_radius: f32,
    pub fill_alpha: f32,
    /// White dot on the click position
    pub dot_radius: f32,
    pub dot_alpha: f32,
}

/// The ripple of a `kind` click `progress` (0..1) through its animation, with the
/// ring growing up to `max_radius` pixels.
pub fn ripple(kind: ClickKind, progress: f32, max_radius: f32) -> Ripple {
    let p = progress.clamp(0.0, 1.0);
    let grow = 1.0 - (1.0 - p).powi(3);
    let radius = max_radius * (0.2 + 0.8 * grow);
    let width = (max_radius * 0.12 * (1.0 - 0.4 * p)).max(1.0);
    // Stays bright while it grows, fades out towards the end
    let alpha = 1.0 - p * p;
    let mut rings = vec![Ring { radius, width, alpha }];
    if kind == ClickKind::Double {
        rings.push(Ring {
            radius: radius * 0.6,
            width,
            alpha,
        });
    }
    Ripple {
        rings,
        fill_radius: radius,
        fill_alpha: alpha * 0.28,
        dot_radius: (max_radius * 0.12).max(1.5),
        dot_alpha: (1.0 - p / 0.5).max(0.0),
    }
}

/// Distance from the click position to the outside of the widest ring, over the
/// whole animation.
fn ripple_extent(max_radius: f32) -> f32 {
    max_radius + (max_radius * 0.12).max(1.0) / 2.0
}

// ─────────────────────────────────────────────────────────────────────────────
// Export: sprite sheet + slot schedule
// ─────────────────────────────────────────────────────────────────────────────

/// Every animation frame of every look in one image, so the export needs a single
/// input however many clicks there are. Frame `k` of a look is the ring `k / fps`
/// seconds after the click. Cells are laid out in a grid per look; one extra
/// empty cell is used while a slot shows nothing.
pub struct ClickSheet {
    pub sprite: Sprite,
    /// Width and height of one cell
    pub cell: usize,
    /// Animation frames per look
    pub frames: usize,
    /// Cells per row and per column of one look's grid
    grid: usize,
}

impl ClickSheet {
    pub fn render(style: &ClickStyle, frame_height: f32, fps: u32) -> Self {
        let max_r = style.radius_px(frame_height);
        let fps = fps.max(1) as f64;
        let frames = (style.duration() * fps).ceil().max(1.0) as usize;
        let cell = ((ripple_extent(max_r) + 2.0) * 2.0).ceil() as usize;
        let center = cell as f32 / 2.0;
        // One extra cell for the blank frame
        let grid = ((frames + 1) as f64).sqrt().ceil() as usize;
        let width = cell * grid * SHEET_KINDS.len();
        let height = cell * grid;
        let mut rgba = vec![0u8; width * height * 4];

        for (col, &kind) in SHEET_KINDS.iter().enumerate() {
            let color = style.color(kind);
            for k in 0..frames {
                let progress = (k as f64 / fps / style.duration()) as f32;
                let look = ripple(kind, progress, max_r);
                let pixels = rasterize(cell, cell, |x, y| shade(&look, color, x - center, y - center));
                let (ox, oy) = cell_origin(cell, grid, col, k);
                for row in 0..cell {
                    let src = row * cell * 4;
                    let dst = ((oy + row) * width + ox) * 4;
                    rgba[dst..dst + cell * 4].copy_from_slice(&pixels[src..src + cell * 4]);
                }
            }
        }

        Self {
            sprite: Sprite {
                width,
                height,
                rgba,
                hotspot: (center, center),
            },
            cell,
            frames,
            grid,
        }
    }

    /// Top-left corner of the cell showing `frame` of `kind`, or of the blank
    /// cell when `frame` is `None`.
    pub fn cell_xy(&self, kind: ClickKind, frame: Option<usize>) -> (usize, usize) {
        match frame {
            Some(k) => cell_origin(self.cell, self.grid, kind.sheet_column(), k.min(self.frames - 1)),
            None => cell_origin(self.cell, self.grid, 0, self.frames),
        }
    }
}

fn cell_origin(cell: usize, grid: usize, column: usize, k: usize) -> (usize, usize) {
    ((column * grid + k % grid) * cell, (k / grid) * cell)
}

/// Premultiplied color of one pixel at (`dx`, `dy`) from the click position.
fn shade(look: &Ripple, color: [u8; 4], dx: f32, dy: f32) -> [f32; 4] {
    let d = (dx * dx + dy * dy).sqrt();
    // Coverage of a disc edge, anti-aliased over one pixel
    let disc = |radius: f32| (radius - d + 0.5).clamp(0.0, 1.0);
    let mut c = premul(color, look.fill_alpha * disc(look.fill_radius));
    for ring in &look.rings {
        let coverage = (ring.width / 2.0 - (d - ring.radius).abs() + 0.5).clamp(0.0, 1.0);
        c = over(c, premul(color, coverage * ring.alpha));
    }
    over(c, premul([255, 255, 255, 255], look.dot_alpha * disc(look.dot_radius)))
}

/// Animation frame (cell index) of a click `age` seconds old.
pub fn frame_index(age: f64, fps: u32, frames: usize) -> usize {
    ((age * fps.max(1) as f64 + 1e-6).floor().max(0.0) as usize).min(frames.saturating_sub(1))
}

/// Pixel position of the top-left corner of a sprite whose `hotspot` sits on the
/// normalized position `norm` in a `width` x `height` video.
pub fn sprite_origin(norm: (f64, f64), width: u32, height: u32, hotspot: (f32, f32)) -> (i64, i64) {
    (
        (norm.0 * width as f64 - hotspot.0 as f64).round() as i64,
        (norm.1 * height as f64 - hotspot.1 as f64).round() as i64,
    )
}

/// Most rings the export draws at the same time; more overlapping clicks are rare.
pub const MAX_SLOTS: usize = 6;

/// Gives every click one of `max_slots` drawing slots, so rings that overlap in
/// time are drawn by different overlays. A slot is reused only `gap` seconds after
/// its last ring ended. Returns the slot of each click (`None` when all slots are
/// busy) and how many slots are used.
pub fn assign_slots(clicks: &[Click], duration: f64, gap: f64, max_slots: usize) -> (Vec<Option<usize>>, usize) {
    let mut free_at: Vec<f64> = Vec::new();
    let slots = clicks
        .iter()
        .map(|c| {
            if let Some(slot) = free_at.iter().position(|&f| f <= c.t) {
                free_at[slot] = c.t + duration + gap;
                Some(slot)
            } else if free_at.len() < max_slots {
                free_at.push(c.t + duration + gap);
                Some(free_at.len() - 1)
            } else {
                None
            }
        })
        .collect();
    (slots, free_at.len())
}

/// What one slot shows on one output frame: the click's sheet cell and where to
/// put it, or nothing.
pub type SlotState = Option<((usize, usize), (i64, i64))>;

/// The state of every slot on output frame `i` (time `i / fps`).
pub fn slot_states(
    clicks: &[Click],
    slots: &[Option<usize>],
    slot_count: usize,
    sheet: &ClickSheet,
    duration: f64,
    fps: u32,
    video: (u32, u32),
    i: u64,
) -> Vec<SlotState> {
    let t = i as f64 / fps.max(1) as f64;
    let mut states = vec![None; slot_count];
    for (idx, age) in active_at(clicks, t, duration) {
        let c = &clicks[idx];
        if let Some(slot) = slots[idx] {
            let k = frame_index(age, fps, sheet.frames);
            let pos = sprite_origin(c.norm, video.0, video.1, sheet.sprite.hotspot);
            states[slot] = Some((sheet.cell_xy(c.kind, Some(k)), pos));
        }
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(t: f64, kind: ClickKind) -> Click {
        Click { t, norm: (0.5, 0.25), kind }
    }

    #[test]
    fn test_click_kinds_from_events() {
        assert_eq!(ClickKind::from_event("left_click"), Some(ClickKind::Left));
        assert_eq!(ClickKind::from_event("double_click"), Some(ClickKind::Double));
        assert_eq!(ClickKind::from_event("right_click"), Some(ClickKind::Right));
        assert_eq!(ClickKind::from_event("middle_click"), Some(ClickKind::Middle));
        assert_eq!(ClickKind::from_event("left_down"), None);
        assert_eq!(ClickKind::from_event("move"), None);
    }

    #[test]
    fn test_active_at_shows_each_click_for_its_duration() {
        let clicks = [click(1.0, ClickKind::Left), click(1.2, ClickKind::Right), click(3.0, ClickKind::Left)];
        let ages = |t: f64| active_at(&clicks, t, 0.5).map(|(i, age)| (clicks[i].t, age)).collect::<Vec<_>>();
        assert!(ages(0.99).is_empty());
        assert_eq!(ages(1.0), vec![(1.0, 0.0)]);
        let both = ages(1.3);
        assert_eq!(both.len(), 2);
        assert!((both[0].1 - 0.3).abs() < 1e-9 && (both[1].1 - 0.1).abs() < 1e-9);
        // Gone once the duration has passed
        assert_eq!(ages(1.5).len(), 1);
        assert!(ages(2.0).is_empty());
        assert_eq!(ages(3.2).len(), 1);
    }

    #[test]
    fn test_ripple_grows_and_fades() {
        let start = ripple(ClickKind::Left, 0.0, 40.0);
        let end = ripple(ClickKind::Left, 1.0, 40.0);
        assert!(start.rings[0].radius < end.rings[0].radius);
        assert!(end.rings[0].radius <= 40.0);
        assert!(start.rings[0].alpha > 0.99 && end.rings[0].alpha < 0.02);
        assert_eq!(ripple(ClickKind::Double, 0.5, 40.0).rings.len(), 2);
        assert_eq!(ripple(ClickKind::Right, 0.5, 40.0).rings.len(), 1);
    }

    #[test]
    fn test_frame_index_follows_age_and_stays_in_range() {
        assert_eq!(frame_index(0.0, 60, 30), 0);
        assert_eq!(frame_index(1.0 / 60.0, 60, 30), 1);
        assert_eq!(frame_index(0.25, 60, 30), 15);
        assert_eq!(frame_index(5.0, 60, 30), 29);
    }

    #[test]
    fn test_sprite_origin_puts_hotspot_on_click() {
        assert_eq!(sprite_origin((0.5, 0.25), 1920, 1080, (40.0, 40.0)), (920, 230));
        assert_eq!(sprite_origin((0.0, 0.0), 1920, 1080, (40.0, 40.0)), (-40, -40));
    }

    #[test]
    fn test_assign_slots_reuses_free_slots_and_caps_overlap() {
        let clicks = [
            click(0.0, ClickKind::Left),
            click(0.1, ClickKind::Left),
            click(0.2, ClickKind::Right),
            click(1.0, ClickKind::Left),
        ];
        let (slots, count) = assign_slots(&clicks, 0.5, 0.05, 6);
        assert_eq!(slots, vec![Some(0), Some(1), Some(2), Some(0)]);
        assert_eq!(count, 3);
        // With only two slots the third overlapping click is skipped
        let (slots, count) = assign_slots(&clicks, 0.5, 0.05, 2);
        assert_eq!(slots, vec![Some(0), Some(1), None, Some(0)]);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_sheet_cells_have_the_ring_and_blank_cell_is_empty() {
        let style = ClickStyle::default();
        let sheet = ClickSheet::render(&style, 1080.0, 30);
        assert_eq!(sheet.frames, 15);
        assert_eq!(sheet.sprite.rgba.len(), sheet.sprite.width * sheet.sprite.height * 4);
        let alpha_sum = |(x0, y0): (usize, usize)| -> u64 {
            let mut sum = 0;
            for y in y0..y0 + sheet.cell {
                for x in x0..x0 + sheet.cell {
                    sum += sheet.sprite.rgba[(y * sheet.sprite.width + x) * 4 + 3] as u64;
                }
            }
            sum
        };
        assert!(alpha_sum(sheet.cell_xy(ClickKind::Left, Some(0))) > 0);
        assert!(alpha_sum(sheet.cell_xy(ClickKind::Right, Some(5))) > 0);
        assert_eq!(alpha_sum(sheet.cell_xy(ClickKind::Left, None)), 0);
        // Right clicks use the right color
        let (x, y) = sheet.cell_xy(ClickKind::Right, Some(0));
        let c = sheet.cell / 2;
        let ring_x = x + c + (style.radius_px(1080.0) * 0.2) as usize;
        let px = &sheet.sprite.rgba[((y + c) * sheet.sprite.width + ring_x) * 4..][..4];
        assert!(px[2] > px[0], "right ring should be blue, got {px:?}");
    }

    #[test]
    fn test_slot_states_place_the_right_cell_at_the_right_time() {
        let sheet = ClickSheet::render(&ClickStyle::default(), 1080.0, 30);
        let clicks = [click(1.0, ClickKind::Right)];
        let (slots, count) = assign_slots(&clicks, 0.5, 0.1, MAX_SLOTS);
        let at = |i| slot_states(&clicks, &slots, count, &sheet, 0.5, 30, (1920, 1080), i);
        assert_eq!(at(29), vec![None]);
        let (cell, pos) = at(30)[0].unwrap();
        assert_eq!(cell, sheet.cell_xy(ClickKind::Right, Some(0)));
        assert_eq!(pos, sprite_origin((0.5, 0.25), 1920, 1080, sheet.sprite.hotspot));
        assert_eq!(at(33)[0].unwrap().0, sheet.cell_xy(ClickKind::Right, Some(3)));
        // 0.5 s at 30 fps = 15 frames, then the ring is gone
        assert!(at(44)[0].is_some());
        assert_eq!(at(45), vec![None]);
    }
}
