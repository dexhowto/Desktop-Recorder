//! The live overlay on Windows: a separate native window over the recorded
//! display, drawn on the CPU and shown with per-pixel alpha
//! (`UpdateLayeredWindow`).
//!
//! The egui window cannot be used for this on Windows: a transparent wgpu
//! window needs DirectComposition, and DirectComposition content is not shown
//! in a layered (click-through) window, so the overlay came out white. A
//! layered window with its own bitmap works with any graphics driver,
//! including VMs. It is topmost, click-through, never focused and left out of
//! the recording like the other windows of the app.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HTTRANSPARENT, RegisterClassW, SW_SHOWNOACTIVATE,
    SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, WM_NCHITTEST, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::overlay::{OverlayScene, COUNTDOWN_CAPTION};

const CLASS_NAME: PCWSTR = w!("DexHowToOverlay");

/// The overlay window and its bitmap.
pub struct OverlayWindow {
    hwnd: HWND,
    /// Monitor rectangle in physical pixels
    rect: RECT,
    /// Physical pixels per point on that monitor
    scale: f32,
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    /// Premultiplied BGRA, top row first (the DIB section's memory)
    pixels: *mut u8,
    last_scene: Option<OverlayScene>,
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        // Clicks go to whatever is below
        return LRESULT(HTTRANSPARENT as isize);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_class() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassW(&class);
    });
}

impl OverlayWindow {
    /// Creates the overlay over `rect` (physical pixels) on a monitor with `scale`.
    pub fn new(rect: RECT, scale: f32) -> Option<Self> {
        register_class();
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("DexHowTo Recorder Overlay"),
                WS_POPUP,
                rect.left,
                rect.top,
                w,
                h,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .ok()?;

            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Negative: rows from the top
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = CreateCompatibleDC(None);
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                let _ = DestroyWindow(hwnd);
                return None;
            };
            let old_bitmap = SelectObject(dc, bitmap.into());

            let overlay = Self {
                hwnd,
                rect,
                scale,
                dc,
                bitmap,
                old_bitmap,
                pixels: bits.cast(),
                last_scene: None,
            };
            overlay.present();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(overlay)
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Whether this overlay covers `rect`.
    pub fn covers(&self, rect: RECT) -> bool {
        self.rect == rect
    }

    fn size(&self) -> (usize, usize) {
        (
            (self.rect.right - self.rect.left) as usize,
            (self.rect.bottom - self.rect.top) as usize,
        )
    }

    fn buffer(&mut self) -> &mut [u8] {
        let (w, h) = self.size();
        unsafe { std::slice::from_raw_parts_mut(self.pixels, w * h * 4) }
    }

    /// Draws `scene` when it changed since the last frame.
    pub fn draw(&mut self, scene: &OverlayScene) {
        if self.last_scene.as_ref() == Some(scene) {
            return;
        }
        let (w, h) = self.size();
        let s = self.scale;
        let buffer = self.buffer();
        buffer.fill(0);
        let mut canvas = Canvas { pixels: buffer, width: w, height: h };

        if let Some(secs) = scene.countdown {
            let c = (w as f32 / 2.0, h as f32 / 2.0);
            canvas.circle(c, 74.0 * s, [0, 0, 0, 150]);
            canvas.ring(c, 74.0 * s, 4.0 * s, [242, 54, 66, 255]);
            canvas.text(&secs.to_string(), c, 72.0 * s, [255, 255, 255, 255]);
            canvas.text(COUNTDOWN_CAPTION, (c.0, c.1 + 104.0 * s), 16.0 * s, [255, 255, 255, 255]);
        }
        for &(from, to, opacity) in &scene.arrows {
            let sprite = crate::arrows::arrow_sprite(
                (from.x * s, from.y * s),
                (to.x * s, to.y * s),
                scene.thickness * s,
                scene.rgba,
            );
            let x = (from.x * s - sprite.hotspot.0).round() as i64;
            let y = (from.y * s - sprite.hotspot.1).round() as i64;
            canvas.sprite(&sprite.rgba, sprite.width, sprite.height, x, y, opacity);
        }

        self.last_scene = Some(scene.clone());
        self.present();
    }

    /// Sends the bitmap to the screen and keeps the window on top.
    fn present(&self) {
        let (w, h) = self.size();
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&POINT { x: self.rect.left, y: self.rect.top }),
                Some(&SIZE { cx: w as i32, cy: h as i32 }),
                Some(self.dc),
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    /// Puts the overlay above windows that became topmost after it.
    pub fn raise(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Premultiplied BGRA pixels with simple anti-aliased drawing.
struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: usize,
    height: usize,
}

impl Canvas<'_> {
    /// Blends an unmultiplied RGBA color over pixel (x, y) with `coverage` 0–1.
    fn blend(&mut self, x: usize, y: usize, rgba: [u8; 4], coverage: f32) {
        let a = rgba[3] as f32 / 255.0 * coverage.clamp(0.0, 1.0);
        if a <= 0.0 || x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 4;
        let p = &mut self.pixels[i..i + 4];
        let keep = 1.0 - a;
        // BGRA order
        for (k, channel) in [rgba[2], rgba[1], rgba[0]].into_iter().enumerate() {
            p[k] = (channel as f32 * a + p[k] as f32 * keep).round() as u8;
        }
        p[3] = (255.0 * a + p[3] as f32 * keep).round() as u8;
    }

    /// Calls `coverage(x, y)` for every pixel in the box around `center`.
    fn shade(&mut self, center: (f32, f32), radius: f32, rgba: [u8; 4], coverage: impl Fn(f32) -> f32) {
        let x0 = (center.0 - radius - 1.0).floor().max(0.0) as usize;
        let y0 = (center.1 - radius - 1.0).floor().max(0.0) as usize;
        let x1 = ((center.0 + radius + 1.0).ceil() as usize).min(self.width);
        let y1 = ((center.1 + radius + 1.0).ceil() as usize).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - center.0).powi(2) + (y as f32 + 0.5 - center.1).powi(2)).sqrt();
                self.blend(x, y, rgba, coverage(d));
            }
        }
    }

    fn circle(&mut self, center: (f32, f32), radius: f32, rgba: [u8; 4]) {
        self.shade(center, radius, rgba, |d| radius + 0.5 - d);
    }

    fn ring(&mut self, center: (f32, f32), radius: f32, width: f32, rgba: [u8; 4]) {
        let half = width / 2.0;
        self.shade(center, radius + half, rgba, |d| half + 0.5 - (d - radius).abs());
    }

    /// Text in egui's default font, centered on `center`.
    fn text(&mut self, text: &str, center: (f32, f32), size: f32, rgba: [u8; 4]) {
        let Ok(font) = FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT) else {
            return;
        };
        let scaled = font.as_scaled(PxScale::from(size));
        let width: f32 = text.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum();
        let mut x = center.0 - width / 2.0;
        // Center the height of the capitals and digits
        let baseline = center.1 + scaled.ascent() * 0.36;
        for c in text.chars() {
            let glyph = scaled.scaled_glyph(c);
            let id = glyph.id;
            let mut glyph = glyph;
            glyph.position = ab_glyph::point(x, baseline);
            x += scaled.h_advance(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let px = bounds.min.x + gx as f32;
                    let py = bounds.min.y + gy as f32;
                    if px >= 0.0 && py >= 0.0 {
                        self.blend(px as usize, py as usize, rgba, coverage);
                    }
                });
            }
        }
    }

    /// Blends an unmultiplied RGBA sprite at (x, y) with `opacity`.
    fn sprite(&mut self, rgba: &[u8], width: usize, height: usize, x: i64, y: i64, opacity: f32) {
        for sy in 0..height {
            let ty = y + sy as i64;
            if ty < 0 {
                continue;
            }
            for sx in 0..width {
                let tx = x + sx as i64;
                if tx < 0 {
                    continue;
                }
                let i = (sy * width + sx) * 4;
                let color = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
                self.blend(tx as usize, ty as usize, color, opacity);
            }
        }
    }
}
