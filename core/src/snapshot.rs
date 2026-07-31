//! The single value the engine publishes and every surface renders from.
//!
//! `runway-snapshot.json` is written to disk on every update and is a supported
//! integration point — status bars, scripts and dashboards can read it without
//! going anywhere near this crate. So treat the field names and the date format
//! as a public contract rather than an implementation detail.
//!
//! Dates are whole seconds with a `Z` suffix and no fractional part. That began
//! as a constraint from a Swift `JSONDecoder`, whose `.iso8601` strategy rejects
//! fractional seconds outright; the decoder is gone but the format is kept,
//! because it costs nothing and strict parsers are common.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::pricing::TokenTotals;

/// Whole-second RFC 3339, in both directions.
pub mod iso8601 {
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn to_string(date: &DateTime<Utc>) -> String {
        date.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    /// Accepts fractional seconds on the way in even though we never write them,
    /// so a file written by any other producer still loads.
    pub fn parse(raw: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }

    pub fn serialize<S: Serializer>(date: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&to_string(date))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        let raw = String::deserialize(d)?;
        parse(&raw).ok_or_else(|| serde::de::Error::custom(format!("unparsable date {raw}")))
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(
            date: &Option<DateTime<Utc>>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match date {
                Some(d) => s.serialize_str(&super::to_string(d)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<DateTime<Utc>>, D::Error> {
            let raw = Option::<String>::deserialize(d)?;
            Ok(raw.as_deref().and_then(super::parse))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitKind {
    /// Rolling 5-hour window.
    Session,
    /// 7-day, all models.
    WeeklyAll,
    /// 7-day, scoped to one model family.
    WeeklyScoped,
    Other,
}

impl LimitKind {
    pub fn window_seconds(&self) -> f64 {
        match self {
            LimitKind::Session => 5.0 * 3600.0,
            _ => 7.0 * 24.0 * 3600.0,
        }
    }

    pub fn from_api(kind: &str) -> LimitKind {
        match kind {
            "session" => LimitKind::Session,
            "weekly_all" => LimitKind::WeeklyAll,
            "weekly_scoped" => LimitKind::WeeklyScoped,
            _ => LimitKind::Other,
        }
    }

    /// The wire value, also used to build the sample-history key.
    pub fn raw(&self) -> &'static str {
        match self {
            LimitKind::Session => "session",
            LimitKind::WeeklyAll => "weeklyAll",
            LimitKind::WeeklyScoped => "weeklyScoped",
            LimitKind::Other => "other",
        }
    }

    pub fn from_raw(raw: &str) -> Option<LimitKind> {
        match raw {
            "session" => Some(LimitKind::Session),
            "weeklyAll" => Some(LimitKind::WeeklyAll),
            "weeklyScoped" => Some(LimitKind::WeeklyScoped),
            "other" => Some(LimitKind::Other),
            _ => None,
        }
    }
}

/// One limit, plus everything Runway derived about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitSnapshot {
    pub kind: LimitKind,
    pub label: String,
    pub percent: f64,
    #[serde(
        rename = "resetsAt",
        with = "iso8601::option",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub resets_at: Option<DateTime<Utc>>,
    #[serde(rename = "isActive")]
    pub is_active: bool,

    /// Actual burn ÷ the burn that would land exactly at 100% at reset.
    /// 1.0 is perfectly paced; above 1.0 runs dry early.
    #[serde(rename = "paceRatio", skip_serializing_if = "Option::is_none", default)]
    pub pace_ratio: Option<f64>,
    /// Projected moment this limit reaches 100% at the current rate.
    #[serde(
        rename = "exhaustsAt",
        with = "iso8601::option",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub exhausts_at: Option<DateTime<Utc>>,
    /// Percentage points per hour you may spend from now on and still land at
    /// 100% exactly at reset. This is the number the whole app is built around.
    #[serde(
        rename = "allowancePercentPerHour",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allowance_percent_per_hour: Option<f64>,
    #[serde(
        rename = "allowanceTokensPerHour",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allowance_tokens_per_hour: Option<f64>,
    #[serde(
        rename = "remainingTokens",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub remaining_tokens: Option<f64>,
    #[serde(
        rename = "remainingValueUSD",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub remaining_value_usd: Option<f64>,
}

impl LimitSnapshot {
    pub fn id(&self) -> String {
        format!("{}|{}", self.kind.raw(), self.label)
    }

    pub fn window_seconds(&self) -> f64 {
        self.kind.window_seconds()
    }

    pub fn time_remaining(&self, now: DateTime<Utc>) -> Option<f64> {
        self.resets_at
            .map(|r| ((r - now).num_milliseconds() as f64 / 1000.0).max(0.0))
    }

    /// True when the projection says we run out before the window resets.
    pub fn runs_dry_early(&self) -> bool {
        match (self.exhausts_at, self.resets_at) {
            (Some(e), Some(r)) => e < r,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub name: String,
    pub tokens: i64,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
}

/// Ledger rollup for the popover.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerSummary {
    #[serde(rename = "windowLabel")]
    pub window_label: String,
    pub tokens: TokenTotals,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
    #[serde(rename = "topProjects")]
    pub top_projects: Vec<LedgerEntry>,
    #[serde(rename = "topModels")]
    pub top_models: Vec<LedgerEntry>,
}

impl Default for LedgerSummary {
    fn default() -> Self {
        LedgerSummary {
            window_label: "This week".into(),
            tokens: TokenTotals::default(),
            cost_usd: 0.0,
            top_projects: vec![],
            top_models: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotHealth {
    /// Fresh API data.
    Live,
    /// Between polls, extrapolated from local logs.
    Estimated,
    /// Rate limited, showing last good data.
    BackingOff,
    Error,
    NoCredentials,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunwaySnapshot {
    #[serde(rename = "generatedAt", with = "iso8601")]
    pub generated_at: DateTime<Utc>,
    #[serde(
        rename = "apiObservedAt",
        with = "iso8601::option",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub api_observed_at: Option<DateTime<Utc>>,
    pub health: SnapshotHealth,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,

    pub limits: Vec<LimitSnapshot>,
    pub ledger: LedgerSummary,
    #[serde(rename = "planLabel", skip_serializing_if = "Option::is_none", default)]
    pub plan_label: Option<String>,
    #[serde(
        rename = "monthlyValueUSD",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub monthly_value_usd: Option<f64>,
}

impl RunwaySnapshot {
    pub fn placeholder() -> Self {
        RunwaySnapshot {
            generated_at: Utc::now(),
            api_observed_at: None,
            health: SnapshotHealth::NoCredentials,
            message: Some("Open Runway to connect".into()),
            limits: vec![],
            ledger: LedgerSummary::default(),
            plan_label: None,
            monthly_value_usd: None,
        }
    }

    /// The limit that will bind first — what you actually care about.
    ///
    /// Ranked in two classes rather than one number, because seconds and
    /// percentage points aren't comparable quantities. Scoring un-projected
    /// limits as `1000 - percent` against seconds-to-exhaustion — which is what
    /// this did originally — puts every un-projected limit ahead of anything
    /// running dry more than ~17 minutes out, the opposite of the intent. So: a
    /// limit actually projected to run out before its window resets binds
    /// first, soonest wins; otherwise the fullest one does.
    pub fn headline(&self) -> Option<&LimitSnapshot> {
        let now = Utc::now();
        let urgency = |l: &LimitSnapshot| -> (u8, f64) {
            match (l.exhausts_at, l.resets_at) {
                (Some(exhausts), Some(resets)) if exhausts < resets => {
                    (0, (exhausts - now).num_milliseconds() as f64 / 1000.0)
                }
                _ => (1, -l.percent),
            }
        };
        self.limits
            .iter()
            .filter(|l| l.percent > 0.0 || l.is_active)
            .min_by(|a, b| {
                let (ac, av) = urgency(a);
                let (bc, bv) = urgency(b);
                ac.cmp(&bc)
                    .then(av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal))
            })
            .or_else(|| self.limits.first())
    }

    pub fn age_seconds(&self, now: DateTime<Utc>) -> f64 {
        (now - self.generated_at).num_milliseconds() as f64 / 1000.0
    }
}

/// Reads and writes the snapshot where every surface can reach it.
pub struct SnapshotStore;

impl SnapshotStore {
    pub fn write(snapshot: &RunwaySnapshot) {
        if let Ok(json) = serde_json::to_vec(snapshot) {
            let _ = paths::write_atomic(&paths::snapshot_path(), &json);
        }
    }

    pub fn read() -> Option<RunwaySnapshot> {
        let bytes = std::fs::read(paths::snapshot_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(h: f64) -> chrono::Duration {
        chrono::Duration::milliseconds((h * 3.6e6) as i64)
    }

    fn limit(percent: f64, exhausts_in_hours: Option<f64>) -> LimitSnapshot {
        LimitSnapshot {
            kind: LimitKind::Session,
            label: "5-hour session".into(),
            percent,
            resets_at: Some(Utc::now() + hours(5.0)),
            is_active: true,
            pace_ratio: None,
            exhausts_at: exhausts_in_hours.map(|h| Utc::now() + hours(h)),
            allowance_percent_per_hour: None,
            allowance_tokens_per_hour: None,
            remaining_tokens: None,
            remaining_value_usd: None,
        }
    }

    /// The widget's decoder rejects fractional seconds, so this is load-bearing.
    #[test]
    fn dates_encode_the_way_swift_expects() {
        let mut snap = RunwaySnapshot::placeholder();
        snap.generated_at = DateTime::parse_from_rfc3339("2026-07-31T05:38:19.123456Z")
            .unwrap()
            .with_timezone(&Utc);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(
            json.contains("\"generatedAt\":\"2026-07-31T05:38:19Z\""),
            "{json}"
        );
        assert!(!json.contains(".123"));
    }

    #[test]
    fn nil_optionals_are_omitted_not_null() {
        // Swift's synthesized encoder omits nil keys; a literal `null` decodes
        // fine too, but staying byte-comparable makes diffing the two writers easy.
        let json = serde_json::to_string(&RunwaySnapshot::placeholder()).unwrap();
        assert!(!json.contains("apiObservedAt"));
        assert!(!json.contains("null"));
    }

    #[test]
    fn kind_raw_values_match_swift() {
        assert_eq!(
            serde_json::to_string(&LimitKind::WeeklyAll).unwrap(),
            "\"weeklyAll\""
        );
        assert_eq!(
            serde_json::to_string(&SnapshotHealth::NoCredentials).unwrap(),
            "\"noCredentials\""
        );
    }

    /// The whole point of the headline: something projected to run out before
    /// its window resets outranks a fuller limit that is merely sitting there.
    #[test]
    fn headline_prefers_a_limit_that_runs_dry_early() {
        let mut snap = RunwaySnapshot::placeholder();
        snap.limits = vec![limit(95.0, None), limit(20.0, Some(0.5))];
        assert_eq!(snap.headline().unwrap().percent, 20.0);
    }

    #[test]
    fn headline_picks_the_soonest_run_dry() {
        let mut snap = RunwaySnapshot::placeholder();
        snap.limits = vec![limit(60.0, Some(3.0)), limit(10.0, Some(0.5))];
        assert_eq!(snap.headline().unwrap().percent, 10.0);
    }

    #[test]
    fn headline_without_projections_falls_back_to_fullness() {
        let mut snap = RunwaySnapshot::placeholder();
        snap.limits = vec![limit(30.0, None), limit(80.0, None)];
        assert_eq!(snap.headline().unwrap().percent, 80.0);
    }

    /// A projection that lands *after* the reset isn't running dry at all, so it
    /// must not jump the queue ahead of a limit that's genuinely nearly full.
    #[test]
    fn a_projection_beyond_reset_does_not_outrank_fullness() {
        let mut snap = RunwaySnapshot::placeholder();
        snap.limits = vec![limit(90.0, None), limit(5.0, Some(9.0))];
        assert_eq!(snap.headline().unwrap().percent, 90.0);
    }

    #[test]
    fn round_trips_through_json() {
        let mut snap = RunwaySnapshot::placeholder();
        snap.limits = vec![limit(42.0, Some(2.0))];
        let json = serde_json::to_string(&snap).unwrap();
        let back: RunwaySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.limits[0].percent, 42.0);
        assert_eq!(back.health, SnapshotHealth::NoCredentials);
    }
}
