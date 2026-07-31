//! Dollar-equivalent value of subscription usage.
//!
//! Subscription plans don't bill per token, so nothing here is an invoice — it
//! answers "what would this have cost on the pay-as-you-go API?", which is the
//! only honest way to compare a month of Claude Code against the plan price.
//!
//! Cache tokens are priced separately and correctly, which most trackers skip:
//! a 5-minute cache write costs 1.25x base input, a 1-hour write costs 2x, and
//! a cache read costs 0.1x. On a long Claude Code session cache reads dominate
//! the token count, so pricing them at full input rate overstates value by an
//! order of magnitude.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug)]
pub struct ModelRate {
    /// USD per million tokens.
    pub input: f64,
    pub output: f64,
}

impl ModelRate {
    const fn new(input: f64, output: f64) -> Self {
        Self { input, output }
    }
    pub fn cache_write_5m(&self) -> f64 {
        self.input * 1.25
    }
    pub fn cache_write_1h(&self) -> f64 {
        self.input * 2.0
    }
    pub fn cache_read(&self) -> f64 {
        self.input * 0.1
    }
}

/// Longest-prefix match wins, so dated snapshots and `[1m]` suffixes resolve to
/// their family without needing an entry each.
///
/// Hand-maintained list prices. Sonnet 5 introductory pricing is deliberately
/// not applied. Re-check when list prices move.
pub const TABLE: &[(&str, ModelRate)] = &[
    ("claude-fable-5", ModelRate::new(10.0, 50.0)),
    ("claude-mythos", ModelRate::new(10.0, 50.0)),
    ("claude-opus-5", ModelRate::new(5.0, 25.0)),
    ("claude-opus-4-8", ModelRate::new(5.0, 25.0)),
    ("claude-opus-4-7", ModelRate::new(5.0, 25.0)),
    ("claude-opus-4-6", ModelRate::new(5.0, 25.0)),
    ("claude-opus-4-5", ModelRate::new(5.0, 25.0)),
    ("claude-opus-4", ModelRate::new(15.0, 75.0)),
    ("claude-opus", ModelRate::new(15.0, 75.0)),
    ("claude-sonnet-5", ModelRate::new(3.0, 15.0)),
    ("claude-sonnet-4-6", ModelRate::new(3.0, 15.0)),
    ("claude-sonnet-4-5", ModelRate::new(3.0, 15.0)),
    ("claude-sonnet", ModelRate::new(3.0, 15.0)),
    ("claude-haiku-4-5", ModelRate::new(1.0, 5.0)),
    ("claude-haiku", ModelRate::new(0.8, 4.0)),
];

pub const FALLBACK: ModelRate = ModelRate::new(5.0, 25.0);

pub fn rate(model: &str) -> ModelRate {
    let key = model.to_lowercase();
    let mut best: Option<(usize, ModelRate)> = None;
    for (prefix, rate) in TABLE {
        if key.starts_with(prefix) && best.map_or(true, |(len, _)| prefix.len() > len) {
            best = Some((prefix.len(), *rate));
        }
    }
    best.map(|(_, r)| r).unwrap_or(FALLBACK)
}

/// Human-facing family label used for grouping in the ledger.
pub fn family(model: &str) -> String {
    let key = model.to_lowercase();
    for (needle, label) in [
        ("fable", "Fable"),
        ("mythos", "Mythos"),
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
    ] {
        if key.contains(needle) {
            return label.to_string();
        }
    }
    "Other".to_string()
}

pub fn cost(tokens: &TokenTotals, model: &str) -> f64 {
    let rate = rate(model);
    const PER_MILLION: f64 = 1_000_000.0;
    (tokens.input as f64 * rate.input
        + tokens.output as f64 * rate.output
        + tokens.cache_write_5m as f64 * rate.cache_write_5m()
        + tokens.cache_write_1h as f64 * rate.cache_write_1h()
        + tokens.cache_read as f64 * rate.cache_read())
        / PER_MILLION
}

/// Token counts split the way the API actually bills them.
///
/// Field names are the Swift property names because the macOS WidgetKit
/// extension decodes this exact JSON.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub output: i64,
    #[serde(rename = "cacheWrite5m", default)]
    pub cache_write_5m: i64,
    #[serde(rename = "cacheWrite1h", default)]
    pub cache_write_1h: i64,
    #[serde(rename = "cacheRead", default)]
    pub cache_read: i64,
}

impl TokenTotals {
    pub fn billable(&self) -> i64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }

    /// Tokens that represent genuinely new work, ignoring cache replay.
    pub fn fresh(&self) -> i64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h
    }
}

impl std::ops::Add for TokenTotals {
    type Output = TokenTotals;
    fn add(self, rhs: TokenTotals) -> TokenTotals {
        TokenTotals {
            input: self.input + rhs.input,
            output: self.output + rhs.output,
            cache_write_5m: self.cache_write_5m + rhs.cache_write_5m,
            cache_write_1h: self.cache_write_1h + rhs.cache_write_1h,
            cache_read: self.cache_read + rhs.cache_read,
        }
    }
}

impl std::ops::AddAssign for TokenTotals {
    fn add_assign(&mut self, rhs: TokenTotals) {
        *self = *self + rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        // Both "claude-opus" and "claude-opus-5" match; the longer one must win,
        // otherwise Opus 5 gets billed at Opus 3's rate.
        assert_eq!(rate("claude-opus-5-20260101").input, 5.0);
        assert_eq!(rate("claude-opus-4-20250101").input, 15.0);
        assert_eq!(rate("claude-opus-5[1m]").input, 5.0);
    }

    #[test]
    fn unknown_model_falls_back() {
        assert_eq!(rate("gpt-9").input, FALLBACK.input);
    }

    #[test]
    fn cache_reads_are_a_tenth_of_input() {
        // The whole accuracy argument in one assertion: 1M cache reads on Opus 5
        // cost $0.50, not $5.00.
        let tokens = TokenTotals {
            cache_read: 1_000_000,
            ..Default::default()
        };
        assert!((cost(&tokens, "claude-opus-5") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn write_ttls_are_priced_apart() {
        let five = TokenTotals {
            cache_write_5m: 1_000_000,
            ..Default::default()
        };
        let hour = TokenTotals {
            cache_write_1h: 1_000_000,
            ..Default::default()
        };
        assert!((cost(&five, "claude-opus-5") - 6.25).abs() < 1e-9);
        assert!((cost(&hour, "claude-opus-5") - 10.0).abs() < 1e-9);
    }

    #[test]
    fn fresh_excludes_cache_replay() {
        let t = TokenTotals {
            input: 1,
            output: 2,
            cache_write_5m: 4,
            cache_write_1h: 8,
            cache_read: 1000,
        };
        assert_eq!(t.fresh(), 15);
        assert_eq!(t.billable(), 1015);
    }
}
