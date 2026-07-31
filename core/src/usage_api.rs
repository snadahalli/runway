//! Talks to the OAuth usage endpoint that backs Claude Code's `/usage` command.
//!
//! Two details matter and are easy to get wrong:
//!
//! 1. `anthropic-beta: oauth-2025-04-20` is required — OAuth tokens are sent on
//!    `Authorization: Bearer`, not `x-api-key`.
//! 2. `User-Agent: claude-code/<version>` is required to land in the normal
//!    rate-limit bucket. Without it you get a far stricter bucket and persistent
//!    429s, which is the single most common way monitors like this break.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::snapshot::iso8601;

// MARK: - Wire types

fn de_date<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
    let raw = Option::<String>::deserialize(d)?;
    Ok(raw.as_deref().and_then(iso8601::parse))
}

#[derive(Debug, Default, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub utilization: f64,
    #[serde(rename = "resets_at", default, deserialize_with = "de_date")]
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScopeModel {
    pub id: Option<String>,
    #[serde(rename = "display_name")]
    pub display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Scope {
    pub model: Option<ScopeModel>,
    pub surface: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Limit {
    pub kind: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub percent: f64,
    pub severity: Option<String>,
    #[serde(rename = "resets_at", default, deserialize_with = "de_date")]
    pub resets_at: Option<DateTime<Utc>>,
    pub scope: Option<Scope>,
    #[serde(rename = "is_active")]
    pub is_active: Option<bool>,
}

/// Response of `GET /api/oauth/usage`.
///
/// The `limits` array is the forward-compatible surface — it carries one entry
/// per active limit with a `kind`/`group`/`scope`, so new limit kinds appear
/// without a schema change. The legacy `five_hour` / `seven_day` objects are
/// decoded as a fallback for older server builds.
#[derive(Debug, Default, Deserialize)]
pub struct UsageResponse {
    #[serde(rename = "five_hour")]
    pub five_hour: Option<Window>,
    #[serde(rename = "seven_day")]
    pub seven_day: Option<Window>,
    pub limits: Option<Vec<Limit>>,
}

// MARK: - Client

#[derive(Debug)]
pub enum UsageAPIError {
    RateLimited { retry_after: Option<f64> },
    Unauthorized,
    Http(u16, Option<String>),
    Transport(String),
    Decoding(String),
}

impl fmt::Display for UsageAPIError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageAPIError::RateLimited {
                retry_after: Some(r),
            } => {
                write!(
                    f,
                    "Rate limited by the usage API. Retrying in {}s.",
                    *r as i64
                )
            }
            UsageAPIError::RateLimited { retry_after: None } => {
                write!(f, "Rate limited by the usage API.")
            }
            UsageAPIError::Unauthorized => {
                write!(
                    f,
                    "Credentials rejected. Run `claude` to refresh your login."
                )
            }
            UsageAPIError::Http(code, body) => {
                write!(f, "Usage API returned HTTP {code}.")?;
                if let Some(body) = body {
                    write!(f, " {body}")?;
                }
                Ok(())
            }
            UsageAPIError::Transport(e) => write!(f, "Network error: {e}"),
            UsageAPIError::Decoding(e) => write!(f, "Could not decode the usage response: {e}"),
        }
    }
}

impl UsageAPIError {
    pub fn is_retryable(&self) -> bool {
        match self {
            UsageAPIError::RateLimited { .. } | UsageAPIError::Transport(_) => true,
            UsageAPIError::Http(code, _) => *code >= 500,
            UsageAPIError::Unauthorized | UsageAPIError::Decoding(_) => false,
        }
    }
}

pub const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";

/// The endpoint is documented as safe at 180s intervals with a correct
/// User-Agent. We never poll faster than this, regardless of settings.
pub const MINIMUM_POLL_INTERVAL: f64 = 180.0;

pub struct UsageAPIClient {
    pub user_agent_version: String,
    agent: ureq::Agent,
}

impl UsageAPIClient {
    pub fn new(user_agent_version: impl Into<String>) -> Self {
        UsageAPIClient {
            user_agent_version: user_agent_version.into(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(20))
                .build(),
        }
    }

    pub fn fetch(&self, token: &str) -> Result<UsageResponse, UsageAPIError> {
        let response = self
            .agent
            .get(ENDPOINT)
            .set("Authorization", &format!("Bearer {token}"))
            .set("anthropic-beta", "oauth-2025-04-20")
            .set(
                "User-Agent",
                &format!("claude-code/{}", self.user_agent_version),
            )
            .set("Accept", "application/json")
            .set("Cache-Control", "no-cache")
            .call();

        match response {
            Ok(resp) => resp
                .into_json::<UsageResponse>()
                .map_err(|e| UsageAPIError::Decoding(e.to_string())),
            Err(ureq::Error::Status(401 | 403, _)) => Err(UsageAPIError::Unauthorized),
            Err(ureq::Error::Status(429, resp)) => {
                let retry_after = resp
                    .header("retry-after")
                    .and_then(|v| v.parse::<f64>().ok());
                Err(UsageAPIError::RateLimited { retry_after })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp
                    .into_string()
                    .ok()
                    .map(|s| s.chars().take(300).collect());
                Err(UsageAPIError::Http(code, body))
            }
            Err(ureq::Error::Transport(t)) => Err(UsageAPIError::Transport(t.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_limits_array() {
        let json = r#"{"limits":[
            {"kind":"session","group":"default","percent":41.5,"resets_at":"2026-07-31T09:00:00Z","is_active":true},
            {"kind":"weekly_scoped","group":"g","percent":12.0,"resets_at":"2026-08-04T00:00:00.000Z",
             "scope":{"model":{"id":"claude-fable-5","display_name":"Fable"}}}
        ]}"#;
        let r: UsageResponse = serde_json::from_str(json).unwrap();
        let limits = r.limits.unwrap();
        assert_eq!(limits[0].percent, 41.5);
        assert_eq!(limits[0].is_active, Some(true));
        // Both fractional and whole-second timestamps show up in the wild.
        assert!(limits[1].resets_at.is_some());
        assert_eq!(
            limits[1]
                .scope
                .as_ref()
                .unwrap()
                .model
                .as_ref()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Fable")
        );
    }

    #[test]
    fn decodes_the_legacy_shape() {
        let json = r#"{"five_hour":{"utilization":33.0,"resets_at":"2026-07-31T09:00:00Z"},
                       "seven_day":{"utilization":10.0}}"#;
        let r: UsageResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.five_hour.unwrap().utilization, 33.0);
        assert!(r.seven_day.unwrap().resets_at.is_none());
    }

    /// New limit kinds must not break decoding — that's the whole point of the
    /// `limits` array being the forward-compatible surface.
    #[test]
    fn unknown_kinds_and_extra_fields_survive() {
        let json = r#"{"limits":[{"kind":"monthly_experimental","percent":5.0,"brand_new_field":1}],
                       "some_future_top_level":{"x":1}}"#;
        let r: UsageResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.limits.unwrap()[0].kind, "monthly_experimental");
    }

    #[test]
    fn retry_classification() {
        assert!(UsageAPIError::RateLimited { retry_after: None }.is_retryable());
        assert!(UsageAPIError::Http(503, None).is_retryable());
        assert!(!UsageAPIError::Http(400, None).is_retryable());
        assert!(!UsageAPIError::Unauthorized.is_retryable());
    }
}
