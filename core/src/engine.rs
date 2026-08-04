//! The engine. Owns the poll loop, the local log scanner, the projection state,
//! and publishes the single snapshot that every surface renders from.
//!
//! Two independent cadences, unchanged from the Swift original:
//!
//! - **API poll** every 180s (a hard floor — see [`crate::usage_api`])
//! - **Local log scan** every 15s, free and unlimited
//!
//! Between API polls the snapshot is marked `Estimated` and each limit's
//! percentage is extrapolated from local token volume using the calibrated
//! tokens-per-percent, clamped so it can never run backwards or exceed 100.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};

use crate::activity::ActivityProfile;
use crate::alarms::{Alarm, AlarmEngine};
use crate::credentials;
use crate::paths;
use crate::pricing;
use crate::projection::{self, Calibrator, SampleHistory, UsageSample};
use crate::settings::Settings;
use crate::snapshot::{
    LedgerEntry, LedgerSummary, LimitKind, LimitSnapshot, RunwaySnapshot, SnapshotHealth,
    SnapshotStore,
};
use crate::transcript::{self, TranscriptScanner, UsageRecord};
use crate::usage_api::{self, UsageAPIClient, UsageAPIError, UsageResponse};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub scan_interval: f64,
    pub settings: Settings,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            scan_interval: 15.0,
            settings: Settings::load(),
        }
    }
}

/// What the network step needs, captured under the lock.
pub struct PollRequest {
    pub version: String,
}

pub enum PollOutcome {
    /// Couldn't read credentials at all — carries the message to show.
    NoCredentials(String),
    Response {
        response: UsageResponse,
        plan: Option<String>,
        expired: bool,
    },
    Failed(UsageAPIError),
}

/// Step 2, with **no lock held**: read the credentials and make the request.
///
/// Both halves can block for a long time — the macOS keychain can put a consent
/// dialog on screen, and the request has a 20s timeout — which is exactly why
/// this is a free function rather than a method.
pub fn execute_poll(request: PollRequest) -> PollOutcome {
    let credentials = match credentials::load() {
        Ok(c) => c,
        Err(e) => return PollOutcome::NoCredentials(e.to_string()),
    };
    let expired = credentials.is_expired();
    let client = UsageAPIClient::new(request.version);
    match client.fetch(&credentials.access_token) {
        Ok(response) => PollOutcome::Response {
            response,
            plan: credentials.subscription_type,
            expired,
        },
        Err(error) => PollOutcome::Failed(error),
    }
}

/// One observed limit before projection is applied.
struct Observation {
    kind: LimitKind,
    label: String,
    percent: f64,
    resets_at: Option<DateTime<Utc>>,
    is_active: bool,
}

pub struct Engine {
    pub config: EngineConfig,
    pub snapshot: RunwaySnapshot,
    pub last_error: Option<String>,
    pub next_poll_at: Option<DateTime<Utc>>,

    scanner: TranscriptScanner,
    history: SampleHistory,
    /// Accumulated tokens-per-percent observations, kept across window
    /// rollovers so a 5-hour reset doesn't discard the estimate.
    calibrator: Calibrator,
    alarms: AlarmEngine,
    /// When you actually work, relearned on every rebuild from the same records
    /// the ledger uses. Uniform until there's enough history to say anything.
    activity: ActivityProfile,

    /// Set when the API told us to slow down. Cleared on the next success.
    consecutive_failures: u32,
    backoff_until: Option<DateTime<Utc>>,

    /// Percentages from the most recent successful API read, used as the anchor
    /// for local extrapolation between polls.
    anchor_percents: HashMap<String, f64>,
    anchor_date: Option<DateTime<Utc>>,

    cli_version: Option<String>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        let mut engine = Engine {
            config,
            snapshot: RunwaySnapshot::placeholder(),
            last_error: None,
            next_poll_at: None,
            scanner: TranscriptScanner::with_defaults(),
            history: SampleHistory::default(),
            calibrator: Calibrator::default(),
            alarms: AlarmEngine::new(),
            activity: ActivityProfile::uniform(),
            consecutive_failures: 0,
            backoff_until: None,
            anchor_percents: HashMap::new(),
            anchor_date: None,
            cli_version: None,
        };
        engine.load_history();
        engine
    }

    /// First pass: read the logs and render whatever we already know, so the UI
    /// has something truthful on screen before the first network round trip.
    pub fn bootstrap(&mut self) {
        self.cli_version = if self.config.settings.user_agent_override.is_empty() {
            self.scanner.detect_cli_version()
        } else {
            Some(self.config.settings.user_agent_override.clone())
        };
        self.scanner.scan();
        let health = self.snapshot.health;
        self.rebuild(health, None, None, None, None);
    }

    pub fn recent_alarms(&self) -> Vec<Alarm> {
        self.alarms.recent.iter().cloned().collect()
    }

    /// Percentage readings for a limit's live window, for the sparkline.
    pub fn series(&self, limit: &LimitSnapshot) -> Vec<UsageSample> {
        self.history.current_window(&limit.id(), limit.resets_at)
    }

    pub fn records(&self) -> &[UsageRecord] {
        &self.scanner.state.records
    }

    /// The learned working-hours profile currently in force.
    pub fn activity(&self) -> &ActivityProfile {
        &self.activity
    }

    // MARK: - API poll
    //
    // Split into three steps so the slow parts — reading the keychain, which can
    // block on a user prompt, and a 20s HTTP request — happen with the engine
    // lock *released*. Holding it across those would freeze every UI surface,
    // since they all read the snapshot through the same mutex.

    /// Step 1, under the lock: decide whether to poll and capture what the
    /// request needs. `None` means we're in backoff and should skip.
    pub fn begin_poll(&mut self, force: bool) -> Option<PollRequest> {
        if !force {
            if let Some(until) = self.backoff_until {
                if until > Utc::now() {
                    return None;
                }
            }
        }

        let version = self
            .cli_version
            .clone()
            .or_else(|| self.scanner.detect_cli_version())
            .unwrap_or_else(|| "2.1.220".to_string());
        self.cli_version = Some(version.clone());

        Some(PollRequest { version })
    }

    /// Step 3, under the lock: fold the outcome back in. Returns any alarms
    /// raised, for the shell to deliver.
    pub fn finish_poll(&mut self, outcome: PollOutcome) -> Vec<Alarm> {
        match outcome {
            PollOutcome::NoCredentials(message) => {
                self.last_error = Some(message.clone());
                self.rebuild(
                    SnapshotHealth::NoCredentials,
                    Some(message),
                    None,
                    None,
                    None,
                );
                self.schedule_next(self.config.settings.effective_poll_interval());
                vec![]
            }
            PollOutcome::Response {
                response,
                plan,
                expired,
            } => {
                if expired {
                    // Claude Code refreshes this itself; we just wait for it.
                    self.rebuild(
                        SnapshotHealth::Error,
                        Some("Access token expired — run `claude` to refresh.".into()),
                        None,
                        None,
                        None,
                    );
                }
                self.consecutive_failures = 0;
                self.backoff_until = None;
                self.last_error = None;
                let alarms = self.ingest(response, plan);
                self.schedule_next(self.config.settings.effective_poll_interval());
                alarms
            }
            PollOutcome::Failed(error) => {
                self.handle(error);
                vec![]
            }
        }
    }

    /// All three steps in one call. Fine for the CLI, which has no UI to block.
    pub fn poll_now(&mut self, force: bool) -> Vec<Alarm> {
        match self.begin_poll(force) {
            Some(request) => {
                let outcome = execute_poll(request);
                self.finish_poll(outcome)
            }
            None => vec![],
        }
    }

    fn handle(&mut self, error: UsageAPIError) {
        let message = error.to_string();
        self.last_error = Some(message.clone());

        if !error.is_retryable() {
            self.rebuild(SnapshotHealth::Error, Some(message), None, None, None);
            self.schedule_next(self.config.settings.effective_poll_interval());
            return;
        }

        self.consecutive_failures += 1;
        // Honour retry-after when the server sent one, otherwise exponential
        // backoff from the poll interval, capped at 15 minutes.
        let suggested = match error {
            UsageAPIError::RateLimited {
                retry_after: Some(retry),
            } => retry + 5.0,
            _ => (self.config.settings.effective_poll_interval()
                * 2f64.powi(self.consecutive_failures as i32 - 1))
            .min(900.0),
        };
        self.backoff_until = Some(Utc::now() + secs(suggested));
        self.rebuild(SnapshotHealth::BackingOff, Some(message), None, None, None);
        self.schedule_next(suggested);
    }

    fn schedule_next(&mut self, interval: f64) {
        self.next_poll_at = Some(Utc::now() + secs(interval));
    }

    // MARK: - Ingestion

    fn ingest(&mut self, response: UsageResponse, plan: Option<String>) -> Vec<Alarm> {
        let now = Utc::now();
        let mut observed: Vec<Observation> = Vec::new();

        match response.limits.as_ref().filter(|l| !l.is_empty()) {
            Some(limits) => {
                for limit in limits {
                    let kind = LimitKind::from_api(&limit.kind);
                    observed.push(Observation {
                        kind,
                        label: label_for(limit, kind),
                        percent: limit.percent,
                        resets_at: limit.resets_at,
                        is_active: limit.is_active.unwrap_or(false),
                    });
                }
            }
            None => {
                if let Some(five) = &response.five_hour {
                    observed.push(Observation {
                        kind: LimitKind::Session,
                        label: "5-hour session".into(),
                        percent: five.utilization,
                        resets_at: five.resets_at,
                        is_active: true,
                    });
                }
                if let Some(seven) = &response.seven_day {
                    observed.push(Observation {
                        kind: LimitKind::WeeklyAll,
                        label: "Weekly".into(),
                        percent: seven.utilization,
                        resets_at: seven.resets_at,
                        is_active: true,
                    });
                }
            }
        }

        for item in &observed {
            let key = format!("{}|{}", item.kind.raw(), item.label);
            self.history.record(
                &key,
                UsageSample {
                    date: now,
                    percent: item.percent,
                    resets_at: item.resets_at,
                },
            );
            self.anchor_percents.insert(key, item.percent);
        }
        self.anchor_date = Some(now);
        self.save_history();

        self.scanner.scan();
        self.rebuild(SnapshotHealth::Live, None, plan, Some(now), Some(observed))
    }

    // MARK: - Local tick

    /// Runs between API polls. Reads new log lines and extrapolates each limit
    /// forward from the last API anchor, so the UI keeps moving without spending
    /// requests against a rate-limited endpoint.
    pub fn local_tick(&mut self) {
        let fresh = self.scanner.scan();
        let health = if self.snapshot.health == SnapshotHealth::Live {
            SnapshotHealth::Estimated
        } else {
            self.snapshot.health
        };
        if fresh.is_empty() && self.snapshot.health != SnapshotHealth::Live {
            let (h, m) = (self.snapshot.health, self.snapshot.message.clone());
            self.rebuild(h, m, None, None, None);
            return;
        }
        let message = self.snapshot.message.clone();
        self.rebuild(health, message, None, None, None);
    }

    // MARK: - Snapshot assembly

    fn rebuild(
        &mut self,
        health: SnapshotHealth,
        message: Option<String>,
        plan: Option<String>,
        observed_at: Option<DateTime<Utc>>,
        observed: Option<Vec<Observation>>,
    ) -> Vec<Alarm> {
        let now = Utc::now();

        // Either the readings we were just handed, or the last known reading per
        // series when this is a local-only refresh.
        let sources: Vec<Observation> = match observed {
            Some(o) => o,
            None => self
                .history
                .series
                .iter()
                .filter_map(|(key, samples)| {
                    let last = samples.last()?;
                    let (raw, label) = key.split_once('|')?;
                    Some(Observation {
                        kind: LimitKind::from_raw(raw)?,
                        label: label.to_string(),
                        percent: last.percent,
                        resets_at: last.resets_at,
                        is_active: true,
                    })
                })
                .collect(),
        };

        let records = self.scanner.state.records.clone();
        // Learned fresh each rebuild: it's a single pass over records we already
        // hold, and it means a change in working pattern shows up without any
        // cache invalidation to get wrong.
        self.activity = ActivityProfile::learn(&records, now);
        let activity = self.activity.clone();
        let mut limits: Vec<LimitSnapshot> = Vec::new();

        for source in &sources {
            let key = format!("{}|{}", source.kind.raw(), source.label);
            let window_samples = self.history.current_window(&key, source.resets_at);
            let window_start = source
                .resets_at
                .map(|r| r - secs(source.kind.window_seconds()));
            let window_records: Vec<UsageRecord> = match window_start {
                Some(start) => transcript::since(&records, start)
                    .into_iter()
                    .cloned()
                    .collect(),
                None => records.clone(),
            };

            let mut percent = source.percent;

            // Between polls, nudge the percentage using local token volume and
            // the calibrated tokens-per-percent. Never let the estimate exceed
            // 100 or run backwards — an estimate that overshoots is worse than
            // one that lags.
            if matches!(
                health,
                SnapshotHealth::Estimated | SnapshotHealth::BackingOff
            ) {
                if let (Some(anchor_date), Some(anchor)) =
                    (self.anchor_date, self.anchor_percents.get(&key).copied())
                {
                    if let Some(calibration) = self
                        .calibrator
                        .calibration(&key)
                        .or_else(|| projection::bootstrap(&window_samples, &window_records))
                    {
                        let since = transcript::since(&records, anchor_date);
                        let extra = transcript::total_tokens(&since).fresh() as f64
                            / calibration.tokens_per_percent.max(1.0);
                        percent = (anchor + extra).max(anchor).min(100.0);
                    }
                }
            }

            limits.push(projection::snapshot(
                source.kind,
                source.label.clone(),
                percent,
                source.resets_at,
                source.is_active,
                &window_samples,
                &window_records,
                &activity,
                self.calibrator.calibration(&key),
                now,
            ));
        }

        limits.sort_by(|a, b| {
            if a.kind == b.kind {
                a.label.cmp(&b.label)
            } else {
                a.kind
                    .window_seconds()
                    .partial_cmp(&b.kind.window_seconds())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        let week_records = transcript::since(&records, now - Duration::days(7));
        let month_cost =
            transcript::total_cost(&transcript::since(&records, now - Duration::days(30)));

        let entry = |b: &transcript::Bucket| LedgerEntry {
            name: b.name.clone(),
            tokens: b.tokens.billable(),
            cost_usd: b.cost,
        };

        let ledger = LedgerSummary {
            window_label: "Last 7 days".into(),
            tokens: transcript::total_tokens(&week_records),
            cost_usd: transcript::total_cost(&week_records),
            top_projects: transcript::breakdown(&week_records, |r| r.project.clone())
                .iter()
                .take(6)
                .map(entry)
                .collect(),
            top_models: transcript::breakdown(&week_records, |r| pricing::family(&r.model))
                .iter()
                .take(5)
                .map(entry)
                .collect(),
        };

        let updated = RunwaySnapshot {
            generated_at: now,
            api_observed_at: observed_at.or(self.snapshot.api_observed_at),
            health,
            message,
            limits,
            ledger,
            plan_label: plan.or_else(|| self.snapshot.plan_label.clone()),
            monthly_value_usd: Some(month_cost),
        };

        self.snapshot = updated;
        SnapshotStore::write(&self.snapshot);

        if health == SnapshotHealth::Live {
            let settings = self.config.settings.clone();
            let snapshot = self.snapshot.clone();
            return self.alarms.evaluate(&snapshot, &settings, now);
        }
        vec![]
    }

    // MARK: - Persistence

    fn load_history(&mut self) {
        let Ok(bytes) = std::fs::read(paths::history_path()) else {
            return;
        };
        let Ok(history) = serde_json::from_slice::<SampleHistory>(&bytes) else {
            return;
        };
        self.history = history;

        self.anchor_date = self
            .history
            .series
            .values()
            .filter_map(|s| s.last())
            .map(|s| s.date)
            .max();
        for (key, samples) in &self.history.series {
            if let Some(last) = samples.last() {
                self.anchor_percents.insert(key.clone(), last.percent);
            }
        }
    }

    fn save_history(&self) {
        if let Ok(bytes) = serde_json::to_vec(&self.history) {
            let _ = paths::write_atomic(&paths::history_path(), &bytes);
        }
        if let Ok(bytes) = serde_json::to_vec(&self.calibrator) {
            let _ = paths::write_atomic(&paths::calibration_path(), &bytes);
        }
    }
}

fn label_for(limit: &usage_api::Limit, kind: LimitKind) -> String {
    match kind {
        LimitKind::Session => "5-hour session".into(),
        LimitKind::WeeklyAll => "Weekly · all models".into(),
        LimitKind::WeeklyScoped => {
            let model = limit
                .scope
                .as_ref()
                .and_then(|s| s.model.as_ref())
                .and_then(|m| m.display_name.clone().or_else(|| m.id.clone()))
                .unwrap_or_else(|| "scoped".to_string());
            format!("Weekly · {model}")
        }
        LimitKind::Other => {
            let spaced = limit.kind.replace('_', " ");
            // Swift's `.capitalized` uppercases every word.
            spaced
                .split(' ')
                .map(|w| {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

fn secs(seconds: f64) -> Duration {
    Duration::milliseconds((seconds * 1000.0) as i64)
}

// MARK: - Threaded handle

/// A running engine plus the thread driving it.
///
/// The shell holds one of these, reads [`EngineHandle::snapshot`] whenever it
/// needs to draw, and gets a callback whenever the snapshot changes.
pub struct EngineHandle {
    inner: Arc<Mutex<Engine>>,
    wake: mpsc::Sender<Wake>,
}

enum Wake {
    PollNow,
    Shutdown,
}

impl EngineHandle {
    /// `on_update` is called on the engine thread every time the snapshot
    /// changes, with any alarms raised by that update.
    pub fn spawn<F>(config: EngineConfig, on_update: F) -> EngineHandle
    where
        F: Fn(&RunwaySnapshot, &[Alarm]) + Send + 'static,
    {
        let scan_interval = config.scan_interval.max(1.0);
        let inner = Arc::new(Mutex::new(Engine::new(config)));
        let (tx, rx) = mpsc::channel::<Wake>();

        {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("runway-engine".into())
                .spawn(move || {
                    // Every callback happens with the lock *released*. Holding
                    // it across `on_update` would invert lock order against any
                    // shell state the callback touches, and the shell's own
                    // reads would contend with a poll in flight.
                    let notify = |alarms: &[Alarm]| {
                        let snapshot = { inner.lock().unwrap().snapshot.clone() };
                        on_update(&snapshot, alarms);
                    };

                    {
                        inner.lock().unwrap().bootstrap();
                    }
                    notify(&[]);

                    // One poll, with the blocking parts outside the lock.
                    let poll = |force: bool| -> (Vec<Alarm>, f64) {
                        let request = { inner.lock().unwrap().begin_poll(force) };
                        let Some(request) = request else {
                            return (vec![], 10.0);
                        };
                        let outcome = execute_poll(request);

                        let mut engine = inner.lock().unwrap();
                        let alarms = engine.finish_poll(outcome);
                        let interval = engine
                            .backoff_until
                            .map(|until| {
                                ((until - Utc::now()).num_milliseconds() as f64 / 1000.0).max(10.0)
                            })
                            .unwrap_or_else(|| engine.config.settings.effective_poll_interval());
                        (alarms, interval)
                    };

                    let mut next_poll = Instant::now();
                    let mut next_scan = Instant::now() + StdDuration::from_secs_f64(scan_interval);

                    loop {
                        if Instant::now() >= next_poll {
                            let (alarms, interval) = poll(false);
                            notify(&alarms);
                            next_poll = Instant::now() + StdDuration::from_secs_f64(interval);
                        }

                        if Instant::now() >= next_scan {
                            {
                                inner.lock().unwrap().local_tick();
                            }
                            notify(&[]);
                            next_scan = Instant::now() + StdDuration::from_secs_f64(scan_interval);
                        }

                        // Sleep until whichever cadence comes first, but stay
                        // responsive to a manual refresh.
                        let wait = next_poll
                            .min(next_scan)
                            .saturating_duration_since(Instant::now());
                        match rx.recv_timeout(wait.max(StdDuration::from_millis(50))) {
                            Ok(Wake::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                            Ok(Wake::PollNow) => {
                                let (alarms, interval) = poll(true);
                                notify(&alarms);
                                next_poll = Instant::now() + StdDuration::from_secs_f64(interval);
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                        }
                    }
                })
                .expect("engine thread");
        }

        EngineHandle { inner, wake: tx }
    }

    pub fn snapshot(&self) -> RunwaySnapshot {
        self.inner.lock().unwrap().snapshot.clone()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }

    pub fn refresh_now(&self) {
        let _ = self.wake.send(Wake::PollNow);
    }

    pub fn shutdown(&self) {
        let _ = self.wake.send(Wake::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_weekly_limits_are_labelled_by_model() {
        let limit = usage_api::Limit {
            kind: "weekly_scoped".into(),
            group: "g".into(),
            percent: 1.0,
            severity: None,
            resets_at: None,
            scope: Some(usage_api::Scope {
                model: Some(usage_api::ScopeModel {
                    id: Some("claude-fable-5".into()),
                    display_name: Some("Fable".into()),
                }),
                surface: None,
            }),
            is_active: Some(true),
        };
        assert_eq!(label_for(&limit, LimitKind::WeeklyScoped), "Weekly · Fable");
    }

    #[test]
    fn scoped_limits_fall_back_to_the_model_id() {
        let limit = usage_api::Limit {
            kind: "weekly_scoped".into(),
            group: String::new(),
            percent: 1.0,
            severity: None,
            resets_at: None,
            scope: Some(usage_api::Scope {
                model: Some(usage_api::ScopeModel {
                    id: Some("claude-fable-5".into()),
                    display_name: None,
                }),
                surface: None,
            }),
            is_active: None,
        };
        assert_eq!(
            label_for(&limit, LimitKind::WeeklyScoped),
            "Weekly · claude-fable-5"
        );
    }

    /// A limit kind we've never seen must still render as something readable
    /// rather than crashing or showing a raw enum name.
    #[test]
    fn unknown_kinds_get_a_readable_label() {
        let limit = usage_api::Limit {
            kind: "monthly_extra_credits".into(),
            group: String::new(),
            percent: 0.0,
            severity: None,
            resets_at: None,
            scope: None,
            is_active: None,
        };
        assert_eq!(label_for(&limit, LimitKind::Other), "Monthly Extra Credits");
    }
}
