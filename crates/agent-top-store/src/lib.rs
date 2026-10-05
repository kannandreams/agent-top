//! agent-top-store: the SQLite file `agent-top sync` fills (RFC-108, DEC-026).
//!
//! Harnesses delete their own transcripts (Claude Code after
//! `cleanupPeriodDays`, 30 by default), and with them every number agent-top
//! computed from them. The store keeps those numbers: one row per session,
//! one per span, one per MCP server a session called. It holds what `--json`
//! shows and nothing else, since `agent-top-core` never reads prompt, reply
//! or tool content.
//!
//! This is the only crate that writes the file. `agent-top-core` stays free
//! of write paths (ADR-005), and the file is written only by `sync`.

use agent_top_core::Harness;
use agent_top_core::harness::{self, HarnessAdapter, SessionSummary, SpanRetention};
use agent_top_core::model::{ToolSpan, parent_turn, project_name};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The schema this build writes, kept in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 2;

/// Recorded on every synced source, so a newer agent-top re-reads what an
/// older one stored and a parser or price fix reaches the stored rows.
pub const WRITER_VERSION: &str = env!("CARGO_PKG_VERSION");

mod query;
pub use query::{Column, Described, QueryResult, Reader, Value};

const SCHEMA_V1: &str = "
CREATE TABLE sessions (
    harness             TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    parent_session_id   TEXT,
    source_path         TEXT NOT NULL,
    cwd                 TEXT,
    project             TEXT,
    model               TEXT,
    harness_version     TEXT,
    attribution         TEXT NOT NULL,
    started_at          INTEGER,
    last_activity       INTEGER,
    turns               INTEGER NOT NULL,
    subagent_turns      INTEGER NOT NULL,
    tool_calls          INTEGER NOT NULL,
    web_searches        INTEGER NOT NULL,
    input               INTEGER NOT NULL,
    cache_write_5m      INTEGER NOT NULL,
    cache_write_1h      INTEGER NOT NULL,
    cache_write_unsplit INTEGER NOT NULL,
    cache_read          INTEGER NOT NULL,
    output              INTEGER NOT NULL,
    cost_usd            REAL NOT NULL,
    unpriced_tokens     INTEGER NOT NULL,
    price_source        TEXT,
    harness_cost_usd    REAL,
    synced_at           INTEGER NOT NULL,
    synced_by_version   TEXT NOT NULL,
    PRIMARY KEY (harness, session_id)
);
CREATE INDEX sessions_last_activity ON sessions (last_activity);

CREATE TABLE spans (
    harness      TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    span_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,
    name         TEXT NOT NULL,
    started_at   INTEGER NOT NULL,
    duration_ms  INTEGER,
    sidechain    INTEGER NOT NULL,
    error        INTEGER NOT NULL,
    parent_seq   INTEGER,
    PRIMARY KEY (harness, session_id, seq)
);

CREATE TABLE mcp_calls (
    harness       TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    server        TEXT NOT NULL,
    calls         INTEGER NOT NULL,
    errors        INTEGER NOT NULL,
    last_call_at  INTEGER,
    PRIMARY KEY (harness, session_id, server)
);

CREATE TABLE sources (
    path               TEXT PRIMARY KEY,
    harness            TEXT NOT NULL,
    session_id         TEXT NOT NULL,
    size               INTEGER,
    mtime_ms           INTEGER,
    synced_at          INTEGER NOT NULL,
    synced_by_version  TEXT NOT NULL
);
";

/// The saved questions. A session counts when it has tokens or turns, as in
/// `agent-top report`, and is dated by its last activity, in UTC.
const SCHEMA_V2: &str = "
CREATE VIEW counted_sessions AS
    SELECT *, input + cache_write_5m + cache_write_1h + cache_write_unsplit + cache_read + output AS tokens
    FROM sessions
    WHERE input + cache_write_5m + cache_write_1h + cache_write_unsplit + cache_read + output > 0 OR turns > 0;

CREATE VIEW cost_by_day AS
    SELECT date(last_activity / 1000, 'unixepoch') AS day, count(*) AS sessions, sum(tokens) AS tokens,
           sum(cost_usd) AS cost_usd, sum(unpriced_tokens) AS unpriced_tokens
    FROM counted_sessions GROUP BY day;

CREATE VIEW cost_by_harness AS
    SELECT harness, count(*) AS sessions, sum(tokens) AS tokens,
           sum(cost_usd) AS cost_usd, sum(unpriced_tokens) AS unpriced_tokens
    FROM counted_sessions GROUP BY harness;

CREATE VIEW cost_by_model AS
    SELECT coalesce(model, 'unknown') AS model, count(*) AS sessions, sum(tokens) AS tokens,
           sum(cost_usd) AS cost_usd, sum(unpriced_tokens) AS unpriced_tokens
    FROM counted_sessions GROUP BY 1;

CREATE VIEW cost_by_project AS
    SELECT coalesce(project, 'unknown') AS project, count(*) AS sessions, sum(tokens) AS tokens,
           sum(cost_usd) AS cost_usd, sum(unpriced_tokens) AS unpriced_tokens
    FROM counted_sessions GROUP BY 1;

CREATE VIEW tool_latency AS
    WITH ranked AS (
        SELECT name, duration_ms, error,
               row_number() OVER (PARTITION BY name ORDER BY duration_ms) AS rank,
               count(*) OVER (PARTITION BY name) AS n
        FROM spans WHERE kind = 'tool' AND duration_ms IS NOT NULL
    )
    SELECT name, max(n) AS calls, sum(error) AS errors,
           min(CASE WHEN rank >= 0.5 * n THEN duration_ms END) AS p50_ms,
           min(CASE WHEN rank >= 0.95 * n THEN duration_ms END) AS p95_ms,
           max(duration_ms) AS max_ms
    FROM ranked GROUP BY name;

CREATE VIEW mcp_errors AS
    SELECT server, count(*) AS sessions, sum(calls) AS calls, sum(errors) AS errors,
           round(1.0 * sum(errors) / sum(calls), 4) AS error_rate, max(last_call_at) AS last_call_at
    FROM mcp_calls GROUP BY server;
";

/// Where the store lives when `--db` is not given: `AGENT_TOP_DB`, else
/// `$XDG_DATA_HOME/agent-top/agent-top.db`, else
/// `~/.local/share/agent-top/agent-top.db`, on every platform.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_TOP_DB") {
        return Some(PathBuf::from(p));
    }
    let dir = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))?;
    Some(dir.join("agent-top").join("agent-top.db"))
}

/// One transcript to sync: which harness wrote it, the id a user would type,
/// and where it is.
#[derive(Debug, Clone)]
pub struct Source {
    pub harness: Harness,
    pub id: String,
    pub path: PathBuf,
}

/// What one sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Transcripts the adapters listed.
    pub seen: u64,
    /// Stored or replaced.
    pub stored: u64,
    /// Same size, time and writer version as last time; not read.
    pub unchanged: u64,
    /// Last written before `since`; not read.
    pub outside_window: u64,
    /// Read as empty where the store already had numbers; the stored row kept.
    pub kept: u64,
    /// Could not be read, with the reason.
    pub failed: Vec<(PathBuf, String)>,
}

enum Outcome {
    Stored,
    Unchanged,
    OutsideWindow,
    Kept,
    Failed(String),
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open the store for writing, creating it (mode 0600 on Unix) and its
    /// directory when missing, and bring its schema up to this build's.
    pub fn open(path: &Path) -> Result<Store> {
        if !path.exists() {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            }
            create_private(path).with_context(|| format!("creating {}", path.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        let mut store = Store { conn, path: path.to_path_buf() };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&mut self) -> Result<()> {
        let v: i64 = self.conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            bail!(
                "{} has store schema {v}, newer than this agent-top ({WRITER_VERSION}) knows ({SCHEMA_VERSION}); upgrade agent-top",
                self.path.display()
            );
        }
        for (version, sql) in [(1, SCHEMA_V1), (2, SCHEMA_V2)] {
            if v < version {
                let tx = self.conn.transaction()?;
                tx.execute_batch(sql)?;
                tx.pragma_update(None, "user_version", version)?;
                tx.commit()?;
            }
        }
        Ok(())
    }

    /// Sync every transcript every adapter lists. With `since`, a transcript
    /// file last written before it is not read.
    pub fn sync(&mut self, since: Option<SystemTime>) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        for adapter in harness::adapters() {
            let harness = adapter.harness();
            for (id, path) in adapter.transcripts() {
                self.sync_one(adapter.as_ref(), &Source { harness, id, path }, since, &mut stats)?;
            }
        }
        Ok(stats)
    }

    /// Sync the given transcripts, each read by its harness's adapter.
    pub fn sync_sources(&mut self, sources: &[Source], since: Option<SystemTime>) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        for src in sources {
            let Some(adapter) = harness::adapter_for(src.harness) else {
                stats.seen += 1;
                stats.failed.push((src.path.clone(), format!("{} has no transcript adapter", src.harness.label())));
                continue;
            };
            self.sync_one(adapter.as_ref(), src, since, &mut stats)?;
        }
        Ok(stats)
    }

    fn sync_one(&mut self, adapter: &dyn HarnessAdapter, src: &Source, since: Option<SystemTime>, stats: &mut SyncStats) -> Result<()> {
        stats.seen += 1;
        // A transcript that cannot be read is reported and skipped; a store
        // that cannot be written stops the sync.
        match self.sync_source(adapter, src, since)? {
            Outcome::Stored => stats.stored += 1,
            Outcome::Unchanged => stats.unchanged += 1,
            Outcome::OutsideWindow => stats.outside_window += 1,
            Outcome::Kept => stats.kept += 1,
            Outcome::Failed(why) => stats.failed.push((src.path.clone(), why)),
        }
        Ok(())
    }

    fn sync_source(&mut self, adapter: &dyn HarnessAdapter, src: &Source, since: Option<SystemTime>) -> Result<Outcome> {
        let stamp = adapter.stamp(&src.path);
        if let (Some(since), Some(st)) = (since, stamp)
            && st.mtime_ms < ms(since)
        {
            return Ok(Outcome::OutsideWindow);
        }
        let path_key = src.path.to_string_lossy().into_owned();
        let prior: Option<(Option<i64>, Option<i64>, String)> = self
            .conn
            .query_row("SELECT size, mtime_ms, synced_by_version FROM sources WHERE path = ?1", [&path_key], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        if let (Some(st), Some((size, mtime, version))) = (stamp, &prior)
            && *size == Some(st.size as i64)
            && *mtime == Some(st.mtime_ms)
            && version == WRITER_VERSION
        {
            return Ok(Outcome::Unchanged);
        }

        let mut tracker = adapter.open(&src.path, SpanRetention::All);
        if let Err(e) = tracker.refresh_all() {
            return Ok(Outcome::Failed(format!("{e:#}")));
        }
        let s = tracker.summary();
        let harness = src.harness.label();
        let now = ms(SystemTime::now());

        // A re-read never replaces stored numbers with none. A transcript that
        // reads as empty where the store has a session is one the harness
        // removed or truncated, and keeping history is the store's job.
        let empty = s.usage.total() == 0 && s.turns == 0 && s.spans.is_empty();
        let tx = self.conn.transaction()?;
        let stored: bool = tx
            .query_row("SELECT 1 FROM sessions WHERE harness = ?1 AND session_id = ?2", params![harness, src.id], |_| Ok(()))
            .optional()?
            .is_some();
        let outcome = if empty && stored {
            Outcome::Kept
        } else {
            replace_session(&tx, harness, &src.id, &src.path, s, now)?;
            Outcome::Stored
        };
        tx.execute(
            "INSERT INTO sources (path, harness, session_id, size, mtime_ms, synced_at, synced_by_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (path) DO UPDATE SET harness = ?2, session_id = ?3, size = ?4, mtime_ms = ?5, synced_at = ?6, synced_by_version = ?7",
            params![path_key, harness, src.id, stamp.map(|s| s.size as i64), stamp.map(|s| s.mtime_ms), now, WRITER_VERSION],
        )?;
        tx.commit()?;
        Ok(outcome)
    }

    /// How many sessions the store holds.
    pub fn session_count(&self) -> Result<u64> {
        Ok(self.conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))? as u64)
    }
}

/// Delete one session's rows and write them again from `s`, in `tx`.
fn replace_session(tx: &Transaction, harness: &str, id: &str, path: &Path, s: &SessionSummary, now: i64) -> Result<()> {
    for table in ["sessions", "spans", "mcp_calls"] {
        tx.execute(&format!("DELETE FROM {table} WHERE harness = ?1 AND session_id = ?2"), params![harness, id])?;
    }
    let u = &s.usage;
    let price_source = s.price_source.and_then(|p| serde_json::to_value(p).ok()).and_then(|v| v.as_str().map(str::to_string));
    tx.execute(
        "INSERT INTO sessions (harness, session_id, parent_session_id, source_path, cwd, project, model, harness_version,
            attribution, started_at, last_activity, turns, subagent_turns, tool_calls, web_searches,
            input, cache_write_5m, cache_write_1h, cache_write_unsplit, cache_read, output,
            cost_usd, unpriced_tokens, price_source, harness_cost_usd, synced_at, synced_by_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'transcript', ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            harness,
            id,
            s.subagent.as_ref().map(|a| a.parent_session_id.as_str()),
            path.to_string_lossy(),
            s.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
            s.cwd.as_deref().map(project_name),
            s.model,
            s.harness_version,
            s.started_at.map(ms),
            s.last_activity.map(ms),
            s.turns as i64,
            s.subagent_turns as i64,
            s.tool_calls as i64,
            s.web_searches as i64,
            u.input as i64,
            u.cache_write_5m as i64,
            u.cache_write_1h as i64,
            u.cache_write_unsplit as i64,
            u.cache_read as i64,
            u.output as i64,
            s.cost_usd,
            s.unpriced_tokens as i64,
            price_source,
            s.harness_cost.as_ref().map(|c| c.usd),
            now,
            WRITER_VERSION,
        ],
    )?;

    let spans: Vec<&ToolSpan> = s.spans.iter().collect();
    let mut insert = tx.prepare(
        "INSERT INTO spans (harness, session_id, seq, span_id, kind, name, started_at, duration_ms, sidechain, error, parent_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for (i, sp) in spans.iter().enumerate() {
        let parent = parent_turn(&spans, i).and_then(|p| spans.iter().position(|q| std::ptr::eq(*q, p)));
        insert.execute(params![
            harness,
            id,
            i as i64,
            sp.id,
            sp.kind.label(),
            sp.name,
            ms(sp.started_at),
            sp.duration_ms.map(|d| d as i64),
            sp.sidechain,
            sp.error,
            parent.map(|p| p as i64),
        ])?;
    }

    let mut insert =
        tx.prepare("INSERT INTO mcp_calls (harness, session_id, server, calls, errors, last_call_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for (server, m) in &s.mcp {
        insert.execute(params![harness, id, server, m.calls as i64, m.errors as i64, m.last_call.map(ms)])?;
    }
    Ok(())
}

/// Milliseconds since the epoch, the store's one time unit.
fn ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map(drop)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path).map(drop)
}
