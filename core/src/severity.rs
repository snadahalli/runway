//! Runway's severity rule.
//!
//! Deliberately not "percentage used" alone. A limit at 60% with eight hours
//! left is fine; the same 60% with forty minutes left and a 3x pace is not. So
//! severity is the worse of two independent readings — how full it is, and how
//! fast it's filling — with the pace reading damped early in a window, where a
//! couple of heavy minutes would otherwise project absurd slopes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::snapshot::LimitSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Calm,
    Watch,
    Tight,
}

impl Severity {
    /// The colours the Swift app used, so the two look identical side by side.
    pub fn hex(&self) -> &'static str {
        match self {
            Severity::Calm => "#3dad73",
            Severity::Watch => "#e0a12e",
            Severity::Tight => "#d94d47",
        }
    }

    pub fn of(limit: &LimitSnapshot, now: DateTime<Utc>) -> Severity {
        let mut level = if limit.percent >= 90.0 {
            Severity::Tight
        } else if limit.percent >= 70.0 {
            Severity::Watch
        } else {
            Severity::Calm
        };

        // Pace only earns a vote once there's enough of the window behind us for
        // the slope to mean anything.
        let elapsed_fraction = match limit.time_remaining(now) {
            Some(remaining) => 1.0 - (remaining / limit.window_seconds()).min(1.0),
            None => 1.0,
        };

        if elapsed_fraction > 0.15 || limit.percent >= 15.0 {
            if let Some(ratio) = limit.pace_ratio {
                let pace = if ratio >= 2.0 {
                    Severity::Tight
                } else if ratio >= 1.15 {
                    Severity::Watch
                } else {
                    Severity::Calm
                };
                level = level.max(pace);
            }
        }

        level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::LimitKind;
    use chrono::Duration;

    fn limit(percent: f64, pace: Option<f64>, remaining_hours: f64) -> LimitSnapshot {
        LimitSnapshot {
            kind: LimitKind::Session,
            label: "5-hour session".into(),
            percent,
            resets_at: Some(Utc::now() + Duration::milliseconds((remaining_hours * 3.6e6) as i64)),
            is_active: true,
            pace_ratio: pace,
            exhausts_at: None,
            allowance_percent_per_hour: None,
            allowance_tokens_per_hour: None,
            remaining_tokens: None,
            remaining_value_usd: None,
        }
    }

    #[test]
    fn fullness_alone_can_raise_severity() {
        assert_eq!(
            Severity::of(&limit(50.0, None, 4.0), Utc::now()),
            Severity::Calm
        );
        assert_eq!(
            Severity::of(&limit(75.0, None, 4.0), Utc::now()),
            Severity::Watch
        );
        assert_eq!(
            Severity::of(&limit(95.0, None, 4.0), Utc::now()),
            Severity::Tight
        );
    }

    /// The rule the product is built on: not-very-full but burning fast is not calm.
    #[test]
    fn pace_alone_can_raise_severity() {
        assert_eq!(
            Severity::of(&limit(30.0, Some(2.5), 2.0), Utc::now()),
            Severity::Tight
        );
        assert_eq!(
            Severity::of(&limit(30.0, Some(1.2), 2.0), Utc::now()),
            Severity::Watch
        );
    }

    /// Twenty minutes into a five-hour window, a couple of heavy minutes project
    /// an absurd slope. Below 15% elapsed and 15% used, pace gets no vote.
    #[test]
    fn pace_is_damped_early_in_a_window() {
        let fresh = limit(3.0, Some(4.3), 4.9);
        assert_eq!(Severity::of(&fresh, Utc::now()), Severity::Calm);
    }

    #[test]
    fn pace_counts_once_enough_has_been_spent() {
        // Same early moment in the window, but 20% already used: the slope is
        // now backed by real volume, so it counts.
        let spent = limit(20.0, Some(4.3), 4.9);
        assert_eq!(Severity::of(&spent, Utc::now()), Severity::Tight);
    }

    #[test]
    fn severity_is_the_worse_of_the_two_readings() {
        // Full but perfectly paced stays at the fullness reading.
        assert_eq!(
            Severity::of(&limit(95.0, Some(0.2), 1.0), Utc::now()),
            Severity::Tight
        );
    }
}
