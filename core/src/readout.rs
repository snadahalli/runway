//! The one-line readout that goes in the menu bar / tray.
//!
//! Lives in the core because two very different renderers consume it — macOS
//! sets it as status-item text, Windows paints it into a 16-pixel icon — and
//! they must never disagree about what the number is.

use chrono::{DateTime, Utc};

use crate::format as fmt;
use crate::settings::MenuBarStyle;
use crate::snapshot::RunwaySnapshot;

/// Short enough for a menu bar; em dash when there's nothing honest to say.
pub fn menu_bar_text(snapshot: &RunwaySnapshot, style: MenuBarStyle, now: DateTime<Utc>) -> String {
    let Some(limit) = snapshot.headline() else {
        return "\u{2014}".into();
    };

    match style {
        MenuBarStyle::PaceRatio => limit
            .pace_ratio
            .map(fmt::ratio)
            // Before there's enough history for a slope, fall back to the plain
            // percentage rather than showing a made-up pace.
            .unwrap_or_else(|| fmt::percent(limit.percent)),
        MenuBarStyle::Allowance => limit
            .allowance_tokens_per_hour
            .map(|t| format!("{}/h", fmt::tokens(t)))
            .or_else(|| {
                limit
                    .allowance_percent_per_hour
                    .map(|p| format!("{}/h", fmt::percent(p)))
            })
            .unwrap_or_else(|| fmt::percent(limit.percent)),
        MenuBarStyle::Percent => fmt::percent(limit.percent),
        MenuBarStyle::TimeLeft => limit
            .exhausts_at
            .map(|e| fmt::duration((e - now).num_milliseconds() as f64 / 1000.0))
            .or_else(|| limit.time_remaining(now).map(fmt::duration))
            .unwrap_or_else(|| "\u{2014}".into()),
    }
}

/// The readout compressed to fit a tray *icon*.
///
/// macOS status items carry real text and get [`menu_bar_text`]. Windows and
/// Linux have to paint the number into the icon, and at 16 physical pixels only
/// about four characters fit — so `418K/h` has to become `418K` and `2h 14m`
/// has to become `2h`. Deciding that here, rather than letting the renderer
/// clip whatever overflows, keeps the choice legible and testable. The full
/// text is always in the tooltip.
pub fn tray_icon_text(
    snapshot: &RunwaySnapshot,
    style: MenuBarStyle,
    now: DateTime<Utc>,
) -> String {
    let full = menu_bar_text(snapshot, style, now);
    match style {
        // "418K/h" -> "418K"; the rate is implied by the tooltip.
        MenuBarStyle::Allowance => full.split('/').next().unwrap_or(&full).to_string(),
        // "2h 14m" -> "2h"; the leading unit is the one that matters at a glance.
        MenuBarStyle::TimeLeft => full.split(' ').next().unwrap_or(&full).to_string(),
        MenuBarStyle::PaceRatio | MenuBarStyle::Percent => full,
    }
}

/// The longer text for the tray tooltip, where there's room to be explicit.
pub fn tooltip(snapshot: &RunwaySnapshot, now: DateTime<Utc>) -> String {
    let Some(limit) = snapshot.headline() else {
        return snapshot
            .message
            .clone()
            .unwrap_or_else(|| "Runway — no data yet".into());
    };

    let mut lines = vec![format!(
        "{} · {} used",
        limit.label,
        fmt::percent(limit.percent)
    )];

    if let Some(allowance) = limit.allowance_tokens_per_hour {
        lines.push(format!(
            "Sustainable from now: {}/h",
            fmt::tokens(allowance)
        ));
    } else if let Some(allowance) = limit.allowance_percent_per_hour {
        lines.push(format!(
            "Sustainable from now: {}/h",
            fmt::percent(allowance)
        ));
    }

    if let Some(ratio) = limit.pace_ratio {
        lines.push(format!("Pace {}", fmt::ratio(ratio)));
    }

    if limit.runs_dry_early() {
        if let Some(exhausts) = limit.exhausts_at {
            lines.push(format!("Runs dry around {}", fmt::clock(exhausts, now)));
        }
    } else if let Some(remaining) = limit.time_remaining(now) {
        lines.push(format!("Resets in {}", fmt::duration(remaining)));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{LimitKind, LimitSnapshot};
    use chrono::Duration;

    fn snap(limit: LimitSnapshot) -> RunwaySnapshot {
        let mut s = RunwaySnapshot::placeholder();
        s.limits = vec![limit];
        s
    }

    fn base(now: DateTime<Utc>) -> LimitSnapshot {
        LimitSnapshot {
            kind: LimitKind::Session,
            label: "5-hour session".into(),
            percent: 42.0,
            resets_at: Some(now + Duration::hours(3)),
            is_active: true,
            pace_ratio: None,
            exhausts_at: None,
            allowance_percent_per_hour: None,
            allowance_tokens_per_hour: None,
            remaining_tokens: None,
            remaining_value_usd: None,
            calibration: None,
        }
    }

    #[test]
    fn each_style_renders_its_own_number() {
        let now = Utc::now();
        let mut l = base(now);
        l.pace_ratio = Some(1.84);
        l.allowance_tokens_per_hour = Some(418_000.0);
        l.exhausts_at = Some(now + Duration::minutes(134));
        let s = snap(l);

        assert_eq!(menu_bar_text(&s, MenuBarStyle::PaceRatio, now), "1.8\u{d7}");
        assert_eq!(menu_bar_text(&s, MenuBarStyle::Allowance, now), "418K/h");
        assert_eq!(menu_bar_text(&s, MenuBarStyle::Percent, now), "42%");
        assert_eq!(menu_bar_text(&s, MenuBarStyle::TimeLeft, now), "2h 14m");
    }

    /// Before calibration there is no pace and no token allowance. Showing a
    /// fabricated number would be worse than showing the plain percentage.
    #[test]
    fn uncalibrated_styles_degrade_to_percent() {
        let now = Utc::now();
        let s = snap(base(now));
        assert_eq!(menu_bar_text(&s, MenuBarStyle::PaceRatio, now), "42%");
        assert_eq!(menu_bar_text(&s, MenuBarStyle::Allowance, now), "42%");
    }

    /// The percent-per-hour allowance lands as soon as there's a reset time,
    /// well before the token figure calibrates — so it's the better fallback
    /// while it exists.
    #[test]
    fn allowance_prefers_tokens_then_percent_then_fullness() {
        let now = Utc::now();
        let mut l = base(now);
        l.allowance_percent_per_hour = Some(19.33);
        assert_eq!(
            menu_bar_text(&snap(l.clone()), MenuBarStyle::Allowance, now),
            "19%/h"
        );
        l.allowance_tokens_per_hour = Some(418_000.0);
        assert_eq!(
            menu_bar_text(&snap(l), MenuBarStyle::Allowance, now),
            "418K/h"
        );
    }

    #[test]
    fn no_limits_is_an_em_dash_not_a_zero() {
        let now = Utc::now();
        let s = RunwaySnapshot::placeholder();
        for style in [
            MenuBarStyle::PaceRatio,
            MenuBarStyle::Allowance,
            MenuBarStyle::Percent,
            MenuBarStyle::TimeLeft,
        ] {
            assert_eq!(menu_bar_text(&s, style, now), "\u{2014}");
        }
    }

    #[test]
    fn tooltip_says_when_it_runs_dry_early() {
        let now = Utc::now();
        let mut l = base(now);
        l.exhausts_at = Some(now + Duration::hours(1));
        l.pace_ratio = Some(3.0);
        let text = tooltip(&snap(l), now);
        assert!(text.contains("Runs dry around"), "{text}");
        assert!(text.contains("Pace 3.0"), "{text}");
    }

    #[test]
    fn tooltip_says_when_it_resets_otherwise() {
        let now = Utc::now();
        let text = tooltip(&snap(base(now)), now);
        assert!(text.contains("Resets in 3h"), "{text}");
    }
}
