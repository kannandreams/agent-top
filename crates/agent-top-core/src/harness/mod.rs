//! Per-harness transcript readers.
//!
//! Each harness writes a different append-only log. A `SessionTracker` turns
//! one of those logs into the harness-neutral `SessionSummary` incrementally.

pub mod claude;
pub mod codex;

use crate::model::{Activity, Harness, TokenUsage};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Default)]
pub struct SessionSummary {
    pub harness: Option<Harness>,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub harness_version: Option<String>,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub unpriced_tokens: u64,
    pub turns: u64,
    pub subagent_turns: u64,
    pub tool_calls: u64,
    pub activity: Activity,
    pub started_at: Option<SystemTime>,
    pub last_activity: Option<SystemTime>,
}

pub trait SessionTracker {
    /// Ingest whatever was appended since the last call. Returns true when
    /// there is still unread data (the byte budget was exhausted).
    fn refresh(&mut self) -> anyhow::Result<bool>;
    fn summary(&self) -> &SessionSummary;
    fn path(&self) -> &Path;
}

/// Bytes ingested per tracker per refresh. Keeps a cold start on a 100 MB
/// transcript from freezing the first frame; the rest streams in on later ticks.
pub const REFRESH_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Parse an RFC 3339 timestamp like `2026-09-03T07:15:34.123Z` into SystemTime
/// without pulling in a date crate. Only the UTC `Z` form is handled, which is
/// what both Claude Code and Codex write.
pub fn parse_rfc3339_utc(s: &str) -> Option<SystemTime> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?.parse::<i64>().ok()?, d.next()?.parse::<u32>().ok()?, d.next()?.parse::<u32>().ok()?);
    let mut t = time.split(':');
    let (h, mi) = (t.next()?.parse::<u64>().ok()?, t.next()?.parse::<u64>().ok()?);
    let sec_str = t.next()?;
    let (sec, frac) = match sec_str.split_once('.') {
        Some((s, f)) => (s.parse::<u64>().ok()?, f),
        None => (sec_str.parse::<u64>().ok()?, ""),
    };
    let nanos: u32 = if frac.is_empty() {
        0
    } else {
        let mut f = frac.to_string();
        f.truncate(9);
        while f.len() < 9 {
            f.push('0');
        }
        f.parse().ok()?
    };
    let days = days_from_civil(y, mo, da);
    let secs = days * 86_400 + (h * 3600 + mi * 60 + sec) as i64;
    if secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos))
}

// Howard Hinnant's days-from-civil algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_timestamps() {
        let t = parse_rfc3339_utc("1970-01-02T00:00:00.000Z").unwrap();
        assert_eq!(t, SystemTime::UNIX_EPOCH + Duration::from_secs(86_400));
        let t = parse_rfc3339_utc("2026-09-03T07:15:34.5Z").unwrap();
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(secs.as_secs(), 1_788_419_734);
        assert_eq!(secs.subsec_millis(), 500);
        assert!(parse_rfc3339_utc("nope").is_none());
    }
}
