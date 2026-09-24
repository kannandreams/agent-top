//! OpenAI Codex CLI: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`.
//!
//! Format notes (verified on Codex CLI 0.149, 2026-09-03):
//! * The first line is `session_meta` with `payload.cwd`, `payload.id`,
//!   `payload.cli_version` and `payload.originator`.
//! * `event_msg` / `token_count` carries `info.total_token_usage`, which is
//!   cumulative for the session; `info` is null on rate-limit-only events.
//!   `input_tokens` includes `cached_input_tokens`.
//! * `task_started` / `task_complete` / `turn_aborted` bracket a turn.
//! * `response_item` with `payload.type` `function_call` or
//!   `custom_tool_call` is one tool call; the matching `*_output` item
//!   carries the same `payload.call_id`, and the two lines' timestamps
//!   bracket the call. That pairing is the trace.
//! * `task_started` and `task_complete` bracket a turn span. An inference
//!   span runs from a user `message` item or a `*_output` item to the next
//!   thing the model produced: a call, a `reasoning` item, a
//!   `web_search_call`, or an assistant `message`.
//! * `response_item` `web_search_call` is one server-side web search.
//! * `info.last_token_usage` beside the cumulative record is the one
//!   response's usage. Repeated cumulative usage marks a snapshot; equal
//!   per-response usage alone need not. Each `*_output` item is filed under
//!   its call's name, re-filed under the MCP server when its
//!   `mcp_tool_call_end` follows, and sized by the consuming response. Codex
//!   writes no compaction marker that was seen, so the ledger's halving
//!   rule stands in. See `ContextLedger`.
//! * Usage ordering (0.130 fixture / 0.154.0 live and source, 2026-09-18):
//!   newer versions emit `token_count` after draining tool outputs, older
//!   ones before. The first model item freezes that response's input batch;
//!   results produced during it wait for the next response.
//!
//! Codex model prices are not in the static table, so cost is reported as
//! unpriced tokens.
//!
//! Process semantics (source-verified against Codex 0.154.0, 2026-09-17):
//! the npm launcher spawns a native runtime, which owns the rollout handles.
//! Native `spawn_agent` creates an in-process session, not an OS process.
//! Its lineage is `payload.source.subagent.thread_spawn.parent_thread_id` in
//! session metadata, or `payload.parent_thread_id` when `thread_source` is
//! explicitly `subagent`, never a PID relationship or `forked_from_id`.
//! Nickname and role come from that source record (or the top-level metadata);
//! older records name the role `agent_type`. Rollouts retain separate usage
//! totals while the TUI groups them by parent. Memory belongs to the process,
//! not to individual logical subagents sharing that process.
//! Live 0.154.0 rollouts (2026-09-17) can copy ancestor `session_meta` records
//! after the child's own header when forking history. The first identified
//! header owns this rollout's identity and lineage; later headers do not.
//!
//! Code mode (live/source verified on 0.154.0, 2026-09-18): `exec` wraps
//! nested tools recorded as `event_msg/item_completed`, with `exec-<uuid>`
//! ids and `started_at_ms`/`completed_at_ms`. Typed metadata supplies names;
//! unknown types are ignored. Counts include wrappers and nested calls;
//! each wrapper's context share is split between its contained children by
//! output-text bytes (evenly if sizes are unavailable). Only sizes are retained;
//! no prompts or inputs are inspected. Ambiguous wrappers keep their name.

use super::{
    AttributeContext, ContextWeights, HarnessAdapter, REFRESH_BUDGET_BYTES, SessionSummary, SessionTracker, SpanRetention,
    parse_rfc3339_utc,
};
use crate::jsonl::TailReader;
use crate::model::{Activity, Attribution, ContextOrigin, Harness, ProcNode, SpanKind, SubagentInfo, TokenUsage};
use crate::pricing::{self, Table};
use crate::process::RawProc;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn codex_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"))
}

pub fn sessions_dir() -> Option<PathBuf> {
    codex_dir().map(|d| d.join("sessions"))
}

/// Rollout files modified after `since`. Walks `YYYY/MM/DD` and prunes by
/// directory mtime so the walk stays cheap on a long history.
/// Rollouts the process has open: the app-server's live threads, or the CLI's
/// one conversation. `None` when the platform cannot say. Filtered to the
/// sessions directory so an unrelated file the process holds (a log, a
/// config) is never mistaken for a thread, and mapped back under the
/// un-canonicalised sessions directory so the paths compare equal to those
/// from `recent_rollouts`.
pub fn rollouts_open_by(pid: u32) -> Option<Vec<PathBuf>> {
    let root = sessions_dir()?;
    let canonical = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let open = crate::openfiles::open_files(pid)?;
    Some(
        open.into_iter()
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .filter_map(|p| p.strip_prefix(&canonical).ok().map(|rel| root.join(rel)))
            .collect(),
    )
}

/// An npm launcher holds no rollouts; its native runtime does. Match the
/// forwarded argv, never just an `Agent` child: a nested agent invocation
/// owns its own sessions even though it is beneath this process.
fn rollout_owner<'a>(root: &'a ProcNode, by_pid: &HashMap<u32, &RawProc>) -> &'a ProcNode {
    let Some(parent) = by_pid.get(&root.pid) else { return root };
    root.children
        .iter()
        .find(|p| p.harness == Some(Harness::Codex) && by_pid.get(&p.pid).is_some_and(|child| child.is_codex_runtime_of(parent)))
        .unwrap_or(root)
}

/// Every rollout written since `since`.
pub fn recent_rollouts(since: SystemTime) -> Vec<PathBuf> {
    let Some(root) = sessions_dir() else { return Vec::new() };
    rollouts_under(&root, since)
}

/// The tree is `YYYY/MM/DD/*.jsonl` and is walked in full, three levels deep,
/// with only the files filtered by mtime. Pruning directories by their mtime
/// looked cheaper and was wrong: a directory's mtime moves only when an entry
/// is created directly inside it, so the year directory is touched once a
/// month and every rollout written after the first of the month was invisible.
/// Pruning by name would be wrong too, since a directory's date says when a
/// thread started, not whether it is still being written to; the app-server
/// keeps a thread for days. A few hundred directories cost a few milliseconds.
pub(crate) fn rollouts_under(root: &Path, since: SystemTime) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, 0, since, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, since: SystemTime, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            if depth < 3 {
                walk(&p, depth + 1, since, out);
            }
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") && md.modified().map(|m| m >= since).unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Cheap header read: cwd and start time from the first line only.
pub fn read_meta(path: &Path) -> Option<(PathBuf, SystemTime)> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(f).read_line(&mut first).ok()?;
    let v: Value = serde_json::from_str(&first).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let cwd = v.pointer("/payload/cwd").and_then(Value::as_str).map(PathBuf::from)?;
    let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_utc)?;
    Some((cwd, ts))
}

/// The Codex adapter: a process is matched to the rollouts it holds open,
/// and only where the platform cannot say to the cwd and activity heuristics.
/// See DEC-006.
#[derive(Default)]
pub struct CodexAdapter {
    /// Recent rollouts with the cwd and start time from their header.
    recent: Vec<(PathBuf, PathBuf, SystemTime)>,
    /// Which rollouts each Codex process has open, gathered before any
    /// attribution so that no process's fallback can claim a thread another
    /// process is demonstrably writing. `None` when the platform cannot say.
    held: HashMap<u32, Option<Vec<PathBuf>>>,
    all_held: HashSet<PathBuf>,
}

impl HarnessAdapter for CodexAdapter {
    fn harness(&self) -> Harness {
        Harness::Codex
    }

    fn rescan(&mut self, since: SystemTime) {
        self.recent = recent_rollouts(since).into_iter().filter_map(|p| read_meta(&p).map(|(cwd, ts)| (p, cwd, ts))).collect();
    }

    fn prepare(&mut self, roots: &[&ProcNode], by_pid: &HashMap<u32, &RawProc>) {
        self.held = roots.iter().map(|r| (r.pid, rollouts_open_by(rollout_owner(r, by_pid).pid))).collect();
        self.all_held = self.held.values().flatten().flatten().cloned().collect();
    }

    fn attribute(&self, root: &ProcNode, _raw: Option<&RawProc>, ctx: &AttributeContext) -> (Vec<PathBuf>, Attribution) {
        let mine: Option<Vec<PathBuf>> =
            self.held.get(&root.pid).and_then(|h| h.as_ref()).map(|h| h.iter().filter(|p| !ctx.attached.contains(*p)).cloned().collect());
        let taken: HashSet<PathBuf> = ctx.attached.union(&self.all_held).cloned().collect();
        attribute(ctx.cwd, ctx.proc_start, mine.as_deref(), &self.recent, &taken, ctx.now, ctx.activity_timeout)
    }

    fn unowned(&self, attached: &HashSet<PathBuf>) -> Vec<PathBuf> {
        self.recent.iter().map(|(p, _, _)| p).filter(|p| !attached.contains(*p)).cloned().collect()
    }

    fn open(&self, path: &Path, spans: SpanRetention) -> Box<dyn SessionTracker> {
        Box::new(CodexTranscript::new(path).with_spans(spans))
    }

    /// Every rollout opens with a `session_meta` record.
    fn detect(&self, path: &Path) -> bool {
        super::head_lines(path).iter().any(|v| v.get("type").and_then(Value::as_str) == Some("session_meta"))
    }

    fn transcripts(&self) -> Vec<(String, PathBuf)> {
        recent_rollouts(SystemTime::UNIX_EPOCH).into_iter().map(|p| (rollout_id(&p), p)).collect()
    }
}

/// The id in `rollout-2026-05-14T21-37-50-<id>`: what follows the fixed-width
/// timestamp. A file named some other way is matched on its whole stem.
pub fn rollout_id(p: &Path) -> String {
    const TS_LEN: usize = "2026-05-14T21-37-50-".len();
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    stem.strip_prefix("rollout-").and_then(|s| s.get(TS_LEN..)).map(str::to_string).unwrap_or(stem)
}

/// Codex conversations belonging to one process, newest activity first.
///
/// `held` are the rollouts the process has open, which is not a guess: Codex
/// opens a thread's rollout when the thread starts and closes it when the
/// thread ends. When the platform can say (`Some`), that list is the answer,
/// an empty one included: a process holding no rollout is hosting no thread,
/// and a rollout nobody holds is a finished conversation for the stopped
/// list. The heuristics below are for when it cannot (`None`).
///
/// A `codex` CLI runs one conversation from the directory it was started in, so
/// a cwd match finds it. The VS Code app-server is a different shape: one
/// long-lived process, running from `/`, hosting any number of conversations
/// over its life. Returning a single rollout for it collapses every one of
/// those into one row and attributes whichever happened to be newest, so this
/// returns all of them that are currently live and lets the caller give each
/// its own row.
///
/// A rollout in `taken` is skipped: one already claimed by another process,
/// or one some process has open, so that two Codex processes cannot both
/// show the same conversation and an older app-server cannot collect the
/// threads of a newer one.
pub(crate) fn attribute(
    cwd: Option<&Path>,
    proc_start: SystemTime,
    held: Option<&[PathBuf]>,
    recent: &[(PathBuf, PathBuf, SystemTime)],
    taken: &HashSet<PathBuf>,
    now: SystemTime,
    activity_timeout: Duration,
) -> (Vec<PathBuf>, Attribution) {
    if let Some(held) = held {
        let mut mine = held.to_vec();
        mine.sort_by_key(|p| std::cmp::Reverse(written_at(p)));
        mine.truncate(MAX_THREADS);
        let attribution = if mine.is_empty() { Attribution::None } else { Attribution::OpenFile };
        return (mine, attribution);
    }

    let slack = Duration::from_secs(60);
    let started_after = |ts: &SystemTime| *ts + slack >= proc_start;
    let candidates = || recent.iter().filter(|(p, _, ts)| started_after(ts) && !taken.contains(p));

    // The CLI case: the conversation runs where the process runs.
    if let Some(cwd) = cwd {
        let mut matched: Vec<&(PathBuf, PathBuf, SystemTime)> = candidates().filter(|(_, c, _)| c == cwd).collect();
        if !matched.is_empty() {
            matched.sort_by_key(|(p, _, _)| std::cmp::Reverse(written_at(p)));
            return (matched.into_iter().map(|(p, _, _)| p.clone()).collect(), Attribution::CwdHeuristic);
        }
    }

    // The app-server case: no cwd to match on, so take the conversations that
    // are actually being written to. A rollout nobody has touched in a while is
    // a finished conversation, not a thread of this process.
    let mut live: Vec<&(PathBuf, PathBuf, SystemTime)> = candidates()
        .filter(|(p, _, _)| written_at(p).map(|w| now.duration_since(w).unwrap_or_default() <= activity_timeout).unwrap_or(false))
        .collect();
    live.sort_by_key(|(p, _, _)| std::cmp::Reverse(written_at(p)));
    live.truncate(MAX_THREADS);
    let attribution = if live.is_empty() { Attribution::None } else { Attribution::CwdHeuristic };
    (live.into_iter().map(|(p, _, _)| p.clone()).collect(), attribution)
}

/// One process is not plausibly running more conversations than this at once,
/// and an unbounded fan-out would let a stale directory fill the table.
const MAX_THREADS: usize = 12;

fn written_at(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn subagent_info(meta: &Value) -> Option<SubagentInfo> {
    let source = meta.pointer("/source/subagent/thread_spawn");
    let parent = source.and_then(|s| s.get("parent_thread_id")).and_then(Value::as_str).or_else(|| {
        (meta.get("thread_source").and_then(Value::as_str) == Some("subagent"))
            .then(|| meta.get("parent_thread_id").and_then(Value::as_str))
            .flatten()
    })?;
    let id = meta.get("id").or_else(|| meta.get("session_id")).and_then(Value::as_str);
    if parent.is_empty() || Some(parent) == id {
        return None;
    }
    let field = |key| source.and_then(|s| s.get(key)).and_then(Value::as_str).or_else(|| meta.get(key).and_then(Value::as_str));
    Some(SubagentInfo {
        parent_session_id: parent.to_string(),
        nickname: field("agent_nickname").map(str::to_string),
        role: field("agent_role").or_else(|| field("agent_type")).map(str::to_string),
    })
}

pub struct CodexTranscript {
    reader: TailReader,
    prices: &'static Table,
    summary: SessionSummary,
    /// Counters naming the turn and inference spans, and the ids of the ones
    /// currently being extended.
    turns: u64,
    inferences: u64,
    turn: Option<String>,
    inference: Option<String>,
    /// Tool calls awaiting their output, by call id, so the output can be
    /// filed under the call's name.
    pending_tools: HashMap<String, PendingTool>,
    /// Completed results awaiting the next model response.
    context_results: Vec<(String, ContextWeights)>,
    response_in_progress: bool,
    /// Per-response and cumulative usage, to skip repeated snapshots.
    last_response: Option<(TokenUsage, Option<TokenUsage>)>,
}

struct PendingTool {
    name: String,
    started_at: SystemTime,
    nested: Vec<NestedSource>,
    ambiguous: bool,
    active: bool,
}

struct NestedSource {
    id: String,
    origin: ContextOrigin,
    name: String,
    ended_at: SystemTime,
    output_bytes: Option<u64>,
}

/// Measure decoded output text, never command arguments or patch contents.
fn nested_output_bytes(item: &Value) -> Option<u64> {
    let text = |key| item.get(key).and_then(Value::as_str).map(|s| s.len() as u64);
    let streams = || match (text("stdout"), text("stderr")) {
        (None, None) => None,
        (out, err) => Some(out.unwrap_or(0) + err.unwrap_or(0)),
    };
    match item.get("type").and_then(Value::as_str)? {
        "CommandExecution" => text("formatted_output").or_else(|| text("aggregated_output")).or_else(streams),
        "FileChange" => streams(),
        "McpToolCall" => match item.get("result").filter(|v| !v.is_null()) {
            Some(result) if result.get("structuredContent").is_some_and(|v| !v.is_null()) => None,
            Some(result) => text_content_bytes(result.get("content")?, "text"),
            None => item.pointer("/error/message").and_then(Value::as_str).map(|s| s.len() as u64),
        },
        "DynamicToolCall" => match item.get("content_items").filter(|v| !v.is_null()) {
            Some(content) => text_content_bytes(content, "inputText"),
            None => text("error"),
        },
        _ => None,
    }
}

fn text_content_bytes(content: &Value, kind: &str) -> Option<u64> {
    content.as_array()?.iter().try_fold(0, |total, item| {
        if item.get("type").and_then(Value::as_str) != Some(kind) {
            return None;
        }
        Some(total + item.get("text")?.as_str()?.len() as u64)
    })
}

impl CodexTranscript {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        CodexTranscript {
            reader: TailReader::new(path),
            prices: pricing::table(),
            summary: SessionSummary { harness: Some(Harness::Codex), ..Default::default() },
            turns: 0,
            inferences: 0,
            turn: None,
            inference: None,
            pending_tools: HashMap::new(),
            context_results: Vec::new(),
            response_in_progress: false,
            last_response: None,
        }
    }

    /// Something was submitted to the model. One inference at a time: a
    /// developer message followed by a user message is one submission.
    fn begin_inference(&mut self, ts: SystemTime) {
        if self.summary.spans.open_of_kind(SpanKind::Inference).is_some() {
            return;
        }
        // A turn that ended without the model replying (aborted) leaves the
        // previous inference open; it produced nothing, so it goes.
        if let Some(id) = self.inference.take() {
            self.summary.spans.discard_open(&id);
        }
        self.inferences += 1;
        let id = format!("inference:{}", self.inferences);
        self.summary.spans.open_kind(id.clone(), "inference".into(), ts, false, SpanKind::Inference);
        self.inference = Some(id);
    }

    /// The model produced something: the inference in progress ends here.
    fn end_inference(&mut self, ts: SystemTime) {
        if let Some(id) = self.inference.take() {
            self.summary.spans.end_at(&id, ts);
        }
    }

    /// See `ClaudeTranscript::with_prices`.
    pub fn with_prices(mut self, prices: &'static Table) -> Self {
        self.prices = prices;
        self
    }

    /// Keep every span instead of the newest `MAX_SPANS`. See `SpanRetention`.
    pub fn with_spans(mut self, retention: SpanRetention) -> Self {
        self.summary.spans = retention.log();
        self
    }

    /// Nested `exec-<uuid>` calls lack response_item pairs; direct calls already
    /// have spans and must not be counted again.
    fn nested_tool_completed(&mut self, payload: &Value) {
        let Some(item) = payload.get("item") else { return };
        let Some(id) = item.get("id").and_then(Value::as_str).filter(|id| id.starts_with("exec-") && id.len() > 5) else {
            return;
        };
        if self.summary.spans.iter().any(|s| s.id == id) || self.pending_tools.values().any(|p| p.nested.iter().any(|n| n.id == id)) {
            return;
        }
        let name = match item.get("type").and_then(Value::as_str) {
            Some("CommandExecution") => match item.get("source").and_then(Value::as_str) {
                Some("unified_exec_startup") => Some("exec_command"),
                Some("unified_exec_interaction") => Some("write_stdin"),
                _ => None,
            },
            Some("FileChange") => Some("apply_patch"),
            Some("McpToolCall" | "DynamicToolCall") => item.get("tool").and_then(Value::as_str).filter(|s| !s.is_empty()),
            _ => None,
        };
        let time =
            |key| payload.get(key).and_then(Value::as_u64).and_then(|ms| SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(ms)));
        let (start, end) = (time("started_at_ms"), time("completed_at_ms"));
        let source = match (name, item.get("type").and_then(Value::as_str)) {
            (Some(_), Some("McpToolCall")) => {
                item.get("server").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|server| (ContextOrigin::Mcp, server))
            }
            (Some(name), _) => Some((ContextOrigin::Tool, name)),
            _ => None,
        };
        // Timing is a heuristic; keep ambiguous results under the wrapper.
        for pending in self.pending_tools.values_mut().filter(|p| p.active && matches!(p.name.as_str(), "exec" | "wait")) {
            match (source, start, end) {
                (Some((origin, name)), Some(start), Some(end)) if start >= pending.started_at && end >= start => {
                    pending.nested.push(NestedSource {
                        id: id.into(),
                        origin,
                        name: name.into(),
                        ended_at: end,
                        output_bytes: nested_output_bytes(item),
                    });
                }
                _ => pending.ambiguous = true,
            }
        }
        let (Some(name), Some(start), Some(end)) = (name, start, end) else { return };
        if end < start {
            return;
        }
        let error = matches!(item.get("status").and_then(Value::as_str), Some("failed" | "declined"))
            || item.get("exit_code").and_then(Value::as_i64).is_some_and(|code| code != 0)
            || item.get("success").and_then(Value::as_bool) == Some(false);
        self.summary.tool_calls += 1;
        self.summary.spans.open(id.to_string(), name.to_string(), start, false);
        self.summary.spans.close(id, end, error);
    }

    fn ingest(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_utc);
        if let Some(ts) = ts {
            if self.summary.started_at.is_none() {
                self.summary.started_at = Some(ts);
            }
            self.summary.last_activity = Some(ts);
        }
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = v.get("payload");
        let ptype = payload.and_then(|p| p.get("type")).and_then(Value::as_str).unwrap_or("");
        if !self.response_in_progress
            && kind == "response_item"
            && (matches!(
                ptype,
                "function_call" | "custom_tool_call" | "local_shell_call" | "tool_search_call" | "web_search_call" | "reasoning"
            ) || (ptype == "message" && payload.and_then(|p| p.get("role")).and_then(Value::as_str) == Some("assistant")))
        {
            for (id, sources) in self.context_results.drain(..) {
                self.summary.context.result_weighted(&id, sources);
            }
            self.response_in_progress = true;
        }
        match kind {
            // Forked history can contain parent headers, including across tail
            // refreshes. They must not replace this rollout's own metadata.
            "session_meta" if self.summary.session_id.is_none() => {
                if let Some(p) = payload {
                    self.summary.session_id = p.get("id").or(p.get("session_id")).and_then(Value::as_str).map(str::to_string);
                    self.summary.subagent = subagent_info(p);
                    self.summary.cwd = p.get("cwd").and_then(Value::as_str).map(PathBuf::from);
                    self.summary.harness_version = p.get("cli_version").and_then(Value::as_str).map(str::to_string);
                }
            }
            "turn_context" => {
                if let Some(m) = payload.and_then(|p| p.get("model")).and_then(Value::as_str) {
                    self.summary.model = Some(m.to_string());
                }
            }
            "event_msg" => match ptype {
                "token_count" => {
                    let total = payload.and_then(|p| p.pointer("/info/total_token_usage")).filter(|v| v.is_object());
                    if let Some(total) = total {
                        let g = |k: &str| total.get(k).and_then(Value::as_u64).unwrap_or(0);
                        self.summary.health.usage_records += 1;
                        if g("input_tokens") + g("output_tokens") + g("cached_input_tokens") == 0 {
                            self.summary.health.empty_usage_records += 1;
                        }
                        let cached = g("cached_input_tokens");
                        let usage = TokenUsage {
                            input: g("input_tokens").saturating_sub(cached),
                            cache_read: cached,
                            output: g("output_tokens"),
                            ..Default::default()
                        };
                        self.summary.usage = usage;
                        let price = self.summary.model.as_deref().and_then(|m| self.prices.lookup(m));
                        match price {
                            Some(p) => {
                                self.summary.cost_breakdown = p.breakdown(&usage);
                                self.summary.cost_usd = self.summary.cost_breakdown.total();
                                self.summary.unpriced_tokens = 0;
                            }
                            None => {
                                self.summary.cost_breakdown = Default::default();
                                self.summary.cost_usd = 0.0;
                                self.summary.unpriced_tokens = usage.total();
                            }
                        }
                    }
                    if let Some(last) = payload.and_then(|p| p.pointer("/info/last_token_usage")).filter(|v| v.is_object()) {
                        let g = |k: &str| last.get(k).and_then(Value::as_u64).unwrap_or(0);
                        let cached = g("cached_input_tokens");
                        let usage = TokenUsage {
                            input: g("input_tokens").saturating_sub(cached),
                            cache_read: cached,
                            output: g("output_tokens"),
                            ..Default::default()
                        };
                        let response = (usage, total.map(|_| self.summary.usage));
                        if usage.prompt() > 0 && self.last_response != Some(response) {
                            let cost = self
                                .summary
                                .model
                                .as_deref()
                                .and_then(|m| self.prices.lookup(m))
                                .map(|p| p.breakdown(&usage))
                                .unwrap_or_default();
                            self.summary.context.response(&usage, &cost);
                            self.last_response = Some(response);
                            self.response_in_progress = false;
                        }
                    }
                    // The rate-limit snapshot rides on every token_count; the
                    // latest one is the current state. Codex also reports other
                    // quotas by `limit_id` (`premium`, seen since 0.131) that
                    // can arrive with no windows at all; one of those says
                    // nothing about the limit and must not blank the last one.
                    if let Some(rl) = payload.and_then(|p| p.get("rate_limits")).filter(|v| v.is_object()) {
                        let rl = parse_rate_limits(rl);
                        if rl.primary.is_some() || rl.secondary.is_some() {
                            self.summary.rate_limit = Some(rl);
                        }
                    }
                }
                "task_started" => {
                    self.summary.activity = Activity::Working;
                    if let Some(ts) = ts {
                        self.turns += 1;
                        let id = format!("turn:{}", self.turns);
                        self.summary.spans.open_kind(id.clone(), "turn".into(), ts, false, SpanKind::Turn);
                        self.turn = Some(id);
                    }
                }
                "user_message" => self.summary.activity = Activity::Working,
                "item_completed" => {
                    if let Some(p) = payload {
                        self.nested_tool_completed(p);
                    }
                }
                // An MCP tool call. Codex records the call as a `response_item`
                // `function_call` too, which the block below counts as a tool
                // call and turns into a span; this line is the only one that
                // names the server, so it feeds the per-server map and nothing
                // else, to avoid double counting. `mcp_tool_call_begin` carries
                // the same `invocation`; the pair brackets the call, but the
                // `end` alone is enough for a count and is the one always
                // present in the versions seen.
                "mcp_tool_call_end" => {
                    if let Some(inv) = payload.and_then(|p| p.get("invocation"))
                        && let Some(server) = inv.get("server").and_then(Value::as_str).filter(|s| !s.is_empty())
                    {
                        let error = payload
                            .and_then(|p| p.get("result"))
                            .and_then(Value::as_object)
                            .map(|r| !r.contains_key("Ok"))
                            .unwrap_or(false);
                        let u = self.summary.mcp.entry(server.to_string()).or_default();
                        u.calls += 1;
                        u.errors += u64::from(error);
                        u.last_call = u.last_call.max(ts);
                        let id = payload.map(call_id).unwrap_or_default();
                        if let Some((_, sources)) = self.context_results.iter_mut().find(|(i, _)| *i == id) {
                            for (origin, name, _) in sources {
                                *origin = ContextOrigin::Mcp;
                                *name = server.to_string();
                            }
                        }
                        self.summary.context.retag(&id, ContextOrigin::Mcp, server);
                    }
                }
                "task_complete" | "turn_aborted" | "error" => {
                    self.summary.activity = Activity::Waiting;
                    if let (Some(ts), Some(id)) = (ts, self.turn.take()) {
                        self.summary.spans.end_at(&id, ts);
                    }
                    if let Some(id) = self.inference.take() {
                        self.summary.spans.discard_open(&id);
                    }
                    // Keep names for late outputs, not overlap attribution.
                    for pending in self.pending_tools.values_mut() {
                        pending.active = false;
                        pending.ambiguous = true;
                    }
                    self.response_in_progress = false;
                }
                _ => {}
            },
            "response_item" => match ptype {
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    self.summary.tool_calls += 1;
                    if let (Some(ts), Some(p)) = (ts, payload) {
                        self.end_inference(ts);
                        let id = call_id(p);
                        let name = p.get("name").and_then(Value::as_str).unwrap_or(ptype);
                        let mut ambiguous = false;
                        if matches!(name, "exec" | "wait") {
                            for pending in
                                self.pending_tools.values_mut().filter(|p| p.active && matches!(p.name.as_str(), "exec" | "wait"))
                            {
                                pending.ambiguous = true;
                                ambiguous = true;
                            }
                        }
                        self.pending_tools.insert(
                            id.clone(),
                            PendingTool { name: name.into(), started_at: ts, nested: Vec::new(), ambiguous, active: true },
                        );
                        self.summary.spans.open(id, name.to_string(), ts, false);
                    }
                }
                "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                    if let (Some(ts), Some(p)) = (ts, payload) {
                        // Wrapper output text is opaque; errors need typed metadata.
                        let id = call_id(p);
                        self.summary.spans.close(&id, ts, false);
                        let sources = match self.pending_tools.remove(&id) {
                            Some(p) if !p.ambiguous && !p.nested.is_empty() && p.nested.iter().all(|n| n.ended_at <= ts) => {
                                let sized = p.nested.iter().all(|n| n.output_bytes.is_some());
                                p.nested
                                    .into_iter()
                                    .map(|n| (n.origin, n.name, if sized { n.output_bytes.unwrap_or(0) } else { 1 }))
                                    .collect()
                            }
                            Some(p) => vec![(ContextOrigin::Tool, p.name, 1)],
                            None => vec![(ContextOrigin::Tool, "tool".into(), 1)],
                        };
                        self.context_results.push((id, sources));
                        self.begin_inference(ts);
                    }
                }
                // A server-side web search: billed per search by OpenAI, but
                // at a rate this table does not carry, so counted only.
                "web_search_call" => {
                    self.summary.web_searches += 1;
                    if let Some(ts) = ts {
                        self.end_inference(ts);
                    }
                }
                "reasoning" => {
                    if let Some(ts) = ts {
                        self.end_inference(ts);
                    }
                }
                "message" => match payload.and_then(|p| p.get("role")).and_then(Value::as_str) {
                    Some("assistant") => {
                        self.summary.turns += 1;
                        self.summary.health.billable_messages += 1;
                        if let Some(ts) = ts {
                            self.end_inference(ts);
                        }
                    }
                    Some("user") => {
                        if let Some(ts) = ts {
                            self.begin_inference(ts);
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
            _ => {}
        }
    }
}

/// Codex's `rate_limits`: a short window (`primary`) and a long one
/// (`secondary`), each a used-percent, a window length and a reset time in
/// epoch seconds, plus the plan and whether the limit is currently hit.
fn parse_rate_limits(v: &Value) -> crate::model::RateLimit {
    use crate::model::{RateLimit, RateWindow};
    let window = |w: Option<&Value>| -> Option<RateWindow> {
        // `"primary": null` is a missing window, not an empty one.
        let w = w.filter(|w| w.is_object())?;
        Some(RateWindow {
            used_percent: w.get("used_percent").and_then(Value::as_f64).unwrap_or(0.0),
            window_minutes: w.get("window_minutes").and_then(Value::as_u64).unwrap_or(0),
            resets_at: w
                .get("resets_at")
                .and_then(Value::as_i64)
                .filter(|s| *s > 0)
                .map(|s| std::time::UNIX_EPOCH + std::time::Duration::from_secs(s as u64)),
        })
    };
    RateLimit {
        primary: window(v.get("primary")),
        secondary: window(v.get("secondary")),
        plan: v.get("plan_type").and_then(Value::as_str).map(str::to_string),
        reached: v.get("rate_limit_reached_type").map(|x| !x.is_null()).unwrap_or(false),
    }
}

/// `call_id` on function calls, `id` on the shell-call variants.
fn call_id(payload: &Value) -> String {
    payload.get("call_id").or_else(|| payload.get("id")).and_then(Value::as_str).unwrap_or_default().to_string()
}

impl SessionTracker for CodexTranscript {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        let (lines, more) = self.reader.read_new_lines(REFRESH_BUDGET_BYTES)?;
        for l in &lines {
            self.ingest(l);
        }
        Ok(more)
    }

    fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    fn path(&self) -> &Path {
        self.reader.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    fn record_response(t: &mut CodexTranscript, input: u64, cached: u64, output: u64) -> String {
        let line = serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "last_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output},
            "total_token_usage":{
                "input_tokens":t.summary.usage.prompt() + input,
                "cached_input_tokens":t.summary.usage.cache_read + cached,
                "output_tokens":t.summary.usage.output + output,
            },
        }}})
        .to_string();
        t.ingest(&line);
        line
    }

    /// The bug this guards: the year and month directories were last touched
    /// when a child directory was created, long before the rollout of
    /// interest was written.
    /// Write a rollout with an explicit modification time.
    ///
    /// Ordering must not be left to how finely the filesystem happens to
    /// timestamp three writes microseconds apart: Linux gave all three the
    /// same mtime, the stable sort preserved insertion order, and the test
    /// failed there while passing on macOS.
    fn rollout(dir: &Path, name: &str, written: SystemTime) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_accessed(written).set_modified(written)).unwrap();
        p
    }

    const TIMEOUT: Duration = Duration::from_secs(15 * 60);

    /// One app-server, several conversations. Every live one must get a row:
    /// returning only the newest is what collapsed them into a single
    /// mis-attributed row.
    #[test]
    fn every_live_codex_thread_is_returned_newest_first() {
        let dir = std::env::temp_dir().join(format!("agent-top-threads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let started = now - Duration::from_secs(600);

        // Distinct write times, oldest first, so "newest first" has a single
        // correct answer.
        let a = rollout(&dir, "a.jsonl", now - Duration::from_secs(300));
        let b = rollout(&dir, "b.jsonl", now - Duration::from_secs(200));
        let c = rollout(&dir, "c.jsonl", now - Duration::from_secs(100));
        let recent: Vec<(PathBuf, PathBuf, SystemTime)> =
            [&a, &b, &c].iter().map(|p| ((*p).clone(), PathBuf::from("/Users/dev/code/one"), started)).collect();

        // The app-server case: the process cwd matches no conversation.
        let (paths, attribution) = attribute(Some(Path::new("/")), started, None, &recent, &HashSet::new(), now, TIMEOUT);
        assert_eq!(paths.len(), 3, "all three conversations get a row");
        assert_eq!(paths[0], c, "newest activity first");
        assert_eq!(attribution, Attribution::CwdHeuristic, "still a heuristic, and still labelled one");

        // A conversation already claimed by another process is not shown twice.
        let taken: HashSet<PathBuf> = [c.clone()].into_iter().collect();
        let (paths, _) = attribute(Some(Path::new("/")), started, None, &recent, &taken, now, TIMEOUT);
        assert_eq!(paths.len(), 2);
        assert!(!paths.contains(&c));

        // A conversation nobody has written to for longer than the activity
        // window has finished; it belongs in the stopped list, not on this
        // process.
        let stale = now + TIMEOUT + Duration::from_secs(60);
        let (paths, attribution) = attribute(Some(Path::new("/")), started, None, &recent, &HashSet::new(), stale, TIMEOUT);
        assert!(paths.is_empty());
        assert_eq!(attribution, Attribution::None);

        // The CLI case: one conversation, in the directory the process runs in.
        let (paths, _) = attribute(Some(Path::new("/Users/dev/code/one")), started, None, &recent, &HashSet::new(), now, TIMEOUT);
        assert_eq!(paths.len(), 3, "a cwd match takes every conversation in that directory");
        assert_eq!(paths[0], c);

        // A rollout that predates the process is not this process's.
        let (paths, _) = attribute(Some(Path::new("/")), now + Duration::from_secs(3600), None, &recent, &HashSet::new(), now, TIMEOUT);
        assert!(paths.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two app-servers at once, the VS Code one and a CLI-spawned one, both
    /// running from `/`. Without the open-file signal the one asked first
    /// took every live thread. The bug this guards was found live on
    /// 2026-09-04: two threads of a fresh app-server were shown on the four
    /// day old VS Code one.
    #[test]
    fn an_open_rollout_belongs_to_the_process_holding_it() {
        let dir = std::env::temp_dir().join(format!("agent-top-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let started = now - Duration::from_secs(600);
        let a = rollout(&dir, "a.jsonl", now - Duration::from_secs(200));
        let b = rollout(&dir, "b.jsonl", now - Duration::from_secs(100));
        let recent: Vec<(PathBuf, PathBuf, SystemTime)> =
            [&a, &b].iter().map(|p| ((*p).clone(), PathBuf::from("/Users/dev/code/one"), started)).collect();

        // The newer app-server holds both rollouts open. It started after the
        // rollouts' recorded start, which the heuristic would reject; the open
        // file settles it.
        let held = vec![a.clone(), b.clone()];
        let (paths, attribution) = attribute(Some(Path::new("/")), now, Some(&held), &recent, &HashSet::new(), now, TIMEOUT);
        assert_eq!(paths, vec![b.clone(), a.clone()], "held rollouts, newest written first");
        assert_eq!(attribution, Attribution::OpenFile);

        // The older app-server holds nothing. Its fallback would have taken
        // both live rollouts; with them marked taken it gets no row.
        let taken: HashSet<PathBuf> = held.iter().cloned().collect();
        let (paths, attribution) = attribute(Some(Path::new("/")), started, None, &recent, &taken, now, TIMEOUT);
        assert!(paths.is_empty());
        assert_eq!(attribution, Attribution::None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollout_owner_follows_the_launcher_runtime_but_not_nested_agents() {
        let proc = |pid, ppid, cmd: &[&str]| RawProc {
            pid,
            ppid,
            name: cmd[0].into(),
            exe: None,
            cmd: cmd.iter().map(|s| (*s).into()).collect(),
            cwd: None,
            cpu_percent: 0.0,
            rss_bytes: 0,
            start_time: 0,
            run_time: 1,
        };
        let procs = [
            proc(10, None, &["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js", "--yolo"]),
            proc(11, Some(10), &["codex", "--yolo"]),
            proc(12, Some(11), &["codex", "exec", "review"]),
        ];
        let (roots, _) = crate::process::build_forest(&procs);
        let by_pid = procs.iter().map(|p| (p.pid, p)).collect();
        let root = &roots[0];
        assert_eq!(rollout_owner(root, &by_pid).pid, 11, "read the native runtime's descriptors, not the launcher's");
        assert_eq!(rollout_owner(&root.children[0], &by_pid).pid, 11, "never claim a nested agent's rollouts");
        assert_eq!(rollout_owner(&root.children[0].children[0], &by_pid).pid, 12);
    }

    #[test]
    fn reads_explicit_subagent_lineage_and_identity() {
        use serde_json::json;

        let mut t = CodexTranscript::new("unused.jsonl");
        t.ingest(
            &json!({
                "type": "session_meta",
                "payload": {
                    "id": "child", "session_id": "root", "cli_version": "0.154.0",
                    "forked_from_id": "history-source",
                    "source": {"subagent": {"thread_spawn": {
                        "parent_thread_id": "parent", "depth": 2,
                        "agent_nickname": "Scout", "agent_role": "explorer"
                    }}}
                }
            })
            .to_string(),
        );
        assert_eq!(t.summary.session_id.as_deref(), Some("child"), "thread id wins over root session_id");
        assert_eq!(
            t.summary.subagent,
            Some(SubagentInfo { parent_session_id: "parent".into(), nickname: Some("Scout".into()), role: Some("explorer".into()) })
        );

        // New metadata also carries explicit lineage at the top level. A bare
        // parent_thread_id without the subagent source must not be inferred.
        let meta = json!({"id": "child", "thread_source": "subagent", "parent_thread_id": "parent", "agent_type": "worker"});
        assert_eq!(
            subagent_info(&meta),
            Some(SubagentInfo { parent_session_id: "parent".into(), nickname: None, role: Some("worker".into()) })
        );
        let meta = json!({"id": "child", "agent_nickname": "Scout", "source": {"subagent": {"thread_spawn": {
            "parent_thread_id": "parent", "agent_type": "worker"
        }}}});
        assert_eq!(subagent_info(&meta).unwrap().nickname.as_deref(), Some("Scout"));
        assert_eq!(subagent_info(&meta).unwrap().role.as_deref(), Some("worker"));
    }

    #[test]
    fn inherited_session_headers_do_not_replace_the_rollouts_identity() {
        use serde_json::json;

        let dir = std::env::temp_dir().join(format!("agent-top-codex-inherited-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("child.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        let own = json!({"type": "session_meta", "payload": {
            "id": "child", "session_id": "parent", "cwd": "/child", "cli_version": "0.154.0",
            "source": {"subagent": {"thread_spawn": {
                "parent_thread_id": "parent", "agent_nickname": "Scout", "agent_role": "explorer"
            }}}
        }});
        writeln!(file, "{own}").unwrap();
        let mut t = CodexTranscript::new(&path);
        t.refresh().unwrap();

        // Forked history follows the child's own header, possibly across refreshes.
        for id in ["parent", "grandparent"] {
            let inherited = json!({"type": "session_meta", "payload": {
                "id": id, "cwd": "/ancestor", "cli_version": "0.149.0", "source": "cli"
            }});
            writeln!(file, "{inherited}").unwrap();
            t.refresh().unwrap();
            assert_eq!(t.summary.session_id.as_deref(), Some("child"));
            assert_eq!(t.summary.cwd.as_deref(), Some(Path::new("/child")));
            assert_eq!(t.summary.harness_version.as_deref(), Some("0.154.0"));
            assert_eq!(
                t.summary.subagent,
                Some(SubagentInfo { parent_session_id: "parent".into(), nickname: Some("Scout".into()), role: Some("explorer".into()) })
            );
        }
        let usage = json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": 42}
        }}});
        writeln!(file, "{usage}").unwrap();
        t.refresh().unwrap();
        assert_eq!(t.summary.usage.input, 42, "continue reading events after inherited headers");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn never_infers_subagents_from_forks_or_incomplete_metadata() {
        use serde_json::json;

        for meta in [
            json!({"id": "child", "source": "cli", "forked_from_id": "parent"}),
            json!({"id": "child", "parent_thread_id": "parent"}),
            json!({"id": "child", "source": {"subagent": "review"}}),
            json!({"id": "child", "source": {"subagent": {"thread_spawn": {"depth": 1}}}}),
            json!({"id": "child", "source": {"subagent": {"thread_spawn": {"parent_thread_id": ""}}}}),
            json!({"id": "child", "thread_source": "subagent", "parent_thread_id": "child"}),
            json!({"id": "child", "source": {"subagent": {"thread_spawn": {"parent_thread_id": 42}}}}),
        ] {
            assert_eq!(subagent_info(&meta), None, "{meta}");
        }
    }

    #[test]
    fn the_rollout_id_follows_the_timestamp() {
        assert_eq!(
            rollout_id(Path::new("/x/2026/05/14/rollout-2026-05-14T21-37-50-01000000-0000-7000-0000-000000000000.jsonl")),
            "01000000-0000-7000-0000-000000000000"
        );
        assert_eq!(rollout_id(Path::new("/x/odd.jsonl")), "odd");
    }

    #[test]
    fn finds_a_fresh_rollout_under_stale_directories() {
        let root = std::env::temp_dir().join(format!("agent-top-rollouts-{}", std::process::id()));
        let day = root.join("2026").join("09").join("04");
        std::fs::create_dir_all(&day).unwrap();
        let fresh = day.join("rollout-fresh.jsonl");
        let stale = day.join("rollout-stale.jsonl");
        std::fs::write(&fresh, "{}\n").unwrap();
        std::fs::write(&stale, "{}\n").unwrap();
        let now = SystemTime::now();
        let long_ago = now - Duration::from_secs(40 * 86_400);
        std::fs::File::open(&stale).unwrap().set_modified(long_ago).unwrap();
        for dir in [&root, &root.join("2026"), &root.join("2026").join("09"), &day] {
            std::fs::File::open(dir).unwrap().set_modified(long_ago).unwrap();
        }
        let found = rollouts_under(&root, now - Duration::from_secs(1800));
        assert_eq!(found, vec![fresh], "the fresh file is found through directories nobody has touched in weeks");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_the_latest_rate_limit_snapshot() {
        let dir = std::env::temp_dir().join(format!("agent-top-codex-rl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-06-16T20:45:04.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":10,"output_tokens":1,"total_tokens":11}}}},"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":1.0,"window_minutes":300,"resets_at":1781660699}},"secondary":{{"used_percent":27.0,"window_minutes":10080,"resets_at":1782080576}},"plan_type":"plus","rate_limit_reached_type":null}}}}}}"#).unwrap();
        // A later snapshot with higher usage; the latest wins.
        writeln!(f, r#"{{"timestamp":"2026-06-16T20:50:00.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":20,"output_tokens":2,"total_tokens":22}}}},"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":42.0,"window_minutes":300,"resets_at":1781660999}},"secondary":{{"used_percent":28.0,"window_minutes":10080,"resets_at":1782080576}},"plan_type":"plus","rate_limit_reached_type":"primary"}}}}}}"#).unwrap();
        let mut t = CodexTranscript::new(&path);
        t.refresh().unwrap();
        let rl = t.summary().rate_limit.as_ref().expect("rate limit parsed");
        assert_eq!(rl.plan.as_deref(), Some("plus"));
        assert!(rl.reached, "the latest snapshot reports the primary window hit");
        let p = rl.primary.expect("primary window");
        assert_eq!(p.used_percent, 42.0, "the latest value, not the first");
        assert_eq!(p.window_minutes, 300);
        assert_eq!(rl.secondary.unwrap().used_percent, 28.0);
        assert_eq!(rl.tightest().map(|w| w.used_percent), Some(42.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_with_no_windows_keeps_the_last_one() {
        let dir = std::env::temp_dir().join(format!("agent-top-codex-rl-premium-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-09-24T10:00:00.000Z","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","primary":{{"used_percent":12.0,"window_minutes":300,"resets_at":1790000000}},"secondary":{{"used_percent":40.0,"window_minutes":10080,"resets_at":1790500000}},"plan_type":"plus","rate_limit_reached_type":null}}}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-09-24T10:00:05.000Z","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"premium","limit_name":null,"primary":null,"secondary":null,"credits":{{"has_credits":false,"unlimited":false,"balance":"0"}},"plan_type":"plus","rate_limit_reached_type":null}}}}}}"#).unwrap();
        drop(f);
        let mut t = CodexTranscript::new(&path);
        t.refresh().unwrap();
        let rl = t.summary().rate_limit.as_ref().expect("the codex snapshot survives");
        assert_eq!(rl.primary.map(|w| w.used_percent), Some(12.0));
        assert_eq!(rl.secondary.map(|w| w.used_percent), Some(40.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn counts_mcp_calls_per_server_from_the_end_event() {
        let dir = std::env::temp_dir().join(format!("agent-top-codex-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // Two MCP servers, one call failing. The matching response_item
        // function_call/output pair is what makes the tool-call count and span;
        // the mcp_tool_call_end is the only line naming the server.
        writeln!(f, r#"{{"timestamp":"2026-05-27T09:00:01.000Z","type":"response_item","payload":{{"type":"function_call","call_id":"c1","name":"github_fetch_file"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-05-27T09:00:02.000Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"c1"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-05-27T09:00:02.100Z","type":"event_msg","payload":{{"type":"mcp_tool_call_end","call_id":"c1","invocation":{{"server":"codex_apps","tool":"github_fetch_file"}},"duration":{{"secs":1,"nanos":0}},"result":{{"Ok":{{}}}}}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-05-27T09:00:05.000Z","type":"event_msg","payload":{{"type":"mcp_tool_call_end","call_id":"c2","invocation":{{"server":"codex_apps","tool":"github_search"}},"duration":{{"secs":0,"nanos":0}},"result":{{"Err":"boom"}}}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-05-27T09:00:07.000Z","type":"event_msg","payload":{{"type":"mcp_tool_call_end","call_id":"c3","invocation":{{"server":"node_repl","tool":"js"}},"duration":{{"secs":0,"nanos":0}},"result":{{"Ok":{{}}}}}}}}"#).unwrap();
        let mut t = CodexTranscript::new(&path);
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.mcp.len(), 2);
        let apps = &s.mcp["codex_apps"];
        assert_eq!((apps.calls, apps.errors), (2, 1));
        assert_eq!(apps.last_call, parse_rfc3339_utc("2026-05-27T09:00:05.000Z"));
        assert_eq!(s.mcp["node_repl"].calls, 1);
        // The one call with a response_item pair is one tool call and one span;
        // the mcp_tool_call_end lines do not add to that.
        assert_eq!(s.tool_calls, 1, "mcp_tool_call_end must not double-count tool calls");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_context_per_tool_from_last_token_usage_and_skips_the_repeated_snapshot() {
        for delayed in [false, true] {
            let mut t = CodexTranscript::new("unused.jsonl").with_prices(pricing::builtin_table());
            t.ingest(r#"{"timestamp":"2026-09-18T09:00:01Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"exec_command"}}"#);
            t.ingest(r#"{"timestamp":"2026-09-18T09:00:01Z","type":"response_item","payload":{"type":"function_call","call_id":"c2","name":"github_fetch_file"}}"#);
            if !delayed {
                record_response(&mut t, 10_000, 0, 100);
            }
            t.ingest(
                r#"{"timestamp":"2026-09-18T09:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}"#,
            );
            t.ingest(
                r#"{"timestamp":"2026-09-18T09:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c2"}}"#,
            );
            t.ingest(r#"{"type":"event_msg","payload":{"type":"mcp_tool_call_end","call_id":"c2","invocation":{"server":"codex_apps","tool":"github_fetch_file"},"result":{"Ok":{}}}}"#);
            if delayed {
                record_response(&mut t, 10_000, 0, 100);
            }
            let initial = t.summary.context.sources();
            assert_eq!(initial.len(), 1);
            assert_eq!((initial[0].name.as_str(), initial[0].tokens), ("other", 10_000));

            t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
            let snapshot = record_response(&mut t, 13_100, 12_000, 40);
            t.ingest(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#);
            t.ingest(&snapshot);
            let c: HashMap<_, _> = t.summary.context.sources().into_iter().map(|c| (c.name.clone(), c)).collect();
            assert_eq!((c["exec_command"].calls, c["exec_command"].tokens), (1, 1_500));
            assert_eq!((c["codex_apps"].calls, c["codex_apps"].tokens, c["codex_apps"].origin), (1, 1_500, ContextOrigin::Mcp));
            assert!(!c.contains_key("github_fetch_file"));
            assert_eq!(c["other"].tokens, 10_100);
            assert_eq!(c["other"].cost_usd, 0.0);
        }
    }

    #[test]
    fn delayed_usage_charges_only_results_consumed_by_that_response() {
        let mut t = CodexTranscript::new("unused.jsonl").with_prices(pricing::builtin_table());
        t.ingest(r#"{"type":"turn_context","payload":{"model":"gpt-5.4-mini"}}"#);
        t.ingest(r#"{"timestamp":"2026-09-18T09:00:01Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"exec_command"}}"#);
        t.ingest(r#"{"timestamp":"2026-09-18T09:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}"#);
        // A later item in the same streamed response must not consume c1.
        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        let snapshot = record_response(&mut t, 1_000, 200, 10);
        let initial = t.summary.context.sources();
        assert_eq!(initial.len(), 1);
        assert_eq!((initial[0].name.as_str(), initial[0].tokens), ("other", 1_000));

        t.ingest(r#"{"type":"response_item","payload":{"type":"reasoning"}}"#);
        t.ingest(r#"{"timestamp":"2026-09-18T09:00:03Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"c2","name":"apply_patch"}}"#);
        t.ingest(
            r#"{"timestamp":"2026-09-18T09:00:04Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c2"}}"#,
        );
        for snapshot in [
            snapshot.as_str(),
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":null}}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{}}}}"#,
        ] {
            t.ingest(snapshot);
        }
        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        assert_eq!(t.summary.context.sources(), initial);
        record_response(&mut t, 1_300, 1_100, 20);
        let sources = t.summary.context.sources();
        let command = sources.iter().find(|s| s.name == "exec_command").unwrap();
        assert_eq!((command.calls, command.tokens), (1, 290));
        assert!(!sources.iter().any(|s| s.name == "apply_patch"));

        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        record_response(&mut t, 1_350, 1_200, 5);
        let sources = t.summary.context.sources();
        let patch = sources.iter().find(|s| s.name == "apply_patch").unwrap();
        assert_eq!((patch.calls, patch.tokens), (1, 30));
        assert_eq!(sources.iter().map(|s| s.tokens).sum::<u64>(), 1_350);
        let prompt_cost = t.summary.cost_breakdown.input + t.summary.cost_breakdown.cache_read;
        assert!(prompt_cost > 0.0);
        assert!((sources.iter().map(|s| s.cost_usd).sum::<f64>() - prompt_cost).abs() < 1e-9);
        assert_eq!((t.summary.usage.prompt(), t.summary.usage.output, t.summary.tool_calls), (3_650, 35, 2));
    }

    #[test]
    fn tool_search_errors_are_not_part_of_the_requesting_responses_prompt() {
        let mut t = CodexTranscript::new("unused.jsonl");
        t.ingest(r#"{"type":"response_item","payload":{"type":"tool_search_call","call_id":"search"}}"#);
        t.ingest(
            r#"{"timestamp":"2026-09-18T09:00:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"search"}}"#,
        );
        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        record_response(&mut t, 1_000, 0, 10);
        let sources = t.summary.context.sources();
        assert_eq!(sources.len(), 1);
        assert_eq!((sources[0].name.as_str(), sources[0].tokens), ("other", 1_000));

        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        record_response(&mut t, 1_060, 0, 10);
        let sources = t.summary.context.sources();
        let tool = sources.iter().find(|s| s.origin == ContextOrigin::Tool).unwrap();
        assert_eq!((tool.calls, tool.tokens), (1, 50));
    }

    #[test]
    fn identical_response_usage_is_not_a_snapshot_when_cumulative_usage_advances() {
        let mut t = CodexTranscript::new("unused.jsonl").with_prices(pricing::builtin_table());
        t.ingest(r#"{"type":"turn_context","payload":{"model":"gpt-5.4-mini"}}"#);
        t.ingest(r#"{"timestamp":"2026-09-18T09:00:01Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"exec_command"}}"#);
        t.ingest(r#"{"timestamp":"2026-09-18T09:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}"#);
        let snapshot = record_response(&mut t, 1_000, 0, 10);
        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        t.ingest(&snapshot);
        assert_eq!(t.summary.context.sources().len(), 1);
        record_response(&mut t, 1_000, 0, 10);
        let sources = t.summary.context.sources();
        let command = sources.iter().find(|s| s.name == "exec_command").unwrap();
        assert_eq!((command.calls, command.tokens), (1, 0));
        assert_eq!(sources.iter().map(|s| s.tokens).sum::<u64>(), 1_000);
        assert!((sources.iter().map(|s| s.cost_usd).sum::<f64>() - t.summary.cost_breakdown.input).abs() < 1e-9);
    }

    #[test]
    fn interrupted_responses_keep_results_for_the_next_consuming_response() {
        for ending in ["turn_aborted", "error"] {
            let mut t = CodexTranscript::new("unused.jsonl");
            let snapshot = record_response(&mut t, 1_000, 0, 10);
            t.ingest(r#"{"timestamp":"2026-09-18T09:00:01Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"exec_command"}}"#);
            t.ingest(
                r#"{"timestamp":"2026-09-18T09:00:02Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}"#,
            );
            t.ingest(&serde_json::json!({"type":"event_msg","payload":{"type":ending}}).to_string());
            t.ingest(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#);
            t.ingest(&snapshot);
            t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
            record_response(&mut t, 1_110, 0, 10);
            let sources = t.summary.context.sources();
            let command = sources.iter().find(|s| s.name == "exec_command").unwrap();
            assert_eq!((command.calls, command.tokens), (1, 100));
        }
    }

    #[test]
    fn reads_cumulative_usage_and_state() {
        let dir = std::env::temp_dir().join(format!("agent-top-codex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:20.787Z","type":"session_meta","payload":{{"id":"01a0","cwd":"/tmp/p","cli_version":"0.149.1"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:21.000Z","type":"turn_context","payload":{{"model":"gpt-5-codex"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:22.000Z","type":"event_msg","payload":{{"type":"task_started"}}}}"#).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"2026-08-28T08:53:23.000Z","type":"response_item","payload":{{"type":"function_call","call_id":"call_1","name":"shell"}}}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:24.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":14778,"cached_input_tokens":12672,"output_tokens":241,"total_tokens":15019}}}}}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:25.000Z","type":"event_msg","payload":{{"type":"token_count","info":null}}}}"#)
            .unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:25.500Z","type":"response_item","payload":{{"type":"web_search_call","status":"completed"}}}}"#).unwrap();
        let mut t = CodexTranscript::new(&path).with_prices(pricing::builtin_table());
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.session_id.as_deref(), Some("01a0"));
        assert_eq!(s.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(s.usage.input, 14778 - 12672);
        assert_eq!(s.usage.cache_read, 12672);
        assert_eq!(s.usage.total(), 15019);
        // gpt-5-codex has no entry of its own; it resolves to gpt-5 by the
        // longest-prefix rule (input 1.25, cache read 0.125, output 10).
        assert_eq!(s.unpriced_tokens, 0, "priced now that OpenAI's rows are in the table");
        assert!((s.cost_usd - (2106.0 * 1.25 + 12672.0 * 0.125 + 241.0 * 10.0) / 1_000_000.0).abs() < 1e-9, "{}", s.cost_usd);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.activity, Activity::Working);
        assert_eq!(read_meta(&path).unwrap().0, PathBuf::from("/tmp/p"));
        assert_eq!(s.web_searches, 1);
        let tools: Vec<_> = s.spans.iter().filter(|sp| sp.kind == SpanKind::Tool).collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "shell");
        assert!(tools[0].is_open(), "no output item yet");
        let turns: Vec<_> = s.spans.iter().filter(|sp| sp.kind == SpanKind::Turn).collect();
        assert_eq!(turns.len(), 1);
        assert!(turns[0].is_open(), "task_started with no task_complete");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pairs_calls_with_their_outputs() {
        let dir = std::env::temp_dir().join(format!("agent-top-codex-spans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:23.000Z","type":"response_item","payload":{{"type":"function_call","call_id":"call_1","name":"exec_command"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:23.100Z","type":"response_item","payload":{{"type":"custom_tool_call","call_id":"call_2","name":"apply_patch"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:24.000Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"call_1","output":"ok"}}}}"#).unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:26.100Z","type":"response_item","payload":{{"type":"custom_tool_call_output","call_id":"call_2","output":"ok"}}}}"#).unwrap();
        // The model answers the outputs 1.5 s after the last one, then the turn completes.
        writeln!(
            f,
            r#"{{"timestamp":"2026-08-28T08:53:27.600Z","type":"response_item","payload":{{"type":"message","role":"assistant"}}}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"timestamp":"2026-08-28T08:53:27.700Z","type":"event_msg","payload":{{"type":"task_complete"}}}}"#).unwrap();
        let mut t = CodexTranscript::new(&path);
        t.refresh().unwrap();
        let all = t.summary().spans.to_vec();
        let spans: Vec<_> = all.iter().filter(|sp| sp.kind == SpanKind::Tool).collect();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "exec_command");
        assert_eq!(spans[0].duration_ms, Some(1_000));
        assert_eq!(spans[1].name, "apply_patch");
        assert_eq!(spans[1].duration_ms, Some(3_000));
        assert_eq!(t.summary().tool_calls, 2);
        // One inference: opened by the first output at :24, not re-opened by the
        // second at :26.1, ended by the assistant message at :27.6.
        let inf: Vec<_> = all.iter().filter(|sp| sp.kind == SpanKind::Inference).collect();
        assert_eq!(inf.len(), 1);
        assert_eq!(inf[0].duration_ms, Some(3_600));
        assert!(all.iter().all(|sp| sp.kind != SpanKind::Turn), "no task_started in this file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_mode_records_nested_commands_and_patches_alongside_exec() {
        // Sanitised 0.154.0 metadata; nested ids are independent of wrapper ids.
        let path = std::env::temp_dir().join(format!("agent-top-codex-nested-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        let mut t = CodexTranscript::new(&path);
        for line in [
            r#"{"timestamp":"2026-09-18T07:22:23.296Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"call_wrapper_1"}}"#,
            r#"{"timestamp":"2026-09-18T07:22:23.334Z","type":"event_msg","payload":{"type":"item_completed","started_at_ms":1789716143334,"completed_at_ms":1789716143334,"item":{"type":"CommandExecution","id":"exec-command-1","source":"unified_exec_startup","status":"completed","exit_code":0}}}"#,
            r#"{"timestamp":"2026-09-18T07:22:23.348Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_wrapper_1"}}"#,
            r#"{"timestamp":"2026-09-18T07:22:30.281Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"call_wrapper_2"}}"#,
            r#"{"timestamp":"2026-09-18T07:22:30.288Z","type":"event_msg","payload":{"type":"item_completed","started_at_ms":1789716150288,"completed_at_ms":1789716150288,"item":{"type":"FileChange","id":"exec-patch-1","status":"completed"}}}"#,
            r#"{"timestamp":"2026-09-18T07:22:30.351Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_wrapper_2"}}"#,
        ] {
            writeln!(f, "{line}").unwrap();
            t.refresh().unwrap();
        }
        let _ = std::fs::remove_file(&path);
        let tools: Vec<_> = t.summary.spans.iter().filter(|s| s.kind == SpanKind::Tool).collect();
        assert_eq!(t.summary.tool_calls, 4, "two wrapper invocations and two actual nested tool invocations");
        assert_eq!(
            tools.iter().map(|s| (s.name.as_str(), s.duration_ms)).collect::<Vec<_>>(),
            [("exec", Some(52)), ("exec_command", Some(0)), ("exec", Some(70)), ("apply_patch", Some(0)),]
        );
        assert!(tools.iter().all(|s| !s.error));
        assert!(t.pending_tools.is_empty(), "completion items do not leave unmatched response calls");
        // Only wrapper outputs contribute to context accounting.
        record_response(&mut t, 1_000, 0, 0);
        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        let usage = TokenUsage { input: 1_100, ..Default::default() };
        let cost = crate::model::CostBreakdown { input: 1.0, ..Default::default() };
        t.summary.context.response(&usage, &cost);
        let sources = t.summary.context.sources();
        for name in ["exec_command", "apply_patch"] {
            let source = sources.iter().find(|s| s.name == name).unwrap();
            assert_eq!((source.calls, source.tokens), (1, 50));
        }
        assert!(!sources.iter().any(|s| s.name == "exec"));

        let mut baseline = crate::harness::ContextLedger::default();
        baseline.response(&TokenUsage { input: 1_000, ..Default::default() }, &Default::default());
        baseline.result("call_wrapper_1", ContextOrigin::Tool, "exec");
        baseline.result("call_wrapper_2", ContextOrigin::Tool, "exec");
        baseline.response(&usage, &cost);
        for input in [1_150, 1_180] {
            let usage = TokenUsage { input, ..Default::default() };
            t.summary.context.response(&usage, &cost);
            baseline.response(&usage, &cost);
            let totals = |ledger: &crate::harness::ContextLedger| {
                ledger
                    .sources()
                    .iter()
                    .fold((0, 0, 0.0), |(calls, tokens, cost), s| (calls + s.calls, tokens + s.tokens, cost + s.cost_usd))
            };
            let (calls, tokens, cost) = totals(&t.summary.context);
            let (old_calls, old_tokens, old_cost) = totals(&baseline);
            assert_eq!((calls, tokens), (old_calls, old_tokens));
            assert!((cost - old_cost).abs() < 1e-9);
        }
    }

    #[test]
    fn nested_output_sizes_use_decoded_text_not_inputs_or_duplicate_fields() {
        use serde_json::json;
        for (item, expected) in [
            (
                json!({"type":"CommandExecution","formatted_output":"é\n", "aggregated_output":"longer output", "stdout":"duplicate"}),
                Some(3),
            ),
            (json!({"type":"CommandExecution","aggregated_output":"abcd","stdout":"duplicate","stderr":"duplicate"}), Some(4)),
            (json!({"type":"CommandExecution","stdout":"ab","stderr":"c"}), Some(3)),
            (json!({"type":"CommandExecution","command":["must not size this"]}), None),
            (json!({"type":"FileChange","stdout":"ok\n","stderr":"!","changes":{"file":"not output"}}), Some(4)),
            (json!({"type":"FileChange","stdout":"","stderr":""}), Some(0)),
            (json!({"type":"FileChange","changes":{"file":"not output"}}), None),
            (json!({"type":"McpToolCall","result":{"content":[{"type":"text","text":"abc"},{"type":"text","text":"d"}]}}), Some(4)),
            (json!({"type":"McpToolCall","result":{"content":[{"type":"image","data":"not text"}]}}), None),
            (json!({"type":"McpToolCall","result":{"content":[],"structuredContent":{"large":"result"}}}), None),
            (json!({"type":"McpToolCall","error":{"message":"failed"}}), Some(6)),
            (json!({"type":"DynamicToolCall","content_items":[{"type":"inputText","text":"abc"}],"arguments":"not output"}), Some(3)),
            (json!({"type":"DynamicToolCall","content_items":[{"type":"inputImage","imageUrl":"not text"}]}), None),
            (json!({"type":"DynamicToolCall","content_items":[{"type":"inputText","text":42}]}), None),
            (json!({"type":"DynamicToolCall","error":"failed"}), Some(6)),
        ] {
            assert_eq!(nested_output_bytes(&item), expected, "{item}");
        }
    }

    #[test]
    fn code_mode_context_weights_children_without_changing_wrapper_totals() {
        use serde_json::json;
        for wrapper in ["exec", "wait"] {
            for (command, patch, expected) in [
                (Some("abc"), Some("d"), (30, 10)),
                (Some("abc"), None, (20, 20)),
                (None, Some("d"), (20, 20)),
                (Some(""), Some(""), (20, 20)),
                (Some(""), Some("d"), (0, 40)),
            ] {
                let mut t = CodexTranscript::new("unused.jsonl").with_prices(pricing::builtin_table());
                t.ingest(r#"{"type":"turn_context","payload":{"model":"gpt-5.4-mini"}}"#);
                t.ingest(
                    &json!({"timestamp":"1970-01-01T00:00:01Z","type":"response_item",
                    "payload":{"type":"custom_tool_call","name":wrapper,"call_id":"wrapper"}})
                    .to_string(),
                );
                for (item, start, end) in [
                    (
                        json!({"type":"CommandExecution","id":"exec-command","source":"unified_exec_startup","formatted_output":command}),
                        1100,
                        1200,
                    ),
                    (json!({"type":"FileChange","id":"exec-patch","stdout":patch}), 1300, 1400),
                ] {
                    let line = json!({"type":"event_msg","payload":{"type":"item_completed","item":item,
                        "started_at_ms":start,"completed_at_ms":end}})
                    .to_string();
                    t.ingest(&line);
                    t.ingest(&line);
                }
                t.ingest(r#"{"timestamp":"1970-01-01T00:00:02Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"wrapper"}}"#);
                record_response(&mut t, 1_000, 0, 10);
                assert_eq!(t.summary.context.sources().len(), 1);
                t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
                let snapshot = record_response(&mut t, 1_050, 1_000, 10);
                t.ingest(&snapshot);
                let sources = t.summary.context.sources();
                let command = sources.iter().find(|s| s.name == "exec_command").unwrap();
                let patch = sources.iter().find(|s| s.name == "apply_patch").unwrap();
                assert_eq!((command.tokens, patch.tokens), expected);
                assert_eq!((command.calls, patch.calls, t.summary.tool_calls), (1, 1, 3));
                assert!(!sources.iter().any(|s| s.name == wrapper));
                assert_eq!(sources.iter().map(|s| s.tokens).sum::<u64>(), 1_050);
                let prompt_cost = t.summary.cost_breakdown.input + t.summary.cost_breakdown.cache_read;
                assert!((sources.iter().map(|s| s.cost_usd).sum::<f64>() - prompt_cost).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn code_mode_context_weights_mcp_and_dynamic_results_and_preserves_errors() {
        use serde_json::json;
        let mut t = CodexTranscript::new("unused.jsonl");
        t.ingest(r#"{"timestamp":"1970-01-01T00:00:01Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"wrapper"}}"#);
        for item in [
            json!({"id":"exec-mcp","type":"McpToolCall","tool":"lookup","server":"docs","result":{"content":[{"type":"text","text":"abc"}]}}),
            json!({"id":"exec-dynamic","type":"DynamicToolCall","tool":"check","success":false,"content_items":[{"type":"inputText","text":"d"}]}),
        ] {
            t.ingest(
                &json!({"type":"event_msg","payload":{"type":"item_completed","started_at_ms":1100,"completed_at_ms":1500,"item":item}})
                    .to_string(),
            );
        }
        t.ingest(r#"{"timestamp":"1970-01-01T00:00:02Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"wrapper"}}"#);
        record_response(&mut t, 1_000, 0, 0);
        t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
        record_response(&mut t, 1_040, 0, 0);
        let sources = t.summary.context.sources();
        let mcp = sources.iter().find(|s| s.name == "docs").unwrap();
        let dynamic = sources.iter().find(|s| s.name == "check").unwrap();
        assert_eq!((mcp.origin, mcp.calls, mcp.tokens), (ContextOrigin::Mcp, 1, 30));
        assert_eq!((dynamic.origin, dynamic.calls, dynamic.tokens), (ContextOrigin::Tool, 1, 10));
        assert!(t.summary.spans.iter().any(|s| s.name == "check" && s.error));
    }

    #[test]
    fn code_mode_context_keeps_ambiguous_wrappers_and_survives_span_eviction() {
        use serde_json::json;
        let child = |id, kind, start, end| {
            json!({
                "item":{"id":id,"type":kind}, "started_at_ms":start, "completed_at_ms":end,
            })
        };
        let patch = child("exec-patch", "FileChange", Some(1250), Some(1500));
        let unknown = child("exec-unknown", "FutureTool", Some(1250), Some(1500));
        for (children, expected, calls) in [
            (vec![patch.clone()], "apply_patch", 1),
            (vec![patch.clone(), patch.clone()], "apply_patch", 1),
            (vec![patch.clone(), child("exec-patch-2", "FileChange", Some(1500), Some(1750))], "apply_patch", 2),
            (vec![patch.clone(), unknown.clone()], "exec", 1),
            (vec![unknown, patch], "exec", 1),
            (vec![child("exec-patch", "FileChange", Some(500), Some(1500))], "exec", 1),
            (vec![child("exec-patch", "FileChange", Some(1250), Some(2500))], "exec", 1),
            (vec![child("exec-patch", "FileChange", None, Some(1500))], "exec", 1),
            (vec![child("exec-patch", "FileChange", Some(1250), None)], "exec", 1),
            (vec![], "exec", 1),
        ] {
            let mut t = CodexTranscript::new("unused.jsonl");
            t.ingest(r#"{"timestamp":"1970-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"wrapper"}}"#);
            for completion in children {
                t.nested_tool_completed(&completion);
            }
            for i in 0..t.summary.spans.cap() {
                t.summary.spans.open(format!("filler-{i}"), "tool".into(), SystemTime::UNIX_EPOCH, false);
            }
            if t.pending_tools["wrapper"].nested.iter().any(|n| n.id == "exec-patch") {
                let before = t.summary.tool_calls;
                t.nested_tool_completed(&child("exec-patch", "FileChange", Some(1250), Some(1500)));
                assert_eq!(t.summary.tool_calls, before);
            }
            t.ingest(r#"{"timestamp":"1970-01-01T00:00:02.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"wrapper"}}"#);
            record_response(&mut t, 1_000, 0, 0);
            t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
            record_response(&mut t, 1_100, 0, 0);
            let sources: Vec<_> = t.summary.context.sources().into_iter().filter(|s| s.origin != ContextOrigin::Other).collect();
            assert_eq!(sources.len(), 1);
            assert_eq!((sources[0].name.as_str(), sources[0].calls, sources[0].tokens), (expected, calls, 100));
        }
    }

    #[test]
    fn code_mode_context_rejects_overlapping_exec_and_wait_but_not_wait_agent() {
        use serde_json::json;
        for other in ["exec", "wait", "wait_agent"] {
            for exec_first in [true, false] {
                let mut t = CodexTranscript::new("unused.jsonl");
                for (id, name) in [("wrapper", "exec"), ("other", other)] {
                    t.ingest(
                        &json!({"timestamp":"1970-01-01T00:00:01.000Z", "type":"response_item",
                        "payload":{"type":"function_call","name":name,"call_id":id}})
                        .to_string(),
                    );
                }
                t.nested_tool_completed(&json!({"started_at_ms":1250,"completed_at_ms":1500,
                    "item":{"id":"exec-patch","type":"FileChange"}}));
                for id in if exec_first { ["wrapper", "other"] } else { ["other", "wrapper"] } {
                    t.ingest(
                        &json!({"timestamp":"1970-01-01T00:00:02.000Z", "type":"response_item",
                        "payload":{"type":"function_call_output","call_id":id}})
                        .to_string(),
                    );
                }
                record_response(&mut t, 1_000, 0, 0);
                t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
                record_response(&mut t, 1_100, 0, 0);
                let sources = t.summary.context.sources();
                assert_eq!(sources.iter().any(|s| s.name == "apply_patch"), other == "wait_agent");
                assert_eq!(sources.iter().map(|s| s.calls).sum::<u64>(), 2);
                assert_eq!(sources.iter().map(|s| s.tokens).sum::<u64>(), 1_100);
            }
        }
    }

    #[test]
    fn unfinished_wrappers_do_not_block_context_attribution_in_later_turns() {
        use serde_json::json;
        for ending in ["task_complete", "turn_aborted", "error"] {
            let mut t = CodexTranscript::new("unused.jsonl");
            for line in [
                json!({"timestamp":"1970-01-01T00:00:01.000Z","type":"response_item",
                    "payload":{"type":"custom_tool_call","name":"exec","call_id":"old"}}),
                json!({"timestamp":"1970-01-01T00:00:02.000Z","type":"event_msg","payload":{"type":ending}}),
                json!({"timestamp":"1970-01-01T00:00:03.000Z","type":"event_msg","payload":{"type":"task_started"}}),
                json!({"timestamp":"1970-01-01T00:00:03.100Z","type":"response_item",
                    "payload":{"type":"custom_tool_call","name":"exec","call_id":"new"}}),
                json!({"type":"event_msg","payload":{"type":"item_completed","started_at_ms":3200,"completed_at_ms":3500,
                    "item":{"type":"FileChange","id":"exec-patch"}}}),
                json!({"timestamp":"1970-01-01T00:00:04.000Z","type":"response_item",
                    "payload":{"type":"custom_tool_call_output","call_id":"new"}}),
                json!({"timestamp":"1970-01-01T00:00:05.000Z","type":"response_item",
                    "payload":{"type":"custom_tool_call_output","call_id":"old"}}),
            ] {
                t.ingest(&line.to_string());
            }
            record_response(&mut t, 1_000, 0, 0);
            t.ingest(r#"{"type":"response_item","payload":{"type":"message","role":"assistant"}}"#);
            record_response(&mut t, 1_100, 0, 0);
            let sources = t.summary.context.sources();
            assert_eq!(sources.len(), 3);
            for name in ["apply_patch", "exec"] {
                let source = sources.iter().find(|s| s.name == name).unwrap();
                assert_eq!((source.calls, source.tokens), (1, 50));
            }
        }
    }

    #[test]
    fn code_mode_completion_metadata_preserves_timing_names_and_errors() {
        use serde_json::json;
        let mut t = CodexTranscript::new("unused.jsonl");
        for (item, name, error) in [
            (json!({"type":"CommandExecution","source":"unified_exec_startup","exit_code":1}), "exec_command", true),
            (json!({"type":"CommandExecution","source":"unified_exec_interaction","status":"completed"}), "write_stdin", false),
            (json!({"type":"FileChange","status":"declined"}), "apply_patch", true),
            (json!({"type":"McpToolCall","tool":"search","status":"failed"}), "search", true),
            (json!({"type":"DynamicToolCall","tool":"lookup","success":false}), "lookup", true),
        ] {
            let mut item = item;
            item["id"] = json!(format!("exec-{name}"));
            let line = json!({
                "type":"event_msg", "timestamp":"2026-09-18T07:22:25.000Z",
                "payload":{"type":"item_completed", "started_at_ms":1000, "completed_at_ms":1250, "item":item},
            })
            .to_string();
            let before = t.summary.tool_calls;
            t.ingest(&line);
            let span = t.summary.spans.iter().next_back().unwrap();
            assert_eq!((span.name.as_str(), span.duration_ms, span.error), (name, Some(250), error));
            assert_eq!(span.started_at, SystemTime::UNIX_EPOCH + Duration::from_secs(1));
            // Replayed completions must not count twice.
            t.ingest(&line);
            assert_eq!(t.summary.tool_calls, before + 1);
        }
        assert_eq!(t.summary.tool_calls, 5);
        assert_eq!(t.summary.spans.len(), 5);
        assert!(t.inference.is_none(), "nested results are not new model requests");
    }

    #[test]
    fn completion_items_do_not_duplicate_direct_calls_or_guess_unknown_tools() {
        use serde_json::json;
        let mut t = CodexTranscript::new("unused.jsonl");
        t.ingest(r#"{"timestamp":"2026-09-18T07:22:23.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_direct"}}"#);
        for item in [
            json!({"type":"CommandExecution","id":"call_direct","source":"unified_exec_startup"}),
            json!({"type":"FileChange","id":"call_patch"}),
            json!({"type":"CommandExecution","id":"exec-user","source":"user_shell"}),
            json!({"type":"CommandExecution","id":"exec-unknown","source":"new_source"}),
            json!({"type":"UnknownTool","id":"exec-future"}),
            json!({"type":"DynamicToolCall","id":"exec-missing-name"}),
            json!({"type":"McpToolCall","id":"exec-empty-name","tool":""}),
            json!({"type":"FileChange","id":"exec-"}),
            json!({"type":"FileChange"}),
        ] {
            t.ingest(
                &json!({
                    "type":"event_msg", "payload":{"type":"item_completed", "started_at_ms":1000, "completed_at_ms":1250, "item":item},
                })
                .to_string(),
            );
        }
        for (start, end) in [(json!(null), json!(1250)), (json!(1000), json!(null)), (json!(1250), json!(1000))] {
            t.ingest(
                &json!({
                    "type":"event_msg", "payload":{"type":"item_completed", "started_at_ms":start, "completed_at_ms":end,
                        "item":{"type":"FileChange","id":"exec-no-timing"}},
                })
                .to_string(),
            );
        }
        t.ingest(r#"{"timestamp":"2026-09-18T07:22:24.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_direct"}}"#);
        assert_eq!(t.summary.tool_calls, 1);
        let tools: Vec<_> = t.summary.spans.iter().filter(|s| s.kind == SpanKind::Tool).collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].duration_ms, Some(1000));
    }
}
