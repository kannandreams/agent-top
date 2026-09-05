//! Gemini CLI: `~/.gemini/tmp/<project>/chats/session-<YYYY-MM-DDTHH-MM>-<id8>.jsonl`.
//!
//! Format notes (read from the recorder in `@google/gemini-cli-core` 0.58.0,
//! `services/chatRecordingService.js`, 2026-09-05; the fixture in
//! `tests/fixtures/gemini-0.58.jsonl` was written by that recorder, driven
//! with a scripted conversation, not captured from a live session):
//! * `<project>` is a slug of the working directory's basename (`agent-top`,
//!   `agent-top-1` on a clash); older versions used a SHA-256 of the path.
//!   `<project>/.project_root` holds the working directory the slug stands
//!   for. `~/.gemini/projects.json` maps paths to slugs too, but the marker
//!   is enough and is also what the CLI trusts when the two disagree.
//! * The first line is metadata: `sessionId`, `projectHash` (a SHA-256 of
//!   the working directory, not the slug), `startTime`, `lastUpdated`,
//!   `kind` (`main` or `subagent`). `{"$set": {...}}` lines update it;
//!   `lastUpdated` is rewritten after every message.
//! * A message is `{"id", "timestamp", "type", "content", ...}`, `type` one of
//!   `user`, `gemini`, `info`, `error`, `warning`. A `gemini` message carries
//!   `model`, `tokens` and, once its tool calls have completed, `toolCalls`.
//!   A message is appended again in full whenever the recorder updates it,
//!   so the same `id` appears more than once and the latest line wins.
//! * `tokens` is one API response's usage: `input` (`promptTokenCount`,
//!   which includes the cached part), `cached`, `output`
//!   (`candidatesTokenCount`, thoughts excluded), `thoughts`, `tool`
//!   (`toolUsePromptTokenCount`) and `total`. Google bills thoughts as
//!   output and tool-use prompt tokens as input, so that is how they are
//!   folded here; the resulting total equals `total`.
//! * A tool result is recorded as a `user` message whose content parts carry
//!   `functionResponse`; a human prompt has `text` parts. Only the part keys
//!   are looked at, never the text.
//! * `toolCalls[]` entries have `id`, `name`, `status` (`success`, `error`,
//!   `cancelled`, ...) and one `timestamp`, stamped when the call completed.
//!   The span runs from the `gemini` message that issued the call to that
//!   completion, so a call still running is not visible until it returns.
//!   `google_web_search` is the built-in web search and is counted as one.
//! * `{"$rewindTo": "<id>"}` removes that message and everything after it
//!   from the conversation. The tokens were spent regardless, so nothing is
//!   subtracted; messages are deduplicated by id so a `$set.messages`
//!   checkpoint that re-lists them does not double count either.
//! * A subagent gets its own file at `chats/<parent sessionId>/<id>.jsonl`
//!   with `kind: "subagent"`. Those are tailed and folded into the parent,
//!   as Claude Code's are.
//! * Turns and inferences are reconstructed from line order: a prompt starts
//!   a turn and an inference, a tool result starts an inference, and each
//!   `gemini` message ends the inference and moves the end of the turn.
//!   There is no end-of-turn marker, so a turn is as long as its last reply.
//! * Legacy `session-*.json` files (one JSON document, rewritten on every
//!   update) are not read; the CLI converts them on resume.
//!
//! Gemini CLI does not hold the file open between writes and publishes no
//! registry of its processes, so a process is matched to its conversation by
//! working directory and start time, and the row says so.

use super::{
    AttributeContext, HarnessAdapter, REFRESH_BUDGET_BYTES, SessionSummary, SessionTracker, SpanLog, SpanRetention, parse_rfc3339_utc,
};
use crate::jsonl::TailReader;
use crate::model::{Activity, Attribution, CostBreakdown, Harness, ProcNode, SpanKind, TokenUsage};
use crate::pricing::{self, Table};
use crate::process::RawProc;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// `~/.gemini`, or `$GEMINI_CLI_HOME/.gemini`, which is the CLI's own override.
pub fn gemini_dir() -> Option<PathBuf> {
    let home = std::env::var_os("GEMINI_CLI_HOME").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".gemini"))
}

/// Where every project's chats live: one directory per project slug.
pub fn tmp_dir() -> Option<PathBuf> {
    gemini_dir().map(|d| d.join("tmp"))
}

/// One main conversation on disk: its file, the working directory its project
/// directory stands for (absent for a pre-slug hashed directory) and the start
/// time from its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub started: Option<SystemTime>,
    pub session_id: Option<String>,
}

/// Every main conversation written since `since`.
pub fn recent_sessions(since: SystemTime) -> Vec<Session> {
    let Some(root) = tmp_dir() else { return Vec::new() };
    sessions_under(&root, since)
}

/// Walk `<root>/<project>/chats/*.jsonl`. Subagent files sit one level deeper
/// under the parent's id and are not sessions of their own, so the walk does
/// not descend. `.project_root` names the working directory.
pub(crate) fn sessions_under(root: &Path, since: SystemTime) -> Vec<Session> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else { return out };
    for proj in projects.flatten() {
        let dir = proj.path();
        let Ok(chats) = std::fs::read_dir(dir.join("chats")) else { continue };
        let cwd = project_root(&dir);
        for f in chats.flatten() {
            let p = f.path();
            let Ok(md) = f.metadata() else { continue };
            if !md.is_file() || p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            if !md.modified().map(|m| m >= since).unwrap_or(false) {
                continue;
            }
            let (session_id, started) = match read_meta(&p) {
                Some((id, ts)) => (Some(id), Some(ts)),
                None => (None, None),
            };
            out.push(Session { path: p, cwd: cwd.clone(), started, session_id });
        }
    }
    out
}

/// The working directory a project directory stands for.
pub fn project_root(project_dir: &Path) -> Option<PathBuf> {
    let s = std::fs::read_to_string(project_dir.join(".project_root")).ok()?;
    let s = s.trim();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

/// Cheap header read: session id and start time from the first line only.
pub fn read_meta(path: &Path) -> Option<(String, SystemTime)> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(f).read_line(&mut first).ok()?;
    let v: Value = serde_json::from_str(&first).ok()?;
    v.get("projectHash")?;
    let id = v.get("sessionId").and_then(Value::as_str)?.to_string();
    let ts = v.get("startTime").and_then(Value::as_str).and_then(parse_rfc3339_utc)?;
    Some((id, ts))
}

/// Where a session's subagent transcripts live: a directory named after the
/// session id, next to the session file.
pub fn subagents_dir(transcript: &Path, session_id: &str) -> Option<PathBuf> {
    Some(transcript.parent()?.join(session_id))
}

/// Same path, as written. `.project_root` holds `path.resolve(cwd)`, and the
/// process table reports the cwd the kernel knows, which on macOS may be the
/// resolved form of a symlinked path; so both are canonicalised when they can
/// be and compared as given when they cannot.
fn same_dir(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

/// The conversation belonging to a Gemini CLI process: the newest session in
/// the process's working directory that started after the process did and
/// that no other process has claimed. One process runs one conversation; a
/// `/chat resume` keeps writing to the resumed file, which the mtime order
/// picks up.
pub(crate) fn attribute(
    cwd: Option<&Path>,
    proc_start: SystemTime,
    recent: &[Session],
    taken: &HashSet<PathBuf>,
) -> (Vec<PathBuf>, Attribution) {
    let Some(cwd) = cwd else { return (Vec::new(), Attribution::None) };
    let slack = Duration::from_secs(60);
    let mut mine: Vec<&Session> = recent
        .iter()
        .filter(|s| !taken.contains(&s.path))
        .filter(|s| s.cwd.as_deref().is_some_and(|c| same_dir(c, cwd)))
        .filter(|s| s.started.is_none_or(|ts| ts + slack >= proc_start))
        .collect();
    mine.sort_by_key(|s| std::cmp::Reverse(std::fs::metadata(&s.path).and_then(|m| m.modified()).ok()));
    match mine.first() {
        Some(s) => (vec![s.path.clone()], Attribution::CwdHeuristic),
        None => (Vec::new(), Attribution::None),
    }
}

/// The Gemini CLI adapter. See the module notes for the layout it reads.
#[derive(Default)]
pub struct GeminiAdapter {
    recent: Vec<Session>,
}

impl HarnessAdapter for GeminiAdapter {
    fn harness(&self) -> Harness {
        Harness::Gemini
    }

    fn rescan(&mut self, since: SystemTime) {
        self.recent = recent_sessions(since);
    }

    fn attribute(&self, _root: &ProcNode, _raw: Option<&RawProc>, ctx: &AttributeContext) -> (Vec<PathBuf>, Attribution) {
        attribute(ctx.cwd, ctx.proc_start, &self.recent, ctx.attached)
    }

    fn unowned(&self, attached: &HashSet<PathBuf>) -> Vec<PathBuf> {
        self.recent.iter().filter(|s| !attached.contains(&s.path)).map(|s| s.path.clone()).collect()
    }

    fn open(&self, path: &Path, spans: SpanRetention) -> Box<dyn SessionTracker> {
        Box::new(GeminiTranscript::new(path).with_spans(spans))
    }

    fn detect(&self, path: &Path) -> bool {
        read_meta(path).is_some()
    }

    fn transcripts(&self) -> Vec<(String, PathBuf)> {
        recent_sessions(SystemTime::UNIX_EPOCH)
            .into_iter()
            .map(|s| {
                let id = s.session_id.unwrap_or_else(|| s.path.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default());
                (id, s.path)
            })
            .collect()
    }
}

/// What one `gemini` message added to the summary, so a later line for the
/// same id can replace it.
#[derive(Debug, Clone, Copy, Default)]
struct Contrib {
    usage: TokenUsage,
    cost: CostBreakdown,
    unpriced: u64,
    has_record: bool,
    empty_record: bool,
}

/// One JSONL file being tailed into a `SessionSummary`: the main
/// conversation, or one subagent's.
struct Parser {
    reader: TailReader,
    summary: SessionSummary,
    /// Whether this file is a subagent's, which marks its spans as sidechain.
    subagent: bool,
    /// Every `gemini` message seen, by id, with what it contributed. A message
    /// is appended again when its tool calls complete, and re-listed by a
    /// checkpoint, so this is what stops double counting.
    messages: BTreeMap<String, Contrib>,
    /// `user` message ids already seen, so a re-appended prompt is not a
    /// second prompt.
    prompts: HashSet<String>,
    /// Tool call ids already turned into spans.
    calls: HashSet<String>,
    inference: Option<String>,
    turn: Option<String>,
    inferences: u64,
    turns: u64,
    prev_ts: Option<SystemTime>,
}

impl Parser {
    fn new(path: impl Into<PathBuf>, spans: SpanRetention, subagent: bool) -> Self {
        Parser {
            reader: TailReader::new(path),
            summary: SessionSummary { harness: Some(Harness::Gemini), spans: spans.log(), ..Default::default() },
            subagent,
            messages: BTreeMap::new(),
            prompts: HashSet::new(),
            calls: HashSet::new(),
            inference: None,
            turn: None,
            inferences: 0,
            turns: 0,
            prev_ts: None,
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
        if let Some(set) = v.get("$set").filter(|s| s.is_object()) {
            self.ingest_metadata(set);
            if let Some(msgs) = set.get("messages").and_then(Value::as_array) {
                for m in msgs {
                    self.ingest_message(m, prices);
                }
            }
            return;
        }
        if v.get("$rewindTo").is_some() {
            // The conversation was cut back; the model owes nothing until the
            // next prompt.
            self.summary.activity = Activity::Waiting;
            return;
        }
        if v.get("projectHash").is_some() {
            self.ingest_metadata(&v);
            if let Some(msgs) = v.get("messages").and_then(Value::as_array) {
                for m in msgs {
                    self.ingest_message(m, prices);
                }
            }
            return;
        }
        if v.get("id").and_then(Value::as_str).is_some() {
            self.ingest_message(&v, prices);
        }
    }

    fn ingest_metadata(&mut self, m: &Value) {
        if let Some(id) = m.get("sessionId").and_then(Value::as_str) {
            self.summary.session_id = Some(id.to_string());
        }
        if let Some(ts) = m.get("startTime").and_then(Value::as_str).and_then(parse_rfc3339_utc) {
            self.summary.started_at = Some(self.summary.started_at.map_or(ts, |s| s.min(ts)));
            if self.summary.last_activity.is_none() {
                self.summary.last_activity = Some(ts);
            }
        }
        if m.get("kind").and_then(Value::as_str) == Some("subagent") {
            self.subagent = true;
        }
    }

    fn ingest_message(&mut self, m: &Value, prices: &Table) {
        let Some(id) = m.get("id").and_then(Value::as_str) else { return };
        let ts = m.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_utc);
        if let Some(ts) = ts {
            if self.summary.started_at.is_none_or(|s| ts < s) {
                self.summary.started_at = Some(ts);
            }
            if self.summary.last_activity.is_none_or(|l| ts > l) {
                self.summary.last_activity = Some(ts);
            }
        }
        match m.get("type").and_then(Value::as_str).unwrap_or("") {
            "user" => self.ingest_user(id, m, ts),
            "gemini" => self.ingest_gemini(id, m, ts, prices),
            _ => {}
        }
        if ts.is_some() {
            self.prev_ts = ts;
        }
    }

    /// A prompt or a tool result: in both cases the model owes a response.
    fn ingest_user(&mut self, id: &str, m: &Value, ts: Option<SystemTime>) {
        if !self.prompts.insert(id.to_string()) {
            return;
        }
        self.summary.activity = Activity::Working;
        let Some(ts) = ts else { return };
        if !is_tool_result(m) {
            self.begin_turn(ts);
        }
        self.begin_inference(ts);
    }

    fn ingest_gemini(&mut self, id: &str, m: &Value, ts: Option<SystemTime>, prices: &Table) {
        if let Some(model) = m.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            self.summary.model = Some(model.to_string());
        }
        let first_time = !self.messages.contains_key(id);
        if first_time {
            self.summary.health.billable_messages += 1;
            self.summary.turns += 1;
            if self.subagent {
                self.summary.subagent_turns += 1;
            }
            // The reply is written in one line, so the inference ends here
            // and the turn now reaches at least this far.
            self.summary.activity = Activity::Waiting;
            if let Some(ts) = ts {
                if let Some(inf) = self.inference.take() {
                    self.summary.spans.end_at(&inf, ts);
                }
                if let Some(turn) = self.turn.as_deref() {
                    self.summary.spans.end_at(turn, ts);
                }
            }
        }
        self.account(id, m, prices);
        self.ingest_tool_calls(m, ts);
    }

    /// Replace whatever this message contributed before with what it says now.
    fn account(&mut self, id: &str, m: &Value, prices: &Table) {
        let model = m.get("model").and_then(Value::as_str).map(str::to_string).or_else(|| self.summary.model.clone());
        let mut c = Contrib::default();
        if let Some(t) = m.get("tokens").filter(|t| t.is_object()) {
            c.has_record = true;
            c.usage = parse_tokens(t);
            c.empty_record = c.usage.total() == 0;
            match model.as_deref().and_then(|m| prices.lookup(m)) {
                Some(p) => c.cost = p.breakdown(&c.usage),
                None => c.unpriced = c.usage.total(),
            }
        }
        let s = &mut self.summary;
        if let Some(old) = self.messages.insert(id.to_string(), c) {
            s.usage.sub(&old.usage);
            s.cost_usd -= old.cost.total();
            s.cost_breakdown.sub(&old.cost);
            s.unpriced_tokens = s.unpriced_tokens.saturating_sub(old.unpriced);
            s.health.usage_records -= u64::from(old.has_record);
            s.health.empty_usage_records -= u64::from(old.empty_record);
        }
        s.usage.add(&c.usage);
        s.cost_usd += c.cost.total();
        s.cost_breakdown.add(&c.cost);
        s.unpriced_tokens += c.unpriced;
        s.health.usage_records += u64::from(c.has_record);
        s.health.empty_usage_records += u64::from(c.empty_record);
    }

    /// Completed tool calls, appended to the message that issued them. Each
    /// becomes a closed span from the message to the call's own timestamp,
    /// and the tool results are about to be submitted, so the model is owed
    /// a response again.
    fn ingest_tool_calls(&mut self, m: &Value, msg_ts: Option<SystemTime>) {
        let Some(calls) = m.get("toolCalls").and_then(Value::as_array) else { return };
        for c in calls {
            let Some(id) = c.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) else { continue };
            if !self.calls.insert(id.to_string()) {
                continue;
            }
            self.summary.tool_calls += 1;
            self.summary.activity = Activity::Working;
            let name = c.get("name").and_then(Value::as_str).unwrap_or("tool");
            if name == "google_web_search" {
                self.summary.web_searches += 1;
            }
            let ended = c.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_utc);
            let Some(started) = msg_ts.or(ended) else { continue };
            let ended = ended.unwrap_or(started);
            let error = c.get("status").and_then(Value::as_str) == Some("error");
            self.summary.spans.open(id.to_string(), name.to_string(), started.min(ended), self.subagent);
            self.summary.spans.close(id, ended, error);
            if self.summary.last_activity.is_none_or(|l| ended > l) {
                self.summary.last_activity = Some(ended);
            }
        }
    }

    /// A prompt starts a turn. One the model never answered is ended where
    /// the last line before this prompt was written.
    fn begin_turn(&mut self, ts: SystemTime) {
        if let Some(id) = self.turn.take()
            && self.summary.spans.open_of_kind(SpanKind::Turn).is_some_and(|s| s.id == id)
        {
            let ended = self.prev_ts.unwrap_or(ts).min(ts);
            self.summary.spans.end_at(&id, ended);
        }
        self.turns += 1;
        let id = format!("turn:{}", self.turns);
        self.summary.spans.open_kind(id.clone(), "turn".into(), ts, self.subagent, SpanKind::Turn);
        self.turn = Some(id);
    }

    /// A submission that got no reply before the next one was not an
    /// inference and is dropped.
    fn begin_inference(&mut self, ts: SystemTime) {
        if let Some(id) = self.inference.take() {
            self.summary.spans.discard_open(&id);
        }
        self.inferences += 1;
        let id = format!("inference:{}", self.inferences);
        self.summary.spans.open_kind(id.clone(), "inference".into(), ts, self.subagent, SpanKind::Inference);
        self.inference = Some(id);
    }
}

/// A `user` message answering tool calls carries `functionResponse` parts.
/// Only the keys are inspected.
fn is_tool_result(m: &Value) -> bool {
    m.get("content").and_then(Value::as_array).is_some_and(|parts| parts.iter().any(|p| p.get("functionResponse").is_some()))
}

/// Gemini's usage, folded the way Google bills it: thoughts are output,
/// tool-use prompt tokens are input, and `input` already includes the cached
/// part, which is priced separately.
fn parse_tokens(t: &Value) -> TokenUsage {
    let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cached = g("cached");
    TokenUsage {
        input: g("input").saturating_sub(cached) + g("tool"),
        cache_read: cached,
        output: g("output") + g("thoughts"),
        ..Default::default()
    }
}

/// A Gemini CLI session: the main conversation plus every subagent file
/// under `chats/<sessionId>/`, folded into one summary. A subagent may run a
/// different model from its parent; each message is priced by the model it
/// names.
pub struct GeminiTranscript {
    main: Parser,
    subagents: BTreeMap<PathBuf, Parser>,
    prices: &'static Table,
    retention: SpanRetention,
    summary: SessionSummary,
}

impl GeminiTranscript {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let retention = SpanRetention::Recent;
        let path = path.into();
        let mut main = Parser::new(&path, retention, false);
        // The transcript never names its working directory; the project
        // directory two levels up does, in `.project_root`.
        main.summary.cwd = path.parent().and_then(Path::parent).and_then(project_root);
        GeminiTranscript {
            main,
            subagents: BTreeMap::new(),
            prices: pricing::table(),
            retention,
            summary: SessionSummary { harness: Some(Harness::Gemini), ..Default::default() },
        }
    }

    /// See `ClaudeTranscript::with_prices`.
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

    /// Pick up subagent files that appeared since the last look: one
    /// directory listing per refresh, and for most sessions a single failed
    /// `open`.
    fn discover_subagents(&mut self) {
        let Some(id) = self.main.summary.session_id.as_deref() else { return };
        let Some(dir) = subagents_dir(self.main.reader.path(), id) else { return };
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") || self.subagents.contains_key(&p) {
                continue;
            }
            let parser = Parser::new(&p, self.retention, true);
            self.subagents.insert(p, parser);
        }
    }

    fn fold(&mut self) {
        let mut s = self.main.summary.clone();
        for c in self.subagents.values() {
            let t = &c.summary;
            s.usage.add(&t.usage);
            s.cost_usd += t.cost_usd;
            s.cost_breakdown.add(&t.cost_breakdown);
            s.unpriced_tokens += t.unpriced_tokens;
            s.turns += t.turns;
            s.subagent_turns += t.subagent_turns;
            s.tool_calls += t.tool_calls;
            s.web_searches += t.web_searches;
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

impl SessionTracker for GeminiTranscript {
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-top-gemini-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const META: &str = r#"{"sessionId":"0a1b2c3d-0000-4000-8000-000000000001","projectHash":"604d","startTime":"2026-09-05T09:00:00.000Z","lastUpdated":"2026-09-05T09:00:00.000Z","kind":"main"}"#;

    #[test]
    fn folds_tokens_the_way_google_bills_them_and_dedupes_by_id() {
        let dir = scratch("tokens");
        let path = dir.join("session-2026-09-05T09-00-0a1b2c3d.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{META}").unwrap();
        writeln!(f, r#"{{"id":"u1","timestamp":"2026-09-05T09:00:02.000Z","type":"user","content":[{{"text":"p"}}]}}"#).unwrap();
        writeln!(f, r#"{{"$set":{{"lastUpdated":"2026-09-05T09:00:02.000Z"}}}}"#).unwrap();
        let g1 = r#"{"id":"g1","timestamp":"2026-09-05T09:00:05.000Z","type":"gemini","content":"","tokens":{"input":12000,"output":40,"cached":9000,"thoughts":300,"tool":0,"total":12340},"model":"gemini-2.5-pro"}"#;
        writeln!(f, "{g1}").unwrap();
        // The same message again, now carrying its completed tool call.
        let with_call = g1.replace(
            r#""model":"gemini-2.5-pro"}"#,
            r#""model":"gemini-2.5-pro","toolCalls":[{"id":"call-1","name":"read_file","status":"success","timestamp":"2026-09-05T09:00:09.000Z"}]}"#,
        );
        writeln!(f, "{with_call}").unwrap();
        let mut t = GeminiTranscript::new(&path).with_prices(pricing::builtin_table());
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.session_id.as_deref(), Some("0a1b2c3d-0000-4000-8000-000000000001"));
        assert_eq!(s.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(s.turns, 1, "one reply, appended twice");
        assert_eq!(s.usage.input, 3000, "prompt tokens minus the cached part");
        assert_eq!(s.usage.cache_read, 9000);
        assert_eq!(s.usage.output, 340, "thoughts are billed as output");
        assert_eq!(s.usage.total(), 12340, "and the fold adds back up to Gemini's total");
        // gemini-2.5-pro: 3000*1.25 + 9000*0.125 + 340*10 = 3750 + 1125 + 3400 micro-dollars
        assert!((s.cost_usd - 0.008275).abs() < 1e-9, "{}", s.cost_usd);
        assert_eq!(s.unpriced_tokens, 0);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.activity, Activity::Working, "a completed call means results are about to be submitted");
        let tools: Vec<_> = s.spans.iter().filter(|sp| sp.kind == SpanKind::Tool).collect();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[0].duration_ms, Some(4_000), "from the message that issued it to its completion");
        assert_eq!(read_meta(&path).unwrap().0, "0a1b2c3d-0000-4000-8000-000000000001");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconstructs_turns_inferences_and_counts_searches() {
        let dir = scratch("turns");
        let path = dir.join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{META}").unwrap();
        writeln!(f, r#"{{"id":"u1","timestamp":"2026-09-05T09:00:00.000Z","type":"user","content":[{{"text":"p"}}]}}"#).unwrap();
        writeln!(f, r#"{{"id":"g1","timestamp":"2026-09-05T09:00:03.000Z","type":"gemini","content":"","tokens":{{"input":10,"output":1,"cached":0,"thoughts":0,"tool":0,"total":11}},"model":"gemini-2.5-flash"}}"#).unwrap();
        writeln!(f, r#"{{"id":"g1","timestamp":"2026-09-05T09:00:03.000Z","type":"gemini","content":"","tokens":{{"input":10,"output":1,"cached":0,"thoughts":0,"tool":0,"total":11}},"model":"gemini-2.5-flash","toolCalls":[{{"id":"c1","name":"google_web_search","status":"success","timestamp":"2026-09-05T09:00:06.000Z"}},{{"id":"c2","name":"run_shell_command","status":"error","timestamp":"2026-09-05T09:00:10.000Z"}}]}}"#).unwrap();
        writeln!(f, r#"{{"id":"u2","timestamp":"2026-09-05T09:00:10.100Z","type":"user","content":[{{"functionResponse":{{"id":"c1"}}}},{{"functionResponse":{{"id":"c2"}}}}]}}"#).unwrap();
        writeln!(f, r#"{{"id":"g2","timestamp":"2026-09-05T09:00:15.000Z","type":"gemini","content":"done","tokens":{{"input":20,"output":5,"cached":10,"thoughts":0,"tool":0,"total":25}},"model":"gemini-2.5-flash"}}"#).unwrap();
        let mut t = GeminiTranscript::new(&path).with_prices(pricing::builtin_table());
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.turns, 2);
        assert_eq!(s.tool_calls, 2);
        assert_eq!(s.web_searches, 1);
        assert_eq!(s.activity, Activity::Waiting);
        let all = s.spans.to_vec();
        let tools: Vec<_> = all.iter().filter(|sp| sp.kind == SpanKind::Tool).collect();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].duration_ms, Some(3_000));
        assert!(!tools[0].error);
        assert_eq!(tools[1].duration_ms, Some(7_000));
        assert!(tools[1].error);
        let inf: Vec<_> = all.iter().filter(|sp| sp.kind == SpanKind::Inference).collect();
        assert_eq!(inf.len(), 2);
        assert_eq!(inf[0].duration_ms, Some(3_000), "prompt at :00, reply at :03");
        assert_eq!(inf[1].duration_ms, Some(4_900), "tool results at :10.1, reply at :15");
        let turns: Vec<_> = all.iter().filter(|sp| sp.kind == SpanKind::Turn).collect();
        assert_eq!(turns.len(), 1, "one prompt; the tool results are not a new turn");
        assert_eq!(turns[0].duration_ms, Some(15_000), "the turn reaches the last reply");
        assert_eq!(s.health.billable_messages, 2);
        assert_eq!(s.health.usage_records, 2);
        assert!(!s.health.fields_unrecognised());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folds_subagent_files_named_after_the_parent() {
        let dir = scratch("sub");
        let path = dir.join("session-2026-09-05T09-00-0a1b2c3d.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{META}").unwrap();
        writeln!(f, r#"{{"id":"u1","timestamp":"2026-09-05T09:00:00.000Z","type":"user","content":[{{"text":"p"}}]}}"#).unwrap();
        writeln!(f, r#"{{"id":"g1","timestamp":"2026-09-05T09:00:03.000Z","type":"gemini","content":"","tokens":{{"input":100,"output":10,"cached":0,"thoughts":0,"tool":0,"total":110}},"model":"gemini-2.5-pro"}}"#).unwrap();
        let mut t = GeminiTranscript::new(&path).with_prices(pricing::builtin_table());
        t.refresh().unwrap();
        assert_eq!(t.summary().usage.total(), 110);
        assert_eq!(t.summary().subagent_turns, 0);

        let sub = subagents_dir(&path, "0a1b2c3d-0000-4000-8000-000000000001").unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        let mut g = std::fs::File::create(sub.join("b2c3.jsonl")).unwrap();
        writeln!(g, r#"{{"sessionId":"b2c3","projectHash":"604d","startTime":"2026-09-05T09:00:04.000Z","lastUpdated":"2026-09-05T09:00:04.000Z","kind":"subagent"}}"#).unwrap();
        writeln!(g, r#"{{"id":"su1","timestamp":"2026-09-05T09:00:04.000Z","type":"user","content":[{{"text":"t"}}]}}"#).unwrap();
        writeln!(g, r#"{{"id":"sg1","timestamp":"2026-09-05T09:00:08.000Z","type":"gemini","content":"","tokens":{{"input":50,"output":5,"cached":0,"thoughts":0,"tool":0,"total":55}},"model":"gemini-2.5-flash","toolCalls":[{{"id":"sc1","name":"grep_search","status":"success","timestamp":"2026-09-05T09:00:09.000Z"}}]}}"#).unwrap();
        t.refresh().unwrap();
        let s = t.summary();
        assert_eq!(s.usage.total(), 165);
        assert_eq!(s.turns, 2);
        assert_eq!(s.subagent_turns, 1);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.model.as_deref(), Some("gemini-2.5-pro"), "the parent's model, not the subagent's");
        let sidechain: Vec<_> = s.spans.iter().filter(|sp| sp.sidechain).collect();
        assert_eq!(sidechain.len(), 3, "the subagent's turn, inference and tool call");
        // 100*1.25 + 10*10 (pro) + 50*0.30 + 5*2.5 (flash) = 125 + 100 + 15 + 12.5 micro-dollars
        assert!((s.cost_usd - 0.0002525).abs() < 1e-9, "{}", s.cost_usd);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rewind_and_a_checkpoint_do_not_change_what_was_spent() {
        let dir = scratch("rewind");
        let path = dir.join("session.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{META}").unwrap();
        let g1 = r#"{"id":"g1","timestamp":"2026-09-05T09:00:03.000Z","type":"gemini","content":"","tokens":{"input":100,"output":10,"cached":0,"thoughts":0,"tool":0,"total":110},"model":"gemini-2.5-pro"}"#;
        writeln!(f, "{g1}").unwrap();
        writeln!(f, r#"{{"$rewindTo":"g1"}}"#).unwrap();
        writeln!(f, r#"{{"$set":{{"messages":[{g1}]}}}}"#).unwrap();
        let mut t = GeminiTranscript::new(&path).with_prices(pricing::builtin_table());
        t.refresh().unwrap();
        assert_eq!(t.summary().usage.total(), 110);
        assert_eq!(t.summary().turns, 1);
        assert_eq!(t.summary().activity, Activity::Waiting);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_sessions_and_attributes_them_by_working_directory() {
        let root = scratch("tmp");
        let one = root.join("example");
        let chats = one.join("chats");
        std::fs::create_dir_all(chats.join("0a1b2c3d-0000-4000-8000-000000000001")).unwrap();
        std::fs::write(one.join(".project_root"), "/Users/dev/code/example\n").unwrap();
        let main = chats.join("session-2026-09-05T09-00-0a1b2c3d.jsonl");
        std::fs::write(&main, format!("{META}\n")).unwrap();
        // A subagent file, one level down: not a session of its own.
        std::fs::write(chats.join("0a1b2c3d-0000-4000-8000-000000000001").join("b2c3.jsonl"), format!("{META}\n")).unwrap();
        // A legacy hashed directory with no marker and a legacy .json file.
        let legacy = root.join("5b2dd62b9d0bddd4");
        std::fs::create_dir_all(legacy.join("chats")).unwrap();
        std::fs::write(legacy.join("chats").join("session-2026-01-01T00-00-abcd1234.json"), "{}").unwrap();

        let mut t = GeminiTranscript::new(&main);
        t.refresh().unwrap();
        assert_eq!(t.summary().cwd.as_deref(), Some(Path::new("/Users/dev/code/example")), "from the marker two levels up");

        let found = sessions_under(&root, SystemTime::UNIX_EPOCH);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, main);
        assert_eq!(found[0].cwd.as_deref(), Some(Path::new("/Users/dev/code/example")));
        assert_eq!(found[0].session_id.as_deref(), Some("0a1b2c3d-0000-4000-8000-000000000001"));
        let started = found[0].started.unwrap();

        let (paths, a) = attribute(Some(Path::new("/Users/dev/code/example")), started - Duration::from_secs(5), &found, &HashSet::new());
        assert_eq!(paths, vec![main.clone()]);
        assert_eq!(a, Attribution::CwdHeuristic, "labelled as the guess it is");
        // Another directory, or a process that started after the session, gets nothing.
        assert!(attribute(Some(Path::new("/Users/dev/code/other")), started, &found, &HashSet::new()).0.is_empty());
        assert!(
            attribute(Some(Path::new("/Users/dev/code/example")), started + Duration::from_secs(120), &found, &HashSet::new()).0.is_empty()
        );
        // Nor does a process claim a session another one already has.
        let taken: HashSet<PathBuf> = [main.clone()].into_iter().collect();
        assert_eq!(attribute(Some(Path::new("/Users/dev/code/example")), started, &found, &taken).1, Attribution::None);
        assert_eq!(attribute(None, started, &found, &HashSet::new()).1, Attribution::None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
