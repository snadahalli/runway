//! Runway's engine, with no UI and no platform assumptions.
//!
//! Everything here runs the same on macOS, Windows and Linux; the places the
//! platforms genuinely differ are isolated in [`paths`] (where files live) and
//! [`credentials`] (keychain versus plain file).

pub mod activity;
pub mod alarms;
pub mod compat;
pub mod credentials;
pub mod engine;
pub mod format;
pub mod paths;
pub mod pricing;
pub mod projection;
pub mod readout;
pub mod settings;
pub mod severity;
pub mod snapshot;
pub mod transcript;
pub mod usage_api;

pub use activity::ActivityProfile;
pub use alarms::{Alarm, AlarmEngine};
pub use engine::{Engine, EngineConfig, EngineHandle};
pub use pricing::TokenTotals;
pub use settings::{MenuBarStyle, Settings};
pub use severity::Severity;
pub use snapshot::{
    LedgerEntry, LedgerSummary, LimitKind, LimitSnapshot, RunwaySnapshot, SnapshotHealth,
    SnapshotStore,
};
