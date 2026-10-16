use screencapturekit::shareable_content::SCShareableContent;
use std::process::Command;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

unsafe extern "C" {
    fn CGEventCreate(source: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *mut std::ffi::c_void) -> CGPoint;
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
    fn CFRelease(cf: *mut std::ffi::c_void);
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Mouse position in global screen points (origin at the top-left of the main display),
/// the same space window positions use.
pub fn global_mouse_position() -> Option<(f32, f32)> {
    unsafe {
        let ev = CGEventCreate(std::ptr::null_mut());
        if ev.is_null() {
            return None;
        }
        let loc = CGEventGetLocation(ev);
        CFRelease(ev);
        Some((loc.x as f32, loc.y as f32))
    }
}

/// Whether the left mouse button is physically held right now.
pub fn left_mouse_button_down() -> bool {
    // 1 = kCGEventSourceStateHIDSystemState, button 0 = left
    unsafe { CGEventSourceButtonState(1, 0) }
}

/// Modifier keys held right now, as macOS event flags (kCGEventFlagMask…).
pub fn modifier_flags() -> u64 {
    // 1 = kCGEventSourceStateHIDSystemState
    unsafe { CGEventSourceFlagsState(1) }
}

/// Position and size of display `id` in global screen points (top-left origin).
pub fn display_bounds_points(id: u32) -> (f32, f32, f32, f32) {
    let bounds = unsafe { CGDisplayBounds(id) };
    (
        bounds.origin.x as f32,
        bounds.origin.y as f32,
        bounds.size.width as f32,
        bounds.size.height as f32,
    )
}

pub fn get_main_display_bounds_points() -> (f32, f32, f32, f32) {
    let main_id = unsafe { CGMainDisplayID() };
    let bounds = unsafe { CGDisplayBounds(main_id) };
    (
        bounds.origin.x as f32,
        bounds.origin.y as f32,
        bounds.size.width as f32,
        bounds.size.height as f32,
    )
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

/// Checks if macOS has granted Screen Recording access to this process
pub fn has_screen_capture_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Requests macOS Screen Recording permission prompt
pub fn request_screen_capture_permission() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

pub fn get_available_displays() -> Result<Vec<DisplayItem>, DisplayQueryError> {
    let main_id = unsafe { CGMainDisplayID() };

    // Check system permission status via CoreGraphics
    let has_cg_perm = has_screen_capture_permission();

    match SCShareableContent::get() {
        Ok(content) => {
            let displays = content.displays();
            if displays.is_empty() {
                return Err(DisplayQueryError::Other("No displays found".to_string()));
            }

            let mut items = Vec::new();
            for (idx, d) in displays.iter().enumerate() {
                let id = d.display_id();
                let is_main = id == main_id;
                let label = if is_main {
                    format!("🖥️ Display {} ({} × {}) — Main Screen", idx + 1, d.width(), d.height())
                } else {
                    format!("🖥️ Display {} ({} × {}) — External", idx + 1, d.width(), d.height())
                };

                items.push(DisplayItem {
                    id,
                    width: d.width(),
                    height: d.height(),
                    is_main,
                    label,
                });
            }

            // Put main display first if present
            items.sort_by(|a, b| b.is_main.cmp(&a.is_main));

            Ok(items)
        }
        Err(e) => {
            // Also trigger native macOS permission request if not granted
            if !has_cg_perm {
                request_screen_capture_permission();
            }

            let err_str = format!("{e:?}");
            if err_str.contains("declined TCC")
                || err_str.contains("TCC")
                || err_str.contains("permission")
                || err_str.contains("NoShareableContent")
                || !has_cg_perm
            {
                if has_cg_perm {
                    // System says granted, but ScreenCaptureKit hasn't refreshed its Mach port yet
                    Err(DisplayQueryError::PermissionDenied(
                        "Permission is toggled ON, but macOS requires quitting and restarting DexHowTo Recorder for the new permission to activate.".to_string(),
                    ))
                } else {
                    Err(DisplayQueryError::PermissionDenied(
                        "Screen Recording permission is required. If running from Terminal, grant permission to Terminal; if running DexHowTo Recorder.app, grant permission to DexHowTo Recorder.".to_string(),
                    ))
                }
            } else {
                Err(DisplayQueryError::Other(err_str))
            }
        }
    }
}

/// Open macOS System Settings directly to Privacy & Security -> Screen Recording
pub fn open_screen_recording_settings() {
    // Request access to trigger macOS system prompt if possible
    request_screen_capture_permission();

    // Open System Settings directly to Screen Recording pane
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();

    // Reveal DexHowTo Recorder.app in Finder so the user can easily drag-and-drop it into System Settings
    let app_path = if std::path::Path::new("/Applications/DexHowTo Recorder.app").exists() {
        "/Applications/DexHowTo Recorder.app"
    } else {
        "DexHowTo Recorder.app"
    };

    let _ = Command::new("open")
        .arg("-R")
        .arg(app_path)
        .spawn();
}

/// Open macOS System Settings directly to Privacy & Security -> Microphone
pub fn open_microphone_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        .spawn();
}

/// Reset TCC permission cache for DexHowTo Recorder and Terminal
pub fn reset_screen_recording_permission() {
    // Reset for bundle ID. Wait for it to finish: requesting while the reset is
    // still running races with it and can leave the app missing from the list.
    let _ = Command::new("tccutil")
        .arg("reset")
        .arg("ScreenCapture")
        .arg("com.dexhowto.recorder")
        .status();

    // Request again so the app re-registers in System Settings > Screen Recording
    request_screen_capture_permission();
}
