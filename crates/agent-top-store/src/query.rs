//! `agent-top sql`: the store, opened so that nothing can change it.

use anyhow::{Context, Result, bail};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

use crate::SCHEMA_VERSION;
use agent_top_core::model::TokenUsage;

/// One cell of a query result.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// The store holds no blobs; a query that makes one gets its length.
    Blob(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// One stored session: what `agent-top report` totals, and the size, time
/// and agent-top version of the transcript when it was synced, so a report
/// can tell whether the file on disk still says the same thing.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSession {
    pub harness: String,
    pub session_id: String,
    pub source_path: String,
    pub model: Option<String>,
    pub project: Option<String>,
    /// Milliseconds since the epoch.
    pub last_activity_ms: Option<i64>,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub unpriced_tokens: u64,
    pub turns: u64,
    pub tool_calls: u64,
    pub tool_calls_lower_bound: bool,
    pub source_size: Option<i64>,
    pub source_mtime_ms: Option<i64>,
    pub synced_by_version: Option<String>,
}

/// A table or view, for `agent-top sql --schema`.
#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    pub name: String,
    pub kind: &'static str,
    pub about: &'static str,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub ty: String,
}

/// What each table and view holds, in the order `--schema` prints them.
const ABOUT: &[(&str, &str)] = &[
    ("sessions", "one row per session, with its tokens and cost"),
    ("spans", "one row per tool call, inference or turn; parent_seq is the turn it ran in"),
    ("mcp_calls", "calls and errors per MCP server per session"),
    ("sources", "the transcripts synced, with the size and time last read"),
    ("counted_sessions", "sessions with tokens or turns, plus a tokens column; what report counts"),
    ("cost_by_day", "sessions, tokens and cost per UTC day of last activity"),
    ("cost_by_harness", "sessions, tokens and cost per harness"),
    ("cost_by_model", "sessions, tokens and cost per model"),
    ("cost_by_project", "sessions, tokens and cost per project (last two path components)"),
    ("tool_latency", "calls, errors and p50, p95 and max duration per tool name"),
    ("mcp_errors", "calls, errors and error rate per MCP server"),
];

/// The store opened read-only: the file cannot be created, written or
/// upgraded through it, and only statements SQLite reports as read-only run.
pub struct Reader {
    conn: Connection,
    path: PathBuf,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader> {
        if !path.exists() {
            bail!("no store at {}; run `agent-top sync` to create one", path.display());
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "query_only", true)?;
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v < SCHEMA_VERSION {
            bail!("{} has store schema {v}; run `agent-top sync` to bring it to {SCHEMA_VERSION}", path.display());
        }
        if v > SCHEMA_VERSION {
            bail!("{} has store schema {v}, newer than this agent-top knows ({SCHEMA_VERSION}); upgrade agent-top", path.display());
        }
        Ok(Reader { conn, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run one statement and collect every row.
    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        let mut stmt = self.conn.prepare(sql)?;
        // `query_only` stops writes to the database; this also stops the
        // statements that write elsewhere, such as `VACUUM INTO`.
        if !stmt.readonly() {
            bail!("agent-top sql runs read-only statements; this one would change something");
        }
        let columns: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
        let n = columns.len();
        let mut rows = Vec::new();
        let mut cursor = stmt.query([])?;
        while let Some(row) = cursor.next()? {
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => Value::Integer(v),
                    ValueRef::Real(v) => Value::Real(v),
                    ValueRef::Text(b) => Value::Text(String::from_utf8_lossy(b).into_owned()),
                    ValueRef::Blob(b) => Value::Blob(b.len()),
                });
            }
            rows.push(out);
        }
        Ok(QueryResult { columns, rows })
    }

    /// Every stored session, with the stamp of its transcript when synced.
    pub fn sessions(&self) -> Result<Vec<StoredSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.harness, s.session_id, s.source_path, s.model, s.project, s.last_activity,
                    s.input, s.cache_write_5m, s.cache_write_1h, s.cache_write_unsplit, s.cache_read, s.output,
                    s.cost_usd, s.unpriced_tokens, s.turns, s.tool_calls, s.tool_calls_lower_bound,
                    src.size, src.mtime_ms, src.synced_by_version
             FROM sessions s LEFT JOIN sources src ON src.path = s.source_path",
        )?;
        let n = |r: &rusqlite::Row, i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
        let rows = stmt.query_map([], |r| {
            Ok(StoredSession {
                harness: r.get(0)?,
                session_id: r.get(1)?,
                source_path: r.get(2)?,
                model: r.get(3)?,
                project: r.get(4)?,
                last_activity_ms: r.get(5)?,
                usage: TokenUsage {
                    input: n(r, 6)?,
                    cache_write_5m: n(r, 7)?,
                    cache_write_1h: n(r, 8)?,
                    cache_write_unsplit: n(r, 9)?,
                    cache_read: n(r, 10)?,
                    output: n(r, 11)?,
                },
                cost_usd: r.get(12)?,
                unpriced_tokens: n(r, 13)?,
                turns: n(r, 14)?,
                tool_calls: n(r, 15)?,
                tool_calls_lower_bound: r.get(16)?,
                source_size: r.get(17)?,
                source_mtime_ms: r.get(18)?,
                synced_by_version: r.get(19)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every table and view the store defines, with its columns.
    pub fn describe(&self) -> Result<Vec<Described>> {
        let mut out = Vec::new();
        for (name, about) in ABOUT {
            let kind: String = self.conn.query_row("SELECT type FROM sqlite_schema WHERE name = ?1", [name], |r| r.get(0))?;
            let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({name})"))?;
            let columns = stmt.query_map([], |r| Ok(Column { name: r.get(1)?, ty: r.get(2)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            out.push(Described { name: name.to_string(), kind: if kind == "view" { "view" } else { "table" }, about, columns });
        }
        Ok(out)
    }
}
