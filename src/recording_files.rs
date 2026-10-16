//! Moving a finished recording, together with its companion files, to a new name or folder.

use std::path::{Path, PathBuf};

/// Where a recording and its companion files ended up.
#[derive(Debug, PartialEq)]
pub struct RecordingFiles {
    pub video: PathBuf,
    pub telemetry: Option<PathBuf>,
    pub camera: Option<PathBuf>,
}

/// Moves `video` to `dest` and its telemetry and camera files alongside, named after it:
/// `Talk.mp4`, `Talk.mouse.jsonl`, `Talk.camera.mp4`. Adds the video's extension to
/// `dest` when it has none, and updates the video name stored in the telemetry.
pub fn move_recording(
    video: &Path,
    dest: &Path,
    telemetry: Option<&Path>,
    camera: Option<&Path>,
) -> Result<RecordingFiles, String> {
    let ext = video.extension().and_then(|e| e.to_str()).unwrap_or("mp4");
    let dest = match dest.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case(ext) => dest.to_path_buf(),
        _ => PathBuf::from(format!("{}.{ext}", dest.display())),
    };
    if dest == video {
        return Ok(RecordingFiles {
            video: video.to_path_buf(),
            telemetry: telemetry.map(Path::to_path_buf),
            camera: camera.map(Path::to_path_buf),
        });
    }

    move_file(video, &dest)?;

    let new_telemetry = match telemetry {
        Some(t) if t.exists() => {
            let target = dest.with_extension("mouse.jsonl");
            move_file(t, &target)?;
            let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let _ = rename_video_in_telemetry(&target, name);
            Some(target)
        }
        _ => None,
    };
    let new_camera = match camera {
        Some(c) if c.exists() => {
            let target = dest.with_extension("camera.mp4");
            move_file(c, &target)?;
            Some(target)
        }
        _ => None,
    };

    Ok(RecordingFiles {
        video: dest,
        telemetry: new_telemetry,
        camera: new_camera,
    })
}

/// Renames, falling back to copy + delete when moving to another disk.
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| format!("Cannot save to {}: {e}", to.display()))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// Points the telemetry's `session_start.video_file` at the renamed video.
fn rename_video_in_telemetry(path: &Path, video_name: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for line in lines.iter_mut() {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("session_start") {
            value["video_file"] = serde_json::Value::String(video_name.to_string());
            *line = serde_json::to_string(&value).map_err(|e| e.to_string())?;
            break;
        }
    }
    std::fs::write(path, lines.join("\n") + "\n").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_recording_moves_companions_and_renames_video_in_telemetry() {
        let base = std::env::temp_dir().join(format!("dexhowto_move_test_{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        let video = src.join("Screen Recording 1.mp4");
        let telemetry = src.join("Screen Recording 1.mouse.jsonl");
        let camera = src.join("Screen Recording 1.camera.mp4");
        std::fs::write(&video, b"video").unwrap();
        std::fs::write(
            &telemetry,
            "{\"type\":\"session_start\",\"video_file\":\"Screen Recording 1.mp4\"}\n{\"type\":\"mouse\",\"t\":0.1}\n",
        )
        .unwrap();
        std::fs::write(&camera, b"camera").unwrap();

        // No extension typed: the video's extension is added
        let moved = move_recording(&video, &dst.join("My Talk"), Some(&telemetry), Some(&camera)).unwrap();

        assert_eq!(moved.video, dst.join("My Talk.mp4"));
        assert_eq!(moved.telemetry, Some(dst.join("My Talk.mouse.jsonl")));
        assert_eq!(moved.camera, Some(dst.join("My Talk.camera.mp4")));
        assert!(!video.exists() && !telemetry.exists() && !camera.exists());
        assert_eq!(std::fs::read(&moved.video).unwrap(), b"video");

        let text = std::fs::read_to_string(moved.telemetry.unwrap()).unwrap();
        assert!(text.lines().next().unwrap().contains("\"video_file\":\"My Talk.mp4\""));
        assert!(text.contains("\"type\":\"mouse\""));

        let _ = std::fs::remove_dir_all(&base);
    }
}
