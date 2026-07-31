//! Compact formatting shared by every surface.
//!
//! Lives in the core rather than the UI so the tray label, the popover and the
//! CLI all round the same way — "418K" must not become "0.4M"
//! depending on which surface drew it.

use chrono::{DateTime, Local, Utc};

pub fn tokens(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1e9 {
        format!("{:.2}B", value / 1e9)
    } else if abs >= 1e6 {
        format!("{:.1}M", value / 1e6)
    } else if abs >= 10_000.0 {
        format!("{:.0}K", value / 1e3)
    } else if abs >= 1000.0 {
        format!("{:.1}K", value / 1e3)
    } else {
        format!("{value:.0}")
    }
}

pub fn usd(value: f64) -> String {
    if value >= 1000.0 {
        format!("${value:.0}")
    } else if value >= 10.0 {
        format!("${value:.1}")
    } else {
        format!("${value:.2}")
    }
}

pub fn percent(value: f64) -> String {
    if value >= 10.0 {
        format!("{value:.0}%")
    } else {
        format!("{value:.1}%")
    }
}

pub fn ratio(value: f64) -> String {
    if value >= 10.0 {
        format!("{value:.0}\u{d7}")
    } else {
        format!("{value:.1}\u{d7}")
    }
}

/// "2h 14m", "48m", "31s" — compact, no leading zeros, never negative.
pub fn duration(seconds: f64) -> String {
    let total = seconds.max(0.0) as i64;
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let minutes = (total % 3600) / 60;
    if days > 0 {
        return if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        };
    }
    if hours > 0 {
        return if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        };
    }
    if minutes > 0 {
        return format!("{minutes}m");
    }
    format!("{total}s")
}

/// "14:20", "tomorrow 09:05", "Thu 09:05" — a wall clock the user can act on,
/// rather than a countdown they have to mentally add to the current time.
///
/// Rendered in local time; the snapshot itself is always UTC.
pub fn clock(date: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let date = date.with_timezone(&Local);
    let now = now.with_timezone(&Local);
    let time = date.format("%H:%M").to_string();

    let day = date.date_naive();
    let today = now.date_naive();
    if day == today {
        return time;
    }
    if day == today.succ_opt().unwrap_or(today) {
        return format!("tomorrow {time}");
    }
    format!("{} {}", date.format("%a"), time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn token_scales() {
        assert_eq!(tokens(999.0), "999");
        assert_eq!(tokens(1500.0), "1.5K");
        assert_eq!(tokens(418_000.0), "418K");
        assert_eq!(tokens(2_400_000.0), "2.4M");
        assert_eq!(tokens(1_250_000_000.0), "1.25B");
    }

    /// The 10K boundary switches from one decimal to none; both sides must stay
    /// on their own side of it.
    #[test]
    fn token_boundaries() {
        assert_eq!(tokens(9_999.0), "10.0K");
        assert_eq!(tokens(10_000.0), "10K");
    }

    #[test]
    fn dollars_lose_precision_as_they_grow() {
        assert_eq!(usd(4.256), "$4.26");
        assert_eq!(usd(31.4), "$31.4");
        assert_eq!(usd(1234.5), "$1234");
    }

    #[test]
    fn percentages_keep_a_decimal_only_when_small() {
        assert_eq!(percent(4.26), "4.3%");
        assert_eq!(percent(41.5), "42%");
    }

    #[test]
    fn durations_are_compact_and_never_negative() {
        assert_eq!(duration(-5.0), "0s");
        assert_eq!(duration(31.0), "31s");
        assert_eq!(duration(2880.0), "48m");
        assert_eq!(duration(8040.0), "2h 14m");
        assert_eq!(duration(7200.0), "2h");
        assert_eq!(duration(194_400.0), "2d 6h");
    }

    #[test]
    fn clock_labels_the_day_when_it_is_not_today() {
        let now = Utc::now();
        assert!(!clock(now + Duration::minutes(30), now).contains(' '));
        let far = clock(now + Duration::days(3), now);
        assert!(far.contains(' '), "expected a weekday prefix, got {far}");
    }
}
