//! Live on-screen overlay shown while recording.
//!
//! During the countdown and the recording, the dock window turns into a
//! transparent, click-through layer over the recorded display. It shows the
//! countdown and the ⌘ arrows live, so you can see what you point at. The
//! recorder leaves this app's windows out of the capture, so none of it is
//! recorded; the arrows are added at export from the telemetry instead.

use std::time::Instant;

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, Vec2};

use crate::arrows::ArrowStyle;
use crate::display::{global_mouse_position, left_mouse_button_down};
use crate::keys::ModifierKey;

/// Shortest arrow, as a fraction of the display height (same rule as at export).
const MIN_LENGTH: f32 = 0.03;

struct Gesture {
    from: Pos2,
    to: Pos2,
    /// A click while ⌘ is held is a ⌘-click shortcut, not an arrow
    clicked: bool,
}

struct ShownArrow {
    from: Pos2,
    to: Pos2,
    released: Instant,
}

pub struct LiveOverlay {
    /// The recorded display, in global screen points
    pub display: Rect,
    gesture: Option<Gesture>,
    shown: Vec<ShownArrow>,
}

impl LiveOverlay {
    pub fn new(display: Rect) -> Self {
        Self {
            display,
            gesture: None,
            shown: Vec::new(),
        }
    }

    fn long_enough(&self, from: Pos2, to: Pos2) -> bool {
        (to - from).length() >= MIN_LENGTH * self.display.height()
    }

    /// Follows ⌘ and the mouse. Call every frame while recording.
    /// `window_min` is where the overlay window really is on screen: macOS keeps
    /// windows below the menu bar, so it can differ from the display's corner.
    fn track_gesture(&mut self, window_min: Pos2, key: ModifierKey) {
        let Some((mx, my)) = global_mouse_position() else {
            return;
        };
        // Overlay coordinates: relative to the overlay window
        let mouse = Pos2::new(mx, my) - window_min.to_vec2();
        let held = key.is_down();
        match (&mut self.gesture, held) {
            (None, true) => {
                self.gesture = Some(Gesture {
                    from: mouse,
                    to: mouse,
                    clicked: left_mouse_button_down(),
                });
            }
            (Some(g), true) => {
                g.to = mouse;
                g.clicked |= left_mouse_button_down();
            }
            (Some(_), false) => {
                if let Some(g) = self.gesture.take() {
                    if !g.clicked && self.long_enough(g.from, g.to) {
                        self.shown.push(ShownArrow {
                            from: g.from,
                            to: g.to,
                            released: Instant::now(),
                        });
                    }
                }
            }
            (None, false) => {}
        }
    }

    /// Draws the overlay for this frame. `countdown` is the remaining seconds
    /// before recording starts; `recording` is true once capture is running.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        countdown: Option<u32>,
        recording: bool,
        style: &ArrowStyle,
        arrow_key: ModifierKey,
    ) {
        let painter = ui.painter().clone();
        let area = ui.max_rect();

        if let Some(secs) = countdown {
            let c = area.center();
            painter.circle_filled(c, 74.0, Color32::from_black_alpha(150));
            painter.circle_stroke(c, 74.0, Stroke::new(4.0, Color32::from_rgb(242, 54, 66)));
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                secs.to_string(),
                FontId::proportional(72.0),
                Color32::WHITE,
            );
            painter.text(
                c + Vec2::new(0.0, 104.0),
                egui::Align2::CENTER_CENTER,
                "Recording starts…",
                FontId::proportional(16.0),
                Color32::WHITE,
            );
        }

        if recording && style.enabled {
            let window_min = ui
                .ctx()
                .input(|i| i.viewport().inner_rect.map(|r| r.min))
                .unwrap_or(self.display.min);
            self.track_gesture(window_min, arrow_key);
            let thickness = style.thickness_px(self.display.height());
            let at = |p: Pos2| area.min + p.to_vec2();

            // Arrows already released fade out like they will in the video
            self.shown
                .retain(|a| a.released.elapsed().as_secs_f64() <= style.hold_secs as f64);
            for arrow in &self.shown {
                let opacity = style.opacity_after_release(arrow.released.elapsed().as_secs_f64());
                crate::editor::draw_arrow(&painter, at(arrow.from), at(arrow.to), thickness, style.rgba, opacity);
            }

            // The arrow being drawn right now follows the mouse
            if let Some(g) = &self.gesture {
                if !g.clicked && self.long_enough(g.from, g.to) {
                    crate::editor::draw_arrow(&painter, at(g.from), at(g.to), thickness, style.rgba, 1.0);
                }
            }
        }

        // Smooth while something moves; otherwise check ⌘ often enough to feel instant
        let busy = countdown.is_some() || self.gesture.is_some() || !self.shown.is_empty();
        let wait = if busy { 16 } else { 40 };
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(wait));
    }
}
