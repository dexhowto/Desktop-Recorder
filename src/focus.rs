//! Do Not Disturb while recording.
//!
//! macOS has no public API to change Focus, so this goes through two small
//! Shortcuts the app installs (with the user's OK in the Shortcuts app):
//! one turns Do Not Disturb on, the other turns it off. They are run with the
//! `shortcuts` command-line tool.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

pub const SHORTCUT_ON: &str = "DexHowTo Focus On";
pub const SHORTCUT_OFF: &str = "DexHowTo Focus Off";

/// True when both shortcuts are in the user's Shortcuts library.
pub fn shortcuts_installed() -> bool {
    let Ok(out) = Command::new("shortcuts").arg("list").output() else {
        return false;
    };
    let list = String::from_utf8_lossy(&out.stdout);
    let has = |name: &str| list.lines().any(|l| l.trim() == name);
    has(SHORTCUT_ON) && has(SHORTCUT_OFF)
}

/// Builds and signs both shortcuts, then opens them so the Shortcuts app asks
/// the user to add them. Runs in the background; signing can take a few seconds.
pub fn install_shortcuts() {
    std::thread::spawn(|| {
        let dir = std::env::temp_dir().join("DexHowTo Focus Shortcuts");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for (name, enabled) in [(SHORTCUT_ON, true), (SHORTCUT_OFF, false)] {
            match build_signed_shortcut(&dir, name, enabled) {
                Ok(path) => {
                    let _ = Command::new("open").arg(&path).status();
                }
                Err(e) => eprintln!("Could not create the {name} shortcut: {e}"),
            }
        }
    });
}

fn build_signed_shortcut(dir: &Path, name: &str, enabled: bool) -> Result<PathBuf, String> {
    let unsigned = dir.join(format!("{name} (unsigned).shortcut"));
    let signed = dir.join(format!("{name}.shortcut"));
    let xml = dir.join(format!("{name}.plist"));
    std::fs::write(&xml, shortcut_plist(enabled)).map_err(|e| e.to_string())?;
    // `shortcuts sign` only reads the binary format
    let converted = Command::new("plutil")
        .args(["-convert", "binary1", "-o"])
        .arg(&unsigned)
        .arg(&xml)
        .status()
        .map_err(|e| e.to_string())?;
    if !converted.success() {
        return Err("plutil failed".into());
    }
    let _ = std::fs::remove_file(&signed);
    let out = Command::new("shortcuts")
        .args(["sign", "--mode", "anyone", "--input"])
        .arg(&unsigned)
        .arg("--output")
        .arg(&signed)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() || !signed.exists() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(signed)
}

/// A one-action shortcut: Set Focus, Do Not Disturb, on or off.
fn shortcut_plist(enabled: bool) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>WFWorkflowClientVersion</key><string>2607.0.2</string>
  <key>WFWorkflowMinimumClientVersion</key><integer>900</integer>
  <key>WFWorkflowIcon</key>
  <dict>
    <key>WFWorkflowIconStartColor</key><integer>4282601983</integer>
    <key>WFWorkflowIconGlyphNumber</key><integer>59511</integer>
  </dict>
  <key>WFWorkflowTypes</key><array/>
  <key>WFWorkflowInputContentItemClasses</key><array/>
  <key>WFWorkflowImportQuestions</key><array/>
  <key>WFWorkflowActions</key>
  <array>
    <dict>
      <key>WFWorkflowActionIdentifier</key><string>is.workflow.actions.dnd.set</string>
      <key>WFWorkflowActionParameters</key>
      <dict>
        <key>Enabled</key><integer>{}</integer>
        <key>FocusModes</key>
        <dict>
          <key>Identifier</key><string>com.apple.donotdisturb.mode.default</string>
          <key>DisplayString</key><string>Do Not Disturb</string>
        </dict>
      </dict>
    </dict>
  </array>
</dict>
</plist>
"#,
        enabled as u8
    )
}

/// Work for the background thread, done in the order it was asked for, so "off"
/// never overtakes "on" when a recording is cancelled right away.
enum Job {
    /// Turn Do Not Disturb on if the shortcuts are installed
    On,
    /// Turn it off again, only if the matching `On` turned it on
    Off,
}

fn send(job: Job) {
    static WORKER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::spawn(move || {
            let mut turned_on = false;
            for job in rx {
                match job {
                    Job::On => {
                        turned_on = shortcuts_installed() && run_shortcut(SHORTCUT_ON);
                    }
                    Job::Off if turned_on => {
                        run_shortcut(SHORTCUT_OFF);
                        turned_on = false;
                    }
                    Job::Off => {}
                }
            }
        });
        Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send(job);
    }
}

fn run_shortcut(name: &str) -> bool {
    Command::new("shortcuts")
        .args(["run", name])
        .status()
        .is_ok_and(|s| s.success())
}

/// Do Not Disturb turned on for one recording. Dropping it turns Do Not Disturb off again.
pub struct DoNotDisturb(());

impl DoNotDisturb {
    /// Turns Do Not Disturb on in the background. Does nothing when the shortcuts
    /// are not installed.
    pub fn begin() -> Self {
        send(Job::On);
        Self(())
    }
}

impl Drop for DoNotDisturb {
    fn drop(&mut self) {
        send(Job::Off);
    }
}
