//! User preferences, persisted as JSON next to the rest of Runway's state.
//!
//! The Swift app used `UserDefaults`, which has no cross-platform equivalent
//! worth the abstraction, so this is a plain file. Every field keeps its Swift
//! default so behaviour doesn't quietly change under anyone who switches.

use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::usage_api::MINIMUM_POLL_INTERVAL;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuBarStyle {
    /// `1.8×` — how fast you're burning vs sustainable.
    PaceRatio,
    /// `240K/h` — what you may still spend per hour.
    Allowance,
    /// `42%` — the conventional readout.
    Percent,
    /// `2h 14m` until the binding limit runs dry.
    TimeLeft,
}

impl MenuBarStyle {
    pub fn label(&self) -> &'static str {
        match self {
            MenuBarStyle::PaceRatio => "Pace ratio",
            MenuBarStyle::Allowance => "Hourly allowance",
            MenuBarStyle::Percent => "Percent used",
            MenuBarStyle::TimeLeft => "Time to dry",
        }
    }

    pub fn explanation(&self) -> &'static str {
        match self {
            MenuBarStyle::PaceRatio => {
                "1.0× lands exactly at the reset. Above that runs dry early."
            }
            MenuBarStyle::Allowance => {
                "Tokens per hour you can still spend and finish the window level."
            }
            MenuBarStyle::Percent => "Percentage of the binding limit consumed.",
            MenuBarStyle::TimeLeft => "Projected time until the binding limit hits 100%.",
        }
    }
}

/// `default` at the container level means a settings file written by an older
/// or newer build still loads — missing keys take the Swift defaults rather than
/// throwing the whole file away.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub poll_interval: f64,
    pub menu_bar_style: MenuBarStyle,
    pub show_menu_bar_spark: bool,

    pub alarms_enabled: bool,
    pub thresholds: Vec<f64>,
    pub predictive_alarms: bool,
    pub pace_alarm_ratio: f64,

    pub quiet_hours_enabled: bool,
    pub quiet_start_hour: u32,
    pub quiet_end_hour: u32,

    pub user_agent_override: String,
    /// Keep the always-on-top desktop panel visible. This is the cross-platform
    /// stand-in for the macOS Notification Centre widget, which has no Windows
    /// equivalent short of MSIX-packaging a Widgets Board provider.
    pub show_hud: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            poll_interval: 180.0,
            menu_bar_style: MenuBarStyle::PaceRatio,
            show_menu_bar_spark: true,
            alarms_enabled: true,
            thresholds: vec![50.0, 80.0, 95.0],
            predictive_alarms: true,
            pace_alarm_ratio: 2.0,
            quiet_hours_enabled: false,
            quiet_start_hour: 22,
            quiet_end_hour: 8,
            user_agent_override: String::new(),
            show_hud: false,
        }
    }
}

impl Settings {
    pub fn path() -> std::path::PathBuf {
        paths::support_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = paths::write_atomic(&Self::path(), &bytes);
        }
    }

    /// Never allow a poll cadence that would get the token rate limited.
    pub fn effective_poll_interval(&self) -> f64 {
        self.poll_interval.max(MINIMUM_POLL_INTERVAL)
    }

    pub fn is_quiet_now(&self) -> bool {
        self.is_quiet_at(Local::now().hour())
    }

    pub fn is_quiet_at(&self, hour: u32) -> bool {
        if !self.quiet_hours_enabled || self.quiet_start_hour == self.quiet_end_hour {
            return false;
        }
        if self.quiet_start_hour < self.quiet_end_hour {
            hour >= self.quiet_start_hour && hour < self.quiet_end_hour
        } else {
            // Wraps past midnight.
            hour >= self.quiet_start_hour || hour < self.quiet_end_hour
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 180s floor is not a preference. A settings file asking for 30s must
    /// not be honoured, however it got there.
    #[test]
    fn poll_interval_cannot_go_below_the_floor() {
        let s = Settings {
            poll_interval: 5.0,
            ..Default::default()
        };
        assert_eq!(s.effective_poll_interval(), MINIMUM_POLL_INTERVAL);
        let s = Settings {
            poll_interval: 600.0,
            ..Default::default()
        };
        assert_eq!(s.effective_poll_interval(), 600.0);
    }

    #[test]
    fn quiet_hours_wrap_past_midnight() {
        let s = Settings {
            quiet_hours_enabled: true,
            quiet_start_hour: 22,
            quiet_end_hour: 8,
            ..Default::default()
        };
        assert!(s.is_quiet_at(23));
        assert!(s.is_quiet_at(2));
        assert!(s.is_quiet_at(7));
        assert!(!s.is_quiet_at(8));
        assert!(!s.is_quiet_at(14));
    }

    #[test]
    fn quiet_hours_within_a_single_day() {
        let s = Settings {
            quiet_hours_enabled: true,
            quiet_start_hour: 9,
            quiet_end_hour: 17,
            ..Default::default()
        };
        assert!(s.is_quiet_at(12));
        assert!(!s.is_quiet_at(18));
        assert!(!s.is_quiet_at(3));
    }

    #[test]
    fn disabled_or_degenerate_ranges_are_never_quiet() {
        assert!(!Settings::default().is_quiet_at(23));
        let s = Settings {
            quiet_hours_enabled: true,
            quiet_start_hour: 10,
            quiet_end_hour: 10,
            ..Default::default()
        };
        assert!(!s.is_quiet_at(10));
    }

    #[test]
    fn unknown_fields_and_partial_files_still_load() {
        let json = r#"{"pollInterval":300,"somethingNew":true}"#;
        let s: Settings = serde_json::from_str(json).unwrap_or_default();
        assert_eq!(s.poll_interval, 300.0);
    }
}
