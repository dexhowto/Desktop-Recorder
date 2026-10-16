//! Keys the user holds while recording to mark mouse movement.
//!
//! Only modifier keys are recorded in the telemetry (with every mouse record), so
//! these are the keys that can be chosen.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModifierKey {
    Command,
    Control,
    Option,
    Shift,
    Fn,
}

impl ModifierKey {
    pub const ALL: [ModifierKey; 5] = [
        ModifierKey::Command,
        ModifierKey::Control,
        ModifierKey::Option,
        ModifierKey::Shift,
        ModifierKey::Fn,
    ];

    /// Short label for buttons and help text.
    pub fn label(self) -> &'static str {
        match self {
            ModifierKey::Command => "⌘ Cmd",
            ModifierKey::Control => "Ctrl",
            ModifierKey::Option => "Option",
            ModifierKey::Shift => "Shift",
            ModifierKey::Fn => "fn",
        }
    }

    /// Name the recorder writes in each telemetry record's `modifiers` list.
    pub fn telemetry_name(self) -> &'static str {
        match self {
            ModifierKey::Command => "cmd",
            ModifierKey::Control => "ctrl",
            ModifierKey::Option => "alt",
            ModifierKey::Shift => "shift",
            ModifierKey::Fn => "fn",
        }
    }

    /// macOS event flag for this key (kCGEventFlagMask…).
    fn flag_mask(self) -> u64 {
        match self {
            ModifierKey::Command => 0x0010_0000,
            ModifierKey::Control => 0x0004_0000,
            ModifierKey::Option => 0x0008_0000,
            ModifierKey::Shift => 0x0002_0000,
            ModifierKey::Fn => 0x0080_0000,
        }
    }

    /// Whether the key is held right now.
    pub fn is_down(self) -> bool {
        crate::display::modifier_flags() & self.flag_mask() != 0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyBindings {
    /// Hold while moving the mouse to draw an arrow
    pub arrow: ModifierKey,
    /// Hold while moving the mouse to keep that exact movement in Clean Move
    pub keep_movement: ModifierKey,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            arrow: ModifierKey::Command,
            keep_movement: ModifierKey::Control,
        }
    }
}
