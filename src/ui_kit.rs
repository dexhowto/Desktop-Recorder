//! Shared look for the app's windows: color tokens, buttons, toggles, cards and the logo.

use eframe::egui::{
    self, Color32, ColorImage, FontId, Margin, Pos2, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};

// ─────────────────────────────────────────────────────────────────────────────
// App Logo
// ─────────────────────────────────────────────────────────────────────────────

/// App logo as 128×128 RGBA, made from `assets/AppIcon-1024.png` with:
/// `ffmpeg -i assets/AppIcon-1024.png -vf scale=128:128:flags=lanczos -f rawvideo -pix_fmt rgba assets/logo_128.rgba`
pub(crate) const LOGO_RGBA: &[u8] = include_bytes!("../assets/logo_128.rgba");
pub(crate) const LOGO_SIZE: usize = 128;

/// Window icon for the dock and editor windows (used when running outside the .app bundle).
pub fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: LOGO_RGBA.to_vec(),
        width: LOGO_SIZE as u32,
        height: LOGO_SIZE as u32,
    }
}

pub(crate) fn logo_texture(ctx: &egui::Context) -> TextureHandle {
    let id = egui::Id::new("dexhowto_logo_texture");
    if let Some(tex) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return tex;
    }
    let image = ColorImage::from_rgba_unmultiplied([LOGO_SIZE, LOGO_SIZE], LOGO_RGBA);
    let tex = ctx.load_texture(
        "dexhowto_logo",
        image,
        TextureOptions {
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::LINEAR
        },
    );
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

// ─────────────────────────────────────────────────────────────────────────────
// Theme & Widgets
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) mod theme {
    use eframe::egui::Color32;

    pub const BG: Color32 = Color32::from_rgb(11, 12, 15);
    pub const PANEL: Color32 = Color32::from_rgb(19, 21, 26);
    pub const SURFACE: Color32 = Color32::from_rgb(26, 29, 36);
    pub const SURFACE_HI: Color32 = Color32::from_rgb(36, 40, 50);
    pub const BORDER: Color32 = Color32::from_rgb(38, 42, 53);
    pub const TEXT: Color32 = Color32::from_rgb(232, 236, 244);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 158, 176);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(96, 104, 122);
    pub const ACCENT: Color32 = Color32::from_rgb(88, 136, 255);
    pub const CLIP: Color32 = Color32::from_rgb(30, 78, 71);
    pub const CLIP_EDGE: Color32 = Color32::from_rgb(56, 140, 122);
    pub const CUT: Color32 = Color32::from_rgb(236, 72, 90);
    pub const MARK_IN: Color32 = Color32::from_rgb(64, 206, 128);
    pub const MARK_OUT: Color32 = Color32::from_rgb(236, 72, 90);
    pub const SUCCESS: Color32 = Color32::from_rgb(64, 206, 128);
    pub const WARN: Color32 = Color32::from_rgb(255, 184, 76);
    pub const SELECTED: Color32 = Color32::from_rgb(255, 208, 84);
    pub const PLAYHEAD: Color32 = Color32::from_rgb(245, 247, 252);
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ButtonKind {
    Ghost,
    Subtle,
    Primary,
    Danger,
    /// Translucent fill and border in the given color (active toggles, warnings).
    Tinted(Color32),
}

pub(crate) fn button_sized(
    ui: &mut egui::Ui,
    kind: ButtonKind,
    text: &str,
    font_size: f32,
    min_size: Vec2,
    enabled: bool,
) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(font_size), theme::TEXT);
    let size = Vec2::new((galley.size().x + 22.0).max(min_size.x), min_size.y);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);

    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = hovered && resp.is_pointer_button_down_on();
        let (mut fill, stroke, mut fg) = match kind {
            ButtonKind::Ghost => (
                if hovered { theme::SURFACE_HI } else { Color32::TRANSPARENT },
                Color32::TRANSPARENT,
                if hovered { theme::TEXT } else { theme::TEXT_DIM },
            ),
            ButtonKind::Subtle => (theme::SURFACE_HI, theme::BORDER, theme::TEXT),
            ButtonKind::Primary => (theme::ACCENT, Color32::TRANSPARENT, Color32::WHITE),
            ButtonKind::Danger => (theme::CUT, Color32::TRANSPARENT, Color32::WHITE),
            ButtonKind::Tinted(c) => (
                c.gamma_multiply(0.16),
                c.gamma_multiply(0.7),
                c.lerp_to_gamma(Color32::WHITE, 0.25),
            ),
        };
        if hovered && kind != ButtonKind::Ghost {
            fill = fill.lerp_to_gamma(Color32::WHITE, 0.08);
        }
        if pressed {
            fill = fill.lerp_to_gamma(Color32::BLACK, 0.15);
        }
        if !enabled {
            fill = fill.gamma_multiply(0.45);
            fg = fg.gamma_multiply(0.35);
        }
        let stroke = if enabled { stroke } else { stroke.gamma_multiply(0.4) };
        ui.painter()
            .rect(rect, 6.0, fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
        let pos = rect.center() - galley.size() / 2.0;
        ui.painter().galley_with_override_text_color(pos, galley, fg);
    }

    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

pub(crate) fn button(ui: &mut egui::Ui, kind: ButtonKind, text: &str, enabled: bool) -> egui::Response {
    button_sized(ui, kind, text, 12.5, Vec2::new(28.0, 28.0), enabled)
}

pub(crate) fn icon_button(ui: &mut egui::Ui, glyph: &str, enabled: bool) -> egui::Response {
    button_sized(ui, ButtonKind::Ghost, glyph, 15.0, Vec2::new(30.0, 30.0), enabled)
}

pub(crate) fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(30.0, 17.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let bg = theme::SURFACE_HI.lerp_to_gamma(theme::ACCENT, t);
    ui.painter().rect_filled(rect, 8.5, bg);
    let cx = egui::lerp((rect.left() + 8.5)..=(rect.right() - 8.5), t);
    ui.painter()
        .circle_filled(Pos2::new(cx, rect.center().y), 6.5, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(crate) fn toggle_row(ui: &mut egui::Ui, label: &str, hint: &str, value: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.5).color(theme::TEXT))
            .on_hover_text(hint);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            toggle_switch(ui, value);
        });
    });
}

pub(crate) fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(10.5)
            .strong()
            .color(theme::TEXT_FAINT),
    );
    ui.add_space(4.0);
}

pub(crate) fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(8.0)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

pub(crate) fn badge(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(10.0)
        .inner_margin(Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(11.0)
                    .color(color.lerp_to_gamma(Color32::WHITE, 0.2)),
            );
        })
        .response
}

pub(crate) fn kbd(ui: &mut egui::Ui, keys: &str) {
    egui::Frame::new()
        .fill(theme::SURFACE_HI)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(4.0)
        .inner_margin(Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(keys).size(10.5).monospace().color(theme::TEXT));
        });
}

pub(crate) fn vertical_divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(9.0, 20.0), Sense::hover());
    ui.painter().line_segment(
        [rect.center_top(), rect.center_bottom()],
        Stroke::new(1.0, theme::BORDER),
    );
}

pub(crate) fn stat_row(ui: &mut egui::Ui, label: &str, value: String, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.0).color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.5).monospace().color(color));
        });
    });
}

pub(crate) fn truncate_middle(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars - head - 1;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

