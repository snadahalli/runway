//! Incrementally reads `~/.claude/projects/**/*.jsonl`.
//!
//! This is the half of Runway that needs no network at all. It gives us
//! per-model, per-project and per-session attribution, and — critically — it
//! lets the UI keep moving between the 180-second API polls without spending
//! extra requests against a rate-limited endpoint.
//!
//! Reads are incremental: we remember a byte offset per file and only parse what
//! was appended since last time. A file that shrank was rotated, so we re-read it
//! from the start.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::paths;
use crate::pricing::{self, TokenTotals};

/// A single billed assistant turn, recovered from Claude Code's local logs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Message id + request id, for dedupe.
    pub id: String,
    #[serde(with = "crate::compat")]
    pub date: DateTime<Utc>,
    pub model: String,
    /// Last path component of `cwd`.
    pub project: String,
    #[serde(rename = "projectPath")]
    pub project_path: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub tokens: TokenTotals,
}

impl UsageRecord {
    pub fn cost(&self) -> f64 {
        pricing::cost(&self.tokens, &self.model)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Cursor {
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScanState {
    #[serde(default)]
    pub cursors: HashMap<String, Cursor>,
    #[serde(default)]
    pub records: Vec<UsageRecord>,
    #[serde(rename = "lastSeenCLIVersion", default)]
    pub last_seen_cli_version: Option<String>,
}

pub struct TranscriptScanner {
    root: PathBuf,
    state_path: PathBuf,
    pub state: ScanState,
}

/// Records older than this are dropped; everything the UI shows is derived from
/// what's retained, so this bounds both memory and the state file.
pub const RETENTION_SECONDS: i64 = 30 * 24 * 3600;

impl TranscriptScanner {
    pub fn new(root: PathBuf, state_path: PathBuf) -> Self {
        let state = std::fs::read(&state_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ScanState>(&bytes).ok())
            .unwrap_or_default();
        TranscriptScanner {
            root,
            state_path,
            state,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(paths::projects_dir(), paths::scan_state_path())
    }

    /// Parses everything appended since the last call. Returns the new records.
    pub fn scan(&mut self) -> Vec<UsageRecord> {
        let files = self.transcript_files();
        let mut fresh: Vec<UsageRecord> = Vec::new();
        let mut known: HashSet<String> = self.state.records.iter().map(|r| r.id.clone()).collect();

        for file in &files {
            let key = file.to_string_lossy().to_string();
            let Ok(meta) = std::fs::metadata(file) else {
                continue;
            };
            let size = meta.len();

            let mut cursor = self.state.cursors.get(&key).copied().unwrap_or_default();
            if size < cursor.offset {
                cursor = Cursor::default(); // rotated
            }
            if size == cursor.offset {
                continue; // unchanged
            }

            let Ok(mut handle) = std::fs::File::open(file) else {
                continue;
            };
            if handle.seek(SeekFrom::Start(cursor.offset)).is_err() {
                continue;
            }
            let mut data = Vec::new();
            if handle.read_to_end(&mut data).is_err() {
                continue;
            }

            // Only consume up to the last complete line; a partial trailing line
            // means Claude Code is mid-write and we'll get it next tick.
            let Some(last_newline) = data.iter().rposition(|b| *b == b'\n') else {
                continue;
            };
            let complete = &data[..=last_newline];

            for line in complete.split(|b| *b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                if let Some(record) = parse_line(line) {
                    if known.insert(record.id.clone()) {
                        fresh.push(record);
                    }
                }
            }

            cursor.offset += complete.len() as u64;
            cursor.size = size;
            self.state.cursors.insert(key, cursor);
        }

        if !fresh.is_empty() {
            self.state.records.extend(fresh.iter().cloned());
        }

        self.prune(&files);
        self.persist();
        fresh
    }

    fn prune(&mut self, files: &[PathBuf]) {
        let cutoff = Utc::now() - Duration::seconds(RETENTION_SECONDS);
        self.state.records.retain(|r| r.date >= cutoff);
        self.state.records.sort_by_key(|r| r.date);

        // Forget cursors for files that no longer exist.
        let existing: HashSet<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        self.state.cursors.retain(|k, _| existing.contains(k));
    }

    fn persist(&self) {
        if let Ok(bytes) = serde_json::to_vec(&self.state) {
            let _ = paths::write_atomic(&self.state_path, &bytes);
        }
    }

    fn transcript_files(&self) -> Vec<PathBuf> {
        WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
            .collect()
    }

    /// The CLI version seen most recently in the logs, used to build the
    /// `User-Agent` the usage endpoint expects. Avoids shelling out to `claude`.
    pub fn detect_cli_version(&self) -> Option<String> {
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = self
            .transcript_files()
            .into_iter()
            .map(|p| {
                let modified = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (modified, p)
            })
            .collect();
        // Most recently written first — the newest transcript has the version
        // of the CLI you're running right now.
        files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

        for (_, file) in files.iter().take(3) {
            let Ok(data) = std::fs::read(file) else {
                continue;
            };
            for line in data.split(|b| *b == b'\n').rev().take(200) {
                if line.is_empty() {
                    continue;
                }
                let Ok(obj) = serde_json::from_slice::<serde_json::Value>(line) else {
                    continue;
                };
                if let Some(version) = obj.get("version").and_then(|v| v.as_str()) {
                    if !version.is_empty() {
                        return Some(version.to_string());
                    }
                }
            }
        }
        None
    }
}

/// Pulls the usage block out of one assistant record. Returns `None` for the
/// ~70% of lines that carry no billing information (user turns, attachments,
/// hooks, mode changes, file-history snapshots …).
pub fn parse_line(line: &[u8]) -> Option<UsageRecord> {
    let obj: serde_json::Value = serde_json::from_slice(line).ok()?;
    if obj.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = obj.get("message")?;
    let usage = message.get("usage")?;
    let model = message.get("model")?.as_str()?.to_string();

    let message_id = message.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let request_id = obj.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
    if message_id.is_empty() && request_id.is_empty() {
        return None;
    }

    let timestamp = obj
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(crate::snapshot::iso8601::parse)
        .unwrap_or_else(Utc::now);

    let cwd = obj.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    let project = if cwd.is_empty() {
        "unknown".to_string()
    } else {
        last_path_component(cwd)
    };

    let int = |v: &serde_json::Value, key: &str| -> i64 {
        v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
    };

    let mut tokens = TokenTotals {
        input: int(usage, "input_tokens"),
        output: int(usage, "output_tokens"),
        cache_read: int(usage, "cache_read_input_tokens"),
        ..Default::default()
    };

    // Prefer the TTL-split breakdown so 5m and 1h writes get their own rate;
    // fall back to the flat total when the split isn't present.
    if let Some(split) = usage.get("cache_creation") {
        tokens.cache_write_5m = int(split, "ephemeral_5m_input_tokens");
        tokens.cache_write_1h = int(split, "ephemeral_1h_input_tokens");
    } else {
        tokens.cache_write_5m = int(usage, "cache_creation_input_tokens");
    }

    // A record with no tokens at all is a control message, not a billed turn.
    if tokens.billable() <= 0 {
        return None;
    }

    Some(UsageRecord {
        id: format!("{message_id}|{request_id}"),
        date: timestamp,
        model,
        project,
        project_path: cwd.to_string(),
        session_id: obj
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        tokens,
    })
}

/// `cwd` is written by whichever platform Claude Code ran on, so a macOS-style
/// path can turn up in a state file read on Windows and vice versa. Split on
/// both separators rather than trusting `Path`, which only knows about the
/// host's convention and would return `C:\work\repo` whole on Unix.
fn last_path_component(cwd: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

// MARK: - Aggregation

pub fn within(
    records: &[UsageRecord],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<&UsageRecord> {
    // Swift's DateInterval.contains is inclusive of both ends.
    records
        .iter()
        .filter(|r| r.date >= start && r.date <= end)
        .collect()
}

pub fn since(records: &[UsageRecord], start: DateTime<Utc>) -> Vec<&UsageRecord> {
    records.iter().filter(|r| r.date >= start).collect()
}

pub fn total_tokens(records: &[&UsageRecord]) -> TokenTotals {
    records
        .iter()
        .fold(TokenTotals::default(), |acc, r| acc + r.tokens)
}

pub fn total_cost(records: &[&UsageRecord]) -> f64 {
    records.iter().map(|r| r.cost()).sum()
}

pub struct Bucket {
    pub name: String,
    pub tokens: TokenTotals,
    pub cost: f64,
}

/// Grouped totals, heaviest first.
pub fn breakdown<F>(records: &[&UsageRecord], key: F) -> Vec<Bucket>
where
    F: Fn(&UsageRecord) -> String,
{
    let mut buckets: HashMap<String, (TokenTotals, f64)> = HashMap::new();
    for record in records {
        let entry = buckets
            .entry(key(record))
            .or_insert((TokenTotals::default(), 0.0));
        entry.0 += record.tokens;
        entry.1 += record.cost();
    }
    let mut out: Vec<Bucket> = buckets
        .into_iter()
        .map(|(name, (tokens, cost))| Bucket { name, tokens, cost })
        .collect();
    out.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

pub fn projects_root_exists(root: &Path) -> bool {
    root.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSISTANT: &str = r#"{"type":"assistant","timestamp":"2026-07-30T10:00:00.500Z","cwd":"/Users/dev/work/runway","sessionId":"s1","requestId":"req_1","version":"2.1.220","message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":5000,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":7}}}}"#;

    #[test]
    fn parses_a_billed_turn() {
        let r = parse_line(ASSISTANT.as_bytes()).expect("should parse");
        assert_eq!(r.id, "msg_1|req_1");
        assert_eq!(r.model, "claude-opus-5");
        assert_eq!(r.project, "runway");
        assert_eq!(r.tokens.cache_read, 5000);
        assert_eq!(r.tokens.cache_write_5m, 100);
        assert_eq!(r.tokens.cache_write_1h, 7);
        // Fractional-second timestamps are what Claude Code actually writes.
        assert_eq!(r.date.to_rfc3339(), "2026-07-30T10:00:00.500+00:00");
    }

    #[test]
    fn ignores_lines_that_are_not_billed_turns() {
        for line in [
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"assistant","message":{"model":"m"}}"#,
            r#"not json at all"#,
            r#"{"type":"assistant","requestId":"r","message":{"id":"m","model":"claude-opus-5","usage":{"input_tokens":0,"output_tokens":0}}}"#,
        ] {
            assert!(parse_line(line.as_bytes()).is_none(), "should skip: {line}");
        }
    }

    #[test]
    fn flat_cache_creation_falls_back_to_the_5m_rate() {
        let line = r#"{"type":"assistant","requestId":"r","message":{"id":"m","model":"claude-opus-5","usage":{"cache_creation_input_tokens":42}}}"#;
        let r = parse_line(line.as_bytes()).unwrap();
        assert_eq!(r.tokens.cache_write_5m, 42);
        assert_eq!(r.tokens.cache_write_1h, 0);
    }

    /// A state file synced from a Windows machine, read on Unix, must still
    /// attribute to `repo` rather than the whole drive path.
    #[test]
    fn windows_paths_attribute_correctly() {
        assert_eq!(last_path_component(r"C:\Users\sn\work\repo"), "repo");
        assert_eq!(last_path_component("/Users/dev/work/repo"), "repo");
        assert_eq!(last_path_component("/Users/dev/work/repo/"), "repo");
        assert_eq!(last_path_component(""), "unknown");
    }

    #[test]
    fn breakdown_sorts_by_cost_descending() {
        let mk = |id: &str, model: &str, out: i64| UsageRecord {
            id: id.into(),
            date: Utc::now(),
            model: model.into(),
            project: "p".into(),
            project_path: "/p".into(),
            session_id: "s".into(),
            tokens: TokenTotals {
                output: out,
                ..Default::default()
            },
        };
        let records = [
            mk("a", "claude-haiku-4-5", 1_000_000),
            mk("b", "claude-opus-5", 1_000_000),
        ];
        let refs: Vec<&UsageRecord> = records.iter().collect();
        let out = breakdown(&refs, |r| pricing::family(&r.model));
        assert_eq!(out[0].name, "Opus");
        assert_eq!(out[1].name, "Haiku");
    }

    #[test]
    fn incremental_scan_does_not_double_count() {
        let dir = std::env::temp_dir().join(format!("runway-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let log = dir.join("proj/session.jsonl");
        let state = dir.join("state.json");

        std::fs::write(&log, format!("{ASSISTANT}\n")).unwrap();
        let mut scanner = TranscriptScanner::new(dir.clone(), state.clone());
        assert_eq!(scanner.scan().len(), 1);
        // Nothing appended: no new records, and no re-parse of what we've seen.
        assert_eq!(scanner.scan().len(), 0);
        assert_eq!(scanner.state.records.len(), 1);

        // A second turn, plus a partial line still being written.
        let second = ASSISTANT
            .replace("msg_1", "msg_2")
            .replace("req_1", "req_2");
        std::fs::write(&log, format!("{ASSISTANT}\n{second}\n{{\"type\":\"assis")).unwrap();
        assert_eq!(scanner.scan().len(), 1);
        assert_eq!(scanner.state.records.len(), 2);

        // The partial line completes on the next tick and is picked up then.
        let third = ASSISTANT
            .replace("msg_1", "msg_3")
            .replace("req_1", "req_3");
        std::fs::write(&log, format!("{ASSISTANT}\n{second}\n{third}\n")).unwrap();
        assert_eq!(scanner.scan().len(), 1);
        assert_eq!(scanner.state.records.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rotated_file_is_reread_from_the_start() {
        let dir = std::env::temp_dir().join(format!("runway-rot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.jsonl");
        let state = dir.join("state.json");

        let second = ASSISTANT
            .replace("msg_1", "msg_2")
            .replace("req_1", "req_2");
        std::fs::write(&log, format!("{ASSISTANT}\n{second}\n")).unwrap();
        let mut scanner = TranscriptScanner::new(dir.clone(), state);
        assert_eq!(scanner.scan().len(), 2);

        // Truncated to something shorter than our cursor.
        let third = ASSISTANT
            .replace("msg_1", "msg_3")
            .replace("req_1", "req_3");
        std::fs::write(&log, format!("{third}\n")).unwrap();
        // Re-read from zero; msg_3 is new, and the already-known ids stay deduped.
        assert_eq!(scanner.scan().len(), 1);
        assert_eq!(scanner.state.records.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
