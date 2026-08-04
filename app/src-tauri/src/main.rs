#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The cross-platform shell around [`runway_core`].
//!
//! Three surfaces, all rendering the same `RunwaySnapshot`:
//!
//! - the **tray item** — text title on macOS, painted icon on Windows
//! - the **popover** — a frameless window anchored to the tray
//! - the **HUD** — an always-on-top desktop panel, which is what stands in for
//!   the macOS Notification Centre widget on platforms that have no such thing
//!
//! The engine runs on its own thread and calls back here on every change; this
//! file is only ever translating a snapshot into pixels.

mod log;
mod tray_icon;
mod updater;

use std::sync::Mutex;

use crate::log::LogError;
use runway_core::readout::tooltip;
// The readout is real text on a macOS status item and painted pixels
// everywhere else, so only one of these is ever compiled in.
#[cfg(target_os = "macos")]
use runway_core::readout::menu_bar_text;
#[cfg(not(target_os = "macos"))]
use runway_core::readout::tray_icon_text;
use runway_core::severity::Severity;
use runway_core::{Alarm, EngineConfig, EngineHandle, RunwaySnapshot, Settings, SnapshotHealth};
use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;

const POPOVER: &str = "popover";
const HUD: &str = "hud";
const TRAY: &str = "runway-tray";

struct AppState {
    engine: EngineHandle,
    settings: Mutex<Settings>,
}

/// Everything the frontend needs for one render, in one round trip.
#[derive(Serialize)]
struct View {
    snapshot: RunwaySnapshot,
    settings: Settings,
    alarms: Vec<Alarm>,
    series: Vec<SeriesPoint>,
    #[serde(rename = "lastError")]
    last_error: Option<String>,
    platform: &'static str,
    activity: ActivityView,
    /// Version of a newer release, if one has been seen. `None` means current.
    #[serde(rename = "updateAvailable")]
    update_available: Option<String>,
    version: &'static str,
}

/// The learned working-hours profile, for display. If the app is going to base
/// pace and run-dry on a model of your week, you should be able to see the model.
#[derive(Serialize)]
struct ActivityView {
    /// False means uniform — i.e. plain calendar time, as before.
    learned: bool,
    /// 168 rate multipliers, `weekday * 24 + hour`, Monday first, local time.
    weights: Vec<f64>,
}

#[derive(Serialize)]
struct SeriesPoint {
    t: i64,
    percent: f64,
}

fn main() {
    tauri::Builder::default()
        // Must be the first plugin registered. Without it, launching Runway
        // again starts a second copy — and a tester who couldn't find the tray
        // icon clicked the shortcut until she had ten running. That isn't
        // merely untidy: each one runs its own engine, so ten of them poll a
        // rate-limited endpoint every 180s between them, and they race each
        // other's byte cursors in scan-state.json.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            log::info(
                "single-instance",
                "second launch — surfacing the running window",
            );
            reveal_popover(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_view,
            refresh,
            save_settings,
            save_hud_position,
            log_error,
            open_link,
            open_about,
            check_for_update,
            install_update,
            set_hud_visible,
            close_popover,
            quit
        ])
        .setup(setup)
        .build(tauri::generate_context!())
        .expect("failed to build Runway")
        .run(|_app, event| {
            // Closing the popover must not exit: this is a tray app, and the
            // engine has to keep polling for alarms to be worth anything.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // No Dock icon on macOS; this is a menu bar app.
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    // First line of every log: enough to make a bug report actionable without a
    // round trip asking which version, which OS, and where the state lives.
    log::info(
        "startup",
        format!(
            "Runway {} on {} — state in {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            runway_core::paths::support_dir().display()
        ),
    );

    // No settings file means this is the first launch.
    let first_run = !Settings::path().exists();
    let settings = Settings::load();
    let show_hud_at_launch = settings.show_hud;

    build_tray(app.handle())?;
    build_windows(app.handle())?;

    // The engine owns both cadences and calls us back on every change. Start it
    // here rather than from a window's lifecycle, so alarms still fire for
    // someone who never opens the popover.
    let handle = app.handle().clone();
    let engine = EngineHandle::spawn(
        EngineConfig {
            scan_interval: 15.0,
            settings: settings.clone(),
        },
        move |snapshot, alarms| {
            apply_snapshot(&handle, snapshot);
            for alarm in alarms.iter().filter(|a| a.deliver) {
                let _ = handle
                    .notification()
                    .builder()
                    .title(&alarm.title)
                    .body(&alarm.body)
                    .show();
            }
        },
    );

    app.manage(AppState {
        engine,
        settings: Mutex::new(settings),
    });
    app.manage(updater::AvailableUpdate::default());
    app.manage(updater::Installing::default());
    updater::spawn_periodic_checks(app.handle());

    if show_hud_at_launch {
        show_window(app.handle(), HUD);
    }

    // Windows puts every new notification-area icon in the hidden overflow
    // flyout, so a tray-only app looks like it failed to launch. Show the
    // popover once, on the first run, so there is proof it is running and
    // somewhere to read the "pin the tray icon" hint.
    if first_run {
        reveal_popover(app.handle());
    }

    Ok(())
}

/// Bring the popover up centred, for a first run or a second launch attempt —
/// neither of which has a tray click position to anchor to.
fn reveal_popover(app: &AppHandle) {
    let Some(window) = app.get_webview_window(POPOVER) else {
        return;
    };
    window.center().or_warn("centre the popover");
    window.show().or_warn("show the popover");
    window.set_focus().or_warn("focus the popover");
}

// MARK: - Windows

fn build_windows(app: &AppHandle) -> tauri::Result<()> {
    // Frameless, resizable-off, hidden until the tray is clicked.
    WebviewWindowBuilder::new(app, POPOVER, WebviewUrl::App("index.html".into()))
        .title("Runway")
        .inner_size(380.0, 560.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;

    let hud = WebviewWindowBuilder::new(app, HUD, WebviewUrl::App("hud.html".into()))
        .title("Runway HUD")
        .inner_size(240.0, 118.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;

    restore_hud_position(&hud);

    Ok(())
}

/// Put the panel back where it was dragged to, if that's still somewhere the
/// user can see. A saved position can easily be off-screen now — an external
/// display unplugged, a laptop docked at a different resolution — and a panel
/// restored into the void looks exactly like a panel that failed to open.
fn restore_hud_position(hud: &tauri::WebviewWindow) {
    let settings = Settings::load();
    let (Some(x), Some(y)) = (settings.hud_x, settings.hud_y) else {
        return;
    };

    let Ok(monitors) = hud.available_monitors() else {
        return;
    };
    let visible = monitors.iter().any(|monitor| {
        let origin = monitor.position();
        let size = monitor.size();
        // Require the panel's top-left to sit inside a monitor with enough room
        // to grab it, rather than clipped to a couple of pixels at the edge.
        x >= origin.x
            && y >= origin.y
            && x < origin.x + size.width as i32 - 60
            && y < origin.y + size.height as i32 - 40
    });

    if visible {
        let _ = hud.set_position(tauri::PhysicalPosition { x, y });
    }
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn toggle_popover(app: &AppHandle, near: Option<tauri::PhysicalPosition<f64>>) {
    let Some(window) = app.get_webview_window(POPOVER) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }

    // Anchor under the tray item, nudged left so the window body sits under the
    // click rather than starting at it, and clamped to the monitor.
    if let Some(anchor) = near {
        if let Ok(Some(monitor)) = window.current_monitor() {
            let screen = monitor.size();
            let size = window.outer_size().unwrap_or(tauri::PhysicalSize {
                width: 380,
                height: 560,
            });
            let mut x = anchor.x - size.width as f64 / 2.0;
            let mut y = anchor.y + 8.0;
            x = x.clamp(
                8.0,
                (screen.width as f64 - size.width as f64 - 8.0).max(8.0),
            );
            // On Windows the tray is usually at the bottom, so a window placed
            // below the click would land off-screen. Flip above when it doesn't fit.
            if y + size.height as f64 > screen.height as f64 {
                y = (anchor.y - size.height as f64 - 8.0).max(8.0);
            }
            let _ = window.set_position(tauri::PhysicalPosition { x, y });
        }
    }

    let _ = window.show();
    let _ = window.set_focus();
}

// MARK: - Tray

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItemBuilder::with_id("open", "Open Runway").build(app)?;
    let refresh = MenuItemBuilder::with_id("refresh", "Refresh now").build(app)?;
    let hud = CheckMenuItemBuilder::with_id("hud", "Desktop panel")
        .checked(Settings::load().show_hud)
        .build(app)?;
    let test_alarm = MenuItemBuilder::with_id("test-alarm", "Send a test alarm").build(app)?;
    let update = MenuItemBuilder::with_id("update", "Check for updates\u{2026}").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Runway").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&open, &refresh, &hud])
        .separator()
        .items(&[&test_alarm, &update, &quit])
        .build()?;

    let icon = to_image(tray_icon::placeholder((150, 150, 150), 32));

    TrayIconBuilder::with_id(TRAY)
        .icon(icon)
        .menu(&menu)
        // The menu must not swallow left-clicks: left opens the popover, right
        // opens the menu. That's the platform convention on both OSes.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => toggle_popover(app, None),
            "refresh" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.engine.refresh_now();
                }
            }
            "hud" => {
                let visible = app
                    .get_webview_window(HUD)
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false);
                set_hud(app, !visible);
            }
            // The rules are unit-tested; *delivery* is per-platform and cannot
            // be. This is the only way to find out whether notifications are
            // permitted and actually appear on a given machine.
            "test-alarm" => {
                log::info("alarms", "sending a test notification");
                let sent = app
                    .notification()
                    .builder()
                    .title("Runway alarms are working")
                    .body("This is what a threshold alert looks like.")
                    .show();
                match sent {
                    Ok(()) => log::info("alarms", "notification handed to the OS"),
                    Err(e) => log::warn(
                        "alarms",
                        format!("{e} — check notification permission for Runway"),
                    ),
                }
            }
            "update" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    updater::check(&app, true).await;
                });
            }
            "quit" => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.engine.shutdown();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                toggle_popover(tray.app_handle(), Some(position));
            }
        })
        .build(app)?;

    Ok(())
}

fn to_image(icon: tray_icon::Rgba) -> Image<'static> {
    Image::new_owned(icon.bytes, icon.width, icon.height)
}

/// Push one snapshot to every surface.
///
/// Called from the engine thread. **The tray work must be posted to the main
/// thread**: `TrayIcon::set_title` and friends dispatch to the event loop and
/// block waiting for a reply, so calling them from here deadlocks outright when
/// the first snapshot arrives during `setup()`, before the loop is running.
/// `run_on_main_thread` queues instead of waiting, which is what we want.
fn apply_snapshot(app: &AppHandle, snapshot: &RunwaySnapshot) {
    let now = chrono::Utc::now();
    let style = app
        .try_state::<AppState>()
        .map(|s| s.settings.lock().unwrap().menu_bar_style)
        // During setup the state isn't managed yet, but the engine's bootstrap
        // callback can already have fired.
        .unwrap_or(runway_core::MenuBarStyle::PaceRatio);

    #[cfg(target_os = "macos")]
    let text = menu_bar_text(snapshot, style, now);
    #[cfg(not(target_os = "macos"))]
    let icon_text = tray_icon_text(snapshot, style, now);
    let tip = tooltip(snapshot, now);
    let headline = snapshot.headline();
    let rgb = match headline.map(|l| Severity::of(l, now)) {
        Some(Severity::Calm) => (61, 173, 115),
        Some(Severity::Watch) => (224, 161, 46),
        Some(Severity::Tight) => (217, 77, 71),
        None => (150, 150, 150),
    };
    let percent = headline.map(|l| l.percent).unwrap_or(0.0);
    let empty = snapshot.limits.is_empty();

    let handle = app.clone();
    app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id(TRAY) else {
            return;
        };
        if empty {
            let _ = tray.set_icon(Some(to_image(tray_icon::placeholder(rgb, 32))));
        } else {
            // macOS status items carry real text, so the icon stays a small
            // gauge and the readout is drawn by the system at the right weight.
            // Nothing else has that, so elsewhere the number is painted in.
            #[cfg(target_os = "macos")]
            {
                let _ = tray.set_title(Some(&text));
                let _ = tray.set_icon(Some(to_image(tray_icon::render("", rgb, percent, 22))));
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = tray.set_icon(Some(to_image(tray_icon::render(
                    &icon_text, rgb, percent, 32,
                ))));
            }
        }
        tray.set_tooltip(Some(&tip)).or_warn("tray tooltip");
    })
    .or_warn("dispatch tray update to the main thread");

    // Windows render from the event; they may legitimately not exist yet.
    app.emit("runway://snapshot", snapshot)
        .or_warn("emit snapshot");
}

fn set_hud(app: &AppHandle, visible: bool) {
    if let Some(window) = app.get_webview_window(HUD) {
        let _ = if visible {
            window.show()
        } else {
            window.hide()
        };
    }
    if let Some(state) = app.try_state::<AppState>() {
        let mut settings = state.settings.lock().unwrap();
        settings.show_hud = visible;
        settings.save();
    }
}

// MARK: - Commands

#[tauri::command]
fn get_view(app: AppHandle, state: tauri::State<'_, AppState>) -> View {
    let settings = state.settings.lock().unwrap().clone();
    let update = app
        .try_state::<updater::AvailableUpdate>()
        .and_then(|s| s.0.lock().unwrap().clone());
    state.engine.with(|engine| {
        let snapshot = engine.snapshot.clone();
        let series = snapshot
            .headline()
            .map(|limit| {
                engine
                    .series(limit)
                    .into_iter()
                    .map(|s| SeriesPoint {
                        t: s.date.timestamp(),
                        percent: s.percent,
                    })
                    .collect()
            })
            .unwrap_or_default();

        View {
            snapshot,
            settings: settings.clone(),
            alarms: engine.recent_alarms(),
            series,
            last_error: engine.last_error.clone(),
            platform: std::env::consts::OS,
            activity: ActivityView {
                learned: engine.activity().learned,
                weights: engine.activity().weights().to_vec(),
            },
            update_available: update,
            version: env!("CARGO_PKG_VERSION"),
        }
    })
}

#[tauri::command]
fn refresh(state: tauri::State<'_, AppState>) {
    state.engine.refresh_now();
}

#[tauri::command]
fn save_settings(app: AppHandle, state: tauri::State<'_, AppState>, mut settings: Settings) {
    {
        let mut current = state.settings.lock().unwrap();
        // The panel's own state is owned elsewhere — position by dragging,
        // visibility by the tray menu and `set_hud_visible` — and the popover's
        // copy of the settings can be minutes stale. Saving it verbatim would
        // snap the panel back to wherever it was when the popover last loaded,
        // or close it because it happened to be shut at the time.
        settings.hud_x = current.hud_x;
        settings.hud_y = current.hud_y;
        settings.show_hud = current.show_hud;
        settings.save();
        *current = settings.clone();
    }
    // The engine reads the poll interval and alarm rules off its own copy.
    state
        .engine
        .with(|engine| engine.config.settings = settings);
    let snapshot = state.engine.snapshot();
    apply_snapshot(&app, &snapshot);
}

#[tauri::command]
async fn check_for_update(app: AppHandle) -> Option<String> {
    updater::check(&app, false).await
}

/// Downloads, verifies and installs, then restarts into the new version.
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    updater::install(&app).await
}

/// Right-clicking the desktop panel brings up the real About panel rather than
/// a second, smaller copy of it. The panel is otherwise a dead end — there is no
/// other way to reach settings or quit from it.
#[tauri::command]
fn open_about(app: AppHandle) {
    reveal_popover(&app);
    app.emit("runway://show-panel", "about")
        .or_warn("ask the popover to show About");
}

/// The only links the app will ever open, by name.
///
/// The webview asks for a *key*, not a URL, so there is no path by which a
/// compromised or confused frontend can hand the OS an arbitrary address to
/// launch. Adding a destination is a deliberate edit here.
const LINKS: &[(&str, &str)] = &[
    ("linkedin", "https://www.linkedin.com/in/snadahalli/"),
    ("github", "https://github.com/snadahalli/runway"),
    ("releases", "https://github.com/snadahalli/runway/releases"),
    ("issues", "https://github.com/snadahalli/runway/issues"),
];

#[tauri::command]
fn open_link(app: AppHandle, target: String) -> Result<(), String> {
    let url = LINKS
        .iter()
        .find(|(name, _)| *name == target)
        .map(|(_, url)| *url)
        .ok_or_else(|| format!("unknown link: {target}"))?;
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_url(url, None::<&str>)
        .map_err(|e| {
            log::warn("open_link", &e);
            e.to_string()
        })
}

/// Uncaught frontend errors, forwarded so they don't die in an invisible
/// webview console. A rejected promise from a permission the capability file
/// forgot looks exactly like a feature that was never wired up.
#[tauri::command]
fn log_error(context: String, message: String) {
    log::warn(&format!("webview/{context}"), message);
}

/// Called as the panel is dragged, debounced on the frontend.
#[tauri::command]
fn save_hud_position(state: tauri::State<'_, AppState>, x: i32, y: i32) {
    let mut settings = state.settings.lock().unwrap();
    settings.hud_x = Some(x);
    settings.hud_y = Some(y);
    settings.save();
}

#[tauri::command]
fn set_hud_visible(app: AppHandle, visible: bool) {
    set_hud(&app, visible);
}

#[tauri::command]
fn close_popover(app: AppHandle) {
    if let Some(window) = app.get_webview_window(POPOVER) {
        let _ = window.hide();
    }
}

#[tauri::command]
fn quit(app: AppHandle, state: tauri::State<'_, AppState>) {
    state.engine.shutdown();
    app.exit(0);
}

/// Not used for rendering, but keeps the health enum exhaustively handled if a
/// new variant is ever added.
#[allow(dead_code)]
fn health_label(health: SnapshotHealth) -> &'static str {
    match health {
        SnapshotHealth::Live => "live",
        SnapshotHealth::Estimated => "estimated",
        SnapshotHealth::BackingOff => "backing off",
        SnapshotHealth::Error => "error",
        SnapshotHealth::NoCredentials => "not connected",
    }
}
