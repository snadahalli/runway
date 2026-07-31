//! Somewhere for problems to go.
//!
//! A tray app launched from Finder or the Start menu has no terminal attached,
//! so `eprintln!` reaches nobody. And the frontend runs in a webview whose
//! console is invisible unless devtools happen to be open. The result is that
//! failures are completely silent: a permission denied by the capability system
//! rejects a promise, nothing catches it, and the feature simply doesn't work
//! with no trace anywhere. That cost real time on the panel's drag handling.
//!
//! So: everything notable goes to stderr *and* to a file, and the frontend
//! forwards its uncaught errors here rather than dropping them.

use std::fmt::Display;
use std::io::Write;

use runway_core::paths;

/// Truncate rather than grow forever. This is a diagnostic aid, not an audit
/// trail, and nobody wants a background app quietly eating a gigabyte.
const MAX_BYTES: u64 = 512 * 1024;

pub fn path() -> std::path::PathBuf {
    paths::support_dir().join("runway.log")
}

pub fn warn(context: &str, message: impl Display) {
    write("WARN", context, message);
}

pub fn info(context: &str, message: impl Display) {
    write("INFO", context, message);
}

fn write(level: &str, context: &str, message: impl Display) {
    let line = format!(
        "{} {level:<5} {context}: {message}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    eprintln!("{line}");

    let path = path();
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// `let _ = ...` with a paper trail. Use anywhere a failure is survivable but
/// shouldn't be invisible.
pub trait LogError {
    fn or_warn(self, context: &str);
}

impl<T, E: Display> LogError for Result<T, E> {
    fn or_warn(self, context: &str) {
        if let Err(e) = self {
            warn(context, e);
        }
    }
}
