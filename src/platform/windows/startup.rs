//! Remembers a start that crashed while opening the window, so the next start
//! can use the other renderer. A native crash in a graphics driver ends the app
//! before Rust can react, so this is the only way to recover from it.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    Wgpu,
    Glow,
}

impl Renderer {
    fn name(self) -> &'static str {
        match self {
            Renderer::Wgpu => "wgpu",
            Renderer::Glow => "glow",
        }
    }
}

/// Exists while a window is being opened; removed after the first frame.
fn marker_path() -> PathBuf {
    crate::platform::app_support_dir().join("opening-window.txt")
}

/// The renderer the last start was opening the window with when it closed.
pub fn previous_crash() -> Option<Renderer> {
    let text = std::fs::read_to_string(marker_path()).ok()?;
    clear();
    match text.trim() {
        "wgpu" => Some(Renderer::Wgpu),
        "glow" => Some(Renderer::Glow),
        _ => None,
    }
}

/// Notes that the window is being opened with `renderer`.
pub fn attempt(renderer: Renderer) {
    let path = marker_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, renderer.name());
}

/// The window is open (or the attempt ended without a crash).
pub fn clear() {
    let _ = std::fs::remove_file(marker_path());
}
