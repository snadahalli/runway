//! Reading state files the original Swift build left behind.
//!
//! Calibration is the expensive thing to lose: `calibrate` needs several
//! consecutive sample pairs before it can turn a percentage into tokens, which
//! on a quiet account takes hours. So this engine reads and writes the *same*
//! `samples.json` and `scan-state.json` that build used, and anyone upgrading
//! keeps their history instead of staring at a blank headline for an afternoon.
//!
//! The catch: those files were written by a plain `JSONEncoder`, whose default
//! date strategy is `.deferredToDate` — a bare `Double` counting seconds from
//! 2001-01-01, not 1970. Encoding those as RFC 3339 would quietly reset every
//! timestamp by 31 years.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serializer};

/// Seconds between the Unix epoch and Foundation's reference date.
pub const SWIFT_REFERENCE_EPOCH: f64 = 978_307_200.0;

pub fn from_swift(interval: f64) -> DateTime<Utc> {
    let unix = interval + SWIFT_REFERENCE_EPOCH;
    Utc.timestamp_nanos((unix * 1e9) as i64)
}

pub fn to_swift(date: &DateTime<Utc>) -> f64 {
    date.timestamp_nanos_opt().unwrap_or(0) as f64 / 1e9 - SWIFT_REFERENCE_EPOCH
}

pub fn serialize<S: Serializer>(date: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(to_swift(date))
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
    Ok(from_swift(f64::deserialize(d)?))
}

pub mod option {
    use super::*;

    pub fn serialize<S: Serializer>(date: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match date {
            Some(d) => s.serialize_f64(super::to_swift(d)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.map(super::from_swift))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_date_is_2001() {
        assert_eq!(from_swift(0.0).to_rfc3339(), "2001-01-01T00:00:00+00:00");
    }

    #[test]
    fn round_trips() {
        let now = Utc::now();
        let back = from_swift(to_swift(&now));
        assert!((back - now).num_milliseconds().abs() <= 1);
    }

    /// Guards the 31-year footgun: if this ever equals the Unix interpretation,
    /// every persisted sample has silently moved.
    #[test]
    fn is_not_unix_epoch() {
        let d = from_swift(770_000_000.0);
        assert_eq!(d.format("%Y").to_string(), "2025");
    }
}
