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

use super::{AttributeContext, HarnessAdapter, REFRESH_BUDGET_BYTES, SessionSummary, SessionTracker, SpanRetention, parse_rfc3339_utc};
use crate::jsonl::TailReader;
use crate::model::{Activity, Attribution, Harness, ProcNode, SpanKind, TokenUsage};
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

    fn prepare(&mut self, roots: &[&ProcNode]) {
        self.held = roots.iter().map(|r| (r.pid, rollouts_open_by(r.pid))).collect();
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
