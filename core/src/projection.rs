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

/// How many tokens (and dollars) one percentage point of a limit represents.
pub struct Calibration {
    pub tokens_per_percent: f64,
    pub dollars_per_percent: f64,
    pub quality: Quality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// A single aggregate ratio over whatever this window has moved so far.
    /// Available within minutes and worth roughly its order of magnitude.
    Provisional,
    /// Median of enough independent observations to reject outliers.
    Calibrated,
}

/// One observation: local token volume against the percentage points it moved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ratio {
    #[serde(with = "crate::compat")]
    pub at: DateTime<Utc>,
    pub tokens: f64,
    pub dollars: f64,
    /// How long the two readings were apart. Zero means the observation predates
    /// this field, in which case its provenance is unknown and it is ignored —
    /// the old code recorded pairs spanning hours, and those are exactly the
    /// ones that need discarding.
    #[serde(default)]
    pub interval_seconds: f64,
}

/// Accumulated calibration observations, per limit.
///
/// These used to be recomputed from the live window on every rebuild, which had
/// two consequences worth fixing: a 5-hour window rollover threw the estimate
/// away and started again from three samples, and a median over three samples
/// moved by 2x between polls with nothing else changing. Ratios are now recorded
/// once as they are observed and retained across windows, so the median is over
/// a couple of dozen observations and stops lurching.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Calibrator {
    #[serde(default)]
    pub ratios: HashMap<String, Vec<Ratio>>,
}

/// Enough observations to trust the median to reject an outlier.
pub const MIN_RATIOS: usize = 5;
/// Beyond this the oldest are dropped; a limit's token value does drift as
/// pricing and model mix change, so this is a moving picture, not a total.
pub const MAX_RATIOS: usize = 32;
pub const RATIO_RETENTION_DAYS: i64 = 21;
/// Below this a percentage change is mostly rounding, and dividing by it
/// produces a wild ratio.
const MIN_DELTA: f64 = 0.5;

/// Above this, the two readings are too far apart to relate to each other.
///
/// `MIN_DELTA` guarded the numerator and nothing guarded the interval, so a
/// reading taken after the app had been closed overnight was compared against
/// one from fourteen hours earlier — summing every token logged in between
/// against whatever the percentage happened to have moved, across several
/// window rollovers. On real data every wild observation followed a long gap:
/// normal 3-15 minute intervals produced 2,000-18,000 tokens/percent, while
/// gaps of 40 minutes to 14 hours produced 141,000-197,000. Generous against
/// the 180s poll floor, so an ordinary late poll still counts.
const MAX_INTERVAL_SECONDS: f64 = 900.0;

impl Calibrator {
    /// Record what happened between two consecutive readings of one limit.
    pub fn observe(
        &mut self,
        key: &str,
        previous: &UsageSample,
        current: &UsageSample,
        records: &[UsageRecord],
    ) {
        let delta = current.percent - previous.percent;
        // The 5-hour window's percentage falls as old requests age out, and a
        // negative delta would invert the ratio.
        if delta < MIN_DELTA {
            return;
        }
        let interval = (current.date - previous.date).num_milliseconds() as f64 / 1000.0;
        if !(0.0..=MAX_INTERVAL_SECONDS).contains(&interval) {
            return;
        }
        let in_window = transcript::within(records, previous.date, current.date);
        if in_window.is_empty() {
            return;
        }
        let tokens = transcript::total_tokens(&in_window).fresh() as f64;
        if tokens <= 0.0 {
            return;
        }

        let list = self.ratios.entry(key.to_string()).or_default();
        list.push(Ratio {
            at: current.date,
            tokens: tokens / delta,
            dollars: transcript::total_cost(&in_window) / delta,
            interval_seconds: interval,
        });

        let cutoff = Utc::now() - Duration::days(RATIO_RETENTION_DAYS);
        list.retain(|r| r.at >= cutoff);
        if list.len() > MAX_RATIOS {
            let excess = list.len() - MAX_RATIOS;
            list.drain(0..excess);
        }
    }

    /// The precise figure, or `None` until there are enough usable observations.
    ///
    /// Observations from before intervals were recorded are dropped rather than
    /// trusted: the code that wrote them accepted pairs spanning hours, and a
    /// median cannot be relied on to outvote contamination it may be full of.
    pub fn calibration(&self, key: &str) -> Option<Calibration> {
        let usable: Vec<&Ratio> = self
            .ratios
            .get(key)?
            .iter()
            .filter(|r| r.interval_seconds > 0.0 && r.interval_seconds <= MAX_INTERVAL_SECONDS)
            .collect();
        if usable.len() < MIN_RATIOS {
            return None;
        }
        Some(Calibration {
            tokens_per_percent: median(&usable.iter().map(|r| r.tokens).collect::<Vec<_>>()),
            dollars_per_percent: median(&usable.iter().map(|r| r.dollars).collect::<Vec<_>>()),
            quality: Quality::Calibrated,
        })
    }
}

/// A coarse figure from whatever the current window has already moved.
///
/// One aggregate ratio rather than a median of several, so it needs no minimum
/// number of observations and appears within minutes instead of hours. It has
/// no outlier rejection and will be off — but a number that is roughly right and
/// labelled provisional beats an empty field, which is what this replaces.
pub fn bootstrap(samples: &[UsageSample], records: &[UsageRecord]) -> Option<Calibration> {
    let mut tokens = 0.0;
    let mut dollars = 0.0;
    let mut moved = 0.0;

    for pair in samples.windows(2) {
        let delta = pair[1].percent - pair[0].percent;
        if delta <= 0.0 {
            continue;
        }
        let in_window = transcript::within(records, pair[0].date, pair[1].date);
        tokens += transcript::total_tokens(&in_window).fresh() as f64;
        dollars += transcript::total_cost(&in_window);
        moved += delta;
    }

    if moved < MIN_DELTA || tokens <= 0.0 {
        return None;
    }
    Some(Calibration {
        tokens_per_percent: tokens / moved,
        dollars_per_percent: dollars / moved,
        quality: Quality::Provisional,
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
    calibration: Option<Calibration>,
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
        calibration: None,
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

    // The precise figure if there are enough observations, otherwise whatever
    // this window has already shown us — flagged, so the UI can say so.
    if let Some(calibration) = calibration.or_else(|| bootstrap(history, records)) {
        snapshot.remaining_tokens = Some(remaining_percent * calibration.tokens_per_percent);
        snapshot.remaining_value_usd = Some(remaining_percent * calibration.dollars_per_percent);
        if let Some(allowance) = snapshot.allowance_percent_per_hour {
            snapshot.allowance_tokens_per_hour = Some(allowance * calibration.tokens_per_percent);
        }
        snapshot.calibration = Some(calibration.quality);
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

    /// Feed a calibrator a run of readings that each move `delta` percent with
    /// `tokens` of local volume in between.
    fn observed(count: usize, delta: f64, tokens: i64) -> (Calibrator, Vec<UsageRecord>) {
        let mut c = Calibrator::default();
        let mut records = Vec::new();
        let mut samples = Vec::new();
        // Three minutes apart, matching the 180s poll floor.
        for i in 0..=count {
            samples.push(sample((i as i64) * 3, (i as f64) * delta));
            if i > 0 {
                records.push(record((i as i64) * 3 - 1, &format!("r{i}"), tokens));
            }
        }
        for pair in samples.windows(2) {
            c.observe("k", &pair[0], &pair[1], &records);
        }
        (c, records)
    }

    #[test]
    fn calibration_needs_enough_observations() {
        let (c, _) = observed(MIN_RATIOS - 1, 10.0, 10_000);
        assert!(
            c.calibration("k").is_none(),
            "should hold out below the minimum"
        );
        let (c, _) = observed(MIN_RATIOS, 10.0, 10_000);
        let cal = c.calibration("k").expect("should calibrate");
        assert_eq!(cal.quality, Quality::Calibrated);
        assert!((cal.tokens_per_percent - 1000.0).abs() < 1e-6);
    }

    /// Median, not mean — one mis-aligned interval must not drag the estimate.
    #[test]
    fn calibration_rejects_an_outlier() {
        let mut c = Calibrator::default();
        let mut records = Vec::new();
        let mut samples = Vec::new();
        for i in 0..=6 {
            samples.push(sample((i as i64) * 3, (i as f64) * 10.0));
            if i > 0 {
                // The third interval is a hundredfold outlier.
                let tokens = if i == 3 { 1_000_000 } else { 10_000 };
                records.push(record((i as i64) * 3 - 1, &format!("r{i}"), tokens));
            }
        }
        for pair in samples.windows(2) {
            c.observe("k", &pair[0], &pair[1], &records);
        }
        let cal = c.calibration("k").unwrap();
        assert!(
            (cal.tokens_per_percent - 1000.0).abs() < 1e-6,
            "outlier leaked into the median: {}",
            cal.tokens_per_percent
        );
    }

    /// The whole point of accumulating: a 5-hour rollover used to discard the
    /// estimate and start again from three samples.
    #[test]
    fn calibration_survives_a_window_rollover() {
        let (mut c, mut records) = observed(MIN_RATIOS, 10.0, 10_000);
        assert!(c.calibration("k").is_some());
        records.push(record(401, "rollover", 10_000));

        // A new window instance: different reset time, percentages restart.
        let fresh_a = UsageSample {
            date: t(400),
            percent: 2.0,
            resets_at: Some(t(900)),
        };
        let fresh_b = UsageSample {
            date: t(430),
            percent: 12.0,
            resets_at: Some(t(900)),
        };
        c.observe("k", &fresh_a, &fresh_b, &records);

        assert!(
            c.calibration("k").is_some(),
            "the accumulated estimate must outlive the window it came from"
        );
    }

    #[test]
    fn calibration_ignores_roll_offs_and_noise() {
        let mut c = Calibrator::default();
        let records = vec![record(1, "a", 10_000)];
        // A percentage that fell, and one that barely moved.
        c.observe("k", &sample(0, 40.0), &sample(3, 35.0), &records);
        c.observe("k", &sample(0, 40.0), &sample(3, 40.2), &records);
        assert!(c.ratios.get("k").map_or(true, |r| r.is_empty()));
    }

    /// Every wild observation on real data followed a long gap — the app having
    /// been closed, or the machine asleep. Comparing a reading against one from
    /// hours earlier sums every token in between against whatever the
    /// percentage did, across several window rollovers.
    #[test]
    fn a_reading_after_a_long_gap_is_not_an_observation() {
        let mut c = Calibrator::default();
        let records = vec![record(15, "a", 500_000)];

        // Fourteen hours apart, as after an overnight.
        let before = UsageSample {
            date: t(0),
            percent: 10.0,
            resets_at: Some(t(300)),
        };
        let after = UsageSample {
            date: t(14 * 60),
            percent: 13.0,
            resets_at: Some(t(1200)),
        };
        c.observe("k", &before, &after, &records);
        assert!(
            c.ratios.get("k").map_or(true, |r| r.is_empty()),
            "a 14-hour gap must not become an observation"
        );

        // A normal poll interval still counts.
        let close = vec![record(1, "b", 30_000)];
        c.observe("k", &sample(0, 10.0), &sample(3, 13.0), &close);
        assert_eq!(c.ratios["k"].len(), 1);
        assert!(c.ratios["k"][0].interval_seconds > 0.0);
    }

    /// Observations written before intervals were recorded have unknown
    /// provenance, and the code that wrote them accepted hours-long pairs.
    #[test]
    fn observations_without_a_known_interval_are_ignored() {
        let mut c = Calibrator::default();
        let legacy: Vec<Ratio> = (0..MIN_RATIOS + 3)
            .map(|i| Ratio {
                at: t(i as i64),
                tokens: 150_000.0,
                dollars: 20.0,
                interval_seconds: 0.0, // as deserialised from an older file
            })
            .collect();
        c.ratios.insert("k".into(), legacy);
        assert!(
            c.calibration("k").is_none(),
            "legacy observations must not be trusted just because there are enough of them"
        );
    }

    #[test]
    fn calibration_is_bounded() {
        let (c, _) = observed(MAX_RATIOS + 20, 5.0, 5_000);
        assert_eq!(c.ratios["k"].len(), MAX_RATIOS);
    }

    /// Rather than showing nothing for hours, one aggregate ratio over whatever
    /// the window has already moved — clearly labelled as provisional.
    #[test]
    fn bootstrap_fills_in_before_calibration() {
        let samples = vec![sample(0, 0.0), sample(3, 10.0)];
        let records = vec![record(1, "a", 10_000)];
        // Not enough for the precise path.
        let mut c = Calibrator::default();
        c.observe("k", &samples[0], &samples[1], &records);
        assert!(c.calibration("k").is_none());

        let b = bootstrap(&samples, &records).expect("bootstrap should fill in");
        assert_eq!(b.quality, Quality::Provisional);
        assert!((b.tokens_per_percent - 1000.0).abs() < 1e-6);
    }

    #[test]
    fn bootstrap_needs_the_window_to_have_moved() {
        let samples = vec![sample(0, 40.0), sample(3, 40.2)];
        let records = vec![record(1, "a", 10_000)];
        assert!(bootstrap(&samples, &records).is_none());
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
                None,
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
