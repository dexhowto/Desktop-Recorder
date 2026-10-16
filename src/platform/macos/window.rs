//! Native window tweaks for the recording overlay (no egui/winit equivalent).
//!
//! The overlay must stay above every other window, including full-screen apps,
//! remote-desktop viewers and other always-on-top windows, and show on every Space.
//!
//! macOS shows a regular (Dock) app's normal window only on that app's own Spaces,
//! never on another app's full-screen Space (Windows App, a full-screen browser…).
//! A non-activating panel of an accessory app (no Dock icon) may show there. macOS
//! checks this when the Space changes: a window that only becomes a panel after
//! the switch to a full-screen Space stays hidden there. So the overlay window is a
//! panel of an accessory app for the whole recording, before any Space switch.

use std::cell::Cell;
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

/// How often to order the overlay front again.
const ORDER_FRONT_INTERVAL: Duration = Duration::from_millis(500);

thread_local! {
    /// winit's window class, while the window is swapped to the panel class
    static WINDOW_CLASS: Cell<Option<&'static AnyClass>> = const { Cell::new(None) };
    static LAST_ORDER_FRONT: Cell<Option<Instant>> = const { Cell::new(None) };
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

/// Puts the overlay above every window and on every Space, as a panel of an
/// accessory app. Cheap; call every frame while the overlay is shown, because
/// egui/winit may reset the window level.
pub fn raise_overlay_window(_display: (f64, f64, f64, f64)) {
    let Some(window) = main_window() else {
        return;
    };
    unsafe {
        make_panel(window);
        let level: isize = msg_send![window, level];
        let behavior: usize = msg_send![window, collectionBehavior];
        if level != OVERLAY_LEVEL || behavior != OVERLAY_BEHAVIOR {
            let _: () = msg_send![window, setLevel: OVERLAY_LEVEL];
            let _: () = msg_send![window, setCollectionBehavior: OVERLAY_BEHAVIOR];
        }
        if LAST_ORDER_FRONT.get().is_some_and(|t| t.elapsed() < ORDER_FRONT_INTERVAL) {
            return;
        }
        LAST_ORDER_FRONT.set(Some(Instant::now()));
        // Bring the overlay onto the current Space (e.g. after a switch to a
        // full-screen app) without activating this app, so the other app keeps the
        // keyboard. isOnActiveSpace is always true for an all-Spaces window, so it
        // cannot tell when this is needed; ordering front is cheap.
        let _: () = msg_send![window, orderFrontRegardless];
    }
}

/// On macOS the dock window itself becomes the overlay while recording.
pub const OVERLAY_IN_DOCK: bool = true;

/// Nothing to do: the dock draws its own rounded, transparent panel.
pub fn style_dock() {}

/// Not used on macOS: the overlay is drawn in the dock window (`OVERLAY_IN_DOCK`).
pub fn show_overlay(_display: (f64, f64, f64, f64), _scene: &crate::overlay::OverlayScene) {}

/// macOS leaves this app out of the recording in the capture filter instead
/// (see `screen_capture`), so there is nothing to do here.
pub fn set_exclude_from_capture(_exclude: bool) {}

/// Makes the main window an ordinary window again (after recording).
pub fn restore_normal_window() {
    LAST_ORDER_FRONT.set(None);
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
