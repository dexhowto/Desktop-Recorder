//! Camera and microphone access on Windows.
//!
//! Windows does not ask desktop apps for permission the way macOS does: access
//! is either allowed for all desktop apps in Settings › Privacy & security, or
//! the device simply delivers silence. So access is always reported as allowed.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Microphone,
    Camera,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Access {
    NotDetermined,
    Restricted,
    Denied,
    Allowed,
}

pub fn status(_device: Device) -> Access {
    Access::Allowed
}

/// Nothing to ask on Windows.
pub fn request(_device: Device) {}

/// Opens Settings at the privacy page for `device`.
pub fn open_privacy_settings(device: Device) {
    let page = match device {
        Device::Microphone => "ms-settings:privacy-microphone",
        Device::Camera => "ms-settings:privacy-webcam",
    };
    let _ = std::process::Command::new("explorer").arg(page).spawn();
}
