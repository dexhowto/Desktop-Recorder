//! Displays, mouse and keyboard state on Windows.
//!
//! Two coordinate spaces are used:
//! - "points" (window positions, the live overlay, the dock drag): physical
//!   pixels divided by the scale of the monitor they are on, which is how
//!   winit places a window that covers one monitor;
//! - physical pixels (telemetry): the space `GetCursorPos` and the captured
//!   video use, so telemetry lines up with the video pixel for pixel.

use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_LBUTTON, VK_LWIN,
    VK_MBUTTON, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows::core::BOOL;

// macOS event flags (kCGEventFlagMask…). The rest of the app and the telemetry
// format use these for modifier keys on every system.
const FLAG_CAPS_LOCK: u64 = 0x0001_0000;
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALT: u64 = 0x0008_0000;
/// The Windows key takes the place of ⌘
const FLAG_COMMAND: u64 = 0x0010_0000;

/// Makes coordinates physical pixels on every monitor. Must run before the
/// first window or display call; winit asks for the same awareness later.
pub fn init_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// One monitor as Windows reports it.
#[derive(Clone, Copy)]
pub(crate) struct Monitor {
    pub handle: HMONITOR,
    /// Number of `\\.\DISPLAYn`, used as the display id
    pub id: u32,
    /// Physical pixels in the virtual screen
    pub rect: RECT,
    pub is_primary: bool,
    /// 1.0 at 100 % scaling, 1.5 at 150 %…
    pub scale: f64,
}

impl Monitor {
    pub fn width(&self) -> u32 {
        (self.rect.right - self.rect.left).max(0) as u32
    }

    pub fn height(&self) -> u32 {
        (self.rect.bottom - self.rect.top).max(0) as u32
    }

    /// x, y, width, height in points.
    fn bounds_points(&self) -> (f32, f32, f32, f32) {
        let s = self.scale;
        (
            (self.rect.left as f64 / s) as f32,
            (self.rect.top as f64 / s) as f32,
            (self.width() as f64 / s) as f32,
            (self.height() as f64 / s) as f32,
        )
    }
}

fn monitor_scale(handle: HMONITOR) -> f64 {
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    let ok = unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok();
    if ok && dpi_x > 0 { dpi_x as f64 / 96.0 } else { 1.0 }
}

fn monitor_info(handle: HMONITOR, fallback_id: u32) -> Option<Monitor> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(handle, (&raw mut info).cast::<MONITORINFO>()) }.as_bool();
    if !ok {
        return None;
    }
    let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
    let name = String::from_utf16_lossy(&info.szDevice[..len]);
    // "\\.\DISPLAY2" -> 2
    let digits: String = name.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    let id = digits.chars().rev().collect::<String>().parse().unwrap_or(fallback_id);
    Some(Monitor {
        handle,
        id,
        rect: info.monitorInfo.rcMonitor,
        // MONITORINFOF_PRIMARY
        is_primary: info.monitorInfo.dwFlags & 1 != 0,
        scale: monitor_scale(handle),
    })
}

/// All monitors, in the order Windows enumerates them.
pub(crate) fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(handle);
        BOOL(1)
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&raw mut handles as isize));
    }
    handles
        .into_iter()
        .enumerate()
        .filter_map(|(i, h)| monitor_info(h, i as u32 + 1))
        .collect()
}

/// The monitor with display id `id`, or the primary one.
pub(crate) fn monitor_by_id(id: u32) -> Option<Monitor> {
    let list = monitors();
    list.iter()
        .find(|m| m.id == id)
        .or_else(|| list.iter().find(|m| m.is_primary))
        .or_else(|| list.first())
        .copied()
}

fn primary_monitor() -> Option<Monitor> {
    let handle = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    monitor_info(handle, 1)
}

fn cursor_physical() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok().map(|_| p)
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(key.0 as i32) } as u16) & 0x8000 != 0
}

/// Mouse position in global points (see the module notes), the same space
/// window positions use.
pub fn global_mouse_position() -> Option<(f32, f32)> {
    let p = cursor_physical()?;
    let handle = unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) };
    let s = monitor_scale(handle);
    Some(((p.x as f64 / s) as f32, (p.y as f64 / s) as f32))
}

/// Whether the left mouse button is physically held right now.
pub fn left_mouse_button_down() -> bool {
    mouse_button_down(0)
}

/// Whether mouse `button` (0 left, 1 right, 2 middle) is held.
pub fn mouse_button_down(button: u32) -> bool {
    match button {
        0 => key_down(VK_LBUTTON),
        1 => key_down(VK_RBUTTON),
        2 => key_down(VK_MBUTTON),
        _ => false,
    }
}

/// Modifier keys held right now, as macOS event flags (kCGEventFlagMask…), so
/// the telemetry format and key bindings are the same on both systems.
pub fn modifier_flags() -> u64 {
    let mut flags = 0;
    if key_down(VK_SHIFT) {
        flags |= FLAG_SHIFT;
    }
    if key_down(VK_CONTROL) {
        flags |= FLAG_CONTROL;
    }
    if key_down(VK_MENU) {
        flags |= FLAG_ALT;
    }
    if key_down(VK_LWIN) || key_down(VK_RWIN) {
        flags |= FLAG_COMMAND;
    }
    // Caps Lock is a toggle: the low bit of GetKeyState says whether it is on
    if unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0 {
        flags |= FLAG_CAPS_LOCK;
    }
    flags
}

/// Cursor position in physical pixels, for telemetry. (0, 0) when it cannot be read.
pub fn telemetry_cursor_position() -> (f64, f64) {
    cursor_physical().map(|p| (p.x as f64, p.y as f64)).unwrap_or((0.0, 0.0))
}

/// Display `id` in the space of `telemetry_cursor_position`: x, y, width, height in pixels.
pub fn telemetry_display_bounds(id: u32) -> (f64, f64, f64, f64) {
    monitor_by_id(id)
        .map(|m| (m.rect.left as f64, m.rect.top as f64, m.width() as f64, m.height() as f64))
        .unwrap_or((0.0, 0.0, 0.0, 0.0))
}

/// Position and size of display `id` in global points.
pub fn display_bounds_points(id: u32) -> (f32, f32, f32, f32) {
    monitor_by_id(id).map(|m| m.bounds_points()).unwrap_or((0.0, 0.0, 1920.0, 1080.0))
}

pub fn get_main_display_bounds_points() -> (f32, f32, f32, f32) {
    primary_monitor().map(|m| m.bounds_points()).unwrap_or((0.0, 0.0, 1920.0, 1080.0))
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayItem {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub is_main: bool,
    pub label: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum DisplayQueryError {
    PermissionDenied(String),
    Other(String),
}

impl std::fmt::Display for DisplayQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionDenied(msg) => write!(f, "Screen recording permission required: {msg}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// Windows lets desktop apps capture the screen without asking.
pub fn has_screen_capture_permission() -> bool {
    true
}

pub fn get_available_displays() -> Result<Vec<DisplayItem>, DisplayQueryError> {
    let list = monitors();
    if list.is_empty() {
        return Err(DisplayQueryError::Other("No displays found".to_string()));
    }
    let mut items: Vec<DisplayItem> = list
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let kind = if m.is_primary { "Main Screen" } else { "External" };
            DisplayItem {
                id: m.id,
                width: m.width(),
                height: m.height(),
                is_main: m.is_primary,
                label: format!("🖥️ Display {} ({} × {}) — {kind}", idx + 1, m.width(), m.height()),
            }
        })
        .collect();
    // Put main display first if present
    items.sort_by(|a, b| b.is_main.cmp(&a.is_main));
    Ok(items)
}

/// Windows has no Screen Recording permission; nothing to open.
pub fn open_screen_recording_settings() {}

/// Opens Settings › Privacy & security › Microphone.
pub fn open_microphone_settings() {
    let _ = std::process::Command::new("explorer")
        .arg("ms-settings:privacy-microphone")
        .spawn();
}

/// Windows has no Screen Recording permission to reset.
pub fn reset_screen_recording_permission() {}
