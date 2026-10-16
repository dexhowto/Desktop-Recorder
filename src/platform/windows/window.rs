//! Native window tweaks while recording on Windows.
//!
//! Unlike on macOS, the dock does not turn into the overlay: it stays as it is
//! (topmost, with its Stop button), and the countdown and live arrows are drawn
//! in a separate native window (`overlay_window.rs`). Every window of the app
//! is left out of the recording with
//! `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` (Windows 10 2004+).

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, SetWindowDisplayAffinity, SetWindowPos,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
};
use windows::core::BOOL;

use super::overlay_window::OverlayWindow;
use crate::overlay::OverlayScene;

/// The dock window stays a dock; the overlay is its own native window.
pub const OVERLAY_IN_DOCK: bool = false;

/// Title of the main (dock) window, set in `main.rs`.
const MAIN_WINDOW_TITLE: &str = "DexHowTo Recorder";

/// How often to put the windows back on top and re-apply the capture exclusion
/// (new windows such as Settings may have opened).
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

thread_local! {
    static EXCLUDE: Cell<bool> = const { Cell::new(false) };
    static LAST_REFRESH: Cell<Option<Instant>> = const { Cell::new(None) };
    static OVERLAY: RefCell<Option<OverlayWindow>> = const { RefCell::new(None) };
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

/// The app's main window (the dock).
fn main_window() -> Option<HWND> {
    own_windows().into_iter().find(|h| title(*h) == MAIN_WINDOW_TITLE)
}

fn set_topmost(hwnd: HWND) {
    unsafe {
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

/// Rounds the opaque dock's corners (Windows 11; ignored on Windows 10).
pub fn style_dock() {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
    let Some(hwnd) = main_window() else {
        return;
    };
    let round = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const round).cast(),
            std::mem::size_of_val(&round) as u32,
        );
    }
}

/// Leaves this app's windows out of the recording (or puts them back in).
/// Applied to every window of the app while recording.
pub fn set_exclude_from_capture(exclude: bool) {
    EXCLUDE.set(exclude);
    LAST_REFRESH.set(None);
}

fn apply_capture_affinity(exclude: bool) {
    let affinity = if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
    for hwnd in own_windows() {
        // Fails before Windows 10 2004; the windows are then recorded too
        if let Err(e) = unsafe { SetWindowDisplayAffinity(hwnd, affinity) } {
            if exclude {
                super::log_line(&format!("Could not leave a window out of the recording: {e}"));
            }
        }
    }
}

/// The recorded monitor in physical pixels and its scale, from its bounds in points.
fn display_monitor(display: (f64, f64, f64, f64)) -> Option<(RECT, f32)> {
    let (x, y, _, _) = display;
    super::display::monitors()
        .into_iter()
        .find(|m| {
            (m.rect.left as f64 / m.scale - x).abs() < 1.0 && (m.rect.top as f64 / m.scale - y).abs() < 1.0
        })
        .map(|m| (m.rect, m.scale as f32))
}

/// Keeps the dock and the overlay on top and out of the recording. `display` is
/// the recorded display in points (x, y, width, height). Cheap; call every
/// frame while recording.
pub fn raise_overlay_window(_display: (f64, f64, f64, f64)) {
    let due = LAST_REFRESH.get().is_none_or(|t| t.elapsed() >= REFRESH_INTERVAL);
    if !due {
        return;
    }
    LAST_REFRESH.set(Some(Instant::now()));
    if let Some(hwnd) = main_window() {
        set_topmost(hwnd);
    }
    OVERLAY.with_borrow(|o| {
        if let Some(o) = o {
            o.raise();
        }
    });
    apply_capture_affinity(EXCLUDE.get());
}

/// Draws `scene` in the overlay over `display` (points), creating the overlay
/// window the first time.
pub fn show_overlay(display: (f64, f64, f64, f64), scene: &OverlayScene) {
    let Some((rect, scale)) = display_monitor(display) else {
        return;
    };
    OVERLAY.with_borrow_mut(|slot| {
        if slot.as_ref().is_none_or(|o| !o.covers(rect)) {
            *slot = OverlayWindow::new(rect, scale);
            match slot {
                Some(o) => {
                    if EXCLUDE.get() {
                        let _ = unsafe { SetWindowDisplayAffinity(o.hwnd(), WDA_EXCLUDEFROMCAPTURE) };
                    }
                }
                None => super::log_line("Could not create the overlay window"),
            }
        }
        if let Some(o) = slot.as_mut() {
            o.draw(scene);
        }
    });
}

/// Removes the overlay and makes the dock an ordinary window again (after recording).
pub fn restore_normal_window() {
    LAST_REFRESH.set(None);
    OVERLAY.with_borrow_mut(|slot| *slot = None);
    if EXCLUDE.replace(false) {
        apply_capture_affinity(false);
    }
}
