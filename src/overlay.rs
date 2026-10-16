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

    /// Follows the arrow key and lets released arrows fade. Call every frame.
    /// `window_min` is where the overlay window really is on screen (points).
    pub fn update(&mut self, window_min: Pos2, recording: bool, style: &ArrowStyle, arrow_key: ModifierKey) {
        if recording && style.enabled {
            self.track_gesture(window_min, arrow_key);
            // Arrows already released fade out like they will in the video
            self.shown
                .retain(|a| a.released.elapsed().as_secs_f64() <= style.hold_secs as f64);
        }
    }

    /// What to draw this frame, relative to the overlay window.
    pub fn scene(&self, countdown: Option<u32>, recording: bool, style: &ArrowStyle) -> OverlayScene {
        let mut arrows = Vec::new();
        if recording && style.enabled {
            for arrow in &self.shown {
                let opacity = style.opacity_after_release(arrow.released.elapsed().as_secs_f64());
                arrows.push((arrow.from, arrow.to, opacity));
            }
            // The arrow being drawn right now follows the mouse
            if let Some(g) = &self.gesture {
                if !g.clicked && self.long_enough(g.from, g.to) {
                    arrows.push((g.from, g.to, 1.0));
                }
            }
        }
        OverlayScene {
            countdown,
            arrows,
            thickness: style.thickness_px(self.display.height()),
            rgba: style.rgba,
        }
    }

    /// Milliseconds until the next frame: smooth while something moves, otherwise
    /// often enough that the arrow key feels instant.
    pub fn repaint_ms(&self, countdown: Option<u32>) -> u64 {
        let busy = countdown.is_some() || self.gesture.is_some() || !self.shown.is_empty();
        if busy { 16 } else { 40 }
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

        let window_min = ui
            .ctx()
            .input(|i| i.viewport().inner_rect.map(|r| r.min))
            .unwrap_or(self.display.min);
        self.update(window_min, recording, style, arrow_key);
        let scene = self.scene(countdown, recording, style);

        if let Some(secs) = scene.countdown {
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
                COUNTDOWN_CAPTION,
                FontId::proportional(16.0),
                Color32::WHITE,
            );
        }

        let at = |p: Pos2| area.min + p.to_vec2();
        for &(from, to, opacity) in &scene.arrows {
            crate::editor::draw_arrow(&painter, at(from), at(to), scene.thickness, scene.rgba, opacity);
        }

        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(self.repaint_ms(countdown)));
    }
}

/// Text under the countdown number.
pub const COUNTDOWN_CAPTION: &str = "Recording starts…";

/// Everything the overlay shows in one frame, in points relative to the
/// overlay window's top-left corner.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayScene {
    pub countdown: Option<u32>,
    /// Arrows as (from, to, opacity)
    pub arrows: Vec<(Pos2, Pos2, f32)>,
    /// Arrow thickness in points
    pub thickness: f32,
    pub rgba: [u8; 4],
}
