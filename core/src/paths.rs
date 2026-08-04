//! Where things live, per platform.
//!
//! Claude Code itself is consistent across platforms — `~/.claude` on macOS and
//! Linux, `%USERPROFILE%\.claude` on Windows, both overridable with
//! `CLAUDE_CONFIG_DIR` — so the only real branching is where *we* put our own
//! state, and the macOS App Group container.

use std::path::PathBuf;

/// `$CLAUDE_CONFIG_DIR` if set, otherwise the `.claude` directory in the user's
/// home. `dirs::home_dir` resolves `%USERPROFILE%` on Windows.
pub fn claude_home() -> PathBuf {
    if let Ok(override_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !override_dir.is_empty() {
            return PathBuf::from(override_dir);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
}

pub fn projects_dir() -> PathBuf {
    claude_home().join("projects")
}

/// Runway's own state — sample history and the transcript scan cursor.
///
/// macOS   `~/Library/Application Support/Runway`
/// Windows `%APPDATA%\Runway`
/// Linux   `~/.local/share/runway`
pub fn support_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = if cfg!(target_os = "linux") {
        base.join("runway")
    } else {
        base.join("Runway")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Directory the published snapshot is written to.
///
/// This used to be the macOS App Group container, because a sandboxed WidgetKit
/// extension could see nothing else. That extension is gone, so the snapshot
/// lives with the rest of our state and macOS stops being a special case.
///
/// Moving it was free: the snapshot is rewritten every 15 seconds, and the
/// state worth keeping — `samples.json`, `scan-state.json` — never lived there.
pub fn snapshot_dir() -> PathBuf {
    support_dir()
}

pub fn snapshot_path() -> PathBuf {
    snapshot_dir().join("runway-snapshot.json")
}

pub fn history_path() -> PathBuf {
    support_dir().join("samples.json")
}

/// Accumulated calibration observations. Separate from `samples.json` so a
/// corrupt or hand-edited history doesn't take the calibration with it.
pub fn calibration_path() -> PathBuf {
    support_dir().join("calibration.json")
}

pub fn scan_state_path() -> PathBuf {
    support_dir().join("scan-state.json")
}

/// Write a file without leaving a half-written one behind if we're interrupted.
///
/// `fs::rename` is atomic within a filesystem on both Unix and Windows, but on
/// Windows it fails if the destination exists — hence the explicit remove.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    if cfg!(windows) && path.exists() {
        let _ = std::fs::remove_file(path);
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Fall back to a plain write rather than losing the update entirely.
            let _ = std::fs::remove_file(&tmp);
            std::fs::write(path, bytes).map_err(|_| e)
        }
    }
}
