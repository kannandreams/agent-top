//! `agent-top report`: what the agents have cost, across every harness, read
//! from the transcripts already on disk.
//!
//! The live table is one moment; this is the history. Each harness keeps its
//! finished sessions (Claude's project transcripts, Codex's rollouts, Gemini's
//! chat files, OpenCode's and Kodelet's databases), so the adapters that build a live row
//! also read a month-old one. This walks all of them inside a window, folds
//! them into a `SessionSummary` each, and totals the cost and tokens grouped by
//! harness, model, project or day. Nothing is written and nothing leaves the
//! machine; it reads the same files the live view does.
//!
//! When a local store exists, sessions whose transcripts are gone come from
//! it, and a transcript unchanged since it was synced is read from the store
//! instead of parsed again. The report never writes the store.

use agent_top_core::Harness;
use agent_top_core::energy::{self, Estimate, Range};
use agent_top_core::harness::SessionSummary;
use agent_top_core::harness::{self, SpanRetention};
use agent_top_core::model::{TokenUsage, project_name};
use agent_top_store::{StoredSession, WRITER_VERSION};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
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
    tool_calls_lower_bound: bool,
    prompt: u64,
    cache_read: u64,
    output: u64,
    /// The group's tokens by model, kept apart so an energy table can be
    /// applied per model when the report is printed. "" is no model.
    by_model: BTreeMap<String, TokenUsage>,
}

impl Bucket {
    /// The prompt tokens not read from cache: fresh input and cache writes.
    fn uncached_prompt(&self) -> u64 {
        self.prompt - self.cache_read
    }

    /// The group's estimate under `table`, model by model.
    fn estimate(&self, table: &energy::Table) -> Estimate {
        let mut e = Estimate::default();
        for (model, usage) in &self.by_model {
            e.add(&table.estimate((!model.is_empty()).then_some(model.as_str()), usage));
        }
        e
    }

    /// Share of the prompt served from cache across the group, when there is a
    /// meaningful amount of prompt to judge.
    fn cache_hit(&self) -> Option<f64> {
        (self.prompt >= 5_000).then(|| self.cache_read as f64 / self.prompt as f64)
    }
}

/// What the report needs from one session, parsed from its transcript or
/// read from the local store.
struct Session {
    harness: String,
    model: Option<String>,
    project: Option<String>,
    last_activity: Option<SystemTime>,
    usage: TokenUsage,
    cost_usd: f64,
    unpriced_tokens: u64,
    turns: u64,
    tool_calls: u64,
    tool_calls_lower_bound: bool,
}

impl Session {
    fn parsed(harness: Harness, s: &SessionSummary) -> Session {
        Session {
            harness: harness.label().to_string(),
            model: s.model.clone(),
            project: s.cwd.as_deref().map(project_name),
            last_activity: s.last_activity,
            usage: s.usage,
            cost_usd: s.cost_usd,
            unpriced_tokens: s.unpriced_tokens,
            turns: s.turns,
            tool_calls: s.tool_calls,
            tool_calls_lower_bound: s.tool_calls_lower_bound,
        }
    }

    fn stored(s: &StoredSession) -> Session {
        Session {
            harness: s.harness.clone(),
            model: s.model.clone(),
            project: s.project.clone(),
            last_activity: s.last_activity_ms.map(|ms| UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)),
            usage: s.usage,
            cost_usd: s.cost_usd,
            unpriced_tokens: s.unpriced_tokens,
            turns: s.turns,
            tool_calls: s.tool_calls,
            tool_calls_lower_bound: s.tool_calls_lower_bound,
        }
    }

    /// In the window, and did something: a session with no tokens and no
    /// turns is a file the harness opened and never used.
    fn counts(&self, since: SystemTime) -> bool {
        self.last_activity.is_some_and(|t| t >= since) && (self.usage.total() > 0 || self.turns > 0)
    }
}

/// Where the report read its sessions.
pub enum Source {
    /// The transcripts on disk; there is no store at this path.
    Transcripts { hint: bool },
    /// The transcripts and the store.
    Store {
        path: PathBuf,
        /// Counted sessions whose transcript is gone.
        only_in_store: u64,
        /// Counted sessions received by `serve --listen`.
        received: u64,
        /// Transcripts unchanged since sync, read from the store.
        reused: u64,
    },
}

impl Bucket {
    fn add(&mut self, s: &Session) {
        self.sessions += 1;
        self.tokens += s.usage.total();
        self.cost += s.cost_usd;
        self.unpriced += s.unpriced_tokens;
        self.turns += s.turns;
        self.tool_calls += s.tool_calls;
        self.tool_calls_lower_bound |= s.tool_calls_lower_bound;
        self.prompt += s.usage.prompt();
        self.cache_read += s.usage.cache_read;
        self.output += s.usage.output;
        let u = self.by_model.entry(s.model.clone().unwrap_or_default()).or_default();
        u.input += s.usage.input;
        u.cache_write_5m += s.usage.cache_write_5m;
        u.cache_write_1h += s.usage.cache_write_1h;
        u.cache_write_unsplit += s.usage.cache_write_unsplit;
        u.cache_read += s.usage.cache_read;
        u.output += s.usage.output;
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
    source: Source,
}

/// Read every session across every harness whose last activity is at or after
/// `since`, and fold them into groups. With a store, sessions whose transcript
/// is gone come from it, and an unchanged transcript is not parsed again.
/// `hint` adds the line that points at `agent-top sync` when there is no store.
pub fn build(since: SystemTime, by: GroupBy, store: Option<(PathBuf, Vec<StoredSession>)>, hint: bool) -> Report {
    build_with(harness::adapters(), since, by, store, hint)
}

fn build_with(
    adapters: Vec<Box<dyn harness::HarnessAdapter>>,
    since: SystemTime,
    by: GroupBy,
    store: Option<(PathBuf, Vec<StoredSession>)>,
    hint: bool,
) -> Report {
    let mut groups: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = Bucket::default();
    let mut scanned = 0;
    let (store_path, rows) = match store {
        Some((path, rows)) => (Some(path), rows),
        None => (None, Vec::new()),
    };
    let mut stored: HashMap<(String, String), StoredSession> =
        rows.into_iter().map(|s| ((s.harness.clone(), s.session_id.clone()), s)).collect();
    let mut reused = 0;
    let mut fold = |s: &Session| {
        if s.counts(since) {
            groups.entry(group_key(by, s)).or_default().add(s);
            total.add(s);
        }
    };

    for adapter in adapters {
        let harness = adapter.harness();
        for (id, path) in adapter.transcripts() {
            scanned += 1;
            let prior = stored.remove(&(harness.label().to_string(), id));
            // A transcript that is a real file and was last written before the
            // window is skipped without parsing it. A virtual path (a harness's
            // database rows) has no mtime, so it is read and judged by its
            // recorded activity.
            if let Ok(md) = std::fs::metadata(&path)
                && let Ok(m) = md.modified()
                && m < since
            {
                continue;
            }
            // Unchanged since this version synced it: the store already has
            // what parsing would produce.
            if let (Some(p), Some(st)) = (&prior, adapter.stamp(&path))
                && p.source_size == Some(st.size as i64)
                && p.source_mtime_ms == Some(st.mtime_ms)
                && p.synced_by_version.as_deref() == Some(WRITER_VERSION)
            {
                reused += 1;
                fold(&Session::stored(p));
                continue;
            }
            // Reuse the adapter's discovery/enrichment cache across sessions.
            let mut tracker = adapter.open(&path, SpanRetention::Recent);
            if tracker.refresh_all().is_err() {
                continue;
            }
            fold(&Session::parsed(harness, tracker.summary()));
        }
    }

    // What is left in the store has no transcript on disk any more.
    let (mut only_in_store, mut received) = (0, 0);
    for row in stored.values() {
        scanned += 1;
        let s = Session::stored(row);
        if s.counts(since) {
            if row.attribution == "telemetry" {
                received += 1;
            } else {
                only_in_store += 1;
            }
        }
        fold(&s);
    }
    let source = match store_path {
        Some(path) => Source::Store { path, only_in_store, received, reused },
        None => Source::Transcripts { hint },
    };
    Report { since, by, groups, total, scanned, source }
}

fn group_key(by: GroupBy, s: &Session) -> String {
    match by {
        GroupBy::Harness => s.harness.clone(),
        GroupBy::Model => s.model.clone().unwrap_or_else(|| "unknown".into()),
        GroupBy::Project => s.project.clone().unwrap_or_else(|| "unknown".into()),
        GroupBy::Day => {
            let (y, m, d) = date_utc(s.last_activity.unwrap_or(UNIX_EPOCH));
            format!("{y:04}-{m:02}-{d:02}")
        }
    }
}

impl Report {
    /// The report as a plain table, sorted by cost then tokens, with a total.
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("agent-top report · since {} · by {}\n\n", format_date(self.since), self.by.label()));
        out.push_str(&format!(
            "{:<22} {:>8} {:>10} {:>10} {:>7} {:>10}\n",
            self.by.label().to_uppercase(),
            "SESSIONS",
            "TOKENS",
            "COST",
            "CACHE",
            "UNPRICED"
        ));

        let mut rows: Vec<(&String, &Bucket)> = self.groups.iter().collect();
        rows.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap_or(std::cmp::Ordering::Equal).then(b.1.tokens.cmp(&a.1.tokens)));
        for (k, b) in rows {
            out.push_str(&format!(
                "{:<22} {:>8} {:>10} {:>10} {:>7} {:>10}\n",
                truncate(k, 22),
                b.sessions,
                tokens(b.tokens),
                cost(b.cost, b.unpriced),
                b.cache_hit().map(|r| format!("{:.0}%", r * 100.0)).unwrap_or_else(|| "-".into()),
                if b.unpriced > 0 { tokens(b.unpriced) } else { "-".into() }
            ));
        }
        out.push_str(&format!("{:-<72}\n", ""));
        let t = &self.total;
        out.push_str(&format!(
            "{:<22} {:>8} {:>10} {:>10} {:>7} {:>10}\n",
            "total",
            t.sessions,
            tokens(t.tokens),
            cost(t.cost, t.unpriced),
            t.cache_hit().map(|r| format!("{:.0}%", r * 100.0)).unwrap_or_else(|| "-".into()),
            if t.unpriced > 0 { tokens(t.unpriced) } else { "-".into() }
        ));
        if t.unpriced > 0 {
            out.push_str(&format!(
                "\n{} tokens ran on models with no price in the table, so their cost is missing from the total.\nAdd those models to ~/.config/agent-top/prices.toml to include them; see `agent-top --prices`.\n",
                tokens(t.unpriced)
            ));
        }
        match &self.source {
            Source::Store { path, only_in_store, received, .. } => {
                out.push_str(&format!("\nRead from the transcripts on disk and the local store at {}", path.display()));
                match only_in_store {
                    0 => {}
                    1 => out.push_str("; 1 session is only in the store, its transcript deleted"),
                    n => out.push_str(&format!("; {n} sessions are only in the store, their transcripts deleted")),
                }
                match received {
                    0 => {}
                    1 => out.push_str("; 1 session was received as telemetry"),
                    n => out.push_str(&format!("; {n} sessions were received as telemetry")),
                }
                out.push_str(".\n");
            }
            Source::Transcripts { hint: true } => {
                out.push_str("\nRead from the transcripts on disk. `agent-top sync` keeps sessions after a harness deletes them.\n")
            }
            Source::Transcripts { hint: false } => {}
        }
        out
    }

    /// The report as JSON, for feeding somewhere else, with each group's
    /// estimate under `table` when one is given.
    pub fn to_json_with(&self, table: Option<&energy::Table>) -> serde_json::Value {
        let group = |b: &Bucket| {
            let mut g = serde_json::json!({
                "sessions": b.sessions, "tokens": b.tokens, "cost_usd": b.cost,
                "unpriced_tokens": b.unpriced, "turns": b.turns, "tool_calls": b.tool_calls,
                "tool_calls_lower_bound": b.tool_calls_lower_bound,
                "prompt_tokens": b.prompt, "cache_read_tokens": b.cache_read,
                "cache_hit_rate": b.cache_hit(),
                "output_tokens": b.output, "uncached_prompt_tokens": b.uncached_prompt(),
            });
            if let Some(t) = table.filter(|t| t.is_usable()) {
                g["estimate"] = estimate_json(&b.estimate(t));
            }
            g
        };
        let mut out = serde_json::json!({
            "since": format_date(self.since),
            "group_by": self.by.label(),
            "scanned": self.scanned,
            "groups": self.groups.iter().map(|(k, b)| (k.clone(), group(b))).collect::<serde_json::Map<_, _>>(),
            "total": group(&self.total),
            "source": match &self.source {
                Source::Store { path, only_in_store, received, reused } => serde_json::json!({
                    "store": path.to_string_lossy(), "store_only_sessions": only_in_store,
                    "received_sessions": received, "reused_from_store": reused,
                }),
                Source::Transcripts { .. } => serde_json::json!({
                    "store": null, "store_only_sessions": 0, "received_sessions": 0, "reused_from_store": 0,
                }),
            },
        });
        if let Some(t) = table {
            out["energy_table"] = table_json(t);
        }
        out
    }

    /// The report with its estimate: the exact activity behind it, then CO2e
    /// and energy as ranges from the user's table. Without a usable table it
    /// prints the activity alone and says where the table goes.
    pub fn to_plain_energy(&self, table: &energy::Table) -> String {
        let usable = table.is_usable();
        let mut out = String::new();
        out.push_str(&format!("agent-top report · since {} · by {} · energy\n\n", format_date(self.since), self.by.label()));
        let head = |out: &mut String| {
            out.push_str(&format!(
                "{:<22} {:>8} {:>10} {:>10} {:>11}",
                self.by.label().to_uppercase(),
                "SESSIONS",
                "OUTPUT",
                "UNCACHED",
                "CACHE READ"
            ));
            if usable {
                out.push_str(&format!(" {:>17} {:>17} {:>17} {:>11}", "CO2E (EST.)", "ENERGY (EST.)", "OF IT CACHE READ", "UNESTIMATED"));
            }
            out.push('\n');
        };
        let line = |out: &mut String, k: &str, b: &Bucket| {
            out.push_str(&format!(
                "{:<22} {:>8} {:>10} {:>10} {:>11}",
                truncate(k, 22),
                b.sessions,
                tokens(b.output),
                tokens(b.uncached_prompt()),
                tokens(b.cache_read)
            ));
            if usable {
                let e = b.estimate(table);
                let some = e.estimated_tokens > 0;
                out.push_str(&format!(
                    " {:>17} {:>17} {:>17} {:>11}",
                    if some { grams(e.co2e_g) } else { "-".into() },
                    if some { watt_hours(e.energy_wh) } else { "-".into() },
                    if some { watt_hours(e.cache_read_wh) } else { "-".into() },
                    if e.unestimated_tokens > 0 { tokens(e.unestimated_tokens) } else { "-".into() }
                ));
            }
            out.push('\n');
        };
        head(&mut out);
        let mut rows: Vec<(&String, &Bucket)> = self.groups.iter().collect();
        rows.sort_by(|a, b| b.1.cost.partial_cmp(&a.1.cost).unwrap_or(std::cmp::Ordering::Equal).then(b.1.tokens.cmp(&a.1.tokens)));
        for (k, b) in rows {
            line(&mut out, k, b);
        }
        out.push_str(&format!("{:-<width$}\n", "", width = if usable { 138 } else { 65 }));
        line(&mut out, "total", &self.total);
        out.push('\n');
        if usable {
            let e = self.total.estimate(table);
            if let (Some(g), Some(src)) = (table.gco2e_per_kwh, &table.grid_source) {
                out.push_str(&format!(
                    "Estimated from {}: watt-hours per token for each model, and {} g CO2e per kWh ({src}). No provider publishes these figures; each entry names its source.\n",
                    table.path.as_deref().map(|p| p.display().to_string()).unwrap_or_default(),
                    range(g, 0)
                ));
            }
            if e.unestimated_tokens > 0 {
                let missing: Vec<&str> = self
                    .total
                    .by_model
                    .keys()
                    .filter(|m| table.entry(m).is_none())
                    .map(|m| if m.is_empty() { "(no model)" } else { m.as_str() })
                    .collect();
                out.push_str(&format!(
                    "{} tokens ran on models with no entry in the table, so they are not in the estimate: {}.\n",
                    tokens(e.unestimated_tokens),
                    missing.join(", ")
                ));
            }
        } else {
            out.push_str(
                "No energy table, so there is no CO2e estimate. No provider publishes energy per token; write the figures you trust to\n",
            );
            out.push_str(&format!(
                "{} with a source for each. See https://agenttop.dev/energy/ for the published figures.\n",
                table.path.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "~/.config/agent-top/energy.toml".into())
            ));
        }
        out
    }
}

fn range_json(r: Range) -> serde_json::Value {
    serde_json::json!({ "low": r.low, "high": r.high })
}

fn estimate_json(e: &Estimate) -> serde_json::Value {
    serde_json::json!({
        "estimated": true,
        "co2e_g": range_json(e.co2e_g),
        "energy_wh": range_json(e.energy_wh),
        "cache_read_wh": range_json(e.cache_read_wh),
        "estimated_tokens": e.estimated_tokens,
        "unestimated_tokens": e.unestimated_tokens,
    })
}

fn table_json(t: &energy::Table) -> serde_json::Value {
    serde_json::json!({
        "path": t.path.as_deref().map(|p| p.to_string_lossy().into_owned()),
        "usable": t.is_usable(),
        "warnings": t.warnings,
        "grid": t.gco2e_per_kwh.map(|g| serde_json::json!({
            "gco2e_per_kwh": range_json(g), "source": t.grid_source, "checked": t.grid_checked,
        })),
        "models": t.entries.iter().map(|e| serde_json::json!({
            "model": e.model, "source": e.source, "checked": e.checked,
            "wh_per_million": {
                "input": range_json(e.coefficients.input), "cache_write": range_json(e.coefficients.cache_write),
                "cache_read": range_json(e.coefficients.cache_read), "output": range_json(e.coefficients.output),
            },
        })).collect::<Vec<_>>(),
    })
}

/// A range in a unit, as "low-high" or one number when the bounds agree.
fn range(r: Range, decimals: usize) -> String {
    let one = |v: f64| format!("{v:.decimals$}");
    if (r.high - r.low).abs() < f64::EPSILON * r.high.max(1.0) { one(r.low) } else { format!("{}-{}", one(r.low), one(r.high)) }
}

/// Picks the unit by the high bound so both ends share it.
fn scaled(r: Range, units: [&str; 3]) -> String {
    let (k, unit) = if r.high >= 1e6 {
        (1e6, units[2])
    } else if r.high >= 1e3 {
        (1e3, units[1])
    } else {
        (1.0, units[0])
    };
    let s = Range { low: r.low / k, high: r.high / k };
    let decimals = if s.high >= 100.0 { 0 } else { 1 };
    format!("{} {unit}", range(s, decimals))
}

fn grams(r: Range) -> String {
    scaled(r, ["g", "kg", "t"])
}

fn watt_hours(r: Range) -> String {
    scaled(r, ["Wh", "kWh", "MWh"])
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
/// `2026-10-06T12:00:00Z`, for log lines.
pub fn timestamp_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = date_utc(t);
    let s = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

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

pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
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
    use agent_top_core::harness::claude::ClaudeAdapter;
    use agent_top_core::harness::{AttributeContext, HarnessAdapter, SessionTracker};
    use agent_top_core::model::{Attribution, ProcNode};
    use agent_top_core::process::RawProc;
    use std::collections::HashSet;
    use std::path::Path;

    /// Claude Code's adapter, listing the transcripts a test gives it.
    struct Listed(Vec<(String, PathBuf)>);

    impl HarnessAdapter for Listed {
        fn harness(&self) -> Harness {
            Harness::Claude
        }
        fn rescan(&mut self, _: SystemTime) {}
        fn attribute(&self, _: &ProcNode, _: Option<&RawProc>, _: &AttributeContext) -> (Vec<PathBuf>, Attribution) {
            (Vec::new(), Attribution::CwdHeuristic)
        }
        fn unowned(&self, _: &HashSet<PathBuf>) -> Vec<PathBuf> {
            Vec::new()
        }
        fn open(&self, path: &Path, spans: SpanRetention) -> Box<dyn SessionTracker> {
            ClaudeAdapter::default().open(path, spans)
        }
        fn detect(&self, _: &Path) -> bool {
            true
        }
        fn transcripts(&self) -> Vec<(String, PathBuf)> {
            self.0.clone()
        }
        fn stamp(&self, path: &Path) -> Option<harness::SourceStamp> {
            ClaudeAdapter::default().stamp(path)
        }
    }

    /// A Claude fixture copied into a fresh directory, and a store synced from it.
    fn synced(tag: &str) -> (PathBuf, PathBuf, Vec<StoredSession>) {
        let dir = std::env::temp_dir().join(format!("agent-top-report-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude-2.1.278.jsonl");
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-top-core/tests/fixtures/claude-2.1.278.jsonl"), &path).unwrap();
        let db = dir.join("agent-top.db");
        let src = agent_top_store::Source { harness: Harness::Claude, id: "claude-2.1.278".into(), path: path.clone() };
        agent_top_store::Store::open(&db).unwrap().sync_sources(&[src], None).unwrap();
        let rows = agent_top_store::Reader::open(&db).unwrap().sessions().unwrap();
        (path, db, rows)
    }

    fn listed(path: &Path) -> Vec<Box<dyn HarnessAdapter>> {
        vec![Box::new(Listed(vec![("claude-2.1.278".into(), path.to_path_buf())]))]
    }

    fn totals(r: &Report) -> serde_json::Value {
        let mut j = r.to_json_with(None);
        j.as_object_mut().unwrap().remove("source");
        j.as_object_mut().unwrap().remove("scanned");
        j
    }

    fn source(r: &Report) -> (u64, u64) {
        match r.source {
            Source::Store { only_in_store, reused, .. } => (only_in_store, reused),
            Source::Transcripts { .. } => panic!("the store was not used"),
        }
    }

    #[test]
    fn an_unchanged_transcript_is_read_from_the_store_with_the_same_numbers() {
        let (path, db, rows) = synced("reuse");
        let plain = build_with(listed(&path), UNIX_EPOCH, GroupBy::Model, None, false);
        let stored = build_with(listed(&path), UNIX_EPOCH, GroupBy::Model, Some((db, rows)), false);
        assert_eq!(source(&stored), (0, 1));
        assert_eq!(totals(&stored), totals(&plain));
        assert_eq!(plain.total.sessions, 1);
    }

    #[test]
    fn a_changed_transcript_is_parsed_again() {
        let (path, db, rows) = synced("changed");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(
            "{\"type\":\"system\",\"subtype\":\"informational\",\"timestamp\":\"2026-09-21T20:30:00.000Z\",\"version\":\"2.1.278\"}\n",
        );
        std::fs::write(&path, text).unwrap();
        let r = build_with(listed(&path), UNIX_EPOCH, GroupBy::Harness, Some((db, rows)), false);
        assert_eq!(source(&r), (0, 0));
        assert_eq!(r.total.sessions, 1);
    }

    #[test]
    fn a_deleted_transcript_is_counted_from_the_store() {
        let (path, db, rows) = synced("deleted");
        let before = build_with(listed(&path), UNIX_EPOCH, GroupBy::Day, None, false);
        std::fs::remove_file(&path).unwrap();
        let gone = build_with(Vec::new(), UNIX_EPOCH, GroupBy::Day, None, true);
        assert_eq!(gone.total.sessions, 0);
        assert!(gone.to_plain().contains("`agent-top sync` keeps sessions"));
        let kept = build_with(Vec::new(), UNIX_EPOCH, GroupBy::Day, Some((db, rows)), false);
        assert_eq!(source(&kept), (1, 0));
        assert_eq!(totals(&kept), totals(&before));
        assert!(kept.to_plain().contains("1 session is only in the store, its transcript deleted."));
    }

    #[test]
    fn the_energy_view_shows_activity_and_estimates_only_listed_models() {
        let (path, _, _) = synced("energy");
        let r = build_with(listed(&path), UNIX_EPOCH, GroupBy::Model, None, false);
        let model = r.total.by_model.keys().next().unwrap().clone();

        let none = energy::Table::default();
        let plain = r.to_plain_energy(&none);
        assert!(plain.contains("No energy table, so there is no CO2e estimate."), "{plain}");
        assert!(!plain.contains("CO2E"), "{plain}");
        let j = r.to_json_with(Some(&none));
        assert_eq!(j["total"]["output_tokens"], r.total.output);
        assert_eq!(j["total"]["uncached_prompt_tokens"].as_u64().unwrap() + r.total.cache_read, r.total.prompt);
        assert!(j["total"].get("estimate").is_none());

        let text = format!(
            "[grid]\ngco2e_per_kwh = [100, 400]\nsource = \"t\"\nchecked = \"2026-11-28\"\n\n[models.\"{model}\"]\ninput = 1\ncache_write = 1\ncache_read = 1\noutput = 1\nsource = \"t\"\nchecked = \"2026-11-28\"\n"
        );
        let table = energy::build(&text, PathBuf::from("/tmp/energy.toml"));
        assert!(table.is_usable(), "{:?}", table.warnings);
        let plain = r.to_plain_energy(&table);
        assert!(plain.contains("CO2E (EST.)") && plain.contains("No provider publishes these figures"), "{plain}");
        let e = &r.to_json_with(Some(&table))["total"]["estimate"];
        // 1 Wh per million tokens of every kind: the energy is the token count.
        let total = r.total.tokens as f64 / 1e6;
        assert!((e["energy_wh"]["low"].as_f64().unwrap() - total).abs() < 1e-9, "{e}");
        assert_eq!(e["unestimated_tokens"], 0);

        let other = energy::build(&text.replace(&model, "some-other-model"), PathBuf::from("/tmp/energy.toml"));
        let plain = r.to_plain_energy(&other);
        assert!(plain.contains("not in the estimate") && plain.contains(&model), "{plain}");
    }

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
    fn timestamps_are_utc_seconds() {
        assert_eq!(timestamp_utc(UNIX_EPOCH + Duration::from_secs(1791244800 + 3 * 3600 + 4 * 60 + 5)), "2026-10-06T03:04:05Z");
    }

    #[test]
    fn civil_dates_round_trip() {
        for &(y, m, d) in &[(1970, 1, 1), (2026, 9, 5), (2000, 2, 29), (2026, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }
}
