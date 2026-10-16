# DexHowTo Recorder (macOS)

A high-performance native macOS screen recorder written in **Rust**, powered by Apple's **ScreenCaptureKit** and **egui / eframe**.

---

- **🎛️ Floating Dock**: Starts as a compact, sleek floating toolbar dock with 4 modular preview cards (Screen, Camera, Microphone, System Audio) and a prominent circular red **REC** button.
- **⏳ 5-Second Countdown**: Initiates a 5-second countdown timer when starting a recording so you have time to prepare your screen.
- **🗔 Auto-Minification to Tray**: When recording starts, the dock automatically hides and minimizes into the macOS menu bar / system tray so your screen remains clear.
- **🔴 System Tray Recording Indicator & Controls**:
  - Displays a dedicated vibrant **red recording dot** (`🔴`) in the macOS menu bar during active recording.
  - Live timer in the menu bar title (`🔴 00:15`).
  - Menu bar controls to **Stop Recording**, **Show Recorder Window**, or **Quit**.
  - Automatically un-hides and focuses the window when recording stops to show your saved video.
- **🖥️ Screen Selector**: Automatically detects all connected displays (built-in MacBook Retina displays, external monitors, UltraWide screens), showing their resolution, index, and primary monitor status.
- **⚡ Hardware Acceleration**: Encodes directly using Apple Silicon / VideoToolbox hardware encoders via ScreenCaptureKit `SCRecordingOutput`.
- **🎬 Modern Video Codecs**:
  - **H.264 (AVC)**: Maximum cross-platform compatibility.
  - **HEVC (H.265)**: High-efficiency compression for smaller file sizes.
  - **Containers**: MP4 and MOV.
- **🎧 Audio Selector**:
  - **🔊 System Audio**: Captures internal Mac sound (YouTube, video calls, music, games).
  - **🎙️ Microphone Audio**: Captures voice commentary from your microphone.
- **🖱️ Cursor Capture**: Toggle mouse cursor visibility on or off.
- **⏱️ Live Recording Stats**: Displays a real-time recording timer (hours/minutes/seconds) and live file size counter.
- **📂 Native Output Picker**: Choose any save folder with native macOS file dialogs, with quick shortcuts to `~/Movies` and `~/Desktop`.
- **🎉 Post-Recording Actions**: One-click **"Reveal in Finder"** and **"Play Video"** buttons once recording is stopped.
- **🔒 macOS Permissions Helper**: Detects Screen Recording & Microphone permission states with direct shortcuts to macOS System Settings and cache reset tools.

---

## Quick Start

### 1. Launch the Native macOS Application
Launch the packaged macOS app bundle:

```bash
open "DexHowTo Recorder.app"
```

Or run via Cargo:

```bash
cargo run --release
```

---

## Troubleshooting macOS Screen Recording Permissions

If macOS reports that permission is missing even though the toggle in **System Settings > Privacy & Security > Screen Recording** is set to **ON**:

1. **Quit & Relaunch Required by macOS**:
   macOS security architecture does **not** grant screen capture rights to a process that is currently running. When you toggle the switch ON, you must completely **Quit & Reopen** the application.
2. **Terminal vs. App Bundle**:
   - If running via `cargo run --release`, macOS attributes the permission to your **Terminal app** (Terminal, iTerm, VS Code, Cursor). Make sure the toggle is ON for your terminal.
   - If running `DexHowTo Recorder.app`, the toggle must be ON for **DexHowTo Recorder**.
3. **Toggle Off and Back On**:
   In *System Settings > Privacy & Security > Screen Recording*, toggle the switch for **DexHowTo Recorder** (or Terminal) **OFF**, wait 2 seconds, toggle it back **ON**, and relaunch the app.
4. **Resetting Corrupt TCC Cache**:
   If macOS holds a stale permission cache, click **"♻️ Reset Permission Cache"** inside the app or run:
   ```bash
   tccutil reset ScreenCapture com.dexhowto.recorder
   ```

---

## Project Structure

```text
├── DexHowTo Recorder.app/        # Packaged & signed macOS application bundle
├── src/
│   ├── main.rs                   # App entry point and window setup
│   ├── app.rs                    # GUI interface, widgets, and state machine
│   ├── config.rs                 # Recording options, codecs, and defaults
│   ├── display.rs                # ScreenCaptureKit display queries & CoreGraphics permissions
│   ├── recorder.rs               # ScreenCaptureKit capture stream & background worker
│   └── tray.rs                   # System tray icon, menu bar controls & countdown
├── .cargo/
│   └── config.toml               # Swift runtime linking and macOS target flags
├── bundle.sh                     # Release build & ad-hoc code-signing script
└── Cargo.toml                    # Rust dependencies
```

---

## Building from Source

To compile and package a new `.app` bundle:

```bash
./bundle.sh
```
