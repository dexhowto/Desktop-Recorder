//! Native window tweaks for the recording overlay (no egui/winit equivalent).
//!
//! The overlay must stay above every other window, including full-screen apps,
//! remote-desktop viewers and other always-on-top windows, and show on every Space.
//!
//! macOS shows a regular (Dock) app's normal window only on that app's own Spaces,
//! never on another app's full-screen Space (Windows App, a full-screen browser…).
//! So while a full-screen app covers the recorded display, the overlay window
//! becomes a non-activating panel of an accessory app (no Dock icon), which macOS
//! does allow there. When no app is full screen, it is a normal window again.

use std::cell::Cell;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;

/// Title of the main (dock / overlay) window, set in `main.rs`.
const MAIN_WINDOW_TITLE: &str = "DexHowTo Recorder";

/// NSScreenSaverWindowLevel: above floating, modal and full-screen windows.
const OVERLAY_LEVEL: isize = 1000;
const NORMAL_LEVEL: isize = 0;

// NSWindowCollectionBehavior flags
const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const STATIONARY: usize = 1 << 4;
const IGNORES_CYCLE: usize = 1 << 6;
const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
const OVERLAY_BEHAVIOR: usize = CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY;

/// NSWindowStyleMaskNonactivatingPanel
const NONACTIVATING_PANEL: usize = 1 << 7;

// NSApplicationActivationPolicy
const POLICY_REGULAR: isize = 0;
const POLICY_ACCESSORY: isize = 1;

/// How often to look for a full-screen app on the recorded display.
const FULL_SCREEN_CHECK_INTERVAL: Duration = Duration::from_millis(500);

thread_local! {
    /// winit's window class, while the window is swapped to the panel class
    static WINDOW_CLASS: Cell<Option<&'static AnyClass>> = const { Cell::new(None) };
    static LAST_CHECK: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn shared_app() -> Option<*mut AnyObject> {
    let app: *mut AnyObject = unsafe { msg_send![class!(NSApplication), sharedApplication] };
    (!app.is_null()).then_some(app)
}

/// The app's main window (the dock, which becomes the overlay while recording).
fn main_window() -> Option<*mut AnyObject> {
    unsafe {
        let app = shared_app()?;
        let windows: *mut AnyObject = msg_send![app, windows];
        if windows.is_null() {
            return None;
        }
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            if window.is_null() {
                continue;
            }
            let title: Option<Retained<NSString>> = msg_send![window, title];
            if title.is_some_and(|t| t.to_string() == MAIN_WINDOW_TITLE) {
                return Some(window);
            }
        }
        None
    }
}

/// An NSPanel subclass that never takes focus (the overlay is click-through).
fn panel_class() -> &'static AnyClass {
    const NAME: &std::ffi::CStr = c"DexHowToOverlayPanel";
    if let Some(cls) = AnyClass::get(NAME) {
        return cls;
    }
    extern "C" fn no(_this: &AnyObject, _cmd: Sel) -> Bool {
        Bool::NO
    }
    let mut builder = ClassBuilder::new(NAME, class!(NSPanel)).expect("panel class name is free");
    unsafe {
        builder.add_method(sel!(canBecomeKeyWindow), no as extern "C" fn(_, _) -> _);
        builder.add_method(sel!(canBecomeMainWindow), no as extern "C" fn(_, _) -> _);
    }
    builder.register()
}

fn set_activation_policy(policy: isize) {
    if let Some(app) = shared_app() {
        unsafe {
            let current: isize = msg_send![app, activationPolicy];
            if current != policy {
                let _: bool = msg_send![app, setActivationPolicy: policy];
            }
        }
    }
}

/// Turns the window into a non-activating panel. winit's window class adds no
/// instance variables to NSWindow, so swapping the class is safe.
unsafe fn make_panel(window: *mut AnyObject) {
    if WINDOW_CLASS.get().is_some() {
        return;
    }
    unsafe {
        let object = &*window;
        let original = AnyObject::set_class(object, panel_class());
        WINDOW_CLASS.set(Some(original));
        let mask: usize = msg_send![window, styleMask];
        let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
        // Panels hide when their app is not active by default
        let _: () = msg_send![window, setHidesOnDeactivate: false];
        let _: () = msg_send![window, setFloatingPanel: true];
    }
    set_activation_policy(POLICY_ACCESSORY);
}

/// Turns the panel back into winit's normal window.
unsafe fn make_window(window: *mut AnyObject) {
    let Some(original) = WINDOW_CLASS.take() else {
        return;
    };
    unsafe {
        let mask: usize = msg_send![window, styleMask];
        let _: () = msg_send![window, setStyleMask: mask & !NONACTIVATING_PANEL];
        AnyObject::set_class(&*window, original);
    }
    set_activation_policy(POLICY_REGULAR);
}

/// Puts the overlay above every window and on every Space, and makes it a panel
/// while a full-screen app covers `display` (global points: x, y, width, height).
/// Cheap; call every frame while the overlay is shown, because egui/winit may
/// reset the window level.
pub fn raise_overlay_window(display: (f64, f64, f64, f64)) {
    let Some(window) = main_window() else {
        return;
    };
    unsafe {
        let due = LAST_CHECK.get().is_none_or(|t| t.elapsed() >= FULL_SCREEN_CHECK_INTERVAL);
        let level: isize = msg_send![window, level];
        let behavior: usize = msg_send![window, collectionBehavior];
        if level != OVERLAY_LEVEL || behavior != OVERLAY_BEHAVIOR {
            let _: () = msg_send![window, setLevel: OVERLAY_LEVEL];
            let _: () = msg_send![window, setCollectionBehavior: OVERLAY_BEHAVIOR];
        }
        if !due {
            return;
        }
        LAST_CHECK.set(Some(Instant::now()));
        let full_screen = full_screen_app_on(display);
        if full_screen {
            make_panel(window);
        } else {
            make_window(window);
        }
        // Bring the overlay onto the current Space (e.g. after a switch to a
        // full-screen app) without activating this app, so the other app keeps the
        // keyboard. isOnActiveSpace is always true for an all-Spaces window, so it
        // cannot tell when this is needed; ordering front is cheap.
        let _: () = msg_send![window, orderFrontRegardless];
    }
}

/// macOS leaves this app out of the recording in the capture filter instead
/// (see `screen_capture`), so there is nothing to do here.
pub fn set_exclude_from_capture(_exclude: bool) {}

/// Makes the main window an ordinary window again (after recording).
pub fn restore_normal_window() {
    LAST_CHECK.set(None);
    let Some(window) = main_window() else {
        return;
    };
    unsafe {
        make_window(window);
        let _: () = msg_send![window, setLevel: NORMAL_LEVEL];
        let _: () = msg_send![window, setCollectionBehavior: 0usize];
        if let Some(app) = shared_app() {
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
        }
    }
}

// --- Full-screen app detection (Core Graphics window list) ---

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

const LIST_ON_SCREEN_ONLY: u32 = 1 << 0;
const LIST_EXCLUDE_DESKTOP: u32 = 1 << 4;
const CF_NUMBER_SINT32: isize = 3;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
    fn CGRectMakeWithDictionaryRepresentation(dict: *const c_void, rect: *mut CGRect) -> bool;
    static kCGWindowLayer: *const c_void;
    static kCGWindowOwnerPID: *const c_void;
    static kCGWindowBounds: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: *const c_void, kind: isize, value: *mut c_void) -> bool;
    fn CFRelease(cf: *mut c_void);
}

unsafe fn dict_i32(dict: *const c_void, key: *const c_void) -> Option<i32> {
    unsafe {
        let number = CFDictionaryGetValue(dict, key);
        let mut value = 0i32;
        (!number.is_null() && CFNumberGetValue(number, CF_NUMBER_SINT32, (&raw mut value).cast()))
            .then_some(value)
    }
}

/// Whether another app's normal window covers all of `display`, menu bar area
/// included: a full-screen app. A maximized window leaves the menu bar visible.
fn full_screen_app_on(display: (f64, f64, f64, f64)) -> bool {
    let own_pid = std::process::id() as i32;
    let (dx, dy, dw, dh) = display;
    unsafe {
        let list = CGWindowListCopyWindowInfo(LIST_ON_SCREEN_ONLY | LIST_EXCLUDE_DESKTOP, 0);
        if list.is_null() {
            return false;
        }
        let mut found = false;
        for i in 0..CFArrayGetCount(list) {
            let info = CFArrayGetValueAtIndex(list, i);
            if dict_i32(info, kCGWindowLayer) != Some(0) || dict_i32(info, kCGWindowOwnerPID) == Some(own_pid) {
                continue;
            }
            let bounds = CFDictionaryGetValue(info, kCGWindowBounds);
            let mut r = CGRect::default();
            if bounds.is_null() || !CGRectMakeWithDictionaryRepresentation(bounds, &mut r) {
                continue;
            }
            if r.x <= dx + 1.0 && r.y <= dy + 1.0 && r.x + r.w >= dx + dw - 1.0 && r.y + r.h >= dy + dh - 1.0 {
                found = true;
                break;
            }
        }
        CFRelease(list.cast_mut());
        found
    }
}
