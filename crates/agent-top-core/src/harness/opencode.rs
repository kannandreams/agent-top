//! OpenCode: sessions in a SQLite database, not a JSONL log.
//!
//! Format notes (verified on OpenCode 1.18.15, 2026-09-05, against the live
//! `opencode.db` on this machine):
//! * The store is `$XDG_DATA_HOME/opencode/opencode.db` (or
//!   `~/.local/share/opencode/opencode.db`), a WAL SQLite database. agent-top
//!   opens it read-only and never writes, which honours the observe-only rule
//!   and does not block OpenCode's own writes.
//! * `session` is one row per conversation and already carries the accounting:
//!   `directory`, `agent` (the agent type, `build` / `explore` / `plan`),
//!   `model` (a JSON blob `{"id","providerID","variant"}`), `cost` (US
//!   dollars, computed by OpenCode), `tokens_input`, `tokens_output`,
//!   `tokens_reasoning`, `tokens_cache_read`, `tokens_cache_write`,
//!   `time_created`, `time_updated` (epoch ms), `parent_id` and `version`.
//!   A subagent is a `session` row whose `parent_id` is the parent's id.
//! * Because OpenCode has already priced the session, its `cost` is used
//!   directly rather than re-priced from agent-top's table: OpenCode runs
//!   third-party models (DeepSeek, and so on) that the table does not carry,
//!   and the harness's own figure is the real one. So an OpenCode row's cost
//!   is exact and never a floor, and `unpriced_tokens` is zero.
//! * `message` is one row per message, `data` JSON with `role`
//!   (`user` / `assistant`) and `time` `{created, completed}` in epoch ms. A
//!   `user` message opens a turn; each `assistant` message is one inference,
//!   from `created` to `completed`, and extends the turn it belongs to, which
//!   ends at the last reply before the next prompt. A reply with no
//!   `completed` is still in flight, so its inference and turn stay open.
//!   Assistant messages are also the turn count.
//! * `part` is one row per message part, `data` JSON with `type`. A `tool`
//!   part has `tool` (the name), `callID` and `state` with `status`
//!   (`completed` / `error` / ...) and `time` `{start,end}` in epoch ms, which
//!   is one tool span. `step-start` / `step-finish`, `reasoning`, `text` and
//!   `patch` parts are not read.
//! * MCP calls (verified on OpenCode 1.18.15, 2026-09-13, with a filesystem
//!   server named `scratch_fs` and a live session): an MCP call is an ordinary
//!   `tool` part whose `tool` is the server name and the tool name joined by
//!   `_`, each with every character outside `[a-zA-Z0-9_-]` replaced by `_`
//!   (`scratch_fs_list_directory`). There is no `mcp` prefix and nothing else
//!   in the part marks it as MCP; a call the server rejects has `status`
//!   `error`. So the name alone cannot say where the server ends, and a
//!   built-in tool with an underscore would look the same. The server names
//!   come from OpenCode's config instead, key names of `mcp` only: the global
//!   `config.json`, `opencode.json` and `opencode.jsonc` in
//!   `$XDG_CONFIG_HOME/opencode` (or `~/.config/opencode`); `opencode.jsonc`,
//!   `opencode.json` and `.opencode/opencode.json[c]` in the session directory
//!   and each parent up to the worktree root; and `~/.opencode`. A tool part
//!   is an MCP call when its name starts with a configured server's sanitised
//!   name and `_`, the longest such name winning. A server configured only
//!   through `OPENCODE_CONFIG` in the agent's own environment cannot be seen
//!   from outside the process and is not counted. Every tool part, MCP or not,
//!   is still counted as a tool call and a span.
//! * Context by source (verified on OpenCode 1.18.15, 2026-09-13): each
//!   `assistant` message is one model response and carries its own `tokens`
//!   `{input, output, reasoning, cache: {read, write}}`, `cost` (US dollars)
//!   and `modelID`. The `tool` parts of a message are the calls that response
//!   made, and the next assistant message's prompt carries their results. So
//!   the ledger is fed per session in `time_created` order: each response's
//!   usage, then that message's tool calls as pending results. A compaction is
//!   written as a user message with a `compaction` part followed by an
//!   assistant message with `summary: true` and `mode: "compaction"`; the
//!   ledger resets after that reply, exactly rather than by the prompt-halved
//!   fallback. The per-message `cost` is one total, so the prompt-side share
//!   the ledger charges is taken from agent-top's price table for the model,
//!   scaled so the classes add up to OpenCode's figure; a model the table does
//!   not price gets tokens and no cost, which the UI shows as `-`. Subagent
//!   ledgers are folded into the parent's.
//!
//! A session has no file of its own, so a tracker is addressed by a virtual
//! path `<db>/<session id>`: unique, stable, and with the session id as its
//! file stem, which is all the collector and the trace resolver need.

use super::{AttributeContext, ContextLedger, HarnessAdapter, RegistryHints, SessionSummary, SessionTracker, SpanRetention};
use crate::model::{Activity, Attribution, ContextOrigin, CostBreakdown, Harness, ProcNode, SpanKind, TokenUsage};
use crate::process::RawProc;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `$XDG_DATA_HOME/opencode`, or `~/.local/share/opencode`.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("OPENCODE_DATA_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(d).join("opencode"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/opencode"))
}

/// The session database, when it exists.
pub fn db_path() -> Option<PathBuf> {
    let p = data_dir()?.join("opencode.db");
    p.exists().then_some(p)
}

/// Open the database read-only. Read-only means agent-top can never write to
/// or lock the file OpenCode is using; the WAL and its shared-memory index are
/// read, not created.
fn open_ro(db: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn from_ms(ms: i64) -> Option<SystemTime> {
    (ms > 0).then(|| UNIX_EPOCH + Duration::from_millis(ms as u64))
}

/// One top-level conversation, enough to attribute it to a process and list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub directory: PathBuf,
    pub created: Option<SystemTime>,
    pub updated: Option<SystemTime>,
}

/// The virtual path that stands for a session on disk: the database path with
/// the session id appended. Never opened as a file; only its stem is read.
pub fn session_path(db: &Path, session_id: &str) -> PathBuf {
    db.join(session_id)
}

/// The session id in a virtual path.
pub fn session_id_of(path: &Path) -> Option<String> {
    path.file_name().map(|f| f.to_string_lossy().into_owned())
}

/// Top-level sessions (no parent) written since `since`, newest activity first.
pub fn recent_sessions(db: &Path, since: SystemTime) -> Vec<Session> {
    let Ok(conn) = open_ro(db) else { return Vec::new() };
    let sql = "SELECT id, directory, time_created, time_updated FROM session \
               WHERE parent_id IS NULL AND time_updated >= ?1 ORDER BY time_updated DESC";
    let Ok(mut stmt) = conn.prepare(sql) else { return Vec::new() };
    let rows = stmt.query_map([to_ms(since)], |r| {
        Ok(Session {
            id: r.get::<_, String>(0)?,
            directory: PathBuf::from(r.get::<_, String>(1)?),
            created: from_ms(r.get::<_, i64>(2)?),
            updated: from_ms(r.get::<_, i64>(3)?),
        })
    });
    rows.map(|it| it.flatten().collect()).unwrap_or_default()
}

/// Where OpenCode's config lives, for reading MCP server names. The fields are
/// the roots that differ per machine, so a test can point them elsewhere.
#[derive(Debug, Clone, Default)]
pub struct ConfigRoots {
    /// `$XDG_CONFIG_HOME/opencode`, or `~/.config/opencode`.
    pub global: Option<PathBuf>,
    /// The home directory, for `~/.opencode`.
    pub home: Option<PathBuf>,
}

impl ConfigRoots {
    pub fn detect() -> ConfigRoots {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let global = std::env::var_os("XDG_CONFIG_HOME")
            .map(|d| PathBuf::from(d).join("opencode"))
            .or_else(|| home.as_ref().map(|h| h.join(".config/opencode")));
        ConfigRoots { global, home }
    }
}

/// Every config file OpenCode would read for a session in `directory`, in no
/// particular order: the global files, the project files from `directory` up
/// to the worktree root (the nearest parent holding `.git`, else the
/// filesystem root), and `~/.opencode`. Files that do not exist are left out
/// by the caller.
fn config_files(directory: &Path, roots: &ConfigRoots) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(g) = &roots.global {
        for name in ["config.json", "opencode.json", "opencode.jsonc"] {
            files.push(g.join(name));
        }
    }
    let project = |dir: &Path, files: &mut Vec<PathBuf>| {
        for name in ["opencode.jsonc", "opencode.json"] {
            files.push(dir.join(name));
            files.push(dir.join(".opencode").join(name));
        }
    };
    for dir in directory.ancestors() {
        project(dir, &mut files);
        if dir.join(".git").exists() {
            break;
        }
    }
    if let Some(h) = &roots.home {
        for name in ["opencode.jsonc", "opencode.json"] {
            files.push(h.join(".opencode").join(name));
        }
    }
    files
}

/// The MCP server names configured for a session in `directory`: the keys of
/// every `mcp` section OpenCode would read, and nothing else from those files.
/// Sorted and deduplicated.
pub fn mcp_server_names(directory: &Path, roots: &ConfigRoots) -> Vec<String> {
    let mut names: Vec<String> = config_files(directory, roots)
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .filter_map(|text| serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)).ok())
        .filter_map(|v| v.get("mcp").and_then(|m| m.as_object()).map(|m| m.keys().cloned().collect::<Vec<_>>()))
        .flatten()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// JSON with comments, as OpenCode's `.jsonc` files are, turned into JSON:
/// `//` and `/* */` comments outside strings are removed, and a comma right
/// before a closing `}` or `]` is dropped.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        out.push(n);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    // Trailing commas: a comma followed only by whitespace and a closer.
    let bytes: Vec<char> = out.chars().collect();
    let mut cleaned = String::with_capacity(out.len());
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            cleaned.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                cleaned.push(bytes[i + 1]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            cleaned.push(c);
        } else if c == ',' {
            let next = bytes[i + 1..].iter().find(|n| !n.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                cleaned.push(c);
            }
        } else {
            cleaned.push(c);
        }
        i += 1;
    }
    cleaned
}

/// A name as OpenCode puts it into a tool key: every character outside
/// `[a-zA-Z0-9_-]` becomes `_`.
fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// The configured server an OpenCode tool name belongs to, if any. The tool
/// key is `<server>_<tool>` with both halves sanitised, so a name matches a
/// server when it starts with the sanitised server name and `_` and has a tool
/// name after it. Of several matches (`github` and `github_enterprise`) the
/// longest is the server.
pub fn mcp_server_of<'a>(tool_name: &str, servers: &'a [String]) -> Option<&'a str> {
    servers
        .iter()
        .map(|s| (s, sanitize(s)))
        .filter(|(_, key)| {
            tool_name.len() > key.len() + 1 && tool_name.starts_with(key.as_str()) && tool_name.as_bytes()[key.len()] == b'_'
        })
        .max_by_key(|(_, key)| key.len())
        .map(|(s, _)| s.as_str())
}

/// The model id inside OpenCode's `model` JSON blob (`{"id":...}`); the raw
/// string if it is not that shape.
fn model_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => v.get("id").and_then(|x| x.as_str()).map(str::to_string).or_else(|| Some(raw.to_string())),
        Err(_) => Some(raw.to_string()),
    }
}

/// The OpenCode adapter. See the module notes for the store it reads.
#[derive(Default)]
pub struct OpenCodeAdapter {
    db: Option<PathBuf>,
    recent: Vec<Session>,
}

impl OpenCodeAdapter {
    fn db(&self) -> Option<PathBuf> {
        self.db.clone().or_else(db_path)
    }
}

impl HarnessAdapter for OpenCodeAdapter {
    fn harness(&self) -> Harness {
        Harness::OpenCode
    }

    fn rescan(&mut self, since: SystemTime) {
        self.db = db_path();
        self.recent = match &self.db {
            Some(db) => recent_sessions(db, since),
            None => Vec::new(),
        };
    }

    fn hints(&self, _pid: u32) -> Option<RegistryHints> {
        None
    }

    /// One process runs one conversation, in the directory it was started in.
    /// The newest top-level session in that directory that started before the
    /// process, and that no other process has claimed, is the row.
    fn attribute(&self, _root: &ProcNode, _raw: Option<&RawProc>, ctx: &AttributeContext) -> (Vec<PathBuf>, Attribution) {
        let (Some(cwd), Some(db)) = (ctx.cwd, self.db()) else { return (Vec::new(), Attribution::None) };
        let slack = Duration::from_secs(60);
        let mut mine: Vec<&Session> = self
            .recent
            .iter()
            .filter(|s| s.directory == cwd)
            .filter(|s| s.created.is_none_or(|c| c + slack >= ctx.proc_start))
            .filter(|s| !ctx.attached.contains(&session_path(&db, &s.id)))
            .collect();
        mine.sort_by_key(|s| std::cmp::Reverse(s.updated));
        match mine.first() {
            Some(s) => (vec![session_path(&db, &s.id)], Attribution::CwdHeuristic),
            None => (Vec::new(), Attribution::None),
        }
    }

    fn unowned(&self, attached: &HashSet<PathBuf>) -> Vec<PathBuf> {
        let Some(db) = self.db() else { return Vec::new() };
        self.recent.iter().map(|s| session_path(&db, &s.id)).filter(|p| !attached.contains(p)).collect()
    }

    fn open(&self, path: &Path, spans: SpanRetention) -> Box<dyn SessionTracker> {
        Box::new(OpenCodeTranscript::new(path, spans))
    }

    /// OpenCode keeps no per-session file, so nothing on disk is detected as
    /// OpenCode; a session is reached through `transcripts` by its id.
    fn detect(&self, _path: &Path) -> bool {
        false
    }

    fn transcripts(&self) -> Vec<(String, PathBuf)> {
        let Some(db) = self.db() else { return Vec::new() };
        recent_sessions(&db, UNIX_EPOCH).into_iter().map(|s| (s.id.clone(), session_path(&db, &s.id))).collect()
    }
}

/// One OpenCode session as a `SessionSummary`, read from the database.
///
/// There is nothing to tail: each refresh re-reads the session row (and its
/// subagent rows and tool parts) and rebuilds the summary. A cheap check on
/// the session's `time_updated` skips the part scan when nothing changed, so a
/// stopped session costs one small query per tick.
pub struct OpenCodeTranscript {
    db: PathBuf,
    session_id: String,
    virtual_path: PathBuf,
    conn: Option<Connection>,
    retention: SpanRetention,
    summary: SessionSummary,
    /// The `time_updated` last read, so an unchanged session is not re-scanned.
    last_updated: Option<i64>,
    /// Where to look for the config that names MCP servers.
    config: ConfigRoots,
}

impl OpenCodeTranscript {
    pub fn new(virtual_path: &Path, retention: SpanRetention) -> Self {
        // The virtual path is `<db>/<session id>`; split it back.
        let session_id = session_id_of(virtual_path).unwrap_or_default();
        let db = virtual_path.parent().map(Path::to_path_buf).unwrap_or_default();
        OpenCodeTranscript {
            db,
            session_id,
            virtual_path: virtual_path.to_path_buf(),
            conn: None,
            retention,
            summary: SessionSummary {
                harness: Some(Harness::OpenCode),
                folds_child_usage: true,
                spans: retention.log(),
                ..Default::default()
            },
            last_updated: None,
            config: ConfigRoots::detect(),
        }
    }

    /// Read MCP server names from these roots instead of this machine's.
    pub fn with_config(mut self, config: ConfigRoots) -> Self {
        self.config = config;
        self
    }

    fn conn(&mut self) -> Option<&Connection> {
        if self.conn.is_none() {
            self.conn = open_ro(&self.db).ok();
        }
        self.conn.as_ref()
    }

    fn reload(&mut self) -> rusqlite::Result<()> {
        let retention = self.retention;
        let id = self.session_id.clone();
        let config = self.config.clone();
        let Some(conn) = self.conn() else { return Ok(()) };

        // The parent row plus every subagent row (parent_id = this session),
        // so the fold is one query.
        // `cost` below is OpenCode's own figure for every row, so the price
        // table never sets this session's cost and must not be named as its
        // source.
        let mut summary = SessionSummary {
            harness: Some(Harness::OpenCode),
            // The query below sums the parent and every `parent_id = parent`
            // row into this one summary.
            folds_child_usage: true,
            price_source: Some(crate::model::PriceSource::Harness),
            spans: retention.log(),
            ..Default::default()
        };
        let mut ids: Vec<(String, bool)> = Vec::new(); // (session id, is subagent)
        {
            let sql = "SELECT id, directory, agent, model, cost, tokens_input, tokens_output, tokens_reasoning, \
                       tokens_cache_read, tokens_cache_write, time_created, time_updated, version, parent_id \
                       FROM session WHERE id = ?1 OR parent_id = ?1 ORDER BY (parent_id IS NOT NULL), time_created";
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query([&id])?;
            while let Some(r) = rows.next()? {
                let row_id: String = r.get(0)?;
                let parent: Option<String> = r.get(13)?;
                let is_sub = parent.is_some();
                ids.push((row_id.clone(), is_sub));

                let usage = TokenUsage {
                    cache_write_unsplit: 0,
                    input: r.get::<_, i64>(5)? as u64,
                    output: (r.get::<_, i64>(6)? + r.get::<_, i64>(7)?) as u64, // output + reasoning
                    cache_read: r.get::<_, i64>(8)? as u64,
                    cache_write_5m: r.get::<_, i64>(9)? as u64,
                    cache_write_1h: 0,
                };
                summary.usage.add(&usage);
                summary.cost_usd += r.get::<_, f64>(4)?;

                if !is_sub {
                    summary.session_id = Some(row_id.clone());
                    summary.cwd = Some(PathBuf::from(r.get::<_, String>(1)?));
                    summary.model = r.get::<_, Option<String>>(3)?.as_deref().and_then(model_id);
                    summary.harness_version = r.get::<_, Option<String>>(12)?;
                    summary.started_at = from_ms(r.get::<_, i64>(10)?);
                    summary.last_activity = from_ms(r.get::<_, i64>(11)?);
                }
            }
        }
        if ids.is_empty() {
            // The session was deleted; keep the empty summary.
            self.summary = summary;
            return Ok(());
        }

        // Turns: assistant messages, split parent vs subagent.
        for (sid, is_sub) in &ids {
            let n: i64 = conn.query_row(
                "SELECT count(*) FROM message WHERE session_id = ?1 AND json_extract(data,'$.role') = 'assistant'",
                [sid],
                |r| r.get(0),
            )?;
            summary.turns += n as u64;
            summary.health.billable_messages += n as u64;
            if *is_sub {
                summary.subagent_turns += n as u64;
            }
        }
        summary.health.usage_records = summary.health.billable_messages;
        if summary.usage.total() == 0 {
            summary.health.empty_usage_records = summary.health.usage_records;
        }

        // Tool calls and their spans, plus turns and inferences from the
        // message times. For the live view only the newest `MAX_SPANS` matter,
        // so the queries are bounded; an export keeps everything.
        for (sid, _is_sub) in &ids {
            summary.tool_calls +=
                conn.query_row("SELECT count(*) FROM part WHERE session_id = ?1 AND json_extract(data,'$.type') = 'tool'", [sid], |r| {
                    r.get::<_, i64>(0)
                })? as u64;
        }
        let limit = match retention {
            SpanRetention::All => -1,
            SpanRetention::Recent => super::MAX_SPANS as i64,
        };
        let sub_ids: HashSet<&str> = ids.iter().filter(|(_, s)| *s).map(|(i, _)| i.as_str()).collect();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");

        // Context by source, over the whole session: the ledger needs every
        // response in order, not the bounded span window.
        let servers = summary.cwd.as_deref().map(|d| mcp_server_names(d, &config)).unwrap_or_default();
        for (sid, is_sub) in &ids {
            let ledger = context_ledger(conn, sid, &servers)?;
            if *is_sub {
                summary.context.merge(&ledger);
            } else {
                let subs = std::mem::take(&mut summary.context);
                summary.context = ledger;
                summary.context.merge(&subs);
            }
        }

        // MCP calls per server, over every tool part rather than the bounded
        // span window, since a count is the whole session. Only when the
        // config names a server: without one, no name can be told apart from
        // a built-in tool, and nothing is guessed.
        if !servers.is_empty() {
            let sql = format!(
                "SELECT json_extract(data,'$.tool'), json_extract(data,'$.state.status'), \
                 json_extract(data,'$.state.time.end'), json_extract(data,'$.state.time.start') \
                 FROM part WHERE json_extract(data,'$.type') = 'tool' AND session_id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|(i, _)| i as &dyn rusqlite::ToSql).collect();
            let mut rows = stmt.query(params.as_slice())?;
            while let Some(r) = rows.next()? {
                let Some(name) = r.get::<_, Option<String>>(0)? else { continue };
                let Some(server) = mcp_server_of(&name, &servers) else { continue };
                let error = r.get::<_, Option<String>>(1)?.as_deref() == Some("error");
                let at = r.get::<_, Option<i64>>(2)?.or(r.get::<_, Option<i64>>(3)?).and_then(from_ms);
                let u = summary.mcp.entry(server.to_string()).or_default();
                u.calls += 1;
                u.errors += u64::from(error);
                u.last_call = u.last_call.max(at);
            }
        }

        // Every span this scan will add, so the bounded log can keep the newest
        // across all three kinds rather than dropping one kind first.
        struct Pending {
            id: String,
            name: String,
            kind: SpanKind,
            start: SystemTime,
            /// `None` for a span still open: a tool with no end, an inference
            /// whose message has not completed, a turn whose reply is not done.
            end: Option<SystemTime>,
            sidechain: bool,
            error: bool,
        }
        let mut pending: Vec<Pending> = Vec::new();

        // Tool spans, from `tool` parts.
        {
            let sql = format!(
                "SELECT session_id, json_extract(data,'$.tool'), json_extract(data,'$.callID'), \
                 json_extract(data,'$.state.status'), json_extract(data,'$.state.time.start'), \
                 json_extract(data,'$.state.time.end') \
                 FROM part WHERE json_extract(data,'$.type') = 'tool' AND session_id IN ({placeholders}) \
                 ORDER BY time_created DESC LIMIT ?{}",
                ids.len() + 1
            );
            let mut stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::ToSql> =
                ids.iter().map(|(i, _)| i as &dyn rusqlite::ToSql).chain(std::iter::once(&limit as &dyn rusqlite::ToSql)).collect();
            let mut rows = stmt.query(params.as_slice())?;
            let mut i = 0;
            while let Some(r) = rows.next()? {
                let sid: String = r.get(0)?;
                let name: String = r.get::<_, Option<String>>(1)?.unwrap_or_else(|| "tool".into());
                let call_id: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
                let status: Option<String> = r.get(3)?;
                let Some(start) = r.get::<_, Option<i64>>(4)?.and_then(from_ms) else { continue };
                let end = r.get::<_, Option<i64>>(5)?.and_then(from_ms);
                let id = if call_id.is_empty() { format!("oc-tool-{i}") } else { call_id };
                i += 1;
                pending.push(Pending {
                    id,
                    name,
                    kind: SpanKind::Tool,
                    start,
                    end: Some(end.unwrap_or(start).max(start)),
                    sidechain: sub_ids.contains(sid.as_str()),
                    error: status.as_deref() == Some("error"),
                });
            }
        }

        // Turn and inference spans, from message times. A `user` message opens
        // a turn; each `assistant` message is one inference (created to
        // completed) and extends the turn it belongs to; the turn ends at the
        // last assistant reply before the next user message. Built per session,
        // since a subagent runs its own turns interleaved in time.
        {
            let sql = format!(
                "SELECT session_id, json_extract(data,'$.role'), json_extract(data,'$.time.created'), \
                 json_extract(data,'$.time.completed') FROM message WHERE session_id IN ({placeholders}) \
                 ORDER BY json_extract(data,'$.time.created') DESC LIMIT ?{}",
                ids.len() + 1
            );
            let mut stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::ToSql> =
                ids.iter().map(|(i, _)| i as &dyn rusqlite::ToSql).chain(std::iter::once(&limit as &dyn rusqlite::ToSql)).collect();
            let mut rows = stmt.query(params.as_slice())?;
            struct Msg {
                sid: String,
                role: String,
                created: SystemTime,
                completed: Option<SystemTime>,
            }
            let mut msgs: Vec<Msg> = Vec::new();
            while let Some(r) = rows.next()? {
                let sid: String = r.get(0)?;
                let role: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
                let Some(created) = r.get::<_, Option<i64>>(2)?.and_then(from_ms) else { continue };
                let completed = r.get::<_, Option<i64>>(3)?.and_then(from_ms);
                msgs.push(Msg { sid, role, created, completed });
            }
            msgs.sort_by_key(|m| m.created);
            // Newest message decides the live activity: a finished reply is
            // waiting, an open reply or a fresh prompt is working.
            match msgs.last() {
                Some(m) if m.role == "assistant" && m.completed.is_some() => summary.activity = Activity::Waiting,
                Some(_) => summary.activity = Activity::Working,
                None => {}
            }
            // Per session, close a turn when the next user message arrives.
            let sessions: Vec<String> = ids.iter().map(|(i, _)| i.clone()).collect();
            for sess in &sessions {
                let sidechain = sub_ids.contains(sess.as_str());
                let mut turn_idx = 0u64;
                let mut turn: Option<(String, SystemTime, Option<SystemTime>)> = None; // id, start, end
                let mut inf_idx = 0u64;
                let flush = |turn: &mut Option<(String, SystemTime, Option<SystemTime>)>, pending: &mut Vec<Pending>| {
                    if let Some((id, start, end)) = turn.take() {
                        pending.push(Pending { id, name: "turn".into(), kind: SpanKind::Turn, start, end, sidechain, error: false });
                    }
                };
                for m in msgs.iter().filter(|m| &m.sid == sess) {
                    if m.role == "user" {
                        flush(&mut turn, &mut pending);
                        turn_idx += 1;
                        turn = Some((format!("turn:{sess}:{turn_idx}"), m.created, None));
                    } else if m.role == "assistant" {
                        inf_idx += 1;
                        pending.push(Pending {
                            id: format!("inf:{sess}:{inf_idx}"),
                            name: "inference".into(),
                            kind: SpanKind::Inference,
                            start: m.created,
                            end: m.completed,
                            sidechain,
                            error: false,
                        });
                        // Open a turn if the window began mid-reply, and move
                        // its end to this reply's completion.
                        let end = m.completed.unwrap_or(m.created);
                        match &mut turn {
                            Some((_, _, e)) => *e = Some(end),
                            None => {
                                turn_idx += 1;
                                turn = Some((format!("turn:{sess}:{turn_idx}"), m.created, Some(end)));
                            }
                        }
                    }
                }
                flush(&mut turn, &mut pending);
            }
        }

        // Insert every span oldest first, so the bounded live log keeps the
        // newest by time no matter which kind it is.
        pending.sort_by_key(|p| p.start);
        for p in pending {
            summary.spans.open_kind(p.id.clone(), p.name, p.start, p.sidechain, p.kind);
            if let Some(end) = p.end {
                if p.error {
                    summary.spans.close(&p.id, end.max(p.start), true);
                } else {
                    summary.spans.end_at(&p.id, end.max(p.start));
                }
            }
        }

        // If there were no messages at all, activity stays unknown.
        self.summary = summary;
        Ok(())
    }
}

/// One session's context ledger, fed from its assistant messages in order.
/// See the module notes for why each message is one response and its tool
/// parts are the results the next response carries.
fn context_ledger(conn: &Connection, session_id: &str, servers: &[String]) -> rusqlite::Result<ContextLedger> {
    let mut calls: std::collections::HashMap<String, Vec<(String, String)>> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT message_id, json_extract(data,'$.callID'), json_extract(data,'$.tool') FROM part \
             WHERE session_id = ?1 AND json_extract(data,'$.type') = 'tool' ORDER BY id",
        )?;
        let mut rows = stmt.query([session_id])?;
        while let Some(r) = rows.next()? {
            let (Some(msg), Some(call), Some(tool)) =
                (r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)
            else {
                continue;
            };
            calls.entry(msg).or_default().push((call, tool));
        }
    }
    let mut ledger = ContextLedger::default();
    let mut stmt = conn.prepare(
        "SELECT id, json_extract(data,'$.tokens.input'), json_extract(data,'$.tokens.output'), \
         json_extract(data,'$.tokens.reasoning'), json_extract(data,'$.tokens.cache.read'), \
         json_extract(data,'$.tokens.cache.write'), json_extract(data,'$.cost'), json_extract(data,'$.modelID'), \
         json_extract(data,'$.summary'), json_extract(data,'$.mode') FROM message \
         WHERE session_id = ?1 AND json_extract(data,'$.role') = 'assistant' ORDER BY time_created, id",
    )?;
    let mut rows = stmt.query([session_id])?;
    let n = |v: Option<i64>| v.unwrap_or(0).max(0) as u64;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let usage = TokenUsage {
            cache_write_unsplit: 0,
            input: n(r.get(1)?),
            output: n(r.get(2)?) + n(r.get(3)?),
            cache_read: n(r.get(4)?),
            cache_write_5m: n(r.get(5)?),
            cache_write_1h: 0,
        };
        let total: f64 = r.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
        let cost = r
            .get::<_, Option<String>>(7)?
            .and_then(|m| crate::pricing::table().lookup(&m))
            .map(|p| scaled(p.breakdown(&usage), total))
            .unwrap_or_default();
        ledger.response(&usage, &cost);
        let summary = r.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0 || r.get::<_, Option<String>>(9)?.as_deref() == Some("compaction");
        if summary {
            ledger.compacted();
        }
        for (call, tool) in calls.remove(&id).unwrap_or_default() {
            match mcp_server_of(&tool, servers) {
                Some(server) => ledger.result(&call, ContextOrigin::Mcp, server),
                None => ledger.result(&call, ContextOrigin::Tool, &tool),
            }
        }
    }
    Ok(ledger)
}

/// A breakdown at list rates, scaled so its classes add up to what OpenCode
/// says the message cost. OpenCode's figure is the real one; the table only
/// says how it divides between input, cache and output. With no OpenCode
/// figure the list-rate breakdown stands.
fn scaled(b: CostBreakdown, total: f64) -> CostBreakdown {
    let list = b.total();
    if total <= 0.0 || list <= 0.0 {
        return b;
    }
    let k = total / list;
    CostBreakdown {
        input: b.input * k,
        cache_write_5m: b.cache_write_5m * k,
        cache_write_1h: b.cache_write_1h * k,
        cache_write_unsplit: b.cache_write_unsplit * k,
        cache_read: b.cache_read * k,
        output: b.output * k,
        web_search: b.web_search * k,
    }
}

impl SessionTracker for OpenCodeTranscript {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        let id = self.session_id.clone();
        let updated: Option<i64> =
            self.conn().and_then(|c| c.query_row("SELECT time_updated FROM session WHERE id = ?1", [&id], |r| r.get(0)).ok());
        // Nothing changed since the last read: keep the summary as is.
        if updated.is_some() && updated == self.last_updated {
            return Ok(false);
        }
        self.last_updated = updated;
        // A schema change or a locked read must not crash the collector; on
        // error the summary is left as it was and the row simply does not update.
        let _ = self.reload();
        Ok(false)
    }

    fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    fn path(&self) -> &Path {
        &self.virtual_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// A minimal OpenCode database with the columns the adapter reads.
    fn make_db(dir: &Path) -> PathBuf {
        let db = dir.join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, directory TEXT, agent TEXT, \
             model TEXT, cost REAL DEFAULT 0, tokens_input INTEGER DEFAULT 0, tokens_output INTEGER DEFAULT 0, \
             tokens_reasoning INTEGER DEFAULT 0, tokens_cache_read INTEGER DEFAULT 0, tokens_cache_write INTEGER DEFAULT 0, \
             time_created INTEGER, time_updated INTEGER, version TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        // A parent session and one subagent.
        conn.execute(
            "INSERT INTO session VALUES ('ses_parent', 'p', NULL, '/tmp/proj', 'build', \
             '{\"id\":\"deepseek-v4-pro\",\"providerID\":\"deepseek\"}', 0.25, 1000, 200, 50, 900000, 0, 1000, 5000, '1.18.15')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session VALUES ('ses_child', 'p', 'ses_parent', '/tmp/proj', 'explore', \
             '{\"id\":\"deepseek-v4-pro\"}', 0.05, 300, 40, 10, 1000, 0, 2000, 3000, '1.18.15')",
            [],
        )
        .unwrap();
        // A user prompt and two assistant replies in the parent, one assistant
        // reply in the subagent, with created/completed times so the turn and
        // inference spans can be built.
        let msg = |role: &str, created: i64, completed: Option<i64>| match completed {
            Some(c) => format!("{{\"role\":\"{role}\",\"time\":{{\"created\":{created},\"completed\":{c}}}}}"),
            None => format!("{{\"role\":\"{role}\",\"time\":{{\"created\":{created}}}}}"),
        };
        for (i, sid, data) in [
            (1, "ses_parent", msg("user", 100, None)),
            (2, "ses_parent", msg("assistant", 110, Some(200))),
            (3, "ses_parent", msg("assistant", 210, Some(300))),
            (4, "ses_child", msg("assistant", 150, Some(180))),
        ] {
            conn.execute("INSERT INTO message VALUES (?1, ?2, ?3, ?4)", rusqlite::params![format!("m{i}"), sid, 1000 + i as i64, data])
                .unwrap();
        }
        // Tool parts: two in the parent (one failing), one in the child.
        let tool = |tool: &str, call: &str, status: &str, start: i64, end: i64| {
            format!(
                "{{\"type\":\"tool\",\"tool\":\"{tool}\",\"callID\":\"{call}\",\"state\":{{\"status\":\"{status}\",\"time\":{{\"start\":{start},\"end\":{end}}}}}}}"
            )
        };
        for (i, sid, data) in [
            (1, "ses_parent", tool("read", "c1", "completed", 1000, 1300)),
            (2, "ses_parent", tool("bash", "c2", "error", 1400, 2400)),
            (3, "ses_child", tool("grep", "c3", "completed", 1500, 1600)),
        ] {
            conn.execute("INSERT INTO part VALUES (?1, 'm', ?2, ?3, ?4)", rusqlite::params![format!("p{i}"), sid, 1000 + i as i64, data])
                .unwrap();
        }
        db
    }

    #[test]
    fn reads_a_session_folds_its_subagent_and_builds_tool_spans() {
        let dir = std::env::temp_dir().join(format!("agent-top-oc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = make_db(&dir);
        let path = session_path(&db, "ses_parent");
        let mut t = OpenCodeTranscript::new(&path, SpanRetention::All);
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.session_id.as_deref(), Some("ses_parent"));
        assert_eq!(s.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(s.cwd.as_deref(), Some(Path::new("/tmp/proj")));
        assert_eq!(s.harness_version.as_deref(), Some("1.18.15"));
        // Parent input 1000 + child 300; output+reasoning parent 250 + child 50.
        assert_eq!(s.usage.input, 1300);
        assert_eq!(s.usage.output, 300);
        assert_eq!(s.usage.cache_read, 901000);
        // OpenCode's own cost, parent + subagent, used directly.
        assert!((s.cost_usd - 0.30).abs() < 1e-9, "{}", s.cost_usd);
        assert_eq!(s.unpriced_tokens, 0, "OpenCode prices its own session");
        assert_eq!(
            s.price_source,
            Some(crate::model::PriceSource::Harness),
            "the cost came from OpenCode, so no price table may be named as its source"
        );
        assert_eq!(s.turns, 3, "two assistant turns in the parent, one in the subagent");
        assert_eq!(s.subagent_turns, 1);
        assert!(s.folds_child_usage, "a child's usage is inside this summary, so the share is worth breaking out");
        assert_eq!(s.tool_calls, 3);
        let tools: Vec<_> = s.spans.iter().filter(|sp| sp.kind == SpanKind::Tool).collect();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, "read");
        assert_eq!(tools[0].duration_ms, Some(300));
        let bash = tools.iter().find(|sp| sp.name == "bash").unwrap();
        assert!(bash.error);
        let grep = tools.iter().find(|sp| sp.name == "grep").unwrap();
        assert!(grep.sidechain, "the subagent's tool call is a sidechain");

        // Inference spans: one per assistant message (created to completed).
        let inf: Vec<_> = s.spans.iter().filter(|sp| sp.kind == SpanKind::Inference).collect();
        assert_eq!(inf.len(), 3, "two in the parent, one in the subagent");
        let parent_inf: Vec<_> = inf.iter().filter(|sp| !sp.sidechain).collect();
        assert_eq!(parent_inf[0].duration_ms, Some(90), "110 to 200");
        assert_eq!(parent_inf[1].duration_ms, Some(90), "210 to 300");
        assert_eq!(inf.iter().find(|sp| sp.sidechain).unwrap().duration_ms, Some(30), "subagent inference 150 to 180");
        // Turn spans: one per session. The parent's runs prompt to last reply.
        let turns: Vec<_> = s.spans.iter().filter(|sp| sp.kind == SpanKind::Turn).collect();
        assert_eq!(turns.len(), 2, "one parent turn, one subagent turn");
        let parent_turn = turns.iter().find(|sp| !sp.sidechain).unwrap();
        assert_eq!(parent_turn.duration_ms, Some(200), "user at 100 to the last reply completing at 300");
        assert!(turns.iter().any(|sp| sp.sidechain), "the subagent has its own turn");
        assert_eq!(s.activity, Activity::Waiting, "the newest message is a completed reply");
        assert!(!s.health.fields_unrecognised());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_sessions_lists_only_top_level_and_attributes_by_directory() {
        let dir = std::env::temp_dir().join(format!("agent-top-oc-attr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = make_db(&dir);
        let found = recent_sessions(&db, UNIX_EPOCH);
        assert_eq!(found.len(), 1, "only the parent, not the subagent");
        assert_eq!(found[0].id, "ses_parent");
        assert_eq!(found[0].directory, PathBuf::from("/tmp/proj"));

        let adapter = OpenCodeAdapter { db: Some(db.clone()), recent: found };
        let ctx = AttributeContext {
            cwd: Some(Path::new("/tmp/proj")),
            proc_start: UNIX_EPOCH + Duration::from_secs(3),
            now: SystemTime::now(),
            attached: &HashSet::new(),
            activity_timeout: Duration::from_secs(900),
        };
        let root = ProcNode {
            pid: 1,
            ppid: None,
            name: "opencode".into(),
            cmdline: "opencode".into(),
            kind: crate::model::ProcKind::Agent,
            harness: Some(Harness::OpenCode),
            cpu_percent: 0.0,
            rss_bytes: 0,
            age_secs: 0,
            cwd: None,
            children: Vec::new(),
        };
        let (paths, attribution) = adapter.attribute(&root, None, &ctx);
        assert_eq!(paths, vec![session_path(&db, "ses_parent")]);
        assert_eq!(attribution, Attribution::CwdHeuristic);
        // A different directory gets nothing.
        let ctx2 = AttributeContext { cwd: Some(Path::new("/tmp/other")), ..ctx };
        assert!(adapter.attribute(&root, None, &ctx2).0.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Values from a real OpenCode 1.18.15 session on 2026-09-13 with a
    /// filesystem server named `scratch_fs`: one call that listed a directory
    /// and one the server refused. The config is `.jsonc` with a comment and a
    /// trailing comma, and names a second server whose name is a prefix of
    /// the first and a third with a character OpenCode sanitises.
    #[test]
    fn counts_mcp_calls_per_configured_server() {
        let dir = std::env::temp_dir().join(format!("agent-top-oc-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("repo/sub");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(dir.join("repo/.git")).unwrap();
        std::fs::write(
            dir.join("repo/opencode.jsonc"),
            r#"{
  // servers for this repo
  "mcp": {
    "scratch_fs": { "type": "local", "command": ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp/x"] },
    "scratch": { "type": "remote", "url": "https://example.invalid/mcp" },
    "my.api": { "type": "remote", "url": "https://example.invalid/api", },
  },
}"#,
        )
        .unwrap();
        // A config above the worktree root is not OpenCode's to read.
        std::fs::write(dir.join("opencode.json"), r#"{"mcp":{"outside":{}}}"#).unwrap();
        let db = make_db(&dir);
        let conn = Connection::open(&db).unwrap();
        conn.execute("UPDATE session SET directory = ?1", [project.to_string_lossy()]).unwrap();
        let tool = |tool: &str, call: &str, status: &str, start: i64, end: i64| {
            format!(
                "{{\"type\":\"tool\",\"tool\":\"{tool}\",\"callID\":\"{call}\",\"state\":{{\"status\":\"{status}\",\"time\":{{\"start\":{start},\"end\":{end}}}}}}}"
            )
        };
        for (i, sid, data) in [
            (
                10,
                "ses_parent",
                tool("scratch_fs_list_directory", "call_00_7aG02jznvnkRRCWvUZzh2824", "completed", 1789297396598, 1789297396604),
            ),
            (
                11,
                "ses_parent",
                tool("scratch_fs_read_text_file", "call_00_1pkIe0AHF5h5X8VTC0EH3246", "error", 1789297397807, 1789297397810),
            ),
            (12, "ses_child", tool("scratch_search", "call_00_child", "completed", 1789297398000, 1789297398100)),
            (13, "ses_parent", tool("my_api_get", "call_00_api", "completed", 1789297399000, 1789297399050)),
            (14, "ses_parent", tool("outside_thing", "call_00_out", "completed", 1789297399100, 1789297399150)),
        ] {
            conn.execute("INSERT INTO part VALUES (?1, 'm', ?2, ?3, ?4)", rusqlite::params![format!("p{i}"), sid, 1000 + i as i64, data])
                .unwrap();
        }
        drop(conn);

        let roots = ConfigRoots { global: Some(dir.join("no-global")), home: Some(dir.join("no-home")) };
        assert_eq!(mcp_server_names(&project, &roots), vec!["my.api".to_string(), "scratch".into(), "scratch_fs".into()]);

        let mut t = OpenCodeTranscript::new(&session_path(&db, "ses_parent"), SpanRetention::Recent).with_config(roots);
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.tool_calls, 8, "every tool part is still a tool call");
        assert_eq!(s.mcp.len(), 3, "{:?}", s.mcp);
        let fs = &s.mcp["scratch_fs"];
        assert_eq!((fs.calls, fs.errors), (2, 1), "the longer server name wins over its prefix");
        assert_eq!(fs.last_call, from_ms(1789297397810));
        assert_eq!(s.mcp["scratch"].calls, 1, "a subagent's call counts for the session");
        assert_eq!(s.mcp["my.api"].calls, 1, "matched through the sanitised name, filed under the configured one");
        assert!(!s.mcp.contains_key("outside"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Three responses, a compaction, and a subagent. The first response's
    /// prompt is all `prompts & replies`; the second grows by the `read`
    /// result plus the first reply; the MCP call's result lands in the third.
    /// The summary reply resets the ledger, so the fourth response's prompt is
    /// new material. A priced model's costs add up to the prompt-side share of
    /// OpenCode's own message costs.
    #[test]
    fn context_by_source_from_messages_in_order() {
        let dir = std::env::temp_dir().join(format!("agent-top-oc-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("opencode.json"), r#"{"mcp":{"scratch_fs":{}}}"#).unwrap();
        let db = dir.join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, directory TEXT, agent TEXT, \
             model TEXT, cost REAL DEFAULT 0, tokens_input INTEGER DEFAULT 0, tokens_output INTEGER DEFAULT 0, \
             tokens_reasoning INTEGER DEFAULT 0, tokens_cache_read INTEGER DEFAULT 0, tokens_cache_write INTEGER DEFAULT 0, \
             time_created INTEGER, time_updated INTEGER, version TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        let model = "claude-opus-5";
        let price = crate::pricing::table().lookup(model).expect("the test model is priced");
        conn.execute(
            "INSERT INTO session VALUES ('s', 'p', NULL, ?1, 'build', ?2, 0, 0, 0, 0, 0, 0, 1, 9, '1.18.15')",
            rusqlite::params![dir.to_string_lossy(), format!("{{\"id\":\"{model}\"}}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session VALUES ('sub', 'p', 's', ?1, 'explore', NULL, 0, 0, 0, 0, 0, 0, 2, 8, '1.18.15')",
            [dir.to_string_lossy()],
        )
        .unwrap();
        struct Reply {
            id: &'static str,
            sid: &'static str,
            created: i64,
            input: u64,
            output: u64,
            reasoning: u64,
            read: u64,
            cost: f64,
            summary: bool,
        }
        let reply = |id, sid, created, input, output, reasoning, read, cost, summary| Reply {
            id,
            sid,
            created,
            input,
            output,
            reasoning,
            read,
            cost,
            summary,
        };
        let replies = [
            reply("m1", "s", 10, 1000, 40, 10, 0, 0.02, false),
            reply("m2", "s", 20, 400, 30, 0, 1050, 0.01, false),
            reply("m3", "s", 30, 300, 20, 0, 1480, 0.01, false),
            reply("m4", "s", 40, 0, 0, 0, 0, 0.0, true),
            reply("m5", "s", 50, 200, 10, 0, 0, 0.005, false),
            reply("k1", "sub", 25, 500, 5, 0, 0, 0.004, false),
        ];
        for r in &replies {
            let mode = if r.summary { "compaction" } else { "build" };
            let data = format!(
                "{{\"role\":\"assistant\",\"mode\":\"{mode}\",\"summary\":{},\"modelID\":\"{model}\",\"cost\":{},\
                 \"tokens\":{{\"input\":{},\"output\":{},\"reasoning\":{},\"cache\":{{\"read\":{},\"write\":0}}}},\
                 \"time\":{{\"created\":{},\"completed\":{}}}}}",
                r.summary,
                r.cost,
                r.input,
                r.output,
                r.reasoning,
                r.read,
                r.created,
                r.created + 5
            );
            conn.execute("INSERT INTO message VALUES (?1, ?2, ?3, ?4)", rusqlite::params![r.id, r.sid, r.created, data]).unwrap();
        }
        let tool = |id: &str, msg: &str, sid: &str, name: &str, call: &str| {
            let data = format!(
                "{{\"type\":\"tool\",\"tool\":\"{name}\",\"callID\":\"{call}\",\"state\":{{\"status\":\"completed\",\"time\":{{\"start\":1,\"end\":2}}}}}}"
            );
            conn.execute("INSERT INTO part VALUES (?1, ?2, ?3, 1, ?4)", rusqlite::params![id, msg, sid, data]).unwrap();
        };
        tool("p1", "m1", "s", "read", "c1");
        tool("p2", "m2", "s", "scratch_fs_list_directory", "c2");
        tool("p3", "m3", "s", "bash", "c3");
        drop(conn);

        let roots = ConfigRoots { global: Some(dir.join("no-global")), home: Some(dir.join("no-home")) };
        let mut t = OpenCodeTranscript::new(&session_path(&db, "s"), SpanRetention::Recent).with_config(roots);
        t.refresh().unwrap();
        let rows: std::collections::HashMap<String, crate::model::ContextSource> =
            t.summary().context.sources().into_iter().map(|c| (c.name.clone(), c)).collect();

        // m1: 1000 prompt, all Other. m2: 1450 prompt, +450 = 50 reply (40+10) + 400 read.
        assert_eq!((rows["read"].calls, rows["read"].tokens), (1, 400), "{rows:?}");
        // m3: 1780 prompt, +330 = 30 reply + 300 scratch_fs.
        assert_eq!((rows["scratch_fs"].calls, rows["scratch_fs"].tokens, rows["scratch_fs"].origin), (1, 300, ContextOrigin::Mcp));
        // m4 is the summary reply with no prompt of its own, so it sizes
        // nothing and resets the ledger; m5's whole 200-token prompt is new and
        // is filed under the one result still pending, bash.
        assert_eq!((rows["bash"].calls, rows["bash"].tokens), (1, 200), "{rows:?}");
        // Other: m1's 1000, m2's 50, m3's 30, and the subagent's first 500.
        assert_eq!(rows[ContextLedger::OTHER].tokens, 1580, "{rows:?}");

        // Costs reconcile to the prompt-side share of OpenCode's message costs.
        let prompt_side = |input: u64, output: u64, read: u64, total: f64| {
            let usage = TokenUsage { input, output, cache_read: read, ..Default::default() };
            let b = scaled(price.breakdown(&usage), total);
            b.input + b.cache_read + b.cache_write_5m + b.cache_write_1h
        };
        let expected: f64 =
            replies.iter().filter(|r| r.input + r.read > 0).map(|r| prompt_side(r.input, r.output + r.reasoning, r.read, r.cost)).sum();
        let got: f64 = rows.values().map(|c| c.cost_usd).sum();
        assert!((expected - got).abs() < 1e-9, "rows {got} vs prompt-side {expected}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_configured_server_means_no_mcp_guess() {
        assert_eq!(mcp_server_of("scratch_fs_list_directory", &[]), None);
        let servers = vec!["github".to_string(), "github_enterprise".to_string()];
        assert_eq!(mcp_server_of("github_enterprise_search", &servers), Some("github_enterprise"));
        assert_eq!(mcp_server_of("github_search", &servers), Some("github"));
        assert_eq!(mcp_server_of("github_", &servers), None, "a server name with no tool after it");
        assert_eq!(mcp_server_of("githubsearch", &servers), None);
        assert_eq!(mcp_server_of("todowrite", &servers), None);
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_are_stripped_but_strings_are_kept() {
        let text = r#"{ "a": "http://x//y", /* block, */ "b": [1, 2,], // line
 "c": "a \"quoted, }\" value", }"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(v["a"], "http://x//y");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(v["c"], "a \"quoted, }\" value");
    }

    #[test]
    fn model_id_is_pulled_from_the_json_blob() {
        assert_eq!(model_id(r#"{"id":"deepseek-v4-pro","providerID":"deepseek"}"#), Some("deepseek-v4-pro".into()));
        assert_eq!(model_id("claude-fable-5-1"), Some("claude-fable-5-1".into()));
        assert_eq!(model_id(""), None);
    }
}
