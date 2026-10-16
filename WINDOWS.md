# DexHowTo Recorder on Windows (phase 1)

## What works

- Display list (EnumDisplayMonitors), recording a display to MP4 with Windows
  Graphics Capture and the Media Foundation H.264 / HEVC encoder.
- Microphone (default input device, WASAPI through `cpal`), merged into the video
  with ffmpeg like on macOS.
- Mouse and keyboard telemetry (GetCursorPos, GetAsyncKeyState) in the same
  `.mouse.jsonl` format. Coordinates are physical pixels, so they match the video.
  The Windows key is written as `cmd`, Alt as `alt`.
- Live overlay while recording: topmost, click-through, and left out of the video
  with `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`.
- Editor and export. Export uses `h264_mf` when the ffmpeg build has it, otherwise
  `libx264`.
- "Show in Explorer" / "Play" use explorer.

Default keys on Windows: Ctrl draws arrows, Shift keeps a movement in Clean Move
(the Windows key would open the Start menu).

## Not yet (disabled in the UI)

- System audio (TODO: WASAPI loopback).
- Camera (TODO: ffmpeg `-f dshow`).
- Installer. For now copy `dexhowto_recorder.exe` and put `ffmpeg.exe` and
  `ffprobe.exe` next to it or on PATH.

## Build

On Windows (MSVC toolchain):

```bat
cargo build --release
```

From a Mac, build a Windows x64 .exe with mingw-w64 (it also runs on Windows 11
for ARM, for example in UTM, through x64 emulation):

```bash
brew install mingw-w64
rustup target add x86_64-pc-windows-gnu
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu
```

The .exe is `target/x86_64-pc-windows-gnu/release/dexhowto_recorder.exe` and only
needs system DLLs.

From a Mac, type-check the MSVC target only:

```bash
rustup target add x86_64-pc-windows-msvc
cargo check --target x86_64-pc-windows-msvc
```

## To test on a real Windows PC

1. App starts without a console window; the dock is centered near the bottom of
   the main display; the tray icon and its menu work.
2. Display list shows every monitor with the right resolution.
3. Record 30 s on the main display: the MP4 plays, has the right size and frame
   rate, and the countdown and arrows are NOT in the video.
4. With "Record own windows" on, the overlay IS in the video.
5. The live overlay is visible (not black, not invisible) and clicks go through it
   to the apps below. Check with both renderers if possible (glow and wgpu).
6. Microphone on: voice is in the video and in sync (clap at the start and end).
7. Telemetry: open the recording in the editor; cursor and clicks line up with the
   video, Ctrl arrows are detected.
8. Mixed scaling: main display at 100 % and a second at 150 %. Record the second
   one: the overlay covers it exactly, arrows follow the mouse, telemetry lines up.
   Then move the dock between monitors.
9. Export in the editor with an ffmpeg that has `h264_mf` and with one that has
   only `libx264`.
10. Windows 10 (yellow capture border cannot be hidden there) and Windows 11.
11. A `.mov` output setting still produces a playable file (the container is MP4).
