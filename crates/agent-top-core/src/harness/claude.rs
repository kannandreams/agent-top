//! Claude Code: `~/.claude/sessions/<pid>.json` registry and
//! `~/.claude/projects/<encoded-cwd>/<session>.jsonl` transcripts.
//!
//! Format notes (verified on Claude Code 2.1.259, 2026-09-03):
//! * One API response is written as several lines, one per content block,
//!   every line carrying the same `message.id` and the same `message.usage`.
//!   Usage must be counted once per id.
//! * `usage.cache_creation.ephemeral_1h_input_tokens` /
//!   `ephemeral_5m_input_tokens` split cache writes by TTL, which have
//!   different prices.
//! * Subagents (Claude Code 2.1.233 and later): each Agent-tool call gets its
//!   own transcript at `<project>/<session>/subagents/agent-<id>.jsonl`, every
//!   line carrying the parent's `sessionId`, `isSidechain: true` and an
//!   `agentId`, with `agent-<id>.meta.json` beside it naming the agent type
//!   and the spawning `toolUseId`. The parent transcript no longer carries any
//!   sidechain lines itself. Claude Code's own cost display includes those
//!   files, so `ClaudeTranscript` tails and folds them in.
//! * A `tool_use` block in an assistant message and the `tool_result` block
//!   that answers it carry the same id in `id` / `tool_use_id`, and their
//!   lines carry the timestamps that bracket the call. That pairing is the
//!   trace: verified 240/240 on a real session.
//! * The registry file has `status: "busy" | "idle"`, which is the harness's
//!   own opinion of its state and beats any transcript heuristic.

use super::{REFRESH_BUDGET_BYTES, SessionSummary, SessionTracker, SpanLog, SpanRetention, parse_rfc3339_utc};
use crate::jsonl::TailReader;
use crate::model::{Activity, Harness, TokenUsage};
use crate::pricing::{self, Table};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn claude_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    home().map(|h| h.join(".claude"))
}

pub fn sessions_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("sessions"))
}

pub fn projects_dir() -> Option<PathBuf> {
    claude_dir().map(|d| d.join("projects"))
}

/// Claude Code's project directory name: every character that is not
/// ASCII alphanumeric becomes `-`, so `/Users/a/x.y` is `-Users-a-x-y`.
pub fn encode_project_path(p: &Path) -> String {
    p.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

pub fn transcript_path(cwd: &Path, session_id: &str) -> Option<PathBuf> {
    projects_dir().map(|d| d.join(encode_project_path(cwd)).join(format!("{session_id}.jsonl")))
}

/// One `~/.claude/sessions/<pid>.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidSession {
    pub pid: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<u64>,
}

impl PidSession {
    pub fn started(&self) -> Option<SystemTime> {
        self.started_at.map(|ms| UNIX_EPOCH + Duration::from_millis(ms))
    }
}

/// Read every registry file. Stale files for dead pids are returned too; the
/// caller reconciles against the process table.
pub fn read_pid_sessions() -> Vec<PidSession> {
    let Some(dir) = sessions_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&p)
            && let Ok(ps) = serde_json::from_str::<PidSession>(&s)
        {
            out.push(ps);
        }
    }
    out
}

/// Transcripts modified after `since`, across all projects.
pub fn recent_transcripts(since: SystemTime) -> Vec<PathBuf> {
    let Some(dir) = projects_dir() else { return Vec::new() };
    let Ok(projects) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out = Vec::new();
    for proj in projects.flatten() {
        let Ok(files) = std::fs::read_dir(proj.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(md) = f.metadata()
                && md.modified().map(|m| m >= since).unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    out
}

/// Fallback attribution when the registry has no entry: the transcript in
/// the cwd's project directory created closest after the process start.
pub fn guess_transcript(cwd: &Path, proc_start: SystemTime) -> Option<PathBuf> {
    let dir = projects_dir()?.join(encode_project_path(cwd));
    let rd = std::fs::read_dir(&dir).ok()?;
    let slack = Duration::from_secs(15);
    let mut best: Option<(Duration, PathBuf)> = None;
    for f in rd.flatten() {
        let p = f.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        // A file we cannot stat is skipped, not fatal: one unreadable
        // transcript must not abandon attribution for the whole directory.
        let Ok(md) = f.metadata() else { continue };
        let Ok(created) = md.created().or_else(|_| md.modified()) else { continue };
        if created + slack < proc_start {
            continue;
        }
        let gap = created.duration_since(proc_start).unwrap_or(Duration::ZERO);
        if best.as_ref().map(|(g, _)| gap < *g).unwrap_or(true) {
            best = Some((gap, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Where Claude Code keeps a session's subagent transcripts: one
/// `agent-<id>.jsonl` per Agent-tool call, next to an `agent-<id>.meta.json`
/// naming the agent type and the `toolUseId` that spawned it.
pub fn subagents_dir(transcript: &Path) -> Option<PathBuf> {
    let stem = transcript.file_stem()?;
    Some(transcript.with_file_name(stem).join("subagents"))
}

/// One JSONL file being tailed into a `SessionSummary`: the main transcript,
/// or one subagent's.
struct Parser {
    reader: TailReader,
    summary: SessionSummary,
    /// Dedupe state: the last API message id seen and what it contributed.
    last_msg_id: Option<String>,
    last_contrib: (TokenUsage, f64, u64),
}

impl Parser {
    fn new(path: impl Into<PathBuf>, spans: SpanRetention) -> Self {
        Parser {
            reader: TailReader::new(path),
            summary: SessionSummary { harness: Some(Harness::Claude), spans: spans.log(), ..Default::default() },
            last_msg_id: None,
            last_contrib: (TokenUsage::default(), 0.0, 0),
        }
    }

    /// Returns how many lines were ingested and whether more are waiting.
    fn refresh(&mut self, prices: &Table) -> anyhow::Result<(usize, bool)> {
        let (lines, more) = self.reader.read_new_lines(REFRESH_BUDGET_BYTES)?;
        for l in &lines {
            self.ingest(l, prices);
        }
        Ok((lines.len(), more))
    }

    fn ingest(&mut self, line: &str, prices: &Table) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_utc) {
            if self.summary.started_at.is_none() {
                self.summary.started_at = Some(ts);
            }
            self.summary.last_activity = Some(ts);
        }
        if self.summary.session_id.is_none() {
            self.summary.session_id = v.get("sessionId").and_then(Value::as_str).map(str::to_string);
        }
        if self.summary.cwd.is_none() {
            self.summary.cwd = v.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        }
        if self.summary.harness_version.is_none() {
            self.summary.harness_version = v.get("version").and_then(Value::as_str).map(str::to_string);
        }
        let sidechain = v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);
        let is_meta = v.get("isMeta").and_then(Value::as_bool).unwrap_or(false);
        let ts = v.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_utc);
        match kind {
            "assistant" => self.ingest_assistant(&v, sidechain, ts, prices),
            "user" if !is_meta => {
                // Either a prompt or a tool_result: in both cases the model owes a response.
                self.summary.activity = Activity::Working;
                if let Some(ts) = ts {
                    self.close_spans(&v, ts);
                }
            }
            _ => {}
        }
    }

    /// A user line answering tool calls: every `tool_result` block closes a span.
    fn close_spans(&mut self, v: &Value, ts: SystemTime) {
        let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else { return };
        for b in content {
            if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else { continue };
            self.summary.spans.close(id, ts, b.get("is_error").and_then(Value::as_bool).unwrap_or(false));
        }
    }

    fn ingest_assistant(&mut self, v: &Value, sidechain: bool, ts: Option<SystemTime>, prices: &Table) {
        let Some(msg) = v.get("message") else { return };
        let id = msg.get("id").and_then(Value::as_str).map(str::to_string);
        let model = msg.get("model").and_then(Value::as_str).unwrap_or("");
        if !model.is_empty() && model != "<synthetic>" {
            self.summary.model = Some(model.to_string());
        }
        if let Some(content) = msg.get("content").and_then(Value::as_array) {
            let calls = content.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"));
            for b in calls {
                self.summary.tool_calls += 1;
                if let (Some(ts), Some(id)) = (ts, b.get("id").and_then(Value::as_str)) {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                    self.summary.spans.open(id.to_string(), name.to_string(), ts, sidechain);
                }
            }
        }
        match msg.get("stop_reason").and_then(Value::as_str) {
            Some("end_turn") | Some("stop_sequence") | Some("max_tokens") | Some("refusal") => {
                self.summary.activity = Activity::Waiting;
            }
            _ => self.summary.activity = Activity::Working,
        }

        // Health is judged on the record being present but unreadable, which is
        // what a renamed field looks like from in here.
        if !same_message_id(id.as_deref(), self.last_msg_id.as_deref()) {
            self.summary.health.billable_messages += 1;
        }
        let usage = match msg.get("usage") {
            Some(u) => {
                let parsed = parse_usage(u);
                self.summary.health.usage_records += 1;
                if parsed.total() == 0 {
                    self.summary.health.empty_usage_records += 1;
                }
                parsed
            }
            None => TokenUsage::default(),
        };
        let price = prices.lookup(model);
        let cost = price.map(|p| p.cost(&usage)).unwrap_or(0.0);
        let unpriced = if price.is_none() { usage.total() } else { 0 };

        let same_message = id.is_some() && id == self.last_msg_id;
        if same_message {
            // Replace the previous contribution from this id with the latest one.
            let (u, c, un) = self.last_contrib;
            self.summary.usage.sub(&u);
            self.summary.cost_usd -= c;
            self.summary.unpriced_tokens = self.summary.unpriced_tokens.saturating_sub(un);
        } else {
            self.summary.turns += 1;
            if sidechain {
                self.summary.subagent_turns += 1;
            }
        }
        self.summary.usage.add(&usage);
        self.summary.cost_usd += cost;
        self.summary.unpriced_tokens += unpriced;
        self.last_msg_id = id;
        self.last_contrib = (usage, cost, unpriced);
    }
}

/// A Claude Code session: the main transcript plus every subagent transcript
/// under its `subagents/` directory, folded into one summary.
///
/// Claude Code bills a subagent's API calls to the session that spawned it
/// and shows them in its own cost display, but writes them to a separate
/// file, so a session that used the Agent tool reads low if only the main
/// transcript is counted. Each subagent file is tailed like the main one and
/// its tokens, cost, turns, tool calls and spans are added to the parent's.
/// A subagent may run a different model from its parent; each line is priced
/// by the model it names, so that is handled without special casing.
pub struct ClaudeTranscript {
    main: Parser,
    /// Keyed by path, so a directory listing adds each subagent once.
    subagents: BTreeMap<PathBuf, Parser>,
    prices: &'static Table,
    retention: SpanRetention,
    /// The fold of `main` and `subagents`, rebuilt whenever any of them read
    /// a line. Cheap: a clone of the main summary and a merge of the span logs.
    summary: SessionSummary,
}

impl ClaudeTranscript {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let retention = SpanRetention::Recent;
        ClaudeTranscript {
            main: Parser::new(path, retention),
            subagents: BTreeMap::new(),
            prices: pricing::table(),
            retention,
            summary: SessionSummary { harness: Some(Harness::Claude), ..Default::default() },
        }
    }

    /// Price with this table instead of the process-wide one. Lets a test
    /// assert a cost without the developer's own price file changing it.
    pub fn with_prices(mut self, prices: &'static Table) -> Self {
        self.prices = prices;
        self
    }

    /// Keep every span instead of the newest `MAX_SPANS`. See `SpanRetention`.
    pub fn with_spans(mut self, retention: SpanRetention) -> Self {
        self.retention = retention;
        self.main.summary.spans = retention.log();
        for p in self.subagents.values_mut() {
            p.summary.spans = retention.log();
        }
        self
    }

    pub fn set_registry_hints(&mut self, ps: &PidSession) {
        let s = &mut self.main.summary;
        s.session_id.get_or_insert_with(|| ps.session_id.clone());
        s.cwd.get_or_insert_with(|| ps.cwd.clone());
        if ps.version.is_some() {
            s.harness_version = ps.version.clone();
        }
        if s.started_at.is_none() {
            s.started_at = ps.started();
        }
        self.fold();
    }

    /// Pick up subagent transcripts that appeared since the last look. One
    /// directory listing per refresh; the directory is small and usually
    /// absent, so this is a single failed `open` for most sessions.
    fn discover_subagents(&mut self) {
        let Some(dir) = subagents_dir(self.main.reader.path()) else { return };
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") || self.subagents.contains_key(&p) {
                continue;
            }
            let parser = Parser::new(&p, self.retention);
            self.subagents.insert(p, parser);
        }
    }

    fn fold(&mut self) {
        let mut s = self.main.summary.clone();
        for c in self.subagents.values() {
            let t = &c.summary;
            s.usage.add(&t.usage);
            s.cost_usd += t.cost_usd;
            s.unpriced_tokens += t.unpriced_tokens;
            s.turns += t.turns;
            s.subagent_turns += t.subagent_turns;
            s.tool_calls += t.tool_calls;
            s.health.billable_messages += t.health.billable_messages;
            s.health.usage_records += t.health.usage_records;
            s.health.empty_usage_records += t.health.empty_usage_records;
            s.last_activity = s.last_activity.max(t.last_activity);
        }
        if !self.subagents.is_empty() {
            let logs = std::iter::once(&self.main.summary.spans).chain(self.subagents.values().map(|c| &c.summary.spans));
            s.spans = SpanLog::merged(logs, self.main.summary.spans.cap());
        }
        self.summary = s;
    }
}

fn same_message_id(a: Option<&str>, b: Option<&str>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x == y)
}

fn parse_usage(u: &Value) -> TokenUsage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cache_write_total = g("cache_creation_input_tokens");
    let (w1h, w5m) = match u.get("cache_creation") {
        Some(cc) => (
            cc.get("ephemeral_1h_input_tokens").and_then(Value::as_u64).unwrap_or(0),
            cc.get("ephemeral_5m_input_tokens").and_then(Value::as_u64).unwrap_or(0),
        ),
        None => (0, 0),
    };
    // Older transcripts have only the total; treat it as 5-minute writes.
    let (w1h, w5m) = if w1h + w5m == 0 { (0, cache_write_total) } else { (w1h, w5m) };
    TokenUsage {
        input: g("input_tokens"),
        cache_write_5m: w5m,
        cache_write_1h: w1h,
        cache_read: g("cache_read_input_tokens"),
        output: g("output_tokens"),
    }
}

impl SessionTracker for ClaudeTranscript {
    fn refresh(&mut self) -> anyhow::Result<bool> {
        let (mut ingested, mut more) = self.main.refresh(self.prices)?;
        self.discover_subagents();
        for c in self.subagents.values_mut() {
            // One unreadable subagent file must not take the session with it.
            if let Ok((n, m)) = c.refresh(self.prices) {
                ingested += n;
                more |= m;
            }
        }
        if ingested > 0 || self.summary.session_id.is_none() {
            self.fold();
        }
        Ok(more)
    }

    fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    fn path(&self) -> &Path {
        self.main.reader.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn encodes_paths_like_claude_code() {
        assert_eq!(
            encode_project_path(Path::new("/Users/atlas/Documents/orbital/forge/agent-top")),
            "-Users-atlas-Documents-orbital-forge-agent-top"
        );
        assert_eq!(encode_project_path(Path::new("/tmp/a.b_c")), "-tmp-a-b-c");
    }

    #[test]
    fn dedupes_usage_by_message_id_and_tracks_state() {
        let dir = std::env::temp_dir().join(format!("agent-top-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let usage = r#"{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50,"cache_creation":{"ephemeral_1h_input_tokens":100,"ephemeral_5m_input_tokens":0}}"#;
        writeln!(f, r#"{{"type":"user","timestamp":"2026-09-03T07:00:00.000Z","sessionId":"abc","cwd":"/tmp/p","message":{{"role":"user","content":"hi"}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:01.000Z","message":{{"id":"msg_1","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{{"type":"text","text":"x"}}],"usage":{usage}}}}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:02.000Z","message":{{"id":"msg_1","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{{"type":"tool_use","name":"Bash"}}],"usage":{usage}}}}}"#).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.turns, 1);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.usage.total(), 1152);
        assert_eq!(s.activity, Activity::Working);
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        // sonnet-5: 2*2 + 100*4 + 1000*0.2 + 50*10 = 4 + 400 + 200 + 500 = 1104 micro-dollars
        assert!((s.cost_usd - 0.001104).abs() < 1e-9);
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:03.000Z","message":{{"id":"msg_2","model":"claude-sonnet-5","stop_reason":"end_turn","content":[],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#).unwrap();
        t.refresh().unwrap();
        assert_eq!(t.summary().turns, 2);
        assert_eq!(t.summary().activity, Activity::Waiting);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folds_subagent_transcripts_into_the_parent() {
        let dir = std::env::temp_dir().join(format!("agent-top-claude-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // The parent spawns an Agent-tool call at 07:00:00, which is still running.
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:00.000Z","sessionId":"abc","cwd":"/tmp/p","message":{{"id":"m1","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_agent","name":"Agent"}}],"usage":{{"input_tokens":100,"output_tokens":10}}}}}}"#).unwrap();
        let mut t = ClaudeTranscript::new(&path).with_prices(pricing::builtin_table());
        t.refresh().unwrap();
        assert_eq!(t.summary().usage.total(), 110);
        assert_eq!(t.summary().subagent_turns, 0);
        // sonnet-5: 100*2 + 10*10 = 300 micro-dollars
        assert!((t.summary().cost_usd - 0.000300).abs() < 1e-9);

        // A subagent transcript appears, on a different model, with its own tool call.
        let sub = subagents_dir(&path).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        let mut g = std::fs::File::create(sub.join("agent-a1.jsonl")).unwrap();
        std::fs::write(sub.join("agent-a1.meta.json"), r#"{"agentType":"Explore"}"#).unwrap();
        writeln!(g, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:01.000Z","sessionId":"abc","isSidechain":true,"agentId":"a1","message":{{"id":"s1","model":"claude-opus-5","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_sub","name":"Grep"}}],"usage":{{"input_tokens":1000,"output_tokens":100}}}}}}"#).unwrap();
        writeln!(g, r#"{{"type":"user","timestamp":"2026-09-03T07:00:03.000Z","sessionId":"abc","isSidechain":true,"agentId":"a1","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_sub"}}]}}}}"#).unwrap();
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.usage.total(), 1210);
        assert_eq!(s.turns, 2);
        assert_eq!(s.subagent_turns, 1);
        assert_eq!(s.tool_calls, 2);
        // opus-5: 1000*5 + 100*25 = 7500 micro-dollars, on top of the parent's 300
        assert!((s.cost_usd - 0.007800).abs() < 1e-9, "{}", s.cost_usd);
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-5"), "the row's model is the parent's");
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        let last = s.last_activity.unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(last % 60, 3, "last activity is the subagent's, which wrote most recently");
        let spans = s.spans.to_vec();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "Agent");
        assert!(spans[0].is_open());
        assert_eq!(spans[1].name, "Grep");
        assert!(spans[1].sidechain);
        assert_eq!(spans[1].duration_ms, Some(2_000));

        // The subagent keeps writing; only the new lines are read.
        writeln!(g, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:04.000Z","sessionId":"abc","isSidechain":true,"agentId":"a1","message":{{"id":"s2","model":"claude-opus-5","stop_reason":"end_turn","content":[],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#).unwrap();
        t.refresh().unwrap();
        assert_eq!(t.summary().usage.total(), 1212);
        assert_eq!(t.summary().subagent_turns, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_spans_from_tool_use_and_tool_result() {
        let dir = std::env::temp_dir().join(format!("agent-top-claude-spans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:00.000Z","message":{{"id":"m1","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_a","name":"Bash"}},{{"type":"tool_use","id":"toolu_b","name":"Read"}}],"usage":{{"input_tokens":1}}}}}}"#).unwrap();
        // Results arrive on one line, in the other order, one of them failed.
        writeln!(f, r#"{{"type":"user","timestamp":"2026-09-03T07:00:02.500Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_b","is_error":true}},{{"type":"tool_result","tool_use_id":"toolu_a","is_error":false}}]}},"toolUseResult":{{}}}}"#).unwrap();
        // A subagent call that has not come back yet.
        writeln!(f, r#"{{"type":"assistant","timestamp":"2026-09-03T07:00:03.000Z","isSidechain":true,"message":{{"id":"m2","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"toolu_c","name":"Grep"}}],"usage":{{"input_tokens":1}}}}}}"#).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.refresh().unwrap();
        let spans = t.summary().spans.to_vec();
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].name, "Bash");
        assert_eq!(spans[0].duration_ms, Some(2_500));
        assert!(!spans[0].error);
        assert_eq!(spans[1].name, "Read");
        assert_eq!(spans[1].duration_ms, Some(2_500));
        assert!(spans[1].error);
        assert!(spans[2].is_open());
        assert!(spans[2].sidechain);
        assert_eq!(t.summary().tool_calls, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
