//! Kodelet: cumulative conversation snapshots in `$KODELET_BASE_PATH/storage.db`
//! (default `~/.kodelet/storage.db`), opened read-only.
//!
//! Format verified on 2026-09-19 against Kodelet v0.6.17-beta (`6dcef0a4`).
//! Source: `pkg/conversations/sqlite`, `pkg/types/llm/usage.go`, and provider
//! persistence code in `pkg/llm`.
//!
//! Non-obvious storage contracts:
//! * JSON columns may be BLOBs despite TEXT affinity. Usage and USD costs are
//!   cumulative snapshots; `conversation_summaries` duplicates the accounting.
//! * Responses input includes cache reads; Anthropic/Chat input excludes them.
//!   Cache writes have no TTL split, so the 5m slot holds the unsplit aggregate.
//! * Forks copy messages/results but reset usage. Only explicit parent metadata
//!   establishes hierarchy; copied results are not new child work.
//! * Compaction discards calls/results but retains usage, with no lifetime tool
//!   counter. Retained/observed calls therefore provide only a lower bound.
//! * Tool-result timestamps mark completion; `metadata.executionTime` is in
//!   nanoseconds. Per-response usage and message timestamps are not persisted.

mod subagent;

use super::{AttributeContext, HarnessAdapter, RegistryHints, SessionSummary, SessionTracker, SpanRetention, parse_rfc3339_utc};
use crate::model::{Activity, Attribution, CostBreakdown, Harness, PriceSource, ProcNode, SubagentInfo, TokenUsage};
use crate::process::{RawProc, kodelet_command};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use subagent::SubagentIndex;

type SharedSubagents = Rc<RefCell<SubagentIndex>>;

pub fn kodelet_dir() -> Option<PathBuf> {
    std::env::var_os("KODELET_BASE_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".kodelet")))
}

pub fn db_path() -> Option<PathBuf> {
    let path = kodelet_dir()?.join("storage.db");
    path.is_file().then_some(path)
}

pub fn session_path(db: &Path, id: &str) -> PathBuf {
    db.join(id)
}

pub fn session_id_of(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}

fn open_ro(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(Duration::ZERO)?;
    Ok(conn)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.trim() == id && id != "." && id != ".." && !id.contains(['/', '\\'])
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)", [name], |r| r.get(0)).unwrap_or(false)
}

/// JSON uses RFC3339Nano; modernc SQLite also writes Go time strings with a
/// space separator, numeric offset and optional zone name. CURRENT_TIMESTAMP
/// (older event tables) is UTC without a zone. Preserve fractional seconds.
fn timestamp(text: &str) -> Option<SystemTime> {
    if text.as_bytes().get(10) == Some(&b'T') {
        return parse_rfc3339_utc(text);
    }
    let mut parts = text.split_whitespace();
    let date = parts.next()?;
    let clock = parts.next()?;
    let zone = parts.next().unwrap_or("Z");
    let zone = if zone == "UTC" { "Z" } else { zone };
    let combined = format!("{date}T{clock}");
    parse_rfc3339_utc(&combined).or_else(|| parse_rfc3339_utc(&format!("{combined}{zone}")))
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[derive(Debug)]
struct Session {
    id: String,
    cwd: Option<PathBuf>,
    created: Option<SystemTime>,
    updated: Option<SystemTime>,
    child: bool,
}

fn recent_sessions(db: &Path, since: SystemTime) -> Vec<Session> {
    let Ok(conn) = open_ro(db) else { return Vec::new() };
    // Avoid both timestamp string ordering (different offsets) and SQLite's
    // date parser (which cannot read all of Go's time.String representations).
    let Ok(mut stmt) =
        conn.prepare("SELECT id, cwd, CAST(created_at AS TEXT), CAST(updated_at AS TEXT), CAST(metadata AS TEXT) FROM conversations")
    else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        let metadata = r.get::<_, Option<String>>(4)?.and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or_default();
        Ok(Session {
            id: r.get(0)?,
            cwd: r.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()).map(PathBuf::from),
            created: timestamp(&r.get::<_, String>(2)?),
            updated: timestamp(&r.get::<_, String>(3)?),
            child: string(&metadata, "parent_conversation_id").is_some(),
        })
    });
    let mut sessions: Vec<_> = rows
        .map(|rows| rows.flatten().filter(|s| s.updated.is_some_and(|at| at >= since) || since == UNIX_EPOCH).collect())
        .unwrap_or_default();
    // A virtual path must stay inside the database, even for a malformed ID.
    sessions.retain(|s| valid_id(&s.id));
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    sessions
}

/// Each conversation is a separate row, including children. Nothing is folded
/// into a parent, so snapshots and exports can sum their accounting once.
#[derive(Default)]
pub struct KodeletAdapter {
    db: Option<PathBuf>,
    recent: Vec<Session>,
    exact: HashMap<u32, (Vec<PathBuf>, Attribution)>,
    reserved: HashSet<PathBuf>,
    hints: HashMap<u32, RegistryHints>,
    modern: bool,
    subagents: RefCell<HashMap<PathBuf, SharedSubagents>>,
}

impl KodeletAdapter {
    fn db(&self) -> Option<PathBuf> {
        self.db.clone().or_else(db_path)
    }

    fn reserve(&mut self, db: &Path, pid: u32, id: &str, attribution: Attribution) {
        if !valid_id(id) {
            return;
        }
        let path = session_path(db, id);
        if self.reserved.insert(path.clone()) {
            self.exact.entry(pid).or_insert_with(|| (Vec::new(), attribution)).0.push(path);
        }
    }

    fn prepare_registry(&mut self, db: &Path, roots: &[&ProcNode], by_pid: &HashMap<u32, &RawProc>) {
        let Ok(conn) = open_ro(db) else { return };
        self.modern = table_exists(&conn, "runner_registrations") || table_exists(&conn, "chat_turns");
        let Some(base) = db.parent() else { return };
        let host = read_json(&base.join("runners/host.json")).and_then(|v| string(&v, "instanceId"));
        let now = SystemTime::now();
        let root_pids: HashSet<_> = roots.iter().map(|r| r.pid).collect();

        // Never use host_pid alone: a remote runner may have exactly the same
        // PID as a local client. A stale registration also cannot own a reused
        // PID: its connection must postdate this process's start.
        if let Some(host) = host {
            let sql = "SELECT DISTINCT rr.conversation_id, r.host_pid, CAST(r.connected_at AS TEXT), \
                       CAST(r.last_heartbeat_at AS TEXT), r.kodelet_version FROM runner_runs rr \
                       JOIN runner_registrations r ON r.id = rr.runner_id JOIN conversations c ON c.id = rr.conversation_id \
                       WHERE rr.status IN ('opening', 'running') AND r.status IN ('idle', 'busy') AND r.host_instance_id = ?1";
            if let Ok(mut stmt) = conn.prepare(sql)
                && let Ok(rows) = stmt.query_map([host], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u32>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
            {
                for (id, pid, connected, heartbeat, version) in rows.flatten() {
                    let Some(raw) = by_pid.get(&pid).filter(|_| root_pids.contains(&pid)) else { continue };
                    let start = UNIX_EPOCH + Duration::from_secs(raw.start_time);
                    if !execution_host(raw)
                        || connected.as_deref().and_then(timestamp).is_none_or(|at| at < start)
                        || !heartbeat
                            .as_deref()
                            .and_then(timestamp)
                            .is_some_and(|at| at >= start && now.duration_since(at).is_ok_and(|age| age <= Duration::from_secs(45)))
                    {
                        continue;
                    }
                    self.reserve(db, pid, &id, Attribution::HarnessRegistry);
                    let hints = self.hints.entry(pid).or_default();
                    hints.version = version;
                    hints.status = Some("busy".into());
                }
            }
        }

        // The local daemon owns model execution even when its workspace runs
        // remotely. Prefer a proven local runner above; otherwise active turn
        // receipts can be assigned to this verified daemon, never to a client.
        let connection_path = base.join("server/connection.json");
        if let Some(connection) = read_json(&connection_path)
            && connection.get("schemaVersion").and_then(Value::as_u64) == Some(1)
            && string(&connection, "instanceId").is_some()
            && let Some(pid) = connection.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok())
            && root_pids.contains(&pid)
            && let Some(raw) = by_pid.get(&pid)
            && kodelet_command(raw.cmd.get(1..).unwrap_or_default()).first().is_some_and(|a| a == "serve")
            && std::fs::metadata(&connection_path)
                .and_then(|m| m.modified())
                .ok()
                .is_some_and(|at| at >= UNIX_EPOCH + Duration::from_secs(raw.start_time))
        {
            self.hints.entry(pid).or_default().version = string(&connection, "version");
            if let Ok(mut stmt) = conn.prepare("SELECT DISTINCT t.conversation_id FROM chat_turns t JOIN conversations c ON c.id = t.conversation_id WHERE t.status IN ('accepted', 'running')")
                && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
            {
                for id in rows.flatten() {
                    self.reserve(db, pid, &id, Attribution::HarnessRegistry);
                    self.hints.entry(pid).or_default().status = Some("busy".into());
                }
            }
        }
    }
}

fn execution_host(raw: &RawProc) -> bool {
    let args = kodelet_command(raw.cmd.get(1..).unwrap_or_default());
    args.first().is_some_and(|a| a == "serve") || (args.first().is_some_and(|a| a == "runner") && args.get(1).is_some_and(|a| a == "start"))
}

impl HarnessAdapter for KodeletAdapter {
    fn harness(&self) -> Harness {
        Harness::Kodelet
    }

    fn rescan(&mut self, since: SystemTime) {
        self.db = db_path();
        self.recent = self.db.as_deref().map(|db| recent_sessions(db, since)).unwrap_or_default();
    }

    fn prepare(&mut self, roots: &[&ProcNode], by_pid: &HashMap<u32, &RawProc>) {
        self.exact.clear();
        self.reserved.clear();
        self.hints.clear();
        self.modern = false;
        let Some(db) = self.db() else { return };
        self.prepare_registry(&db, roots, by_pid);
    }

    fn hints(&self, pid: u32) -> Option<RegistryHints> {
        self.hints.get(&pid).cloned()
    }

    fn attribute(&self, root: &ProcNode, raw: Option<&RawProc>, ctx: &AttributeContext) -> (Vec<PathBuf>, Attribution) {
        if let Some((paths, attribution)) = self.exact.get(&root.pid) {
            let paths: Vec<_> = paths.iter().filter(|p| !ctx.attached.contains(*p)).cloned().collect();
            return if paths.is_empty() { (paths, Attribution::None) } else { (paths, *attribution) };
        }
        let (Some(cwd), Some(db), Some(raw)) = (ctx.cwd, self.db(), raw) else { return (Vec::new(), Attribution::None) };
        if self.modern || !execution_host(raw) || raw.cmd.iter().any(|a| a == "--server" || a.starts_with("--server=")) {
            return (Vec::new(), Attribution::None);
        }
        let matches: Vec<_> = self
            .recent
            .iter()
            .filter(|s| !s.child && s.cwd.as_deref() == Some(cwd))
            .filter(|s| s.created.is_some_and(|at| at + Duration::from_secs(60) >= ctx.proc_start))
            .filter(|s| s.updated.is_some_and(|at| ctx.now.duration_since(at).is_ok_and(|age| age <= ctx.activity_timeout)))
            .map(|s| session_path(&db, &s.id))
            .filter(|p| !self.reserved.contains(p) && !ctx.attached.contains(p))
            .collect();
        if matches.len() == 1 { (matches, Attribution::CwdHeuristic) } else { (Vec::new(), Attribution::None) }
    }

    fn unowned(&self, attached: &HashSet<PathBuf>) -> Vec<PathBuf> {
        let Some(db) = self.db() else { return Vec::new() };
        self.recent.iter().map(|s| session_path(&db, &s.id)).filter(|p| !attached.contains(p)).collect()
    }

    fn open(&self, path: &Path, spans: SpanRetention) -> Box<dyn SessionTracker> {
        let mut tracker = KodeletTranscript::new(path, spans);
        if let Some(base) = path.parent().and_then(Path::parent) {
            tracker.subagents = Some(
                self.subagents
                    .borrow_mut()
                    .entry(base.to_path_buf())
                    .or_insert_with(|| Rc::new(RefCell::new(SubagentIndex::new(base.to_path_buf()))))
                    .clone(),
            );
        }
        Box::new(tracker)
    }

    fn detect(&self, _path: &Path) -> bool {
        false
    }

    fn transcripts(&self) -> Vec<(String, PathBuf)> {
        let Some(db) = self.db() else { return Vec::new() };
        recent_sessions(&db, UNIX_EPOCH).into_iter().map(|s| (s.id.clone(), session_path(&db, &s.id))).collect()
    }
}

pub struct KodeletTranscript {
    path: PathBuf,
    retention: SpanRetention,
    conn: Option<Connection>,
    data_version: Option<i64>,
    stamp: Option<SessionStamp>,
    subagents: Option<SharedSubagents>,
    sidecar: Option<SubagentInfo>,
    /// Call ID -> whether it is a native web search. Only metadata is retained,
    /// so a compaction cannot make an already observed invocation disappear.
    observed_calls: HashMap<String, bool>,
    summary: SessionSummary,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SessionStamp {
    updated: Option<String>,
    turns: u64,
    turn_updated: Option<String>,
    status: Option<String>,
}

impl SessionStamp {
    fn read(conn: &Connection, id: &str) -> rusqlite::Result<Self> {
        let mut stamp = Self {
            updated: conn.query_row("SELECT CAST(updated_at AS TEXT) FROM conversations WHERE id = ?1", [id], |r| r.get(0)).optional()?,
            ..Default::default()
        };
        if table_exists(conn, "chat_turns") {
            (stamp.turns, stamp.turn_updated) =
                conn.query_row("SELECT count(*), CAST(max(updated_at) AS TEXT) FROM chat_turns WHERE conversation_id = ?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            stamp.status = conn
                .query_row(
                    "SELECT status FROM chat_turns WHERE conversation_id = ?1 ORDER BY created_at DESC, turn_id DESC LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
        }
        Ok(stamp)
    }
}

impl KodeletTranscript {
    pub fn new(path: &Path, retention: SpanRetention) -> Self {
        Self {
            path: path.to_path_buf(),
            retention,
            conn: None,
            data_version: None,
            stamp: None,
            subagents: None,
            sidecar: None,
            observed_calls: HashMap::new(),
            summary: SessionSummary { harness: Some(Harness::Kodelet), spans: retention.log(), ..Default::default() },
        }
    }
}

impl SessionTracker for KodeletTranscript {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        let Some(db) = self.path.parent() else { return Ok(false) };
        let Some(id) = session_id_of(&self.path) else { return Ok(false) };
        let sidecar = self.subagents.as_ref().and_then(|index| {
            let mut index = index.borrow_mut();
            index.refresh();
            index.get(&id).cloned()
        });
        if self.conn.is_none() {
            self.conn = Some(open_ro(db)?);
        }
        let conn = self.conn.as_ref().expect("opened above");
        let version = conn.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?;
        if self.data_version == Some(version) && self.sidecar == sidecar {
            return Ok(false);
        }
        // One read snapshot, including usage, tools and turn status. Only mark
        // it cached after success, so a failed/busy read is retried next tick.
        let tx = conn.unchecked_transaction()?;
        let stamp = SessionStamp::read(&tx, &id)?;
        if self.stamp.as_ref() == Some(&stamp) && self.sidecar == sidecar {
            // data_version covers the whole database: another conversation or
            // a runner heartbeat must not make us scan this history again.
            tx.commit()?;
            self.data_version = Some(version);
            return Ok(false);
        }
        let (mut summary, calls) = read_summary(&tx, &id, self.retention, &stamp, sidecar.as_ref())?;
        tx.commit()?;
        if summary.session_id.is_none() || summary.started_at != self.summary.started_at {
            self.observed_calls.clear();
        }
        self.observed_calls.extend(calls);
        summary.tool_calls = self.observed_calls.len() as u64;
        summary.web_searches = self.observed_calls.values().filter(|search| **search).count() as u64;
        self.summary = summary;
        self.stamp = Some(stamp);
        self.data_version = Some(version);
        self.sidecar = sidecar;
        Ok(false)
    }

    fn summary(&self) -> &SessionSummary {
        &self.summary
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

fn read_summary(
    conn: &Connection,
    id: &str,
    retention: SpanRetention,
    stamp: &SessionStamp,
    sidecar: Option<&SubagentInfo>,
) -> anyhow::Result<(SessionSummary, HashMap<String, bool>)> {
    let mut summary = SessionSummary { harness: Some(Harness::Kodelet), spans: retention.log(), ..Default::default() };
    let record = conn.query_row(
        "SELECT cwd, provider, CAST(usage AS TEXT), CAST(metadata AS TEXT), CAST(created_at AS TEXT), CAST(updated_at AS TEXT) FROM conversations WHERE id = ?1", [id],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?)),
    ).optional()?;
    let Some((cwd, provider, usage, metadata, created, updated)) = record else { return Ok((summary, HashMap::new())) };
    let usage: Value = serde_json::from_str(&usage)?;
    let metadata: Value = serde_json::from_str(metadata.as_deref().unwrap_or("null"))?;
    summary.session_id = Some(id.to_owned());
    // Compaction can precede our first read, and not every provider leaves a
    // structural marker. Never imply a full lifetime count from a snapshot.
    summary.tool_calls_lower_bound = true;
    summary.cwd = cwd.filter(|s| !s.is_empty()).map(PathBuf::from);
    summary.model = string(&metadata, "model").or_else(|| metadata.get("config_snapshot").and_then(|v| string(v, "model")));
    summary.started_at = timestamp(&created);
    summary.last_activity = timestamp(&updated);
    let parent = string(&metadata, "parent_conversation_id");
    if let Some(parent) = parent.as_ref().filter(|parent| valid_id(parent) && *parent != id) {
        summary.subagent = Some(SubagentInfo {
            parent_session_id: parent.clone(),
            nickname: string(&metadata, "conversation_name"),
            role: subagent::role(&metadata).map(str::to_owned),
        });
    }
    if let Some(info) = sidecar.filter(|info| valid_id(&info.parent_session_id) && info.parent_session_id != id) {
        match summary.subagent.as_mut() {
            Some(central) if central.parent_session_id == info.parent_session_id => {
                central.nickname = central.nickname.take().or_else(|| info.nickname.clone());
                central.role = central.role.take().or_else(|| info.role.clone());
            }
            None if parent.is_none() => summary.subagent = Some(info.clone()),
            _ => {}
        }
    }
    let fork = metadata.get("conversation_fork").is_some_and(Value::is_object);
    let responses = provider == "openai" && responses_mode(conn, id, &metadata)?;
    accounting(&usage, responses, &mut summary);

    // Durable turn receipts are local to this conversation, unlike the copied
    // provider history of a fork. They also survive context compaction.
    summary.turns = stamp.turns;
    if let Some(status) = stamp.status.as_deref() {
        summary.activity = if matches!(status, "accepted" | "running") { Activity::Working } else { Activity::Waiting };
    }
    if !fork {
        summary.health.billable_messages = conn.query_row("SELECT count(*) FROM conversations c, json_each(CAST(c.raw_messages AS TEXT)) m WHERE c.id = ?1 AND json_extract(m.value, '$.role') = 'assistant' AND COALESCE(json_extract(m.value, '$.type'), 'message') = 'message'", [id], |r| r.get(0))?;
        if summary.turns == 0 {
            summary.turns = summary.health.billable_messages;
        }
    }
    if summary.activity == Activity::Unknown {
        let tail: Option<String> = conn.query_row("SELECT CASE WHEN json_extract(m.value, '$.role') = 'assistant' AND COALESCE(json_extract(m.value, '$.type'), 'message') = 'message' THEN 'waiting' ELSE 'working' END FROM conversations c, json_each(CAST(c.raw_messages AS TEXT)) m WHERE c.id = ?1 ORDER BY CAST(m.key AS INTEGER) DESC LIMIT 1", [id], |r| r.get(0)).optional()?;
        // A pristine fork's copied tail says nothing about new work.
        if !fork || summary.usage.total() > 0 {
            summary.activity = match tail.as_deref() {
                Some("waiting") => Activity::Waiting,
                Some(_) => Activity::Working,
                None => Activity::Unknown,
            };
        }
    }
    if usage.is_object() {
        summary.health.usage_records = summary.health.billable_messages;
    }
    if summary.usage.total() == 0 {
        summary.health.empty_usage_records = summary.health.usage_records;
    }
    let calls = read_tools(conn, id, fork, &mut summary)?;
    Ok((summary, calls))
}

fn responses_mode(conn: &Connection, id: &str, metadata: &Value) -> rusqlite::Result<bool> {
    if let Some(mode) =
        string(metadata, "api_mode").or_else(|| metadata.pointer("/config_snapshot/openai").and_then(|v| string(v, "api_mode")))
    {
        return Ok(mode == "responses");
    }
    let first: Option<String> =
        conn.query_row("SELECT json_extract(CAST(raw_messages AS TEXT), '$[0].type') FROM conversations WHERE id = ?1", [id], |r| {
            r.get(0)
        })?;
    Ok(matches!(
        first.as_deref(),
        Some("message" | "reasoning" | "function_call" | "function_call_output" | "compaction" | "compaction_summary")
    ))
}

fn accounting(usage: &Value, responses: bool, summary: &mut SessionSummary) {
    let count = |key| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let cached = count("cacheReadInputTokens");
    summary.usage = TokenUsage {
        input: if responses { count("inputTokens").saturating_sub(cached) } else { count("inputTokens") },
        output: count("outputTokens"),
        cache_read: cached,
        cache_write_5m: 0,
        cache_write_1h: 0,
        // One cumulative counter with no TTL breakdown saved, so it cannot go
        // in either priced bucket.
        cache_write_unsplit: count("cacheCreationInputTokens"),
    };
    let mut costs = [0.0; 4];
    for (i, (key, tokens)) in [
        ("inputCost", summary.usage.input),
        ("outputCost", summary.usage.output),
        ("cacheReadCost", cached),
        ("cacheCreationCost", summary.usage.cache_write()),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(cost) = usage.get(key).and_then(Value::as_f64).filter(|cost| cost.is_finite() && *cost >= 0.0) {
            costs[i] = cost;
        } else {
            summary.unpriced_tokens += tokens;
        }
    }
    summary.cost_breakdown =
        CostBreakdown { input: costs[0], output: costs[1], cache_read: costs[2], cache_write_unsplit: costs[3], ..Default::default() };
    summary.cost_usd = summary.cost_breakdown.total();
    // Kodelet's own figures are the only prices this row can have, whether or
    // not it recorded one. Leaving the source empty would let the collector
    // fill it in from the price table, which never priced anything here;
    // `unpriced_tokens` is what says a figure is missing.
    summary.price_source = Some(PriceSource::Harness);
}

fn read_tools(conn: &Connection, id: &str, fork: bool, summary: &mut SessionSummary) -> anyhow::Result<HashMap<String, bool>> {
    let mut seen = HashMap::new();
    let mut spans = Vec::new();
    let mut stmt = conn.prepare("SELECT t.key, json_extract(t.value, '$.toolName'), json_extract(t.value, '$.timestamp'), json_extract(t.value, '$.success'), json_extract(t.value, '$.metadata.executionTime') FROM conversations c, json_each(CAST(c.tool_results AS TEXT)) t WHERE c.id = ?1 AND t.type = 'object'")?;
    let rows = stmt.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<bool>>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    for (call_id, name, at, success, nanos) in rows.flatten() {
        let ended = at.as_deref().and_then(timestamp);
        if fork && ended.zip(summary.started_at).is_none_or(|(end, created)| end < created) {
            continue;
        }
        let Some(name) = name.filter(|s| !s.is_empty()) else { continue };
        if call_id.is_empty() {
            continue;
        }
        seen.insert(call_id.clone(), name == "openai_web_search");
        if let Some(end) = ended {
            let duration = Duration::from_nanos(nanos.unwrap_or(0).max(0) as u64);
            let start = end.checked_sub(duration).unwrap_or(end);
            spans.push((start, end, call_id, name, success == Some(false)));
        }
    }
    // Keep timestamps in order before applying the live retention cap. JSON
    // map order is call-id order, not completion or invocation order.
    spans.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
    for (start, end, call_id, name, error) in spans {
        summary.spans.open(call_id.clone(), name, start, summary.subagent.is_some());
        summary.spans.close(&call_id, end, error);
    }
    if !fork {
        // Count older/missing structured results too, deduplicated by call ID.
        // SQL projects only structural fields, never content/input/output.
        let sql = "WITH messages AS (SELECT m.value FROM conversations c, json_each(CAST(c.raw_messages AS TEXT)) m WHERE c.id = ?1) \
            SELECT json_extract(b.value, '$.id'), json_extract(b.value, '$.name') FROM messages m, \
            json_each(CASE WHEN json_type(m.value, '$.content') = 'array' THEN json_extract(m.value, '$.content') ELSE '[]' END) b \
            WHERE json_extract(b.value, '$.type') = 'tool_use' \
            UNION SELECT json_extract(t.value, '$.id'), json_extract(t.value, '$.function.name') FROM messages m, json_each(json_extract(m.value, '$.tool_calls')) t \
            UNION SELECT json_extract(value, '$.call_id'), CASE WHEN json_extract(value, '$.type') = 'web_search_call' THEN 'openai_web_search' ELSE json_extract(value, '$.name') END FROM messages WHERE json_extract(value, '$.type') IN ('function_call', 'web_search_call')";
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([id], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?;
        for (call_id, name) in rows.flatten() {
            if let Some(call_id) = call_id.filter(|s| !s.is_empty()) {
                seen.entry(call_id).or_insert(name.as_deref() == Some("openai_web_search"));
            }
        }
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProcKind, SpanKind};
    use rusqlite::params;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    const CREATED: &str = "2026-09-19T10:00:00Z";
    const UPDATED: &str = "2026-09-19T10:05:00Z";

    struct Fixture {
        dir: PathBuf,
        conn: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir =
                std::env::temp_dir().join(format!("agent-top-kodelet-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            let conn = Connection::open(dir.join("storage.db")).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE conversations (
                id TEXT PRIMARY KEY, cwd TEXT, provider TEXT NOT NULL, raw_messages TEXT NOT NULL,
                usage TEXT NOT NULL, metadata TEXT, tool_results TEXT, created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL);",
            )
            .unwrap();
            Self { dir, conn }
        }

        fn db(&self) -> PathBuf {
            self.dir.join("storage.db")
        }

        fn insert(&self, id: &str, provider: &str, metadata: Value, usage: Value, messages: Value, tools: Value) {
            // Match the Go driver's []byte bindings, not just JSON TEXT fixtures.
            self.conn
                .execute(
                    "INSERT INTO conversations VALUES (?1, '/work', ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        provider,
                        serde_json::to_vec(&messages).unwrap(),
                        serde_json::to_vec(&usage).unwrap(),
                        serde_json::to_vec(&metadata).unwrap(),
                        serde_json::to_vec(&tools).unwrap(),
                        CREATED,
                        UPDATED,
                    ],
                )
                .unwrap();
        }

        fn basic(&self, id: &str) {
            self.insert(id, "anthropic", json!({"model":"custom-model"}), usage(), json!([]), json!({}));
        }

        fn tracker(&self, id: &str) -> KodeletTranscript {
            KodeletTranscript::new(&session_path(&self.db(), id), SpanRetention::All)
        }

        fn adapter(&self) -> KodeletAdapter {
            KodeletAdapter { db: Some(self.db()), recent: recent_sessions(&self.db(), UNIX_EPOCH), ..Default::default() }
        }

        fn runtime(&self) {
            self.conn
                .execute_batch(
                    "CREATE TABLE runner_registrations (id TEXT PRIMARY KEY, host_pid INTEGER, host_instance_id TEXT, status TEXT,
                connected_at DATETIME, last_heartbeat_at DATETIME, kodelet_version TEXT);
                CREATE TABLE runner_runs (id TEXT PRIMARY KEY, conversation_id TEXT, runner_id TEXT, status TEXT);
                CREATE TABLE chat_turns (conversation_id TEXT, turn_id TEXT, status TEXT, created_at DATETIME, updated_at DATETIME);",
                )
                .unwrap();
        }

        fn receipt(&self, id: &str, status: &str) {
            self.conn.execute("INSERT INTO chat_turns VALUES (?1, 'turn-1', ?2, ?3, ?4)", params![id, status, CREATED, UPDATED]).unwrap();
        }

        fn json_file(&self, path: &str, data: Value) {
            let path = self.dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_vec(&data).unwrap()).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn usage() -> Value {
        json!({"inputTokens":100, "outputTokens":20, "cacheReadInputTokens":60, "cacheCreationInputTokens":10,
            "inputCost":0.1, "outputCost":0.2, "cacheReadCost":0.03, "cacheCreationCost":0.04,
            "currentContextWindow":45, "maxContextWindow":200000})
    }

    fn process(pid: u32, args: &[&str]) -> (RawProc, ProcNode) {
        let raw = RawProc {
            pid,
            ppid: None,
            name: "kodelet".into(),
            exe: None,
            cmd: args.iter().map(|s| (*s).into()).collect(),
            cwd: Some("/work".into()),
            cpu_percent: 0.0,
            rss_bytes: 0,
            start_time: 1,
            run_time: 1,
        };
        let node = ProcNode {
            pid,
            ppid: None,
            name: raw.name.clone(),
            cmdline: raw.cmdline(),
            kind: ProcKind::Agent,
            harness: Some(Harness::Kodelet),
            cpu_percent: 0.0,
            rss_bytes: 0,
            age_secs: 1,
            cwd: raw.cwd.clone(),
            children: vec![],
        };
        (raw, node)
    }

    fn context(attached: &HashSet<PathBuf>) -> AttributeContext<'_> {
        AttributeContext {
            cwd: Some(Path::new("/work")),
            proc_start: timestamp(CREATED).unwrap(),
            now: timestamp(UPDATED).unwrap(),
            attached,
            activity_timeout: Duration::from_secs(900),
        }
    }

    #[test]
    fn accepts_go_sqlite_and_json_timestamps_without_losing_nanoseconds() {
        let expected = timestamp("2026-09-19T10:00:00.123456789Z").unwrap();
        for time in [
            "2026-09-19 10:00:00.123456789 +0000 UTC",
            "2026-09-19 11:00:00.123456789 +0100 BST",
            "2026-09-19 10:00:00.123456789+00:00",
            "2026-09-19 10:00:00.123456789",
            "2026-09-19T11:00:00.123456789+01:00",
        ] {
            assert_eq!(timestamp(time), Some(expected), "{time}");
        }
        assert_eq!(timestamp("0001-01-01T00:00:00Z"), None);
        assert_eq!(timestamp("not a time"), None);
        assert_eq!(expected.duration_since(UNIX_EPOCH).unwrap().subsec_nanos(), 123456789);
    }

    #[test]
    fn accounts_cumulative_provider_usage_without_repricing_or_cache_double_counting() {
        let fixture = Fixture::new();
        for (id, provider, metadata, messages, expected_input) in [
            ("anthropic", "anthropic", json!({"model":"custom-model"}), json!([]), 100),
            ("chat", "openai", json!({"api_mode":"chat_completions"}), json!([]), 100),
            ("responses", "openai", json!({"api_mode":"responses"}), json!([]), 40),
            ("responses-old", "openai", json!({}), json!([{"type":"message","role":"user"}]), 40),
            ("config", "openai", json!({"config_snapshot":{"model":"another","openai":{"api_mode":"responses"}}}), json!([]), 40),
        ] {
            fixture.insert(id, provider, metadata, usage(), messages, json!({}));
            let mut tracker = fixture.tracker(id);
            tracker.refresh_all().unwrap();
            let summary = tracker.summary();
            assert_eq!(
                summary.usage,
                TokenUsage { input: expected_input, output: 20, cache_read: 60, cache_write_unsplit: 10, ..Default::default() }
            );
            assert!((summary.cost_usd - 0.37).abs() < 1e-12);
            assert_eq!(summary.price_source, Some(PriceSource::Harness));
            assert_eq!(summary.unpriced_tokens, 0);
            assert_eq!(summary.started_at, timestamp(CREATED));
            assert_eq!(summary.last_activity, timestamp(UPDATED));
            assert!(summary.context.is_empty(), "cumulative usage cannot reconstruct per-response context");
        }
    }

    #[test]
    fn missing_or_invalid_prices_are_unpriced_not_guessed() {
        let fixture = Fixture::new();
        fixture.insert(
            "missing",
            "anthropic",
            json!({"model":"claude-sonnet-5"}),
            json!({"inputTokens":100,"outputTokens":20}),
            json!([]),
            json!({}),
        );
        let mut tracker = fixture.tracker("missing");
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().unpriced_tokens, 120);
        assert_eq!(tracker.summary().cost_usd, 0.0);
        // The model is in the built-in table, so an empty source here would let
        // the collector name that table as this row's price. Kodelet says the
        // row is its own either way, and `unpriced_tokens` says what is missing.
        assert_eq!(tracker.summary().price_source, Some(PriceSource::Harness));

        fixture
            .conn
            .execute(
                "UPDATE conversations SET usage = ?1, updated_at = datetime(updated_at, '+1 second') WHERE id = 'missing'",
                [json!({"inputTokens":100,"outputTokens":20,"inputCost":-1,"outputCost":0.5}).to_string()],
            )
            .unwrap();
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().unpriced_tokens, 100);
        assert_eq!(tracker.summary().cost_usd, 0.5);
        assert_eq!(tracker.summary().price_source, Some(PriceSource::Harness));
    }

    #[test]
    fn refresh_replaces_snapshots_observes_wal_and_retries_failed_reads() {
        let fixture = Fixture::new();
        fixture.basic("session");
        let mut tracker = fixture.tracker("session");
        tracker.refresh_all().unwrap();
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().usage.input, 100);
        assert!(tracker.conn.as_ref().unwrap().execute("DELETE FROM conversations", []).is_err());
        assert_eq!(tracker.conn.as_ref().unwrap().query_row("PRAGMA busy_timeout", [], |r| r.get::<_, u64>(0)).unwrap(), 0);
        fixture
            .conn
            .execute(
                "UPDATE conversations SET usage = ?1, updated_at = datetime(updated_at, '+1 second')",
                [json!({"inputTokens":150}).to_string()],
            )
            .unwrap();
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().usage.input, 150);
        fixture.conn.execute("UPDATE conversations SET usage = 'not json', updated_at = datetime(updated_at, '+1 second')", []).unwrap();
        assert!(tracker.refresh().is_err());
        assert!(tracker.refresh().is_err(), "failed read was not cached");
        assert_eq!(tracker.summary().usage.input, 150);
        fixture
            .conn
            .execute("UPDATE conversations SET usage = ?1, updated_at = datetime(updated_at, '+1 second')", [usage().to_string()])
            .unwrap();
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().usage.input, 100, "a corrected cumulative snapshot may shrink");
        fixture.conn.execute("DELETE FROM conversations", []).unwrap();
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().usage.total(), 0);
        let missing = fixture.dir.join("absent.db");
        assert!(open_ro(&missing).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn counts_calls_across_provider_layouts_once_and_only_times_structured_results() {
        let fixture = Fixture::new();
        let tools = json!({
            "b": {"toolName":"bash","success":false,"timestamp":"2026-09-19T10:00:05.500Z","metadata":{"executionTime":2500000000_i64}},
            "a": {"toolName":"file_read","success":true,"timestamp":"2026-09-19T10:00:02Z"}
        });
        for (id, provider, metadata, messages) in [
            (
                "anthropic",
                "anthropic",
                json!({}),
                json!([
                    {"role":"assistant","content":[{"type":"tool_use","id":"a","name":"file_read"},{"type":"tool_use","id":"b","name":"bash"}]},
                    {"role":"assistant","content":[{"type":"tool_use","id":"c","name":"skill"}]}
                ]),
            ),
            (
                "chat",
                "openai",
                json!({"api_mode":"chat_completions"}),
                json!([
                    {"role":"assistant","tool_calls":[{"id":"a","function":{"name":"file_read"}},{"id":"b","function":{"name":"bash"}},{"id":"c","function":{"name":"skill"}}]}
                ]),
            ),
            (
                "responses",
                "openai",
                json!({"api_mode":"responses"}),
                json!([
                    {"type":"function_call","call_id":"a","name":"file_read"},{"type":"function_call","call_id":"b","name":"bash"},
                    {"type":"function_call_output","call_id":"b"},{"type":"function_call","call_id":"c","name":"skill"}
                ]),
            ),
        ] {
            fixture.insert(id, provider, metadata, usage(), messages, tools.clone());
            let mut tracker = fixture.tracker(id);
            tracker.refresh_all().unwrap();
            let summary = tracker.summary();
            assert_eq!(summary.tool_calls, 3, "{id}");
            let spans = summary.spans.to_vec();
            assert_eq!(spans.len(), 2);
            assert_eq!(spans[0].id, "a");
            assert_eq!(spans[0].duration_ms, Some(0));
            assert_eq!(spans[1].started_at, timestamp("2026-09-19T10:00:03Z").unwrap());
            assert_eq!(spans[1].duration_ms, Some(2500));
            assert!(spans[1].error);
            assert!(spans.iter().all(|s| s.kind == SpanKind::Tool && !s.is_open()));
        }
    }

    #[test]
    fn children_keep_own_usage_and_forks_do_not_recount_inherited_tools_or_turns() {
        let fixture = Fixture::new();
        let inherited =
            json!({"toolName":"bash","success":true,"timestamp":"2026-09-19T09:00:00Z","metadata":{"executionTime":1000000000}});
        let fresh =
            json!({"toolName":"code_search","success":true,"timestamp":"2026-09-19T10:04:00Z","metadata":{"executionTime":2000000000_i64}});
        let history = json!([{"role":"assistant","content":[{"type":"tool_use","id":"old","name":"bash"}]}]);
        fixture.insert("parent", "anthropic", json!({}), usage(), history.clone(), json!({"old":inherited.clone()}));
        fixture.insert(
            "child",
            "anthropic",
            json!({"parent_conversation_id":"parent","conversation_name":"Scout",
            "conversation_fork":{"source_conversation_id":"parent","mode":"live_snapshot"}}),
            usage(),
            history.clone(),
            json!({"old":inherited.clone(),"new":fresh,"undated":{"toolName":"bash","success":true}}),
        );
        fixture.insert(
            "sibling",
            "anthropic",
            json!({"conversation_fork":{"source_conversation_id":"parent"}}),
            json!({}),
            history,
            json!({"old":inherited}),
        );
        let mut parent = fixture.tracker("parent");
        let mut child = fixture.tracker("child");
        let mut sibling = fixture.tracker("sibling");
        parent.refresh_all().unwrap();
        child.refresh_all().unwrap();
        sibling.refresh_all().unwrap();
        assert_eq!(parent.summary().usage.input + child.summary().usage.input, 200);
        assert_eq!(parent.summary().tool_calls, 1);
        assert_eq!(child.summary().tool_calls, 1);
        assert_eq!(child.summary().turns, 0, "untimestamped copied messages are not fresh turns");
        assert_eq!(
            child.summary().subagent,
            Some(SubagentInfo { parent_session_id: "parent".into(), nickname: Some("Scout".into()), role: None })
        );
        assert!(child.summary().spans.iter().all(|s| s.sidechain && s.id == "new"));
        assert!(sibling.summary().subagent.is_none(), "fork provenance is not a hierarchy parent");
        assert_eq!(sibling.summary().tool_calls, 0);
        assert_eq!(sibling.summary().activity, Activity::Unknown);
        assert_eq!(fixture.adapter().transcripts().len(), 3, "exports include children and siblings separately");
    }

    #[test]
    fn adapter_shares_sidecar_index_and_preserves_central_lineage_and_accounting() {
        let fixture = Fixture::new();
        let dir = fixture.dir.join("extensions/data/subagent");
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = Connection::open(dir.join("subagents.sqlite")).unwrap();
        sidecar
            .execute_batch(
                "PRAGMA journal_mode=WAL;
            CREATE TABLE alembic_version(version_num TEXT);
            INSERT INTO alembic_version VALUES ('0002_canceling_state');
            CREATE TABLE agents(child_conversation_id TEXT, owner_conversation_id TEXT, name TEXT);",
            )
            .unwrap();
        let adapter = fixture.adapter();
        for (id, metadata, owner, expected) in [
            ("child", json!({}), "parent", Some(("parent", "sidecar", "subagent"))),
            ("matching", json!({"parent_conversation_id":"parent"}), "parent", Some(("parent", "sidecar", "subagent"))),
            (
                "search",
                json!({"parent_conversation_id":"parent","conversation_name":"central","profile":"code-search"}),
                "parent",
                Some(("parent", "central", "code-search")),
            ),
            (
                "conflict",
                json!({"parent_conversation_id":"other","conversation_name":"central","profile":"code-search"}),
                "parent",
                Some(("other", "central", "code-search")),
            ),
            ("self", json!({}), "self", None),
            ("invalid", json!({}), "../parent", None),
            ("central-self", json!({"parent_conversation_id":"central-self"}), "parent", None),
        ] {
            fixture.insert(id, "anthropic", metadata, usage(), json!([]), json!({}));
            sidecar.execute("INSERT INTO agents VALUES (?1, ?2, 'sidecar')", params![id, owner]).unwrap();
            // Reset the shared index to exercise a rescan without a five-second
            // sleep; production refreshes are throttled by the helper itself.
            if let Some(index) = adapter.subagents.borrow().get(&fixture.dir) {
                *index.borrow_mut() = SubagentIndex::new(fixture.dir.clone());
            }
            let mut tracker = adapter.open(&session_path(&fixture.db(), id), SpanRetention::All);
            tracker.refresh_all().unwrap();
            let summary = tracker.summary();
            assert_eq!(
                summary.subagent.as_ref().map(|info| (
                    info.parent_session_id.as_str(),
                    info.nickname.as_deref().unwrap(),
                    info.role.as_deref().unwrap()
                )),
                expected,
                "{id}"
            );
            assert_eq!(summary.usage.input, 100);
            assert!((summary.cost_usd - 0.37).abs() < 1e-12);
            assert_eq!(summary.activity, Activity::Unknown, "sidecars never imply activity");
        }

        let path = session_path(&fixture.db(), "child");
        let mut first = adapter.open(&path, SpanRetention::All);
        let mut second = adapter.open(&path, SpanRetention::All);
        let index = adapter.subagents.borrow().get(&fixture.dir).unwrap().clone();
        assert_eq!(adapter.subagents.borrow().len(), 1);
        assert_eq!(Rc::strong_count(&index), 4, "adapter and both trackers share one index");
        first.refresh_all().unwrap();
        second.refresh_all().unwrap();
        sidecar.execute("UPDATE agents SET name = 'renamed' WHERE child_conversation_id = 'child'", []).unwrap();
        *index.borrow_mut() = SubagentIndex::new(fixture.dir.clone());
        first.refresh_all().unwrap();
        second.refresh_all().unwrap();
        assert_eq!(first.summary().subagent.as_ref().unwrap().nickname.as_deref(), Some("renamed"));
        assert_eq!(second.summary().subagent, first.summary().subagent, "sidecar changes refresh unchanged central snapshots");
    }

    #[test]
    fn turn_receipts_override_copied_history_and_update_without_conversation_save() {
        let fixture = Fixture::new();
        fixture.runtime();
        fixture.insert(
            "child",
            "openai",
            json!({"conversation_fork":{"source_conversation_id":"parent"}}),
            usage(),
            json!([{"role":"assistant"},{"role":"assistant"},{"role":"assistant"}]),
            json!({}),
        );
        fixture.receipt("child", "running");
        let mut tracker = fixture.tracker("child");
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().turns, 1);
        assert_eq!(tracker.summary().activity, Activity::Working);
        fixture.conn.execute("UPDATE chat_turns SET status = 'succeeded'", []).unwrap();
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().activity, Activity::Waiting);
        assert_eq!(tracker.summary().turns, 1);
    }

    #[test]
    fn unrelated_database_writes_do_not_rebuild_unchanged_sessions() {
        let fixture = Fixture::new();
        fixture.runtime();
        fixture.basic("session");
        fixture.basic("other");
        fixture.receipt("session", "succeeded");
        let mut tracker = fixture.tracker("session");
        tracker.refresh_all().unwrap();
        // The old String remains allocated until a replacement summary has
        // been built, so its address changes if we unnecessarily rebuild it.
        let model_address = tracker.summary().model.as_ref().unwrap().as_ptr();
        let version = tracker.data_version;
        fixture
            .conn
            .execute(
                "UPDATE conversations SET usage = ?1, updated_at = datetime(updated_at, '+1 second') WHERE id = 'other'",
                [usage().to_string()],
            )
            .unwrap();
        tracker.refresh_all().unwrap();
        assert_ne!(tracker.data_version, version, "the shared database changed");
        assert_eq!(tracker.summary().model.as_ref().unwrap().as_ptr(), model_address, "only the cheap session signature was read");
        assert_eq!(tracker.summary().usage.input, 100);
    }

    #[test]
    fn observed_tool_counts_survive_compaction_but_cold_starts_report_only_retained_calls() {
        for fork in [false, true] {
            let fixture = Fixture::new();
            let metadata = if fork {
                json!({"api_mode":"responses","conversation_fork":{"source_conversation_id":"parent"}})
            } else {
                json!({"api_mode":"responses"})
            };
            let result = |name| json!({"toolName":name,"success":true,"timestamp":"2026-09-19T10:00:10Z"});
            fixture.insert(
                "session",
                "openai",
                metadata,
                usage(),
                json!([]),
                json!({
                    "call-1": result("bash"), "call-2": result("openai_web_search")
                }),
            );
            let mut live = fixture.tracker("session");
            live.refresh_all().unwrap();
            assert_eq!(live.summary().tool_calls, 2);
            assert!(live.summary().tool_calls_lower_bound);

            // Matches ResetContextStateLocked: old results disappear but
            // cumulative usage remains; new calls then fill the fresh context.
            fixture
                .conn
                .execute(
                    "UPDATE conversations SET raw_messages = ?1, tool_results = ?2, updated_at = datetime(updated_at, '+1 second')",
                    params![
                        json!([{"type":"compaction"},{"type":"function_call","call_id":"call-3","name":"bash"}]).to_string(),
                        json!({"call-3":result("bash")}).to_string()
                    ],
                )
                .unwrap();
            live.refresh_all().unwrap();
            assert_eq!(live.summary().tool_calls, 3, "previously observed calls must not disappear");
            assert_eq!(live.summary().web_searches, 1);
            live.refresh_all().unwrap();
            assert_eq!(live.summary().tool_calls, 3, "refresh must not count a call twice");

            let mut cold = fixture.tracker("session");
            cold.refresh_all().unwrap();
            assert_eq!(cold.summary().tool_calls, 1, "deleted history is not recoverable on a cold start");
            assert!(cold.summary().tool_calls_lower_bound, "one must be displayed as ≥1, not an exact lifetime total");
            assert_eq!(cold.summary().usage, live.summary().usage, "tokens still come from cumulative usage");

            fixture.conn.execute("DELETE FROM conversations", []).unwrap();
            live.refresh_all().unwrap();
            assert_eq!(live.summary().tool_calls, 0, "a deleted session does not retain ghost counters");
        }
    }

    #[test]
    fn failed_admissions_and_inherited_messages_are_not_parser_drift_evidence() {
        let fixture = Fixture::new();
        fixture.runtime();
        fixture.insert("failed", "anthropic", json!({}), json!({}), json!([{"role":"user"}]), json!({}));
        for i in 0..3 {
            fixture
                .conn
                .execute("INSERT INTO chat_turns VALUES ('failed', ?1, 'failed', ?2, ?2)", params![format!("turn-{i}"), CREATED])
                .unwrap();
        }
        let mut tracker = fixture.tracker("failed");
        tracker.refresh_all().unwrap();
        assert_eq!(tracker.summary().turns, 3);
        assert_eq!(tracker.summary().health.billable_messages, 0);
        assert!(!tracker.summary().health.fields_unrecognised());
        let replies = json!([{"role":"assistant"},{"role":"assistant"},{"role":"assistant"}]);
        fixture.insert("drift", "anthropic", json!({}), json!({"renamedTokens":100}), replies.clone(), json!({}));
        fixture.insert("fork", "anthropic", json!({"conversation_fork":{"source_conversation_id":"drift"}}), json!({}), replies, json!({}));
        let mut drift = fixture.tracker("drift");
        let mut fork = fixture.tracker("fork");
        drift.refresh_all().unwrap();
        fork.refresh_all().unwrap();
        assert!(drift.summary().health.fields_unrecognised());
        assert!(!fork.summary().health.fields_unrecognised());
    }

    #[test]
    fn live_retention_is_bounded_but_exports_keep_all_spans() {
        let fixture = Fixture::new();
        let count = super::super::MAX_SPANS + 20;
        let tools: serde_json::Map<String, Value> = (0..count)
            .map(|i| {
                (
                    format!("call-{i:04}"),
                    json!({"toolName":"bash","success":true,"timestamp":"2026-09-19T10:00:10Z","metadata":{"executionTime":1000000}}),
                )
            })
            .collect();
        fixture.insert("session", "anthropic", json!({}), usage(), json!([]), tools.into());
        let mut live = KodeletTranscript::new(&session_path(&fixture.db(), "session"), SpanRetention::Recent);
        let mut export = fixture.tracker("session");
        live.refresh_all().unwrap();
        export.refresh_all().unwrap();
        assert_eq!(live.summary().tool_calls, count as u64);
        assert_eq!(live.summary().spans.len(), super::super::MAX_SPANS);
        assert_eq!(export.summary().spans.len(), count);
        assert_eq!(live.summary().spans.iter().next().unwrap().id, "call-0020");
    }

    #[test]
    fn discovery_reads_only_metadata_and_exports_all_valid_ids() {
        let fixture = Fixture::new();
        for id in ["old", "new", "../escape"] {
            fixture.basic(id);
        }
        fixture.conn.execute("UPDATE conversations SET raw_messages = 'not json', tool_results = 'not json'", []).unwrap();
        fixture.conn.execute("UPDATE conversations SET updated_at = '2026-09-18 10:00:00 +0000 UTC' WHERE id = 'old'", []).unwrap();
        let sessions = recent_sessions(&fixture.db(), timestamp(CREATED).unwrap());
        assert_eq!(sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["new"]);
        let adapter = fixture.adapter();
        assert_eq!(adapter.transcripts().len(), 2);
        let attached = HashSet::from([session_path(&fixture.db(), "new")]);
        assert_eq!(adapter.unowned(&attached), vec![session_path(&fixture.db(), "old")]);
        assert!(!adapter.detect(&fixture.db()));
    }

    #[test]
    fn daemon_receipts_are_exact_and_thin_clients_cannot_steal_them() {
        let fixture = Fixture::new();
        fixture.runtime();
        fixture.basic("parent");
        fixture.basic("child");
        fixture.basic("idle");
        fixture.receipt("parent", "running");
        fixture.receipt("child", "accepted");
        fixture.receipt("idle", "succeeded");
        fixture.json_file(
            "server/connection.json",
            json!({"schemaVersion":1,"pid":10,"instanceId":"daemon","version":"0.6.17-beta","managed":false}),
        );
        let (daemon_raw, daemon) = process(10, &["kodelet", "--profile", "work", "serve"]);
        let (client_raw, client) = process(20, &["kodelet", "run", "--resume", "parent"]);
        let mut adapter = fixture.adapter();
        adapter.prepare(&[&client, &daemon], &HashMap::from([(10, &daemon_raw), (20, &client_raw)]));
        let attached = HashSet::new();
        assert_eq!(adapter.attribute(&client, Some(&client_raw), &context(&attached)).1, Attribution::None);
        let (paths, attribution) = adapter.attribute(&daemon, Some(&daemon_raw), &context(&attached));
        assert_eq!(attribution, Attribution::HarnessRegistry);
        assert_eq!(
            paths.into_iter().collect::<HashSet<_>>(),
            HashSet::from([session_path(&fixture.db(), "parent"), session_path(&fixture.db(), "child")])
        );
        assert_eq!(adapter.hints(10).unwrap().version.as_deref(), Some("0.6.17-beta"));
        assert_eq!(adapter.hints(10).unwrap().status.as_deref(), Some("busy"));
        // File existence and a matching PID are insufficient after PID reuse.
        let mut reused = daemon_raw.clone();
        reused.start_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 10;
        adapter.prepare(&[&daemon], &HashMap::from([(10, &reused)]));
        assert!(adapter.attribute(&daemon, Some(&reused), &context(&attached)).0.is_empty());
    }

    #[test]
    fn local_runner_identity_and_freshness_prevent_remote_or_stale_pid_matches() {
        let fixture = Fixture::new();
        fixture.runtime();
        fixture.basic("local");
        fixture.basic("remote");
        fixture.json_file("runners/host.json", json!({"version":1,"instanceId":"this-host"}));
        fixture
            .conn
            .execute_batch(
                "INSERT INTO runner_registrations VALUES ('r-local',10,'this-host','busy',datetime('now'),datetime('now'),'v-local');
            INSERT INTO runner_registrations VALUES ('r-remote',20,'other-host','busy',datetime('now'),datetime('now'),'v-remote');
            INSERT INTO runner_runs VALUES ('run-local','local','r-local','running');
            INSERT INTO runner_runs VALUES ('run-remote','remote','r-remote','running');",
            )
            .unwrap();
        let (local_raw, local) = process(10, &["kodelet", "runner", "start"]);
        let (collision_raw, collision) = process(20, &["kodelet", "serve"]);
        let mut adapter = fixture.adapter();
        adapter.prepare(&[&collision, &local], &HashMap::from([(10, &local_raw), (20, &collision_raw)]));
        let attached = HashSet::new();
        assert_eq!(
            adapter.attribute(&local, Some(&local_raw), &context(&attached)),
            (vec![session_path(&fixture.db(), "local")], Attribution::HarnessRegistry)
        );
        assert!(adapter.attribute(&collision, Some(&collision_raw), &context(&attached)).0.is_empty());
        fixture.conn.execute("UPDATE runner_registrations SET last_heartbeat_at = datetime('now','-60 seconds')", []).unwrap();
        adapter.prepare(&[&local], &HashMap::from([(10, &local_raw)]));
        assert!(adapter.attribute(&local, Some(&local_raw), &context(&attached)).0.is_empty());
    }

    #[test]
    fn pre_registry_cwd_fallback_is_labelled_unambiguous_and_never_for_clients() {
        let fixture = Fixture::new();
        fixture.basic("session");
        let (raw, node) = process(10, &["kodelet", "serve"]);
        let (client_raw, client) = process(20, &["kodelet", "run"]);
        let mut adapter = fixture.adapter();
        let attached = HashSet::new();
        assert_eq!(
            adapter.attribute(&node, Some(&raw), &context(&attached)),
            (vec![session_path(&fixture.db(), "session")], Attribution::CwdHeuristic)
        );
        assert!(adapter.attribute(&client, Some(&client_raw), &context(&attached)).0.is_empty());
        adapter.reserve(&fixture.db(), 30, "session", Attribution::HarnessRegistry);
        assert!(adapter.attribute(&node, Some(&raw), &context(&attached)).0.is_empty());
        fixture.basic("other");
        adapter = fixture.adapter();
        assert!(adapter.attribute(&node, Some(&raw), &context(&attached)).0.is_empty(), "ambiguous cwd is not attribution");
    }
}
