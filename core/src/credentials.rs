//! OAuth credentials issued to Claude Code.
//!
//! Runway never mints or refreshes tokens — Claude Code owns that lifecycle. We
//! re-read the source on every poll so that a refresh performed by the CLI is
//! picked up automatically on the next tick.
//!
//! Storage differs by platform, and Windows is the easy one:
//!
//! | macOS   | login keychain, generic password `Claude Code-credentials` |
//! | Windows | `%USERPROFILE%\.claude\.credentials.json`, plain file       |
//! | Linux   | `~/.claude/.credentials.json`, plain file                  |
//!
//! The file is tried on every platform regardless, because macOS falls back to
//! it when the keychain is unavailable (CI, headless, some SSH sessions).

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::paths;

#[derive(Clone, Debug, PartialEq)]
pub struct OAuthCredentials {
    pub access_token: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub subscription_type: Option<String>,
    pub rate_limit_tier: Option<String>,
}

impl OAuthCredentials {
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|e| e <= Utc::now())
    }
}

#[derive(Debug)]
pub enum CredentialsError {
    NotFound,
    Malformed(String),
    KeychainDenied(i32),
}

impl fmt::Display for CredentialsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialsError::NotFound => write!(
                f,
                "No Claude Code credentials found. Run `claude` once to sign in."
            ),
            CredentialsError::Malformed(detail) => {
                write!(f, "Credentials could not be read: {detail}")
            }
            CredentialsError::KeychainDenied(status) => write!(
                f,
                "Keychain access was denied (OSStatus {status}). Allow Runway to read the \u{201c}Claude Code-credentials\u{201d} item."
            ),
        }
    }
}

impl std::error::Error for CredentialsError {}

pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Keychain first on macOS (that's where Claude Code puts it), then the on-disk
/// file the CLI uses everywhere else.
pub fn load() -> Result<OAuthCredentials, CredentialsError> {
    #[cfg(target_os = "macos")]
    {
        match keychain_data() {
            Ok(Some(data)) => return parse(&data),
            Ok(None) => {}
            Err(e) => {
                // A denied prompt shouldn't hide a perfectly good file fallback,
                // but if there's no file either, report the keychain problem —
                // it's the actionable one.
                if let Some(data) = file_data() {
                    return parse(&data);
                }
                return Err(e);
            }
        }
    }

    match file_data() {
        Some(data) => parse(&data),
        None => Err(CredentialsError::NotFound),
    }
}

pub fn credential_file_candidates() -> Vec<PathBuf> {
    let mut urls = Vec::new();
    if let Ok(override_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !override_dir.is_empty() {
            urls.push(PathBuf::from(override_dir).join(".credentials.json"));
        }
    }
    urls.push(paths::claude_home().join(".credentials.json"));
    urls
}

fn file_data() -> Option<Vec<u8>> {
    credential_file_candidates()
        .into_iter()
        .find_map(|path| std::fs::read(path).ok())
}

#[cfg(target_os = "macos")]
fn keychain_data() -> Result<Option<Vec<u8>>, CredentialsError> {
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};

    let result = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .load_data(true)
        .limit(Limit::Max(1))
        .search();

    match result {
        Ok(items) => {
            for item in items {
                if let SearchResult::Data(data) = item {
                    return Ok(Some(data));
                }
            }
            Ok(None)
        }
        Err(e) => {
            const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
            if e.code() == ERR_SEC_ITEM_NOT_FOUND {
                Ok(None)
            } else {
                Err(CredentialsError::KeychainDenied(e.code()))
            }
        }
    }
}

fn parse(data: &[u8]) -> Result<OAuthCredentials, CredentialsError> {
    let root: serde_json::Value = serde_json::from_slice(data)
        .map_err(|_| CredentialsError::Malformed("payload is not JSON".into()))?;
    let oauth = root
        .get("claudeAiOauth")
        .ok_or_else(|| CredentialsError::Malformed("missing claudeAiOauth".into()))?;

    let token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CredentialsError::Malformed("missing accessToken".into()))?;

    // `expiresAt` is milliseconds since epoch.
    let expires_at = oauth
        .get("expiresAt")
        .and_then(|v| v.as_f64())
        .and_then(|ms| Utc.timestamp_millis_opt(ms as i64).single());

    let string = |key: &str| {
        oauth
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    Ok(OAuthCredentials {
        access_token: token.to_string(),
        expires_at,
        subscription_type: string("subscriptionType"),
        rate_limit_tier: string("rateLimitTier"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_cli_payload() {
        let json = br#"{"claudeAiOauth":{"accessToken":"sk-tok","expiresAt":1785000000000,"subscriptionType":"max","rateLimitTier":"default"}}"#;
        let c = parse(json).unwrap();
        assert_eq!(c.access_token, "sk-tok");
        assert_eq!(c.subscription_type.as_deref(), Some("max"));
        assert_eq!(
            c.expires_at.unwrap().to_rfc3339(),
            "2026-07-25T17:20:00+00:00"
        );
    }

    #[test]
    fn rejects_payloads_we_cannot_use() {
        assert!(matches!(
            parse(b"not json"),
            Err(CredentialsError::Malformed(_))
        ));
        assert!(matches!(parse(b"{}"), Err(CredentialsError::Malformed(_))));
        assert!(matches!(
            parse(br#"{"claudeAiOauth":{"accessToken":""}}"#),
            Err(CredentialsError::Malformed(_))
        ));
    }

    #[test]
    fn missing_expiry_is_not_expired() {
        let c = parse(br#"{"claudeAiOauth":{"accessToken":"t"}}"#).unwrap();
        assert!(!c.is_expired());
    }

    #[test]
    fn past_expiry_is_expired() {
        let c =
            parse(br#"{"claudeAiOauth":{"accessToken":"t","expiresAt":1000000000000}}"#).unwrap();
        assert!(c.is_expired());
    }
}
