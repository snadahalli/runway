//! When you actually work, learned from your own logs.
//!
//! Everything else in Runway used to measure a window in calendar hours: the
//! allowance was `remaining ÷ hours until reset`, which quietly assumes you burn
//! tokens evenly through nights and weekends. Nobody does. On the machine this
//! was written on, 90% of tokens fell in 30 of the 168 hour-of-week slots — so
//! the flat model understated the sustainable working-hours rate by ~5.6x, and
//! a slope measured during the working day, extrapolated across a calendar week,
//! reported a 2.45x pace when the honest figure was 0.26x.
//!
//! So: bucket the transcript records — which are already being read, for the
//! ledger — into an hour-of-week histogram, and measure windows in *expected
//! work* rather than elapsed time.
//!
//! The profile is a rate multiplier per slot with mean 1.0 across the week, so a
//! uniform profile reproduces the old calendar behaviour exactly. That's the
//! fallback whenever there isn't enough history to learn anything.

use chrono::{DateTime, Datelike, Duration, Local, Timelike, Utc};

use crate::transcript::UsageRecord;

pub const SLOTS: usize = 168; // 7 days x 24 hours

/// Records older than this don't describe your current rhythm.
const LEARN_WINDOW_DAYS: i64 = 28;

/// Blend towards uniform. Without this a slot you've simply never worked in
/// would be a hard zero, which makes "expected work" stop advancing and run-dry
/// projections shoot to infinity the moment you work an unusual hour.
const SMOOTHING: f64 = 0.15;

/// Below this much history the histogram is noise, so stay uniform.
const MIN_DISTINCT_DAYS: usize = 3;

/// A "working hour" is defined as the mean intensity of the busiest slots that
/// together hold this share of the week's activity.
const ACTIVE_COVERAGE: f64 = 0.8;

/// Resolution of the walk over a window. A week is 672 steps at this size,
/// which is nothing, and it sidesteps every hour-boundary and DST edge case
/// that stepping exactly on the hour would introduce.
const STEP: Duration = Duration::minutes(15);

#[derive(Clone, Debug)]
pub struct ActivityProfile {
    /// Rate multiplier per hour-of-week slot, mean 1.0. Index is
    /// `weekday_from_monday * 24 + hour`, in **local** time — the rhythm being
    /// modelled is a human one.
    weights: [f64; SLOTS],
    /// False when we fell back to uniform, i.e. this is calendar time again.
    pub learned: bool,
}

impl Default for ActivityProfile {
    fn default() -> Self {
        Self::uniform()
    }
}

impl ActivityProfile {
    pub fn uniform() -> Self {
        ActivityProfile {
            weights: [1.0; SLOTS],
            learned: false,
        }
    }

    /// Build a profile from transcript records. Falls back to uniform when
    /// there isn't enough history to say anything honest.
    pub fn learn(records: &[UsageRecord], now: DateTime<Utc>) -> Self {
        let cutoff = now - Duration::days(LEARN_WINDOW_DAYS);
        let mut totals = [0.0f64; SLOTS];
        let mut total = 0.0;
        let mut days = std::collections::HashSet::new();

        for record in records.iter().filter(|r| r.date >= cutoff) {
            // Cache reads are ~97% of raw volume and don't represent new work,
            // so weight the rhythm by fresh tokens — the same measure
            // calibration uses.
            let fresh = record.tokens.fresh() as f64;
            if fresh <= 0.0 {
                continue;
            }
            let local = record.date.with_timezone(&Local);
            totals[slot_of(local)] += fresh;
            total += fresh;
            days.insert(local.date_naive());
        }

        if total <= 0.0 || days.len() < MIN_DISTINCT_DAYS {
            return Self::uniform();
        }

        // Normalise to mean 1.0 across the week, then blend towards uniform.
        let mut weights = [0.0f64; SLOTS];
        for (index, sum) in totals.iter().enumerate() {
            let share = sum / total; // fraction of a week's work in this slot
            weights[index] = (1.0 - SMOOTHING) * share * SLOTS as f64 + SMOOTHING;
        }

        ActivityProfile {
            weights,
            learned: true,
        }
    }

    /// The rate multiplier right now. 1.0 is an average hour of the week.
    pub fn intensity_at(&self, at: DateTime<Utc>) -> f64 {
        self.weights[slot_of(at.with_timezone(&Local))]
    }

    /// Fraction of a typical week's work falling in `[from, to)`.
    ///
    /// A full week returns 1.0 by construction, so this is directly comparable
    /// against "fraction of the limit consumed".
    pub fn work_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
        self.walk(from, to, |acc, weight, hours| acc + weight * hours) / SLOTS as f64
    }

    /// Expected number of *working* hours in `[from, to)`, where one working
    /// hour means "an hour as busy as your typical busy hour".
    ///
    /// For a uniform profile this is just elapsed hours, which is what makes
    /// the allowance reduce to the old `remaining ÷ hours to reset`.
    pub fn active_hours_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
        let reference = self.typical_active_intensity();
        if reference <= 0.0 {
            return 0.0;
        }
        self.walk(from, to, |acc, weight, hours| acc + weight * hours) / reference
    }

    /// Intensity of a typical *working* hour: the mean over the busiest slots
    /// that together account for [`ACTIVE_COVERAGE`] of the week's activity.
    ///
    /// Equals exactly 1.0 for a uniform profile, which is what keeps the
    /// allowance reducing to calendar time when nothing has been learned.
    ///
    /// The obvious closed form — `Σr²/Σr`, the participation-weighted mean — was
    /// tried first and is far too sensitive to one outlier: a single unusually
    /// heavy afternoon accounted for 21% of a fortnight's tokens on the machine
    /// this was developed on and dragged the estimate from ~4.3 to 8.6, roughly
    /// doubling the reported allowance. A mean over a *set* of slots doesn't
    /// move like that.
    fn typical_active_intensity(&self) -> f64 {
        let total: f64 = self.weights.iter().sum();
        if total <= 0.0 {
            return 1.0;
        }
        let mut sorted = self.weights;
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

        let target = ACTIVE_COVERAGE * total;
        let mut accumulated = 0.0;
        let mut count = 0usize;
        for weight in sorted {
            accumulated += weight;
            count += 1;
            if accumulated >= target {
                break;
            }
        }
        if count == 0 {
            return 1.0;
        }
        accumulated / count as f64
    }

    /// Walk forward from `from` until the accumulated work reaches `target`
    /// (measured like [`work_between`]). `None` if that takes longer than
    /// `limit`, which is the honest answer for "you'll never get there".
    pub fn time_to_accumulate(
        &self,
        from: DateTime<Utc>,
        target: f64,
        limit: Duration,
    ) -> Option<DateTime<Utc>> {
        if target <= 0.0 {
            return Some(from);
        }
        let deadline = from + limit;
        let mut cursor = from;
        let mut acc = 0.0;
        let step_hours = STEP.num_seconds() as f64 / 3600.0;

        while cursor < deadline {
            let weight = self.weights[slot_of(cursor.with_timezone(&Local))];
            let chunk = weight * step_hours / SLOTS as f64;
            if acc + chunk >= target {
                // Interpolate within the step rather than rounding to 15 minutes.
                let fraction = if chunk > 0.0 {
                    (target - acc) / chunk
                } else {
                    0.0
                };
                let millis = (STEP.num_milliseconds() as f64 * fraction) as i64;
                return Some(cursor + Duration::milliseconds(millis));
            }
            acc += chunk;
            cursor += STEP;
        }
        None
    }

    fn walk<F>(&self, from: DateTime<Utc>, to: DateTime<Utc>, mut fold: F) -> f64
    where
        F: FnMut(f64, f64, f64) -> f64,
    {
        if to <= from {
            return 0.0;
        }
        let mut acc = 0.0;
        let mut cursor = from;
        while cursor < to {
            let end = (cursor + STEP).min(to);
            let hours = (end - cursor).num_milliseconds() as f64 / 3.6e6;
            let weight = self.weights[slot_of(cursor.with_timezone(&Local))];
            acc = fold(acc, weight, hours);
            cursor = end;
        }
        acc
    }

    /// The 168 multipliers, for display. Index is `weekday * 24 + hour`, Monday
    /// first, local time.
    pub fn weights(&self) -> &[f64; SLOTS] {
        &self.weights
    }
}

fn slot_of(local: DateTime<Local>) -> usize {
    local.weekday().num_days_from_monday() as usize * 24 + local.hour() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::TokenTotals;
    use chrono::TimeZone;

    /// A record at a given local weekday/hour.
    fn record(id: &str, weekday: u32, hour: u32, tokens: i64) -> UsageRecord {
        // 2026-07-06 is a Monday.
        let day = 6 + weekday;
        let local = Local
            .with_ymd_and_hms(2026, 7, day, hour, 30, 0)
            .single()
            .expect("valid local time");
        UsageRecord {
            id: id.into(),
            date: local.with_timezone(&Utc),
            model: "claude-opus-5".into(),
            project: "p".into(),
            project_path: "/p".into(),
            session_id: "s".into(),
            tokens: TokenTotals {
                input: tokens,
                ..Default::default()
            },
        }
    }

    fn now() -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(2026, 7, 13, 12, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    fn nine_to_five() -> ActivityProfile {
        // Mon–Fri, 09:00–17:00, nothing at nights or weekends.
        let mut records = Vec::new();
        for weekday in 0..5u32 {
            for hour in 9..17u32 {
                records.push(record(&format!("{weekday}-{hour}"), weekday, hour, 1000));
            }
        }
        ActivityProfile::learn(&records, now())
    }

    #[test]
    fn a_uniform_profile_reproduces_calendar_time() {
        // This is the property that makes the change safe: with no history, or
        // too little, every derived number must match the old behaviour.
        let p = ActivityProfile::uniform();
        let start = now();
        let end = start + Duration::days(7);
        assert!((p.work_between(start, end) - 1.0).abs() < 1e-9);
        assert!((p.active_hours_between(start, start + Duration::hours(5)) - 5.0).abs() < 1e-6);
        assert!((p.intensity_at(start) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn too_little_history_stays_uniform() {
        assert!(!ActivityProfile::learn(&[], now()).learned);
        // Two days of data isn't a rhythm.
        let records = vec![record("a", 0, 10, 500), record("b", 1, 10, 500)];
        assert!(!ActivityProfile::learn(&records, now()).learned);
    }

    #[test]
    fn history_older_than_the_learning_window_is_ignored() {
        let records = vec![
            record("a", 0, 10, 500),
            record("b", 1, 10, 500),
            record("c", 2, 10, 500),
        ];
        // Same records, evaluated two months later.
        let much_later = now() + Duration::days(60);
        assert!(!ActivityProfile::learn(&records, much_later).learned);
        assert!(ActivityProfile::learn(&records, now()).learned);
    }

    #[test]
    fn a_working_pattern_is_learned() {
        let p = nine_to_five();
        assert!(p.learned);
        let monday_11 = Local
            .with_ymd_and_hms(2026, 7, 13, 11, 0, 0)
            .single()
            .unwrap();
        let monday_03 = Local
            .with_ymd_and_hms(2026, 7, 13, 3, 0, 0)
            .single()
            .unwrap();
        let sunday_11 = Local
            .with_ymd_and_hms(2026, 7, 12, 11, 0, 0)
            .single()
            .unwrap();

        assert!(
            p.intensity_at(monday_11.with_timezone(&Utc)) > 3.0,
            "work hour should be busy"
        );
        assert!(
            p.intensity_at(monday_03.with_timezone(&Utc)) < 0.3,
            "3am should be quiet"
        );
        assert!(
            p.intensity_at(sunday_11.with_timezone(&Utc)) < 0.3,
            "sunday should be quiet"
        );
    }

    #[test]
    fn weights_still_average_one_across_the_week() {
        // The mean-1.0 normalisation is what keeps a full week worth 1.0 of work
        // whatever the shape of the profile.
        let p = nine_to_five();
        let mean: f64 = p.weights().iter().sum::<f64>() / SLOTS as f64;
        assert!((mean - 1.0).abs() < 1e-9, "mean was {mean}");

        let start = Local
            .with_ymd_and_hms(2026, 7, 13, 0, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!((p.work_between(start, start + Duration::days(7)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_night_advances_expected_work_far_less_than_a_workday_morning() {
        let p = nine_to_five();
        let night_start = Local
            .with_ymd_and_hms(2026, 7, 13, 1, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let morning_start = Local
            .with_ymd_and_hms(2026, 7, 13, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let night = p.work_between(night_start, night_start + Duration::hours(4));
        let morning = p.work_between(morning_start, morning_start + Duration::hours(4));
        assert!(morning > night * 10.0, "morning {morning} vs night {night}");
    }

    #[test]
    fn active_hours_ignore_the_hours_you_do_not_work() {
        let p = nine_to_five();
        // Friday 17:00 to Monday 09:00 — a whole weekend, almost no working time.
        let friday_evening = Local
            .with_ymd_and_hms(2026, 7, 17, 17, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let monday_morning = Local
            .with_ymd_and_hms(2026, 7, 20, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let hours = p.active_hours_between(friday_evening, monday_morning);
        assert!(
            hours < 3.0,
            "a weekend should be worth almost no working hours, got {hours}"
        );

        // A single working day is worth roughly its eight hours.
        let monday_9 = Local
            .with_ymd_and_hms(2026, 7, 13, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let monday_17 = Local
            .with_ymd_and_hms(2026, 7, 13, 17, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let workday = p.active_hours_between(monday_9, monday_17);
        assert!(
            (6.0..=10.0).contains(&workday),
            "expected ~8 working hours, got {workday}"
        );
    }

    #[test]
    fn run_dry_lands_in_working_hours_not_the_middle_of_the_night() {
        let p = nine_to_five();
        // Start Monday 15:00 with two working hours' worth of budget left.
        let start = Local
            .with_ymd_and_hms(2026, 7, 13, 15, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let two_hours = p.work_between(start, start + Duration::hours(2));
        let dry = p
            .time_to_accumulate(start, two_hours, Duration::days(14))
            .unwrap();
        assert_eq!(
            dry.with_timezone(&Local).hour(),
            16,
            "should be 16:xx Monday"
        );

        // Four hours of budget from 15:00 must spill to Tuesday morning, not to
        // 19:00 on Monday — you stop working at 17:00.
        let four = p.work_between(start, start + Duration::hours(2)) * 2.0;
        let dry = p
            .time_to_accumulate(start, four, Duration::days(14))
            .unwrap();
        let local = dry.with_timezone(&Local);
        assert_eq!(
            local.weekday().num_days_from_monday(),
            1,
            "should be Tuesday"
        );
        assert!(
            (9..12).contains(&local.hour()),
            "should be Tuesday morning, got {local}"
        );
    }

    #[test]
    fn unreachable_targets_return_none() {
        let p = nine_to_five();
        let start = now();
        assert!(p
            .time_to_accumulate(start, 100.0, Duration::days(14))
            .is_none());
    }

    #[test]
    fn a_zero_target_is_already_reached() {
        let p = nine_to_five();
        let start = now();
        assert_eq!(
            p.time_to_accumulate(start, 0.0, Duration::days(1)),
            Some(start)
        );
    }

    /// One freakishly heavy afternoon must not redefine what a working hour is.
    /// The `Σr²/Σr` estimator this replaced roughly doubled the allowance on
    /// real data because a single slot held 21% of a fortnight's tokens.
    #[test]
    fn one_outlier_slot_does_not_define_a_working_hour() {
        let mut records: Vec<UsageRecord> = Vec::new();
        for weekday in 0..5u32 {
            for hour in 9..17u32 {
                records.push(record(&format!("{weekday}-{hour}"), weekday, hour, 1000));
            }
        }
        let steady = ActivityProfile::learn(&records, now());

        // Same week, but Wednesday 15:00 held 20% of the fortnight's tokens —
        // the shape of the real data that exposed this.
        records.push(record("spike", 2, 15, 10_000));
        let spiky = ActivityProfile::learn(&records, now());

        let a = steady.typical_active_intensity();
        let b = spiky.typical_active_intensity();
        assert!(
            (b - a).abs() / a < 0.35,
            "the outlier moved the working-hour reference from {a:.2} to {b:.2}"
        );

        // A working day still measures as roughly a working day. It shifts a
        // little, and should: if a fifth of the week lands in one hour, every
        // other hour genuinely is a smaller share of the week.
        let mon = Local
            .with_ymd_and_hms(2026, 7, 13, 9, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let steady_hours = steady.active_hours_between(mon, mon + Duration::hours(8));
        let spiky_hours = spiky.active_hours_between(mon, mon + Duration::hours(8));
        assert!(
            (spiky_hours - steady_hours).abs() / steady_hours < 0.4,
            "{steady_hours:.2} vs {spiky_hours:.2}"
        );
    }

    #[test]
    fn a_working_hour_is_one_hour_for_a_uniform_profile() {
        // The exact-1.0 property is what makes the uniform fallback identical to
        // the calendar model it replaced.
        assert!((ActivityProfile::uniform().typical_active_intensity() - 1.0).abs() < 1e-12);
    }

    /// Smoothing exists so an unusual hour can't wedge the projection.
    #[test]
    fn never_worked_slots_are_not_hard_zeros() {
        let p = nine_to_five();
        let sunday_4am = Local
            .with_ymd_and_hms(2026, 7, 12, 4, 0, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        assert!(p.intensity_at(sunday_4am) > 0.0);
        // And time still advances through a weekend, just slowly.
        assert!(p.work_between(sunday_4am, sunday_4am + Duration::hours(6)) > 0.0);
    }
}
