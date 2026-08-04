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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
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

/// Set while an install is in flight.
///
/// A 4MB download over a slow link takes over a minute, during which the button
/// said "Installing…" and nothing else moved. Both of us read that as a dead
/// button and clicked again — and two installs then raced to replace the same
/// bundle. It survived, which is luck rather than design.
#[derive(Default)]
pub struct Installing(pub AtomicBool);

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
    // Refuse a second install rather than letting two of them race to replace
    // the same bundle.
    let guard = app.state::<Installing>();
    if guard.0.swap(true, Ordering::SeqCst) {
        log::info("updater", "install already in progress — ignoring");
        return Err("An update is already installing.".into());
    }
    let result = install_inner(app).await;
    if result.is_err() {
        app.state::<Installing>().0.store(false, Ordering::SeqCst);
    }
    result
}

async fn install_inner(app: &AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;

    log::info("updater", format!("installing v{}", update.version));

    // Report progress. Without it a minute of silence is indistinguishable from
    // a failure — which is exactly how this was first misdiagnosed.
    let handle = app.clone();
    let mut downloaded: usize = 0;
    let mut last_percent: i64 = -1;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk;
                let percent = total
                    .map(|t| (downloaded as f64 / t as f64 * 100.0) as i64)
                    .unwrap_or(-1);
                // One event per whole percent, not per chunk.
                if percent != last_percent {
                    last_percent = percent;
                    let _ = handle.emit("runway://update-progress", percent);
                }
            },
            || {},
        )
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
