//! `agent-top report`: what the agents have cost, across every harness, read
//! from the transcripts already on disk.
//!
//! The live table is one moment; this is the history. Each harness keeps its
//! finished sessions (Claude's project transcripts, Codex's rollouts, Gemini's
//! chat files, OpenCode's database), so the same adapters that build a live row
//! also read a month-old one. This walks all of them inside a window, folds
//! them into a `SessionSummary` each, and totals the cost and tokens grouped by
//! harness, model, project or day. Nothing is written and nothing leaves the
//! machine; it reads the same files the live view does.

use agent_top_core::Harness;
use agent_top_core::harness::SessionSummary;
use agent_top_core::harness::{self, SpanRetention};
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GroupBy {
    /// One row per harness.
    Harness,
    /// One row per model id.
    Model,
    /// One row per working directory (its last two path components).
    Project,
    /// One row per calendar day (UTC) of last activity.
    Day,
}

impl GroupBy {
    fn label(self) -> &'static str {
        match self {
            GroupBy::Harness => "harness",
            GroupBy::Model => "model",
            GroupBy::Project => "project",
            GroupBy::Day => "day",
        }
    }
}

/// Parse `--since`: `all`, a duration like `12h`, `7d`, `2w`, or a date
/// `YYYY-MM-DD` (interpreted as UTC midnight).
pub fn parse_since(s: &str) -> Result<SystemTime> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok(UNIX_EPOCH);
    }
    if let Some((y, mo, d)) = parse_date(s) {
        let days = days_from_civil(y, mo, d);
        if days < 0 {
            bail!("date {s} is before 1970");
        }
        return Ok(UNIX_EPOCH + Duration::from_secs(days as u64 * 86_400));
    }
    if let Some(dur) = s.strip_suffix('h').and_then(|n| n.parse::<u64>().ok()) {
        return since_ago(dur * 3600);
    }
    if let Some(dur) = s.strip_suffix('d').and_then(|n| n.parse::<u64>().ok()) {
        return since_ago(dur * 86_400);
    }
    if let Some(dur) = s.strip_suffix('w').and_then(|n| n.parse::<u64>().ok()) {
        return since_ago(dur * 7 * 86_400);
    }
    bail!("--since wants `all`, a duration like 7d / 12h / 2w, or a date YYYY-MM-DD, not {s:?}");
}

fn since_ago(secs: u64) -> Result<SystemTime> {
    Ok(SystemTime::now().checked_sub(Duration::from_secs(secs)).unwrap_or(UNIX_EPOCH))
}

fn parse_date(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.split('-');
    let y = it.next()?.parse().ok()?;
    let mo = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, mo, d))
}

/// One accumulated group: how many sessions and what they came to.
#[derive(Default, Clone)]
struct Bucket {
    sessions: u64,
    tokens: u64,
    cost: f64,
    unpriced: u64,
    turns: u64,
    tool_calls: u64,
}

impl Bucket {
    fn add(&mut self, s: &SessionSummary) {
        self.sessions += 1;
        self.tokens += s.usage.total();
        self.cost += s.cost_usd;
        self.unpriced += s.unpriced_tokens;
        self.turns += s.turns;
        self.tool_calls += s.tool_calls;
    }
}

/// The whole report, ready to print or serialise.
pub struct Report {
    since: SystemTime,
    by: GroupBy,
    groups: BTreeMap<String, Bucket>,
    total: Bucket,
    /// Sessions that parsed but fell outside the window, for the header count.
    scanned: u64,
}

/// Read every session across every harness whose last activity is at or after
/// `since`, and fold them into groups.
pub fn build(since: SystemTime, by: GroupBy) -> Report {
    let mut groups: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = Bucket::default();
    let mut scanned = 0;

    for adapter in harness::adapters() {
        let harness = adapter.harness();
        for (_id, path) in adapter.transcripts() {
            scanned += 1;
            // A transcript that is a real file and was last written before the
            // window is skipped without parsing it. A virtual path (OpenCode's
            // database rows) has no mtime, so it is read and judged by its
            // recorded activity.
            if let Ok(md) = std::fs::metadata(&path)
                && let Ok(m) = md.modified()
                && m < since
            {
                continue;
            }
            let Some(mut tracker) = harness::open_transcript(&path, harness, SpanRetention::Recent) else { continue };
            if tracker.refresh_all().is_err() {
                continue;
            }
            let s = tracker.summary();
            if s.last_activity.map(|t| t < since).unwrap_or(true) {
                continue;
            }
            if s.usage.total() == 0 && s.turns == 0 {
                continue;
            }
            let key = group_key(by, harness, s);
            groups.entry(key).or_default().add(s);
            total.add(s);
        }
    }
    Report { since, by, groups, total, scanned }
}

fn group_key(by: GroupBy, harness: Harness, s: &SessionSummary) -> String {
    match by {
        GroupBy::Harness => harness.label().to_string(),
        GroupBy::Model => s.model.clone().unwrap_or_else(|| "unknown".into()),
        GroupBy::Project => s.cwd.as_deref().map(project_name).unwrap_or_else(|| "unknown".into()),
        GroupBy::Day => {
            let (y, m, d) = date_utc(s.last_activity.unwrap_or(UNIX_EPOCH));
            format!("{y:04}-{m:02}-{d:02}")
        }
    }
}

/// The last two components of a path, so `/Users/me/code/app` reads as
/// `code/app` and two projects called `app` are still told apart.
fn project_name(p: &Path) -> String {
    let names: Vec<String> = p
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let tail = names.iter().rev().take(2).rev().cloned().collect::<Vec<_>>().join("/");
    if tail.is_empty() { p.to_string_lossy().into_owned() } else { tail }
}

impl Report {
    /// The report as a plain table, sorted by cost then tokens, with a total.
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("agent-top report · since {} · by {}\n\n", format_date(self.since), self.by.label()));
        out.push_str(&format!(
            "{:<22} {:>8} {:>10} {:>10} {:>10}\n",
            self.by.label().to_uppercase(),
            "SESSIONS",
            "TOKENS",
            "COST",
            "UNPRICED"
        ));

        let mut rows: Vec<(&String, &Bucket)> = self.groups.iter().collect();
        rows.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap_or(std::cmp::Ordering::Equal).then(b.1.tokens.cmp(&a.1.tokens)));
        for (k, b) in rows {
            out.push_str(&format!(
                "{:<22} {:>8} {:>10} {:>10} {:>10}\n",
                truncate(k, 22),
                b.sessions,
                tokens(b.tokens),
                cost(b.cost, b.unpriced),
                if b.unpriced > 0 { tokens(b.unpriced) } else { "-".into() }
            ));
        }
        out.push_str(&format!("{:-<64}\n", ""));
        let t = &self.total;
        out.push_str(&format!(
            "{:<22} {:>8} {:>10} {:>10} {:>10}\n",
            "total",
            t.sessions,
            tokens(t.tokens),
            cost(t.cost, t.unpriced),
            if t.unpriced > 0 { tokens(t.unpriced) } else { "-".into() }
        ));
        if t.unpriced > 0 {
            out.push_str(&format!(
                "\n{} tokens ran on models with no price in the table, so their cost is missing from the total.\nAdd those models to ~/.config/agent-top/prices.toml to include them; see `agent-top --prices`.\n",
                tokens(t.unpriced)
            ));
        }
        out
    }

    /// The report as JSON, for feeding somewhere else.
    pub fn to_json(&self) -> serde_json::Value {
        let group = |b: &Bucket| {
            serde_json::json!({
                "sessions": b.sessions, "tokens": b.tokens, "cost_usd": b.cost,
                "unpriced_tokens": b.unpriced, "turns": b.turns, "tool_calls": b.tool_calls,
            })
        };
        serde_json::json!({
            "since": format_date(self.since),
            "group_by": self.by.label(),
            "scanned": self.scanned,
            "groups": self.groups.iter().map(|(k, b)| (k.clone(), group(b))).collect::<serde_json::Map<_, _>>(),
            "total": group(&self.total),
        })
    }
}

/// USD, with a `+` when the figure is a floor because some tokens were unpriced.
fn cost(usd: f64, unpriced: u64) -> String {
    let floor = if unpriced > 0 { "+" } else { "" };
    format!("${usd:.2}{floor}")
}

fn tokens(n: u64) -> String {
    crate::format::tokens(n)
}

fn truncate(s: &str, n: usize) -> String {
    crate::format::truncate(s, n)
}

fn format_date(t: SystemTime) -> String {
    if t == UNIX_EPOCH {
        return "the beginning".into();
    }
    let (y, m, d) = date_utc(t);
    format!("{y:04}-{m:02}-{d:02}")
}

/// UTC calendar date of a `SystemTime`.
fn date_utc(t: SystemTime) -> (i64, u32, u32) {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    civil_from_days((secs / 86_400) as i64)
}

// Howard Hinnant's days-from-civil and its inverse, matching the one in
// `harness/mod.rs`; kept here so the report crate needs no date dependency.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_since_forms() {
        assert_eq!(parse_since("all").unwrap(), UNIX_EPOCH);
        let d = parse_since("2026-08-08").unwrap();
        assert_eq!(date_utc(d), (2026, 8, 8));
        assert!(parse_since("7d").unwrap() < SystemTime::now());
        assert!(parse_since("12h").unwrap() < SystemTime::now());
        assert!(parse_since("2w").unwrap() < SystemTime::now());
        assert!(parse_since("nonsense").is_err());
        assert!(parse_since("2026-13-01").is_err());
    }

    #[test]
    fn civil_dates_round_trip() {
        for &(y, m, d) in &[(1970, 1, 1), (2026, 9, 5), (2000, 2, 29), (2026, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn project_name_keeps_two_components() {
        assert_eq!(project_name(Path::new("/Users/me/code/app")), "code/app");
        assert_eq!(project_name(Path::new("/app")), "app");
    }
}
