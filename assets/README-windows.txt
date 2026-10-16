DexHowTo Recorder for Windows (beta, x64)

Run "DexHowTo Recorder.exe". It needs Windows 10 2004 or later, or Windows 11.
On Windows 11 for ARM (for example in UTM on a Mac) it runs through x64 emulation.
The exe is not signed, so SmartScreen may warn: click "More info", then "Run anyway".

ffmpeg: recording the screen works without it, but the microphone, the editor
and export need ffmpeg.exe and ffprobe.exe. Put both next to the .exe or on PATH
(for example the "essentials" build from https://www.gyan.dev/ffmpeg/builds/).

Not available yet on Windows: system audio, camera.
Default keys: hold Ctrl while moving the mouse to draw an arrow,
Shift to keep a movement in Clean Move.

If the app does not open:
- A message box should say why. Errors are also written to log.txt
  in this folder (next to the .exe); log.previous.txt holds the run before.
- Or run "DexHowTo Recorder (console).exe" from a Command Prompt
  (cd to this folder, then type: "DexHowTo Recorder (console).exe")
  and copy what it prints.
- "DexHowTo Recorder.exe" --glow starts with OpenGL instead of Direct3D 12.
