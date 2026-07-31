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
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            get_view,
            refresh,
            save_settings,
            save_hud_position,
            log_error,
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

    if show_hud_at_launch {
        show_window(app.handle(), HUD);
    }

    Ok(())
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
    let quit = MenuItemBuilder::with_id("quit", "Quit Runway").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&open, &refresh, &hud])
        .separator()
        .items(&[&quit])
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
fn get_view(state: tauri::State<'_, AppState>) -> View {
    let settings = state.settings.lock().unwrap().clone();
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
