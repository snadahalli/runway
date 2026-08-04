//! Fires at most once per (limit, window instance, rule).
//!
//! The dedupe key includes the window's reset timestamp, so a threshold that
//! fired in this 5-hour window fires again cleanly in the next one without any
//! manual reset — and a limit hovering at 80.1% doesn't spam on every poll.
//!
//! Evaluation happens here; *delivery* is the shell's job, because that's the
//! part that differs per platform. Quiet hours suppress delivery, not
//! evaluation, so nothing is silently lost from the in-app log.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::format::{self as fmt};
use crate::paths;
use crate::settings::Settings;
use crate::snapshot::{LimitSnapshot, RunwaySnapshot};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub key: String,
    pub title: String,
    pub body: String,
    #[serde(with = "crate::snapshot::iso8601")]
    pub date: DateTime<Utc>,
    /// False when quiet hours are in force: recorded, but not shown as a toast.
    pub deliver: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct FiredKeys {
    #[serde(default)]
    keys: Vec<String>,
}

pub struct AlarmEngine {
    fired: Vec<String>,
    /// Where the dedupe set is persisted. `None` keeps it in memory only, which
    /// is what tests want — evaluation writes on every fire, and a test suite
    /// has no business touching the user's real state directory.
    store: Option<std::path::PathBuf>,
    pub recent: VecDeque<Alarm>,
}

const MAX_FIRED_KEYS: usize = 500;
const MAX_RECENT: usize = 25;

impl Default for AlarmEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AlarmEngine {
    pub fn new() -> Self {
        Self::persisting_to(Self::default_path())
    }

    pub fn persisting_to(store: std::path::PathBuf) -> Self {
        let fired = std::fs::read(&store)
            .ok()
            .and_then(|b| serde_json::from_slice::<FiredKeys>(&b).ok())
            .map(|f| f.keys)
            .unwrap_or_default();
        AlarmEngine {
            fired,
            store: Some(store),
            recent: VecDeque::new(),
        }
    }

    /// In-memory only; nothing is written to disk.
    pub fn ephemeral() -> Self {
        AlarmEngine {
            fired: vec![],
            store: None,
            recent: VecDeque::new(),
        }
    }

    pub fn default_path() -> std::path::PathBuf {
        paths::support_dir().join("fired-alarms.json")
    }

    fn mark_fired(&mut self, key: &str) {
        self.fired.push(key.to_string());
        // Bounded so the key set can't grow forever across weeks of windows.
        if self.fired.len() > MAX_FIRED_KEYS {
            let excess = self.fired.len() - MAX_FIRED_KEYS;
            self.fired.drain(0..excess);
        }
        let Some(store) = &self.store else { return };
        if let Ok(bytes) = serde_json::to_vec(&FiredKeys {
            keys: self.fired.clone(),
        }) {
            let _ = paths::write_atomic(store, &bytes);
        }
    }

    fn has_fired(&self, key: &str) -> bool {
        self.fired.iter().any(|k| k == key)
    }

    /// Returns the alarms raised by this snapshot. Only ever call on a `live`
    /// snapshot — an extrapolated percentage crossing 80% is a guess, and an
    /// alarm you can't stand behind is worse than no alarm.
    pub fn evaluate(
        &mut self,
        snapshot: &RunwaySnapshot,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Vec<Alarm> {
        if !settings.alarms_enabled {
            return vec![];
        }

        let mut raised = Vec::new();
        for limit in &snapshot.limits {
            let window_id = limit
                .resets_at
                .map(|r| r.timestamp().to_string())
                .unwrap_or_else(|| "none".to_string());
            let base = format!("{}|{}|{}", limit.kind.raw(), limit.label, window_id);

            self.thresholds(limit, &base, settings, now, &mut raised);
            self.prediction(limit, &base, settings, now, &mut raised);
            self.pace(limit, &base, settings, now, &mut raised);
        }

        for alarm in &raised {
            self.recent.push_front(alarm.clone());
        }
        while self.recent.len() > MAX_RECENT {
            self.recent.pop_back();
        }

        raised
    }

    fn thresholds(
        &mut self,
        limit: &LimitSnapshot,
        base: &str,
        settings: &Settings,
        now: DateTime<Utc>,
        out: &mut Vec<Alarm>,
    ) {
        let mut thresholds = settings.thresholds.clone();
        thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        for threshold in thresholds {
            if limit.percent < threshold {
                continue;
            }
            let key = format!("{base}|threshold|{}", threshold as i64);
            if self.has_fired(&key) {
                continue;
            }
            self.mark_fired(&key);

            let remaining = limit
                .time_remaining(now)
                .map(|r| format!(" · resets in {}", fmt::duration(r)))
                .unwrap_or_default();
            let mut body = format!("{} used{remaining}.", fmt::percent(limit.percent));
            if let Some(allowance) = limit.allowance_tokens_per_hour {
                body.push_str(&format!(
                    " You can still spend {}/h to finish level.",
                    fmt::tokens(allowance)
                ));
            }
            out.push(self.make(
                key,
                format!("{} at {}%", limit.label, threshold as i64),
                body,
                settings,
                now,
            ));
        }
    }

    fn prediction(
        &mut self,
        limit: &LimitSnapshot,
        base: &str,
        settings: &Settings,
        now: DateTime<Utc>,
        out: &mut Vec<Alarm>,
    ) {
        if !settings.predictive_alarms || !limit.runs_dry_early() {
            return;
        }
        let (Some(exhausts), Some(resets)) = (limit.exhausts_at, limit.resets_at) else {
            return;
        };

        let early = (resets - exhausts).num_milliseconds() as f64 / 1000.0;
        let until = (exhausts - now).num_milliseconds() as f64 / 1000.0;
        // Only worth saying when it's both meaningfully early and close enough
        // to act on. Beyond 3 days out the projection isn't trustworthy.
        if early <= 900.0 || until >= 3.0 * 24.0 * 3600.0 {
            return;
        }

        let key = format!("{base}|predict");
        if self.has_fired(&key) {
            return;
        }
        self.mark_fired(&key);

        out.push(self.make(
            key,
            format!("{} will run dry early", limit.label),
            format!(
                "At the current pace you hit 100% around {} — {} before the window resets.",
                fmt::clock(exhausts, now),
                fmt::duration(early)
            ),
            settings,
            now,
        ));
    }

    fn pace(
        &mut self,
        limit: &LimitSnapshot,
        base: &str,
        settings: &Settings,
        now: DateTime<Utc>,
        out: &mut Vec<Alarm>,
    ) {
        let Some(ratio) = limit.pace_ratio else {
            return;
        };
        if ratio < settings.pace_alarm_ratio {
            return;
        }
        // Pointless to warn about pace when there's barely anything left to burn.
        if limit.percent >= 95.0 {
            return;
        }

        let key = format!("{base}|pace");
        if self.has_fired(&key) {
            return;
        }
        self.mark_fired(&key);

        out.push(self.make(
            key,
            format!("Burning {} sustainable pace", fmt::ratio(ratio)),
            format!(
                "{} is at {}. Sustainable from here is {}/h.",
                limit.label,
                fmt::percent(limit.percent),
                fmt::tokens(limit.allowance_tokens_per_hour.unwrap_or(0.0))
            ),
            settings,
            now,
        ));
    }

    fn make(
        &self,
        key: String,
        title: String,
        body: String,
        settings: &Settings,
        now: DateTime<Utc>,
    ) -> Alarm {
        Alarm {
            key,
            title,
            body,
            date: now,
            deliver: !settings.is_quiet_now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{LimitKind, SnapshotHealth};
    use chrono::Duration;

    fn engine() -> AlarmEngine {
        AlarmEngine::ephemeral()
    }

    /// Regression guard: evaluation persists on every fire, and a test run must
    /// not leave anything behind in the user's real state directory.
    #[test]
    fn an_ephemeral_engine_writes_nothing() {
        let path = AlarmEngine::default_path();
        let before = std::fs::read(&path).ok();
        let mut e = AlarmEngine::ephemeral();
        e.evaluate(
            &snap(vec![limit(99.0, 1.0)]),
            &Settings::default(),
            Utc::now(),
        );
        assert_eq!(std::fs::read(&path).ok(), before);
    }

    fn snap(limits: Vec<LimitSnapshot>) -> RunwaySnapshot {
        let mut s = RunwaySnapshot::placeholder();
        s.health = SnapshotHealth::Live;
        s.limits = limits;
        s
    }

    fn limit(percent: f64, resets_in_hours: f64) -> LimitSnapshot {
        LimitSnapshot {
            kind: LimitKind::Session,
            label: "5-hour session".into(),
            percent,
            resets_at: Some(Utc::now() + Duration::milliseconds((resets_in_hours * 3.6e6) as i64)),
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
    fn crossing_a_threshold_fires_exactly_once() {
        let mut e = engine();
        let s = Settings::default();
        let now = Utc::now();

        let first = e.evaluate(&snap(vec![limit(82.0, 2.0)]), &s, now);
        assert_eq!(first.len(), 2, "50 and 80 both crossed: {first:?}");

        // Still at 82% on the next poll: nothing new.
        let second = e.evaluate(&snap(vec![limit(82.5, 2.0)]), &s, now);
        assert!(second.is_empty());

        // Crossing 95 adds one more.
        let third = e.evaluate(&snap(vec![limit(96.0, 2.0)]), &s, now);
        assert_eq!(third.len(), 1);
        assert!(third[0].title.contains("95%"));
    }

    /// The dedupe key carries the reset time, so the next window starts clean
    /// with no manual reset anywhere.
    #[test]
    fn a_new_window_re_arms_the_same_thresholds() {
        let mut e = engine();
        let s = Settings::default();
        let now = Utc::now();

        assert_eq!(e.evaluate(&snap(vec![limit(82.0, 2.0)]), &s, now).len(), 2);
        // Same limit, different reset instant: the window rolled.
        let next_window = limit(82.0, 7.0);
        assert_eq!(e.evaluate(&snap(vec![next_window]), &s, now).len(), 2);
    }

    #[test]
    fn quiet_hours_record_but_do_not_deliver() {
        let mut e = engine();
        let s = Settings {
            quiet_hours_enabled: true,
            quiet_start_hour: 0,
            quiet_end_hour: 24, // always quiet, whatever the local clock says
            ..Default::default()
        };
        let raised = e.evaluate(&snap(vec![limit(82.0, 2.0)]), &s, Utc::now());
        assert!(!raised.is_empty());
        assert!(raised.iter().all(|a| !a.deliver));
        assert_eq!(e.recent.len(), raised.len(), "still logged in-app");
    }

    #[test]
    fn disabled_alarms_evaluate_to_nothing() {
        let mut e = engine();
        let s = Settings {
            alarms_enabled: false,
            ..Default::default()
        };
        assert!(e
            .evaluate(&snap(vec![limit(99.0, 1.0)]), &s, Utc::now())
            .is_empty());
    }

    #[test]
    fn pace_alarm_is_pointless_when_nearly_full() {
        let mut e = engine();
        let s = Settings {
            thresholds: vec![],
            ..Default::default()
        };
        let mut l = limit(97.0, 1.0);
        l.pace_ratio = Some(5.0);
        assert!(e.evaluate(&snap(vec![l]), &s, Utc::now()).is_empty());
    }

    #[test]
    fn prediction_needs_to_be_early_enough_to_matter() {
        let mut e = engine();
        let s = Settings {
            thresholds: vec![],
            ..Default::default()
        };
        let now = Utc::now();

        // Runs dry 5 minutes early: below the 15-minute floor, not worth saying.
        let mut barely = limit(40.0, 2.0);
        barely.exhausts_at = Some(now + Duration::minutes(115));
        assert!(e.evaluate(&snap(vec![barely]), &s, now).is_empty());

        // Runs dry an hour early: worth saying.
        let mut clearly = limit(40.0, 2.0);
        clearly.exhausts_at = Some(now + Duration::minutes(60));
        let raised = e.evaluate(&snap(vec![clearly]), &s, now);
        assert_eq!(raised.len(), 1);
        assert!(raised[0].title.contains("run dry early"));
    }

    /// The dedupe set is the only thing standing between a limit parked at
    /// 80.1% and a notification on every poll, forever. It has to survive a
    /// relaunch, which means actually round-tripping through the file.
    #[test]
    fn the_dedupe_set_survives_a_restart() {
        let store = std::env::temp_dir().join(format!("runway-fired-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&store);
        let settings = Settings::default();
        let now = Utc::now();
        let limit = limit(82.0, 2.0);

        let mut first = AlarmEngine::persisting_to(store.clone());
        assert_eq!(
            first
                .evaluate(&snap(vec![limit.clone()]), &settings, now)
                .len(),
            2
        );

        // A fresh engine reading the same file must not re-fire them.
        let mut second = AlarmEngine::persisting_to(store.clone());
        assert!(second
            .evaluate(&snap(vec![limit]), &settings, now)
            .is_empty());

        let _ = std::fs::remove_file(&store);
    }

    /// Bounded so weeks of rolling windows can't grow the file without limit,
    /// and — the part that would actually hurt — so trimming keeps the *newest*
    /// keys rather than dropping the ones still in force.
    #[test]
    fn the_dedupe_set_is_bounded_and_drops_the_oldest() {
        let mut e = AlarmEngine::ephemeral();
        for i in 0..MAX_FIRED_KEYS + 50 {
            e.mark_fired(&format!("key-{i}"));
        }
        assert_eq!(e.fired.len(), MAX_FIRED_KEYS);
        assert!(!e.has_fired("key-0"), "oldest should have been dropped");
        assert!(
            e.has_fired(&format!("key-{}", MAX_FIRED_KEYS + 49)),
            "newest must be retained"
        );
    }

    #[test]
    fn the_in_app_log_is_bounded_too() {
        let mut e = AlarmEngine::ephemeral();
        let settings = Settings::default();
        // Each new window instance re-arms every threshold, so this generates
        // far more alarms than the log is allowed to keep.
        for hours in 1..40 {
            e.evaluate(
                &snap(vec![limit(99.0, hours as f64)]),
                &settings,
                Utc::now(),
            );
        }
        assert_eq!(e.recent.len(), MAX_RECENT);
    }

    #[test]
    fn predictive_alarms_can_be_turned_off() {
        let mut e = engine();
        let now = Utc::now();
        let s = Settings {
            thresholds: vec![],
            predictive_alarms: false,
            ..Default::default()
        };
        let mut l = limit(40.0, 2.0);
        l.exhausts_at = Some(now + Duration::minutes(30));
        assert!(l.runs_dry_early());
        assert!(e.evaluate(&snap(vec![l]), &s, now).is_empty());
    }

    /// Every limit is evaluated, not just the headline one — a weekly limit in
    /// trouble must still fire while the session limit is the one on display.
    #[test]
    fn each_limit_is_evaluated_independently() {
        let mut e = engine();
        let s = Settings {
            thresholds: vec![80.0],
            ..Default::default()
        };
        let session = limit(85.0, 2.0);
        let weekly = LimitSnapshot {
            kind: LimitKind::WeeklyAll,
            label: "Weekly · all models".into(),
            ..limit(90.0, 40.0)
        };
        let raised = e.evaluate(&snap(vec![session, weekly]), &s, Utc::now());
        assert_eq!(raised.len(), 2);
        let titles: Vec<&str> = raised.iter().map(|a| a.title.as_str()).collect();
        assert!(titles.iter().any(|t| t.contains("5-hour session")));
        assert!(titles.iter().any(|t| t.contains("Weekly")));
    }

    /// The pace rule is `>=`, so a limit sitting exactly on the configured
    /// ratio should fire rather than hover one hair below it forever.
    #[test]
    fn pace_alarm_fires_exactly_at_the_configured_ratio() {
        let s = Settings {
            thresholds: vec![],
            pace_alarm_ratio: 2.0,
            ..Default::default()
        };
        for (ratio, expected) in [(1.99, 0), (2.0, 1), (2.5, 1)] {
            let mut e = engine();
            let mut l = limit(40.0, 2.0);
            l.pace_ratio = Some(ratio);
            assert_eq!(
                e.evaluate(&snap(vec![l]), &s, Utc::now()).len(),
                expected,
                "ratio {ratio}"
            );
        }
    }

    /// Thresholds fire lowest-first, so the log reads in the order things
    /// actually happened even when several are crossed in one reading.
    #[test]
    fn thresholds_fire_in_ascending_order() {
        let mut e = engine();
        let s = Settings {
            thresholds: vec![95.0, 50.0, 80.0],
            ..Default::default()
        };
        let raised = e.evaluate(&snap(vec![limit(96.0, 1.0)]), &s, Utc::now());
        let order: Vec<String> = raised.iter().map(|a| a.title.clone()).collect();
        assert_eq!(order.len(), 3);
        assert!(order[0].contains("50%"), "{order:?}");
        assert!(order[1].contains("80%"), "{order:?}");
        assert!(order[2].contains("95%"), "{order:?}");
    }

    /// The body is the whole value of a threshold alarm — "82% used" alone
    /// tells you nothing you couldn't see.
    #[test]
    fn threshold_bodies_carry_the_actionable_numbers() {
        let mut e = engine();
        let mut l = limit(82.0, 2.0);
        l.allowance_tokens_per_hour = Some(418_000.0);
        let raised = e.evaluate(
            &snap(vec![l]),
            &Settings {
                thresholds: vec![80.0],
                ..Default::default()
            },
            Utc::now(),
        );
        let body = &raised[0].body;
        assert!(body.contains("82% used"), "{body}");
        // Not "2h": a sliver of the window has already elapsed by the time the
        // body is built, so it formats as 1h 59m. Asserting the exact string
        // would fail on wall-clock jitter rather than on anything real.
        assert!(body.contains("resets in 1h"), "{body}");
        assert!(body.contains("418K/h"), "{body}");
    }

    #[test]
    fn prediction_beyond_three_days_is_not_trustworthy() {
        let mut e = engine();
        let s = Settings {
            thresholds: vec![],
            ..Default::default()
        };
        let now = Utc::now();
        let mut l = LimitSnapshot {
            kind: LimitKind::WeeklyAll,
            ..limit(10.0, 24.0 * 7.0)
        };
        l.exhausts_at = Some(now + Duration::days(4));
        assert!(e.evaluate(&snap(vec![l]), &s, now).is_empty());
    }
}
