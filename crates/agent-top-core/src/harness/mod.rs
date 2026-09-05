//! Per-harness transcript readers.
//!
//! Each harness writes a different append-only log. A `SessionTracker` turns
//! one of those logs into the harness-neutral `SessionSummary` incrementally.

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;

use crate::model::{Activity, Attribution, CostBreakdown, Harness, ProcNode, SpanKind, TokenUsage, ToolSpan};
use crate::process::RawProc;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Evidence that the parser still understands the file it is reading.
///
/// Every field is read with a fallback to zero, which is the right behaviour
/// for a genuinely absent field and the wrong behaviour for a renamed one: a
/// harness that renames `usage` next week would show a user 0 tokens and $0.00
/// with no error at all. So count the usage records seen and how many of them
/// yielded nothing. Records present and all of them empty is not a quiet
/// session, it is a parser that has fallen behind the format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseHealth {
    /// Model responses seen. Each of these should account for some tokens.
    pub billable_messages: u64,
    /// Usage records found on them. Zero of these, with messages present, means
    /// the record itself moved or was renamed.
    pub usage_records: u64,
    /// Records found but yielding nothing, which is what a renamed field inside
    /// an intact record looks like.
    pub empty_usage_records: u64,
}

impl ParseHealth {
    /// Enough responses to accuse the parser rather than the session. A couple
    /// of odd messages must not raise the alarm.
    const MIN_EVIDENCE: u64 = 3;

    /// The session did work that must have cost tokens, and we read none.
    ///
    /// Covers both ways a format change reaches us: the usage record moving or
    /// being renamed, so we never find one, and the fields inside it being
    /// renamed, so we find records that read as empty. Neither raises an error
    /// on its own, because every field falls back to zero.
    pub fn fields_unrecognised(&self) -> bool {
        self.billable_messages >= Self::MIN_EVIDENCE && self.usage_records == self.empty_usage_records
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionSummary {
    pub harness: Option<Harness>,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub harness_version: Option<String>,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    /// `cost_usd` by kind of token. See `Agent::cost_breakdown`.
    pub cost_breakdown: CostBreakdown,
    pub unpriced_tokens: u64,
    pub turns: u64,
    pub subagent_turns: u64,
    pub tool_calls: u64,
    /// See `Agent::web_searches`.
    pub web_searches: u64,
    pub spans: SpanLog,
    /// Calls to each MCP server, by the server's name.
    pub mcp: BTreeMap<String, McpUsage>,
    pub health: ParseHealth,
    pub activity: Activity,
    pub started_at: Option<SystemTime>,
    pub last_activity: Option<SystemTime>,
    /// How close the session is to its rate limit, when the harness writes it.
    pub rate_limit: Option<crate::model::RateLimit>,
}

/// What a transcript says about one MCP server: how often it was called,
/// how often that failed, and when it was last called.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpUsage {
    pub calls: u64,
    pub errors: u64,
    pub last_call: Option<SystemTime>,
}

impl McpUsage {
    pub fn add(&mut self, o: &McpUsage) {
        self.calls += o.calls;
        self.errors += o.errors;
        self.last_call = self.last_call.max(o.last_call);
    }
}

/// The server behind an MCP tool name. Claude Code names them
/// `mcp__<server>__<tool>`; the server part may itself contain underscores,
/// so the split is on the first double underscore after the prefix and the
/// tool part is whatever follows the last one.
pub fn mcp_server_of(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let server = match rest.rfind("__") {
        Some(i) => &rest[..i],
        None => rest,
    };
    if server.is_empty() { None } else { Some(server) }
}

/// Spans kept per session by the live tracker. A screenful of waterfall is
/// a few dozen rows; the rest is history nobody scrolls to in a live view, and
/// every span costs a clone on each refresh. An export wants the whole session
/// and uses `SpanLog::unbounded` in a separate pass; see `SpanRetention`.
///
/// Sized for roughly a hundred tool calls: each model response also adds an
/// inference span and each human prompt a turn span.
pub const MAX_SPANS: usize = 256;

/// A bounded, in-order log of tool spans, built by pairing a harness's
/// "call started" and "call finished" records by call id.
///
/// Records arrive interleaved and out of order (agents run tools in parallel),
/// so a span is closed by searching back for the still-open span with that id
/// rather than assuming the most recent one.
#[derive(Debug, Clone)]
pub struct SpanLog {
    spans: VecDeque<ToolSpan>,
    cap: usize,
}

impl Default for SpanLog {
    fn default() -> Self {
        SpanLog { spans: VecDeque::new(), cap: MAX_SPANS }
    }
}

impl SpanLog {
    /// A log that keeps every span. For a one-shot pass over a whole
    /// transcript, never for the live tracker, where the memory and the clone
    /// per refresh would grow with the session.
    pub fn unbounded() -> Self {
        SpanLog { spans: VecDeque::new(), cap: usize::MAX }
    }

    /// Record the start of a tool call. Ignored when that id is already open,
    /// so a transcript line replayed by the harness does not double-count.
    pub fn open(&mut self, id: String, name: String, at: SystemTime, sidechain: bool) {
        self.open_kind(id, name, at, sidechain, SpanKind::Tool);
    }

    /// `open`, for any kind of span.
    pub fn open_kind(&mut self, id: String, name: String, at: SystemTime, sidechain: bool, kind: SpanKind) {
        if id.is_empty() || self.spans.iter().any(|s| s.is_open() && s.id == id) {
            return;
        }
        if self.spans.len() >= self.cap {
            self.spans.pop_front();
        }
        self.spans.push_back(ToolSpan { id, name, started_at: at, duration_ms: None, sidechain, error: false, kind });
    }

    /// Move the end of the newest span with this id to `at`, open or not. An
    /// inference span grows as the response streams in, one content block
    /// per line, and its end is wherever the last block landed.
    pub fn end_at(&mut self, id: &str, at: SystemTime) {
        let Some(s) = self.spans.iter_mut().rev().find(|s| s.id == id) else { return };
        s.duration_ms = Some(at.duration_since(s.started_at).map(|d| d.as_millis() as u64).unwrap_or(0));
    }

    /// The newest open span of this kind, if any.
    pub fn open_of_kind(&self, kind: SpanKind) -> Option<&ToolSpan> {
        self.spans.iter().rev().find(|s| s.is_open() && s.kind == kind)
    }

    /// Remove the newest span with this id if it is still open. For a span
    /// that turned out not to be one: an inference that never produced a
    /// reply because the user interrupted or submitted again.
    pub fn discard_open(&mut self, id: &str) {
        if let Some(i) = self.spans.iter().rposition(|s| s.is_open() && s.id == id) {
            self.spans.remove(i);
        }
    }

    /// Close the open call with this id. A result whose call scrolled out of
    /// the window, or that we never saw start, is dropped.
    pub fn close(&mut self, id: &str, at: SystemTime, error: bool) {
        let Some(s) = self.spans.iter_mut().rev().find(|s| s.is_open() && s.id == id) else { return };
        s.duration_ms = Some(at.duration_since(s.started_at).map(|d| d.as_millis() as u64).unwrap_or(0));
        s.error = error;
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Oldest first. Double-ended so callers can take the newest spans without
    /// collecting the whole log first, which the UI and the golden tests both do.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ToolSpan> + ExactSizeIterator {
        self.spans.iter()
    }

    pub fn to_vec(&self) -> Vec<ToolSpan> {
        self.spans.iter().cloned().collect()
    }

    /// One log from several, ordered by start time, keeping the newest `cap`.
    /// A parent's spans and its subagents' spans interleave in wall-clock
    /// order, which is what a waterfall wants.
    pub fn merged<'a>(logs: impl IntoIterator<Item = &'a SpanLog>, cap: usize) -> SpanLog {
        let mut spans: Vec<ToolSpan> = logs.into_iter().flat_map(|l| l.spans.iter().cloned()).collect();
        spans.sort_by_key(|s| s.started_at);
        if spans.len() > cap {
            spans.drain(..spans.len() - cap);
        }
        SpanLog { spans: spans.into(), cap }
    }

    pub fn cap(&self) -> usize {
        self.cap
    }
}

/// How many of a session's tool spans a tracker keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanRetention {
    /// The newest `MAX_SPANS`, enough for the live waterfall. The default.
    #[default]
    Recent,
    /// Every span in the transcript, for a trace export. Memory grows with
    /// the session, so this is for a single pass, not a tracker kept across
    /// refreshes.
    All,
}

impl SpanRetention {
    pub(crate) fn log(self) -> SpanLog {
        match self {
            SpanRetention::Recent => SpanLog::default(),
            SpanRetention::All => SpanLog::unbounded(),
        }
    }
}

pub trait SessionTracker {
    /// Ingest whatever was appended since the last call. Returns true when
    /// there is still unread data (the byte budget was exhausted).
    fn refresh(&mut self) -> anyhow::Result<bool>;
    fn summary(&self) -> &SessionSummary;
    fn path(&self) -> &Path;

    /// Ingest the whole file, however many refreshes that takes. For a
    /// one-shot read such as an export; the live collector spreads a large
    /// transcript over several ticks instead.
    fn refresh_all(&mut self) -> anyhow::Result<()> {
        while self.refresh()? {}
        Ok(())
    }
}

/// What a harness's own registry says about one of its processes, when it
/// keeps one (Claude Code's `~/.claude/sessions/<pid>.json`). Every field is
/// optional; a harness with no registry returns none of this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryHints {
    pub name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub version: Option<String>,
    /// The harness's own word for its state (`busy`, `idle`, ...), which beats
    /// any transcript heuristic.
    pub status: Option<String>,
}

/// What the collector knows about a process when it asks an adapter which
/// transcript is the process's.
pub struct AttributeContext<'a> {
    pub cwd: Option<&'a Path>,
    pub proc_start: SystemTime,
    pub now: SystemTime,
    /// Transcripts already given to another process this pass. An adapter
    /// must not hand one out twice.
    pub attached: &'a HashSet<PathBuf>,
    /// A transcript idle for longer than this is a finished conversation, not
    /// a thread of a process that cannot otherwise be matched.
    pub activity_timeout: Duration,
}

/// One harness, as the collector sees it: where its transcripts are, which
/// belongs to which process, and how to read one. The collector holds a list
/// of these and never names a harness itself, so adding a harness is one
/// module and one line in `adapters()`. Process recognition stays in
/// `process::classify_agent`, which also knows the harnesses that have no
/// transcript adapter yet.
pub trait HarnessAdapter {
    fn harness(&self) -> Harness;

    /// Re-list the transcripts written since `since`. Called every
    /// `fs_scan_interval`, not every tick.
    fn rescan(&mut self, since: SystemTime);

    /// Called once per pass with this harness's root processes, before any
    /// of them is attributed. For work that must see every process at once:
    /// Codex reads which rollouts each process holds open here, so that no
    /// process's fallback can claim a thread another is demonstrably writing.
    fn prepare(&mut self, _roots: &[&ProcNode]) {}

    /// The harness's own registry entry for a process, if it keeps one.
    fn hints(&self, _pid: u32) -> Option<RegistryHints> {
        None
    }

    /// The transcripts this process is writing, newest activity first, and
    /// how sure the adapter is. One per conversation: a Codex app-server hosts
    /// many, a CLI runs one, a process with none gets an empty list.
    fn attribute(&self, root: &ProcNode, raw: Option<&RawProc>, ctx: &AttributeContext) -> (Vec<PathBuf>, Attribution);

    /// Recently written transcripts no process owns: the stopped list.
    fn unowned(&self, attached: &HashSet<PathBuf>) -> Vec<PathBuf>;

    /// A tracker for one transcript.
    fn open(&self, path: &Path, spans: SpanRetention) -> Box<dyn SessionTracker>;

    /// Whether this harness wrote the file, judged from its first few lines.
    fn detect(&self, path: &Path) -> bool;

    /// Every transcript on disk, however old, with the id a user would type
    /// to name it. For `agent-top trace --session <id>`.
    fn transcripts(&self) -> Vec<(String, PathBuf)>;
}

/// Every harness that has a transcript adapter, in the order they are asked.
/// The order matters to `detect` alone: Gemini's metadata line carries a
/// `sessionId` like Claude Code's lines do, so it is asked first.
pub fn adapters() -> Vec<Box<dyn HarnessAdapter>> {
    vec![
        Box::new(codex::CodexAdapter::default()),
        Box::new(gemini::GeminiAdapter::default()),
        Box::new(opencode::OpenCodeAdapter::default()),
        Box::new(claude::ClaudeAdapter::default()),
    ]
}

/// The adapter for one harness, or none when it has only a process table entry.
pub fn adapter_for(harness: Harness) -> Option<Box<dyn HarnessAdapter>> {
    adapters().into_iter().find(|a| a.harness() == harness)
}

/// Which harness wrote a transcript, judged from its first few lines. Anything
/// no adapter recognises is not a transcript agent-top reads.
pub fn detect(path: &Path) -> Option<Harness> {
    adapters().iter().find(|a| a.detect(path)).map(|a| a.harness())
}

/// A tracker for a transcript whose harness is already known, or none when
/// that harness has no transcript adapter.
pub fn open_transcript(path: &Path, harness: Harness, spans: SpanRetention) -> Option<Box<dyn SessionTracker>> {
    adapter_for(harness).map(|a| a.open(path, spans))
}

/// The first few lines of a file, parsed, for `HarnessAdapter::detect`.
pub(crate) fn head_lines(path: &Path) -> Vec<serde_json::Value> {
    use std::io::{BufRead, BufReader};
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    BufReader::new(f).lines().map_while(Result::ok).take(5).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

/// Bytes ingested per tracker per refresh. Keeps a cold start on a 100 MB
/// transcript from freezing the first frame; the rest streams in on later ticks.
pub const REFRESH_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Parse an RFC 3339 timestamp like `2026-09-03T07:15:34.123Z` into SystemTime
/// without pulling in a date crate. Only the UTC `Z` form is handled, which is
/// what both Claude Code and Codex write.
pub fn parse_rfc3339_utc(s: &str) -> Option<SystemTime> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?.parse::<i64>().ok()?, d.next()?.parse::<u32>().ok()?, d.next()?.parse::<u32>().ok()?);
    let mut t = time.split(':');
    let (h, mi) = (t.next()?.parse::<u64>().ok()?, t.next()?.parse::<u64>().ok()?);
    let sec_str = t.next()?;
    let (sec, frac) = match sec_str.split_once('.') {
        Some((s, f)) => (s.parse::<u64>().ok()?, f),
        None => (sec_str.parse::<u64>().ok()?, ""),
    };
    let nanos: u32 = if frac.is_empty() {
        0
    } else {
        let mut f = frac.to_string();
        f.truncate(9);
        while f.len() < 9 {
            f.push('0');
        }
        f.parse().ok()?
    };
    let days = days_from_civil(y, mo, da);
    let secs = days * 86_400 + (h * 3600 + mi * 60 + sec) as i64;
    if secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos))
}

// Howard Hinnant's days-from-civil algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn pairs_spans_by_id_out_of_order() {
        let mut log = SpanLog::default();
        log.open("a".into(), "Bash".into(), at(10), false);
        log.open("b".into(), "Read".into(), at(11), true);
        // Replay of the same start line must not open a second span.
        log.open("a".into(), "Bash".into(), at(10), false);
        // Results come back in the other order.
        log.close("b", at(12), false);
        log.close("a", at(14), true);
        // A result with no matching call is ignored.
        log.close("zzz", at(15), false);
        let v = log.to_vec();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "Bash");
        assert_eq!(v[0].duration_ms, Some(4_000));
        assert!(v[0].error);
        assert_eq!(v[1].duration_ms, Some(1_000));
        assert!(v[1].sidechain);
        assert!(!v[1].error);
    }

    #[test]
    fn keeps_the_newest_spans_and_reports_open_ones() {
        let mut log = SpanLog::default();
        for i in 0..(MAX_SPANS + 10) {
            log.open(format!("id{i}"), "T".into(), at(i as u64), false);
            log.close(&format!("id{i}"), at(i as u64), false);
        }
        assert_eq!(log.len(), MAX_SPANS);
        assert_eq!(log.iter().next().unwrap().id, "id10");
        log.open("live".into(), "Bash".into(), at(500), false);
        let last = log.to_vec().pop().unwrap();
        assert!(last.is_open());
        assert_eq!(last.elapsed_ms(at(503)), 3_000);
    }

    #[test]
    fn end_at_moves_the_end_of_any_kind_of_span() {
        let mut log = SpanLog::default();
        log.open_kind("inference:1".into(), "inference".into(), at(10), false, SpanKind::Inference);
        assert!(log.open_of_kind(SpanKind::Inference).is_some());
        assert!(log.open_of_kind(SpanKind::Turn).is_none());
        // The response streams in over three lines; the span ends at the last one.
        log.end_at("inference:1", at(11));
        log.end_at("inference:1", at(13));
        log.end_at("nope", at(99));
        let v = log.to_vec();
        assert_eq!(v[0].duration_ms, Some(3_000));
        assert_eq!(v[0].kind, SpanKind::Inference);
        assert!(log.open_of_kind(SpanKind::Inference).is_none());
        // Discarding only removes open spans; the ended one stays.
        log.discard_open("inference:1");
        assert_eq!(log.len(), 1);
        log.open_kind("inference:2".into(), "inference".into(), at(20), false, SpanKind::Inference);
        log.discard_open("inference:2");
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn unbounded_log_keeps_everything() {
        let mut log = SpanLog::unbounded();
        for i in 0..(MAX_SPANS * 3) {
            log.open(format!("id{i}"), "T".into(), at(i as u64), false);
            log.close(&format!("id{i}"), at(i as u64 + 1), false);
        }
        assert_eq!(log.len(), MAX_SPANS * 3);
        assert_eq!(log.iter().next().unwrap().id, "id0");
        assert_eq!(SpanRetention::default(), SpanRetention::Recent);
    }

    #[test]
    fn detects_the_harness_from_the_first_lines() {
        let dir = std::env::temp_dir().join(format!("agent-top-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let codex = dir.join("rollout.jsonl");
        std::fs::write(&codex, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n").unwrap();
        let claude = dir.join("s.jsonl");
        // A summary line first, as Claude Code writes on resume, then a real one.
        std::fs::write(&claude, "{\"type\":\"summary\",\"leafUuid\":\"u\"}\n{\"type\":\"user\",\"sessionId\":\"abc\"}\n").unwrap();
        let other = dir.join("other.jsonl");
        std::fs::write(&other, "{\"hello\":1}\nnot json\n").unwrap();
        assert_eq!(detect(&codex), Some(Harness::Codex));
        assert_eq!(detect(&claude), Some(Harness::Claude));
        assert_eq!(detect(&other), None);
        assert_eq!(detect(&dir.join("missing.jsonl")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_the_server_behind_an_mcp_tool() {
        assert_eq!(mcp_server_of("mcp__filesystem__read_file"), Some("filesystem"));
        assert_eq!(mcp_server_of("mcp__chrome-devtools__take_screenshot"), Some("chrome-devtools"));
        assert_eq!(mcp_server_of("mcp__claude_ai_Gmail__authenticate"), Some("claude_ai_Gmail"));
        assert_eq!(mcp_server_of("mcp__odd"), Some("odd"));
        assert_eq!(mcp_server_of("mcp____x"), None);
        assert_eq!(mcp_server_of("Bash"), None);
    }

    #[test]
    fn accuses_the_parser_only_with_enough_evidence() {
        // Healthy: records found on the messages, tokens read from them.
        let h = ParseHealth { billable_messages: 40, usage_records: 40, empty_usage_records: 0 };
        assert!(!h.fields_unrecognised());
        // One odd message among many is a message, not a format change.
        let h = ParseHealth { billable_messages: 40, usage_records: 40, empty_usage_records: 39 };
        assert!(!h.fields_unrecognised());
        // Fields inside the record renamed: records found, all of them empty.
        let h = ParseHealth { billable_messages: 40, usage_records: 40, empty_usage_records: 40 };
        assert!(h.fields_unrecognised());
        // The record itself renamed or moved: messages, but no records at all.
        // This is the case a naive check misses, because there is nothing to count.
        let h = ParseHealth { billable_messages: 40, usage_records: 0, empty_usage_records: 0 };
        assert!(h.fields_unrecognised());
        // Too early to tell: a session that has barely started.
        let h = ParseHealth { billable_messages: 2, usage_records: 0, empty_usage_records: 0 };
        assert!(!h.fields_unrecognised());
        // Nothing parsed at all is silence, not evidence.
        assert!(!ParseHealth::default().fields_unrecognised());
    }

    #[test]
    fn parses_timestamps() {
        let t = parse_rfc3339_utc("1970-01-02T00:00:00.000Z").unwrap();
        assert_eq!(t, SystemTime::UNIX_EPOCH + Duration::from_secs(86_400));
        let t = parse_rfc3339_utc("2026-09-03T07:15:34.5Z").unwrap();
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(secs.as_secs(), 1_788_419_734);
        assert_eq!(secs.subsec_millis(), 500);
        assert!(parse_rfc3339_utc("nope").is_none());
    }
}
