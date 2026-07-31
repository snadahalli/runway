//! Turns raw percentages into the numbers the UI is built around: a pace ratio,
//! a projected run-dry moment, and — the part nobody else does — an allowance
//! expressed in tokens and dollars rather than opaque percentage points.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::activity::ActivityProfile;
use crate::snapshot::{LimitKind, LimitSnapshot};
use crate::transcript::{self, UsageRecord};

/// One observation of a single limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageSample {
    #[serde(with = "crate::compat")]
    pub date: DateTime<Utc>,
    pub percent: f64,
    #[serde(
        rename = "resetsAt",
        with = "crate::compat::option",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub resets_at: Option<DateTime<Utc>>,
}

/// Per-limit history, persisted so projections survive a relaunch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SampleHistory {
    #[serde(default)]
    pub series: HashMap<String, Vec<UsageSample>>,
}

pub const HISTORY_RETENTION_SECONDS: i64 = 14 * 24 * 3600;
pub const MAX_PER_SERIES: usize = 2000;

impl SampleHistory {
    pub fn record(&mut self, key: &str, sample: UsageSample) {
        let list = self.series.entry(key.to_string()).or_default();

        // Skip duplicates — the API only moves every few minutes.
        if let Some(last) = list.last() {
            if (last.percent - sample.percent).abs() < 0.001
                && last.resets_at == sample.resets_at
                && (sample.date - last.date).num_seconds() < 60
            {
                return;
            }
        }

        list.push(sample);
        let cutoff = Utc::now() - Duration::seconds(HISTORY_RETENTION_SECONDS);
        list.retain(|s| s.date >= cutoff);
        if list.len() > MAX_PER_SERIES {
            list.drain(0..list.len() - MAX_PER_SERIES);
        }
    }

    /// Samples belonging to the window instance currently in flight. A window
    /// instance is identified by its reset time — when that changes, the window
    /// rolled over and the old samples must not contaminate the new slope.
    pub fn current_window(&self, key: &str, resets_at: Option<DateTime<Utc>>) -> Vec<UsageSample> {
        let Some(list) = self.series.get(key) else {
            return vec![];
        };
        let Some(resets_at) = resets_at else {
            return list.clone();
        };
        list.iter()
            .filter(|s| match s.resets_at {
                Some(r) => (r - resets_at).num_seconds().abs() < 300,
                None => false,
            })
            .cloned()
            .collect()
    }
}

pub struct Calibration {
    pub tokens_per_percent: f64,
    pub dollars_per_percent: f64,
}

/// Least-squares slope in percentage points per **calendar** hour.
///
/// No longer drives pace or run-dry — those are activity-weighted now, see
/// [`snapshot`]. Kept because "how fast is this moving right now" is still a
/// real question, and the sparkline's slope should have a number attached.
pub fn burn_rate(samples: &[UsageSample]) -> Option<f64> {
    if samples.len() < 3 {
        return None;
    }
    let first = samples.first()?;
    let last = samples.last()?;
    let span = (last.date - first.date).num_milliseconds() as f64 / 1000.0;
    if span < 600.0 {
        return None; // < 10 minutes of history says nothing
    }

    let origin_hours = first.date.timestamp_millis() as f64 / 3.6e6;
    let (mut sum_x, mut sum_y, mut sum_xy, mut sum_xx) = (0.0, 0.0, 0.0, 0.0);
    for sample in samples {
        let x = sample.date.timestamp_millis() as f64 / 3.6e6 - origin_hours;
        let y = sample.percent;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_xx += x * x;
    }
    let n = samples.len() as f64;
    let denominator = n * sum_xx - sum_x * sum_x;
    if denominator.abs() <= 1e-9 {
        return None;
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denominator;
    Some(slope.max(0.0))
}

/// How many tokens (and dollars) one percentage point of a limit represents.
///
/// Derived by pairing consecutive API observations with the local token volume
/// recorded in between, then taking the median ratio — median rather than mean
/// because a single mis-aligned pair would otherwise dominate.
pub fn calibrate(samples: &[UsageSample], records: &[UsageRecord]) -> Option<Calibration> {
    if samples.len() < 2 {
        return None;
    }

    let mut token_ratios: Vec<f64> = Vec::new();
    let mut dollar_ratios: Vec<f64> = Vec::new();

    for pair in samples.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        let delta = current.percent - previous.percent;
        // Ignore noise and roll-offs: the 5-hour window's percentage falls as
        // old requests age out, and a negative delta would invert the ratio.
        if delta < 0.5 {
            continue;
        }

        let in_window = transcript::within(records, previous.date, current.date);
        if in_window.is_empty() {
            continue;
        }

        let tokens = transcript::total_tokens(&in_window).fresh() as f64;
        let dollars = transcript::total_cost(&in_window);
        if tokens <= 0.0 {
            continue;
        }

        token_ratios.push(tokens / delta);
        dollar_ratios.push(dollars / delta);
    }

    if token_ratios.len() < 3 {
        return None;
    }
    Some(Calibration {
        tokens_per_percent: median(&token_ratios),
        dollars_per_percent: median(&dollar_ratios),
    })
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

/// Below this much of the window's expected work elapsed, a pace ratio is
/// arithmetic on noise — dividing by a near-zero denominator.
const MIN_EXPECTED_FRACTION: f64 = 0.02;

/// Assembles the full derived picture for one limit.
///
/// Windows are measured in **expected work** rather than elapsed time, using
/// the caller's [`ActivityProfile`]. For a uniform profile every formula here
/// reduces exactly to the calendar-time version it replaced.
#[allow(clippy::too_many_arguments)]
pub fn snapshot(
    kind: LimitKind,
    label: String,
    percent: f64,
    resets_at: Option<DateTime<Utc>>,
    is_active: bool,
    history: &[UsageSample],
    records: &[UsageRecord],
    profile: &ActivityProfile,
    now: DateTime<Utc>,
) -> LimitSnapshot {
    let mut snapshot = LimitSnapshot {
        kind,
        label,
        percent,
        resets_at,
        is_active,
        pace_ratio: None,
        exhausts_at: None,
        allowance_percent_per_hour: None,
        allowance_tokens_per_hour: None,
        remaining_tokens: None,
        remaining_value_usd: None,
    };

    let remaining_percent = (100.0 - percent).max(0.0);

    if let Some(resets_at) = resets_at {
        let window_start = resets_at - secs(kind.window_seconds());

        // Allowance: spend this fast per working hour from now on and you land
        // exactly at 100% the moment the window resets. Everything else is
        // measured against it.
        let active_hours = profile.active_hours_between(now, resets_at);
        if active_hours > 0.01 {
            snapshot.allowance_percent_per_hour = Some(remaining_percent / active_hours);
        }

        // Pace: how much of the limit you've spent, against how much of this
        // window's work you'd normally have done by now. 1.0 is on schedule.
        //
        // This replaces a least-squares slope, and is better in two ways: it
        // needs no minimum sample span, so it's honest from the very first
        // reading; and it can't mistake a burst during working hours for a rate
        // sustained through the night.
        let total_work = profile.work_between(window_start, resets_at);
        let done_work = profile.work_between(window_start, now);
        if total_work > 0.0 {
            let expected = (done_work / total_work).clamp(0.0, 1.0);
            let actual = percent / 100.0;
            if expected >= MIN_EXPECTED_FRACTION {
                snapshot.pace_ratio = Some(actual / expected);
            }

            // Run dry: keep spending per unit of expected work at the rate
            // you've established, and this is when you hit 100%. Walking the
            // profile forward means the answer lands in a working hour rather
            // than at 3am on a Sunday.
            if actual > 0.0 && done_work > 0.0 {
                let target = done_work / actual;
                if target < total_work {
                    // `target < total_work` means it's reached before the reset,
                    // so walking as far as the reset is always enough.
                    let horizon = (resets_at - window_start) + Duration::hours(1);
                    if let Some(at) = profile.time_to_accumulate(window_start, target, horizon) {
                        snapshot.exhausts_at = Some(at.max(now));
                    }
                }
            }
        }
    }

    if let Some(calibration) = calibrate(history, records) {
        snapshot.remaining_tokens = Some(remaining_percent * calibration.tokens_per_percent);
        snapshot.remaining_value_usd = Some(remaining_percent * calibration.dollars_per_percent);
        if let Some(allowance) = snapshot.allowance_percent_per_hour {
            snapshot.allowance_tokens_per_hour = Some(allowance * calibration.tokens_per_percent);
        }
    }

    snapshot
}

fn secs(seconds: f64) -> Duration {
    Duration::milliseconds((seconds * 1000.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::TokenTotals;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-31T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(minutes)
    }

    fn sample(minutes: i64, percent: f64) -> UsageSample {
        UsageSample {
            date: t(minutes),
            percent,
            resets_at: Some(t(300)),
        }
    }

    fn record(minutes: i64, id: &str, fresh_tokens: i64) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            date: t(minutes),
            model: "claude-opus-5".into(),
            project: "p".into(),
            project_path: "/p".into(),
            session_id: "s".into(),
            tokens: TokenTotals {
                input: fresh_tokens,
                ..Default::default()
            },
        }
    }

    #[test]
    fn burn_rate_needs_three_samples_and_ten_minutes() {
        assert!(burn_rate(&[sample(0, 0.0), sample(30, 10.0)]).is_none());
        // Three samples but only 5 minutes of span: still not enough.
        assert!(burn_rate(&[sample(0, 0.0), sample(2, 1.0), sample(5, 2.0)]).is_none());
        assert!(burn_rate(&[sample(0, 0.0), sample(30, 10.0), sample(60, 20.0)]).is_some());
    }

    #[test]
    fn burn_rate_is_percent_per_hour() {
        // 20 points over two hours = 10 points/hour.
        let rate = burn_rate(&[sample(0, 0.0), sample(60, 10.0), sample(120, 20.0)]).unwrap();
        assert!((rate - 10.0).abs() < 1e-6, "got {rate}");
    }

    /// The 5-hour window rolls, so its percentage falls as well as rises. A
    /// negative slope must clamp to zero rather than projecting a limit that
    /// refills forever.
    #[test]
    fn burn_rate_never_goes_negative() {
        let rate = burn_rate(&[sample(0, 50.0), sample(60, 30.0), sample(120, 10.0)]).unwrap();
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn calibration_needs_three_usable_pairs() {
        let samples = vec![sample(0, 0.0), sample(30, 10.0), sample(60, 20.0)];
        let records = vec![record(10, "a", 1000), record(40, "b", 1000)];
        // Only two pairs exist, so there can be at most two ratios.
        assert!(calibrate(&samples, &records).is_none());
    }

    #[test]
    fn calibration_takes_the_median_ratio() {
        let samples = vec![
            sample(0, 0.0),
            sample(30, 10.0),
            sample(60, 20.0),
            sample(90, 30.0),
        ];
        // 10 points per interval. Middle interval is a wild outlier; the median
        // must ignore it rather than let it set the whole calibration.
        let records = vec![
            record(15, "a", 10_000),
            record(45, "b", 900_000),
            record(75, "c", 10_000),
        ];
        let c = calibrate(&samples, &records).unwrap();
        assert!(
            (c.tokens_per_percent - 1000.0).abs() < 1e-6,
            "got {}",
            c.tokens_per_percent
        );
    }

    #[test]
    fn calibration_ignores_intervals_where_the_window_rolled_back() {
        let samples = vec![
            sample(0, 40.0),
            sample(30, 35.0), // rolled off, negative delta
            sample(60, 45.0),
            sample(90, 55.0),
            sample(120, 65.0),
        ];
        let records: Vec<UsageRecord> = (0..5)
            .map(|i| record(i * 30 + 5, &format!("r{i}"), 10_000))
            .collect();
        let c = calibrate(&samples, &records).unwrap();
        // Three valid +10 pairs at 10k tokens each => 1000 tokens per point.
        assert!((c.tokens_per_percent - 1000.0).abs() < 1e-6);
    }

    fn flat() -> ActivityProfile {
        ActivityProfile::uniform()
    }

    /// With a uniform profile every formula must reproduce the calendar-time
    /// behaviour it replaced. This is the safety net for the whole change.
    #[test]
    fn allowance_lands_exactly_at_reset() {
        // 60% used, 4 hours left => 10 points/hour lands on 100 at reset.
        let s = snapshot(
            LimitKind::Session,
            "5-hour session".into(),
            60.0,
            Some(t(240)),
            true,
            &[],
            &[],
            &flat(),
            t(0),
        );
        assert!((s.allowance_percent_per_hour.unwrap() - 10.0).abs() < 1e-4);
    }

    #[test]
    fn pace_is_spent_over_expected_by_now() {
        // A 5-hour window, 2 hours in: 40% of the window elapsed. Having spent
        // 80% is exactly 2x pace — and it needs no sample history at all.
        let s = snapshot(
            LimitKind::Session,
            "5-hour session".into(),
            80.0,
            Some(t(180)),
            true,
            &[],
            &[],
            &flat(),
            t(0),
        );
        assert!(
            (s.pace_ratio.unwrap() - 2.0).abs() < 1e-3,
            "got {:?}",
            s.pace_ratio
        );
        assert!(s.runs_dry_early());
    }

    /// The old model needed 3 samples and 10 minutes before it would say
    /// anything. This one is honest immediately, which is the whole point.
    #[test]
    fn pace_needs_no_sample_history() {
        let s = snapshot(
            LimitKind::Session,
            "5-hour session".into(),
            25.0,
            Some(t(150)),
            true,
            &[],
            &[],
            &flat(),
            t(0),
        );
        assert!(s.pace_ratio.is_some());
        assert!((s.pace_ratio.unwrap() - 0.5).abs() < 1e-3);
        assert!(!s.runs_dry_early(), "half pace lands under 100 at reset");
    }

    #[test]
    fn pace_is_withheld_at_the_very_start_of_a_window() {
        // One minute into a 7-day window the denominator is noise.
        let s = snapshot(
            LimitKind::WeeklyAll,
            "Weekly".into(),
            0.2,
            Some(t(7 * 24 * 60 - 1)),
            true,
            &[],
            &[],
            &flat(),
            t(0),
        );
        assert!(s.pace_ratio.is_none());
    }

    #[test]
    fn no_reset_time_means_no_projection_at_all() {
        let s = snapshot(
            LimitKind::Other,
            "x".into(),
            10.0,
            None,
            true,
            &[],
            &[],
            &flat(),
            t(0),
        );
        assert!(s.allowance_percent_per_hour.is_none());
        assert!(s.pace_ratio.is_none());
        assert!(!s.runs_dry_early());
    }

    /// The regression this whole model exists to fix: a 9-to-5 user one day into
    /// a weekly window. Calendar time says they're behind schedule and fine;
    /// their own rhythm says a third of the week's work is already done.
    #[test]
    fn a_working_rhythm_changes_the_verdict() {
        use crate::pricing::TokenTotals;
        use chrono::TimeZone;

        let mut records = Vec::new();
        for weekday in 0..5u32 {
            for hour in 9..17u32 {
                let local = chrono::Local
                    .with_ymd_and_hms(2026, 7, 6 + weekday, hour, 30, 0)
                    .single()
                    .unwrap();
                records.push(UsageRecord {
                    id: format!("{weekday}-{hour}"),
                    date: local.with_timezone(&Utc),
                    model: "claude-opus-5".into(),
                    project: "p".into(),
                    project_path: "/p".into(),
                    session_id: "s".into(),
                    tokens: TokenTotals {
                        input: 1000,
                        ..Default::default()
                    },
                });
            }
        }

        // Weekly window running Monday 09:00 -> the following Monday 09:00.
        // It's now Tuesday 17:00: a third of the calendar week gone, but two of
        // five working days already behind us.
        let now = chrono::Local
            .with_ymd_and_hms(2026, 7, 14, 17, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let reset = chrono::Local
            .with_ymd_and_hms(2026, 7, 20, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let learned = ActivityProfile::learn(&records, now);
        assert!(learned.learned);

        let args = |p: &ActivityProfile| {
            snapshot(
                LimitKind::WeeklyAll,
                "Weekly".into(),
                10.0,
                Some(reset),
                true,
                &[],
                &[],
                p,
                now,
            )
        };

        let calendar = args(&flat());
        let weighted = args(&learned);

        // Calendar: 32 of 168 hours gone (19%), 10% spent => 0.52x.
        assert!(
            (calendar.pace_ratio.unwrap() - 0.52).abs() < 0.05,
            "{:?}",
            calendar.pace_ratio
        );
        // Rhythm-aware: two of five working days are done (~40% of the week's
        // actual work), so the same 10% is a good deal further under pace.
        assert!(
            weighted.pace_ratio.unwrap() < calendar.pace_ratio.unwrap() * 0.75,
            "weighted {:?} should be well under calendar {:?}",
            weighted.pace_ratio,
            calendar.pace_ratio
        );
        // And the allowance per working hour is far larger than per calendar hour.
        assert!(
            weighted.allowance_percent_per_hour.unwrap()
                > calendar.allowance_percent_per_hour.unwrap() * 2.0
        );
    }

    #[test]
    fn history_dedupes_repeat_readings() {
        let mut h = SampleHistory::default();
        h.record("k", sample(0, 10.0));
        h.record("k", sample(0, 10.0));
        assert_eq!(h.series["k"].len(), 1);
        // A real move is recorded even seconds later.
        h.record("k", sample(0, 11.0));
        assert_eq!(h.series["k"].len(), 2);
    }

    /// When a window rolls over its reset time changes, and the previous
    /// instance's samples must not contaminate the new slope.
    #[test]
    fn current_window_isolates_the_live_instance() {
        let mut h = SampleHistory::default();
        h.record(
            "k",
            UsageSample {
                date: t(0),
                percent: 90.0,
                resets_at: Some(t(60)),
            },
        );
        h.record(
            "k",
            UsageSample {
                date: t(70),
                percent: 5.0,
                resets_at: Some(t(360)),
            },
        );
        h.record(
            "k",
            UsageSample {
                date: t(100),
                percent: 9.0,
                resets_at: Some(t(360)),
            },
        );
        let live = h.current_window("k", Some(t(360)));
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].percent, 5.0);
    }
}
