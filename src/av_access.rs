//! macOS camera and microphone permission (AVFoundation authorization).
//!
//! ScreenCaptureKit does not ask for microphone access by itself: without it the
//! microphone is silently left out of the recording. Ask before recording.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Microphone,
    Camera,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// macOS has not asked the user yet
    NotDetermined,
    /// Blocked by a device policy
    Restricted,
    Denied,
    Allowed,
}

fn media_type(device: Device) -> Retained<NSString> {
    // AVMediaTypeAudio / AVMediaTypeVideo
    NSString::from_str(match device {
        Device::Microphone => "soun",
        Device::Camera => "vide",
    })
}

pub fn status(device: Device) -> Access {
    let media = media_type(device);
    let raw: isize = unsafe {
        msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*media]
    };
    match raw {
        3 => Access::Allowed,
        2 => Access::Denied,
        1 => Access::Restricted,
        _ => Access::NotDetermined,
    }
}

/// Shows the macOS permission prompt if the user has not decided yet.
/// The answer arrives asynchronously; poll `status` afterwards.
pub fn request(device: Device) {
    if status(device) != Access::NotDetermined {
        return;
    }
    let media = media_type(device);
    let handler = RcBlock::new(|_granted: Bool| {});
    unsafe {
        let _: () = msg_send![
            class!(AVCaptureDevice),
            requestAccessForMediaType: &*media,
            completionHandler: &*handler
        ];
    }
}

/// Opens System Settings at the privacy pane for `device`.
pub fn open_privacy_settings(device: Device) {
    let pane = match device {
        Device::Microphone => "Privacy_Microphone",
        Device::Camera => "Privacy_Camera",
    };
    let _ = std::process::Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{pane}"))
        .spawn();
}
