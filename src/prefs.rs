//! Saved preferences: the defaults the app starts with.
//!
//! Stored as JSON in `~/Library/Application Support/DexHowTo Recorder/settings.json`.
//! The Settings window edits these; the dock's quick toggles only change the
//! current session and start from these values on every launch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::arrows::ArrowStyle;
use crate::clicks::ClickStyle;
use crate::config::RecorderConfig;
use crate::export::CursorStyle;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Recording options the dock starts with
    pub recorder: RecorderConfig,
    /// Cursor look the export dialog starts with
    pub export_cursor: CursorStyle,
    /// Click rings the editor preview and export dialog start with
    pub export_clicks: ClickStyle,
    /// Arrows drawn with the arrow key: live while recording, in the editor, and on export
    pub arrows: ArrowStyle,
    /// Keys held while recording to draw arrows / keep exact mouse movement
    pub keys: crate::keys::KeyBindings,
    /// Turn on Clean Move when a recording opens in the editor
    pub clean_move: bool,
    /// Frame rate, size, data rate, codec and audio of every export
    pub export_encoding: crate::encoding::ExportEncoding,
}

fn settings_path() -> PathBuf {
    crate::platform::app_support_dir().join("settings.json")
}

impl Preferences {
    /// Loads the saved preferences; missing or unreadable settings fall back to defaults.
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    eprintln!("Warning: could not save settings: {e}");
                }
            }
            Err(e) => eprintln!("Warning: could not save settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preferences_round_trip_and_tolerate_missing_fields() {
        let mut prefs = Preferences::default();
        prefs.recorder.shows_cursor = false;
        prefs.arrows.hold_secs = 6.5;
        prefs.export_clicks.enabled = false;
        prefs.export_clicks.right_rgba = [1, 2, 3, 255];
        let text = serde_json::to_string(&prefs).unwrap();
        assert_eq!(serde_json::from_str::<Preferences>(&text).unwrap(), prefs);

        // Older settings files without newer fields still load, with defaults filled in
        let partial: Preferences =
            serde_json::from_str(r#"{"recorder": {"shows_cursor": false}}"#).unwrap();
        assert!(!partial.recorder.shows_cursor);
        assert_eq!(partial.arrows, ArrowStyle::default());
        assert_eq!(partial.export_clicks, ClickStyle::default());
    }
}
