//! Checking for, and installing, new versions.
//!
//! An updater is remote code execution on someone else's machine, so the
//! signature is the whole security story. Every artifact is signed at release
//! time with a minisign key whose public half is baked into `tauri.conf.json`;
//! the plugin refuses anything that doesn't verify. That key is Tauri's own and
//! has nothing to do with Apple notarisation or an Authenticode certificate —
//! which is why signed auto-update works here even though the *installers* are
//! unsigned and both platforms complain on first install.
//!
//! Checks run on the backend rather than from the webview, so the updater's
//! commands need no capability grant and the popover can't be tricked into
//! triggering an install.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::log::{self, LogError};

/// Wait this long after launch before the first check. Starting up is already
/// doing a keychain read, a log scan and an API poll; the update check is the
/// least urgent thing in the queue.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);

/// Re-check daily. A tray app can stay running for weeks.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// The newest version seen, if it's newer than what's running. Surfaced in the
/// popover; `None` means we're current or haven't looked yet.
#[derive(Default)]
pub struct AvailableUpdate(pub Mutex<Option<String>>);

pub fn spawn_periodic_checks(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio_sleep(FIRST_CHECK_DELAY).await;
        loop {
            check(&app, false).await;
            tokio_sleep(CHECK_INTERVAL).await;
        }
    });
}

async fn tokio_sleep(duration: Duration) {
    tauri::async_runtime::spawn_blocking(move || std::thread::sleep(duration))
        .await
        .ok();
}

/// Look for a newer version. `announce` sends a notification when one is found,
/// which is what an explicit "Check for updates" from the tray menu wants and
/// the silent daily check does not.
pub async fn check(app: &AppHandle, announce: bool) -> Option<String> {
    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(e) => {
            log::warn("updater", e);
            return None;
        }
    };

    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            log::info("updater", format!("v{version} available"));
            if let Some(state) = app.try_state::<AvailableUpdate>() {
                *state.0.lock().unwrap() = Some(version.clone());
            }
            if announce {
                notify(
                    app,
                    &format!("Runway {version} is available"),
                    "Open Runway to install it.",
                );
            }
            Some(version)
        }
        Ok(None) => {
            if let Some(state) = app.try_state::<AvailableUpdate>() {
                *state.0.lock().unwrap() = None;
            }
            if announce {
                notify(app, "Runway is up to date", env!("CARGO_PKG_VERSION"));
            }
            None
        }
        Err(e) => {
            // A failed check is not worth interrupting anyone over — no network,
            // GitHub down, a manifest not yet published. Log it and move on.
            let reason = e.to_string();
            log::warn("updater check", &reason);
            if announce {
                notify(app, "Couldn't check for updates", &reason);
            }
            None
        }
    }
}

/// Download, verify and install. The app restarts into the new version.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;

    log::info("updater", format!("installing v{}", update.version));
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| {
            log::warn("updater install", &e);
            e.to_string()
        })?;

    log::info("updater", "installed — restarting");
    app.restart();
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .or_warn("update notification");
}
