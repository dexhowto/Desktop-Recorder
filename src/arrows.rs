//! Pointer arrows drawn with the Command key.
//!
//! While recording, pressing ⌘ starts an arrow where the mouse is; while ⌘ is held
//! the arrow's tip follows the mouse, and when ⌘ is released the arrow stays for
//! the chosen time and then fades out. The recorder already logs the held
//! modifier keys with every mouse record, so arrows are found in the telemetry.

use crate::editor::{TelemetrySample, TelemetrySession};
use crate::export::{over, premul, signed_distance_polygon, smoothstep, Sprite};

/// Shortest arrow, as a fraction of the screen height. Shorter ⌘ presses are
/// keyboard shortcuts (⌘C, ⌘Tab…), not arrows.
const MIN_LENGTH: f64 = 0.03;

/// Holding ⌘ longer than this is not treated as an arrow gesture.
const MAX_HOLD_SECS: f64 = 20.0;

/// How long an arrow takes to fade out at the end of its time on screen.
const FADE_OUT_SECS: f64 = 0.4;

#[derive(Clone, Debug, PartialEq)]
pub struct Arrow {
    /// When ⌘ was pressed (seconds into the recording): the arrow starts growing
    pub pressed_t: f64,
    /// When ⌘ was released: the arrow stops following the mouse
    pub released_t: f64,
    /// Tail, in normalized screen coordinates (0..1)
    pub from: (f64, f64),
    /// Final tip, in normalized screen coordinates (0..1)
    pub to: (f64, f64),
    /// Tip position while ⌘ was held: (seconds, normalized point), in time order
    pub path: Vec<(f64, (f64, f64))>,
}

impl Arrow {
    /// Where the tip is at time `t` (follows the recorded mouse while ⌘ is held).
    pub fn tip_at(&self, t: f64) -> (f64, f64) {
        if t >= self.released_t {
            return self.to;
        }
        let Some(next) = self.path.iter().position(|(pt, _)| *pt >= t) else {
            return self.to;
        };
        if next == 0 {
            return self.path[0].1;
        }
        let (t0, p0) = self.path[next - 1];
        let (t1, p1) = self.path[next];
        let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
        (p0.0 + (p1.0 - p0.0) * k, p0.1 + (p1.1 - p0.1) * k)
    }
}

/// Whether an arrow from `from` to `to` is long enough to show (not a shortcut).
pub fn long_enough(from: (f64, f64), to: (f64, f64), aspect: f64) -> bool {
    let (dx, dy) = ((to.0 - from.0) * aspect, to.1 - from.1);
    (dx * dx + dy * dy).sqrt() >= MIN_LENGTH
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ArrowStyle {
    /// Show arrows in the preview and draw them into the export
    pub enabled: bool,
    /// Unmultiplied sRGBA
    pub rgba: [u8; 4],
    /// How long an arrow stays on screen after ⌘ is released
    pub hold_secs: f32,
    /// Shaft thickness in pixels at 1080p
    pub thickness: f32,
}

impl Default for ArrowStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            rgba: [236, 52, 68, 255],
            hold_secs: 3.0,
            thickness: 7.0,
        }
    }
}

impl ArrowStyle {
    pub fn thickness_px(&self, frame_height: f32) -> f32 {
        self.thickness * frame_height / 1080.0
    }

    /// Opacity `age` seconds after ⌘ was released: fully visible, then a short
    /// fade out at the end of the hold time.
    pub fn opacity_after_release(&self, age: f64) -> f32 {
        let hold = self.hold_secs as f64;
        if age < 0.0 {
            return 1.0;
        }
        if age > hold {
            return 0.0;
        }
        ((hold - age) / FADE_OUT_SECS).min(1.0) as f32
    }

    /// The arrow at recording time `t`: (tail, tip, opacity), or `None` when not on screen.
    pub fn state_at(&self, arrow: &Arrow, t: f64, aspect: f64) -> Option<((f64, f64), (f64, f64), f32)> {
        if t < arrow.pressed_t {
            return None;
        }
        if t <= arrow.released_t {
            let tip = arrow.tip_at(t);
            return long_enough(arrow.from, tip, aspect).then_some((arrow.from, tip, 1.0));
        }
        let opacity = self.opacity_after_release(t - arrow.released_t);
        (opacity > 0.0).then_some((arrow.from, arrow.to, opacity))
    }
}

/// Finds every ⌘ press-move-release gesture in the telemetry.
/// `aspect` is the screen width divided by its height.
/// `key` is the modifier held to draw an arrow (see Settings › Defaults › Keys).
pub fn detect_arrows(
    telemetry: &TelemetrySession,
    aspect: f64,
    key: crate::keys::ModifierKey,
) -> Vec<Arrow> {
    struct Gesture {
        pressed_t: f64,
        from: (f64, f64),
        path: Vec<(f64, (f64, f64))>,
        clicked: bool,
    }

    let mut arrows = Vec::new();
    let mut current: Option<Gesture> = None;
    let pos = |s: &TelemetrySample| (s.norm_x, s.norm_y);

    for sample in &telemetry.samples {
        let held = sample.modifier_held(key.telemetry_name());
        match (&mut current, held) {
            (None, true) if sample.is_inside => {
                current = Some(Gesture {
                    pressed_t: sample.t,
                    from: pos(sample),
                    path: vec![(sample.t, pos(sample))],
                    clicked: false,
                });
            }
            (Some(g), true) => {
                if sample.is_inside {
                    g.path.push((sample.t, pos(sample)));
                }
                // ⌘-click is a selection shortcut, not an arrow
                if sample.event.ends_with("_down") || sample.event.ends_with("_click") {
                    g.clicked = true;
                }
            }
            (Some(g), false) => {
                let last = g.path.last().map_or(g.from, |p| p.1);
                let to = if sample.is_inside { pos(sample) } else { last };
                let held_for = sample.t - g.pressed_t;
                if !g.clicked && long_enough(g.from, to, aspect) && held_for <= MAX_HOLD_SECS {
                    let mut path = std::mem::take(&mut g.path);
                    path.push((sample.t, to));
                    arrows.push(Arrow {
                        pressed_t: g.pressed_t,
                        released_t: sample.t,
                        from: g.from,
                        to,
                        path,
                    });
                }
                current = None;
            }
            _ => {}
        }
    }
    arrows
}

/// Shaft and head outline of an arrow from `from` to `to` (pixels), `t` thick.
/// Returns (shaft end, head triangle).
pub fn arrow_geometry(from: (f32, f32), to: (f32, f32), t: f32) -> ((f32, f32), [(f32, f32); 3]) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let (ux, uy) = (dx / len, dy / len);
    let head_len = (t * 3.6).min(len * 0.6);
    let head_half = t * 1.8;
    let base = (to.0 - ux * head_len, to.1 - uy * head_len);
    let shaft_end = (to.0 - ux * head_len * 0.7, to.1 - uy * head_len * 0.7);
    let head = [
        to,
        (base.0 - uy * head_half, base.1 + ux * head_half),
        (base.0 + uy * head_half, base.1 - ux * head_half),
    ];
    (shaft_end, head)
}

fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((a.0 + t * dx - p.0).powi(2) + (a.1 + t * dy - p.1).powi(2)).sqrt()
}

/// Space around the arrow's line that its border and shadow need.
fn arrow_margin(thickness: f32) -> f32 {
    let t = thickness.max(1.0);
    t * 2.5 + (t * 0.35).max(1.5) + t * 0.9 + 2.0
}

/// Draws an arrow with a white border and soft shadow into an unmultiplied RGBA
/// canvas (`width` × `height`), touching only the pixels around the arrow.
pub fn draw_arrow_into(
    canvas: &mut [u8],
    width: usize,
    height: usize,
    from: (f32, f32),
    to: (f32, f32),
    thickness: f32,
    rgba: [u8; 4],
) {
    let t = thickness.max(1.0);
    let border = (t * 0.35).max(1.5);
    let blur = t * 0.9;
    let margin = arrow_margin(t);
    let (shaft_end, head) = arrow_geometry(from, to, t);
    let head = head.to_vec();

    // Distance to the arrow body, negative inside
    let sd = |x: f32, y: f32| -> f32 {
        let shaft = distance_to_segment((x, y), from, shaft_end) - t / 2.0;
        shaft.min(signed_distance_polygon(x, y, &head) - t * 0.15)
    };

    let x0 = (from.0.min(to.0) - margin).floor().max(0.0) as usize;
    let y0 = (from.1.min(to.1) - margin).floor().max(0.0) as usize;
    let x1 = ((from.0.max(to.0) + margin).ceil().max(0.0) as usize).min(width);
    let y1 = ((from.1.max(to.1) + margin).ceil().max(0.0) as usize).min(height);
    for py in y0..y1 {
        for px in x0..x1 {
            let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
            let d = sd(x, y);
            let shadow = 1.0 - smoothstep(-blur, blur, sd(x, y - t * 0.35) - border);
            let mut c = premul([0, 0, 0, 90], shadow);
            c = over(c, premul([255, 255, 255, 255], 0.5 - (d - border)));
            c = over(c, premul(rgba, 0.5 - d));
            if c[3] > 0.0 {
                let i = (py * width + px) * 4;
                for k in 0..3 {
                    canvas[i + k] = (c[k] / c[3] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                canvas[i + 3] = (c[3] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Arrow image with a white border and soft shadow. `hotspot` is where the tail
/// (`from`) sits inside the sprite, so it can be placed at `from - hotspot`.
pub fn arrow_sprite(from: (f32, f32), to: (f32, f32), thickness: f32, rgba: [u8; 4]) -> Sprite {
    let margin = arrow_margin(thickness);
    let min_x = from.0.min(to.0) - margin;
    let min_y = from.1.min(to.1) - margin;
    let width = (from.0.max(to.0) + margin - min_x).ceil() as usize;
    let height = (from.1.max(to.1) + margin - min_y).ceil() as usize;
    let local = |p: (f32, f32)| (p.0 - min_x, p.1 - min_y);
    let (a, b) = (local(from), local(to));
    let mut data = vec![0u8; width * height * 4];
    draw_arrow_into(&mut data, width, height, a, b, thickness, rgba);
    Sprite {
        width,
        height,
        rgba: data,
        hotspot: (a.0, a.1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(t: f64, x: f64, y: f64, event: &str, cmd: bool) -> TelemetrySample {
        let mods = if cmd { json!(["cmd"]) } else { json!([]) };
        TelemetrySample {
            t,
            time_ms: (t * 1000.0) as u64,
            frame: 0,
            norm_x: x,
            norm_y: y,
            x: x * 1920.0,
            y: y * 1080.0,
            is_inside: true,
            event: event.to_string(),
            button: None,
            left_down: false,
            right_down: false,
            click_duration_ms: None,
            click_count: None,
            raw_json: json!({ "modifiers": mods }),
        }
    }

    fn session(samples: Vec<TelemetrySample>) -> TelemetrySession {
        TelemetrySession {
            session_start: json!({"type": "session_start"}),
            samples,
            session_end: None,
            jsonl_path: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn test_detect_arrows_from_cmd_gestures() {
        let tel = session(vec![
            sample(0.5, 0.10, 0.10, "move", false),
            // ⌘ pressed, mouse moved, released: an arrow from (0.2,0.2) to (0.6,0.5)
            sample(1.0, 0.20, 0.20, "modifier_change", true),
            sample(1.2, 0.40, 0.35, "move", true),
            sample(1.4, 0.60, 0.50, "move", true),
            sample(1.5, 0.60, 0.50, "modifier_change", false),
            // ⌘C style shortcut: no movement, ignored
            sample(3.0, 0.60, 0.50, "modifier_change", true),
            sample(3.1, 0.60, 0.50, "modifier_change", false),
            // ⌘-click: ignored even though the mouse moved
            sample(4.0, 0.10, 0.80, "modifier_change", true),
            sample(4.2, 0.50, 0.80, "left_down", true),
            sample(4.3, 0.50, 0.80, "left_click", true),
            sample(4.4, 0.50, 0.80, "modifier_change", false),
        ]);
        let arrows = detect_arrows(&tel, 16.0 / 9.0, crate::keys::ModifierKey::Command);
        assert_eq!(arrows.len(), 1);
        assert_eq!(arrows[0].from, (0.20, 0.20));
        assert_eq!(arrows[0].to, (0.60, 0.50));
        assert_eq!(arrows[0].pressed_t, 1.0);
        assert_eq!(arrows[0].released_t, 1.5);
        // The tip followed the mouse while ⌘ was held
        let tip = arrows[0].tip_at(1.2);
        assert!((tip.0 - 0.40).abs() < 1e-9 && (tip.1 - 0.35).abs() < 1e-9);
    }

    #[test]
    fn test_arrow_grows_with_the_mouse_then_stays_and_fades() {
        let style = ArrowStyle::default();
        let arrow = Arrow {
            pressed_t: 1.0,
            released_t: 2.0,
            from: (0.1, 0.1),
            to: (0.5, 0.5),
            path: vec![(1.0, (0.1, 0.1)), (1.5, (0.3, 0.3)), (2.0, (0.5, 0.5))],
        };
        let aspect = 16.0 / 9.0;
        // Not shown before ⌘ is pressed, or while still too short
        assert!(style.state_at(&arrow, 0.9, aspect).is_none());
        assert!(style.state_at(&arrow, 1.0, aspect).is_none());
        // While held, the tip follows the recorded mouse
        let (_, tip, opacity) = style.state_at(&arrow, 1.25, aspect).unwrap();
        assert!((tip.0 - 0.2).abs() < 1e-9 && (tip.1 - 0.2).abs() < 1e-9);
        assert_eq!(opacity, 1.0);
        // After release: same arrow, fully visible, fading only at the very end
        let (_, tip, opacity) = style.state_at(&arrow, 3.0, aspect).unwrap();
        assert_eq!(tip, (0.5, 0.5));
        assert_eq!(opacity, 1.0);
        assert!(style.state_at(&arrow, 4.9, aspect).unwrap().2 < 0.5);
        assert!(style.state_at(&arrow, 5.1, aspect).is_none());
    }

    #[test]
    fn test_arrow_sprite_tip_and_tail_are_drawn() {
        let sprite = arrow_sprite((100.0, 100.0), (300.0, 200.0), 8.0, [255, 0, 0, 255]);
        let alpha = |x: f32, y: f32| {
            sprite.rgba[((y as usize) * sprite.width + x as usize) * 4 + 3]
        };
        let (hx, hy) = sprite.hotspot;
        // Tail is on the hotspot; the tip is 200px right and 100px down from it
        assert!(alpha(hx + 2.0, hy + 1.0) > 200);
        assert!(alpha(hx + 196.0, hy + 98.0) > 200);
        assert_eq!(alpha(1.0, (sprite.height - 2) as f32), 0);
    }
}
