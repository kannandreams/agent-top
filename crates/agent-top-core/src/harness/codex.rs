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
//!
//! Codex model prices are not in the static table, so cost is reported as
//! unpriced tokens.

use super::{REFRESH_BUDGET_BYTES, SessionSummary, SessionTracker, SpanRetention, parse_rfc3339_utc};
use crate::jsonl::TailReader;
use crate::model::{Activity, Harness, SpanKind, TokenUsage};
use crate::pricing::{self, Table};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
        match kind {
            "session_meta" => {
                if let Some(p) = payload {
                    self.summary.session_id = p.get("id").or(p.get("session_id")).and_then(Value::as_str).map(str::to_string);
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
                    if let Some(total) = payload.and_then(|p| p.pointer("/info/total_token_usage")) {
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
                "task_complete" | "turn_aborted" | "error" => {
                    self.summary.activity = Activity::Waiting;
                    if let (Some(ts), Some(id)) = (ts, self.turn.take()) {
                        self.summary.spans.end_at(&id, ts);
                    }
                    if let Some(id) = self.inference.take() {
                        self.summary.spans.discard_open(&id);
                    }
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
                        self.summary.spans.open(id, name.to_string(), ts, false);
                    }
                }
                "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                    if let (Some(ts), Some(p)) = (ts, payload) {
                        // Codex reports the result as an opaque string, and
                        // agent-top does not read tool output, so a failed call
                        // is not distinguishable from a successful one here.
                        self.summary.spans.close(&call_id(p), ts, false);
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

    /// The bug this guards: the year and month directories were last touched
    /// when a child directory was created, long before the rollout of
    /// interest was written.
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
        let mut t = CodexTranscript::new(&path);
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.session_id.as_deref(), Some("01a0"));
        assert_eq!(s.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(s.usage.input, 14778 - 12672);
        assert_eq!(s.usage.cache_read, 12672);
        assert_eq!(s.usage.total(), 15019);
        assert_eq!(s.unpriced_tokens, 15019);
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
}
