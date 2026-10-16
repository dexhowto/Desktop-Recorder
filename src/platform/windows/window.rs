//! Native window tweaks for the recording overlay on Windows.
//!
//! egui/winit already make the overlay click-through (`MousePassthrough` sets
//! `WS_EX_LAYERED | WS_EX_TRANSPARENT`) and topmost (`WindowLevel::AlwaysOnTop`).
//! This module keeps it above windows that become topmost later, and leaves
//! this app's windows out of the recording with
//! `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` (Windows 10 2004+).

use std::cell::Cell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
};
use windows::core::BOOL;

/// Title of the main (dock / overlay) window, set in `main.rs`.
const MAIN_WINDOW_TITLE: &str = "DexHowTo Recorder";

/// How often to put the overlay back on top and re-apply the capture exclusion
/// (new windows such as Settings may have opened).
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

thread_local! {
    static EXCLUDE: Cell<bool> = const { Cell::new(false) };
    static LAST_REFRESH: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Top-level windows of this process.
fn own_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == std::process::id() {
            list.push(hwnd);
        }
        BOOL(1)
    }
    let mut list: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&raw mut list as isize));
    }
    list
}

fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..len.min(buf.len())])
}

/// The app's main window (the dock, which becomes the overlay while recording).
fn main_window() -> Option<HWND> {
    own_windows().into_iter().find(|h| title(*h) == MAIN_WINDOW_TITLE)
}

/// Leaves this app's windows out of the recording (or puts them back in).
/// Applied to every window of the app while the overlay is shown.
pub fn set_exclude_from_capture(exclude: bool) {
    EXCLUDE.set(exclude);
    LAST_REFRESH.set(None);
}

fn apply_capture_affinity(exclude: bool) {
    let affinity = if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    for hwnd in own_windows() {
        // Fails before Windows 10 2004; the overlay is then recorded too
        if let Err(e) = unsafe { SetWindowDisplayAffinity(hwnd, affinity) } {
            if exclude {
                eprintln!("Warning: could not leave the overlay out of the recording: {e}");
            }
        }
    }
}

/// The recorded monitor's rectangle in physical pixels, from its bounds in points.
fn display_rect_pixels(display: (f64, f64, f64, f64)) -> Option<RECT> {
    let (x, y, _, _) = display;
    super::display::monitors()
        .into_iter()
        .find(|m| {
            (m.rect.left as f64 / m.scale - x).abs() < 1.0 && (m.rect.top as f64 / m.scale - y).abs() < 1.0
        })
        .map(|m| m.rect)
}

/// Keeps the overlay topmost, click-through, never focused, out of the
/// recording and exactly over the recorded display. `display` is that display
/// in points (x, y, width, height). Cheap; call every frame while the overlay
/// is shown.
pub fn raise_overlay_window(display: (f64, f64, f64, f64)) {
    let due = LAST_REFRESH.get().is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL);
    if !due {
        return;
    }
    LAST_REFRESH.set(Some(Instant::now()));
    let Some(hwnd) = main_window() else {
        return;
    };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        // Click-through needs both flags; winit sets them, this only checks.
        // Never focused, so the app being recorded keeps the keyboard.
        let wanted = ex | (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0) as isize;
        if wanted != ex {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
        }
        // winit converts the position egui asked for with the scale of the
        // monitor the dock was on, which is off when the recorded monitor has
        // another scale. Place the window in physical pixels instead.
        let mut current = RECT::default();
        let _ = GetWindowRect(hwnd, &mut current);
        let target = display_rect_pixels(display).filter(|r| *r != current);
        // Above windows that became topmost after the overlay
        match target {
            Some(r) => {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE,
                );
            }
            None => {
                let _ = SetWindowPos(
                    hwnd,
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
    apply_capture_affinity(EXCLUDE.get());
}

/// Makes the main window an ordinary window again (after recording).
pub fn restore_normal_window() {
    LAST_REFRESH.set(None);
    if let Some(hwnd) = main_window() {
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let normal = ex & !(WS_EX_NOACTIVATE.0 as isize);
            if normal != ex {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, normal);
            }
        }
    }
    if EXCLUDE.replace(false) {
        apply_capture_affinity(false);
    }
}
