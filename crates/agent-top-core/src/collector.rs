//! Joins the process table with the transcripts into a `Snapshot`.

use crate::harness::claude::{self, ClaudeTranscript, PidSession};
use crate::harness::codex::{self, CodexTranscript};
use crate::harness::{SessionSummary, SessionTracker};
use crate::model::*;
use crate::process::{ProcessScanner, RawProc, build_forest, session_id_from_args};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct CollectorOptions {
    /// How long after its last write a process-less transcript still shows as `stopped`.
    pub stopped_window: Duration,
    /// How often to re-list transcript directories.
    pub fs_scan_interval: Duration,
    /// A transcript idle for longer than this counts as idle even if the
    /// harness never wrote an end-of-turn marker.
    pub activity_timeout: Duration,
}

impl Default for CollectorOptions {
    fn default() -> Self {
        CollectorOptions {
            stopped_window: Duration::from_secs(30 * 60),
            fs_scan_interval: Duration::from_secs(5),
            activity_timeout: Duration::from_secs(15 * 60),
        }
    }
}

pub struct Collector {
    opts: CollectorOptions,
    scanner: ProcessScanner,
    trackers: HashMap<PathBuf, Box<dyn SessionTracker>>,
    last_fs_scan: Option<Instant>,
    recent_claude: Vec<PathBuf>,
    recent_codex: Vec<(PathBuf, PathBuf, SystemTime)>,
}

impl Collector {
    pub fn new(opts: CollectorOptions) -> Self {
        Collector {
            opts,
            scanner: ProcessScanner::new(),
            trackers: HashMap::new(),
            last_fs_scan: None,
            recent_claude: Vec::new(),
            recent_codex: Vec::new(),
        }
    }

    fn rescan_fs_if_due(&mut self) {
        let due = self.last_fs_scan.map(|t| t.elapsed() >= self.opts.fs_scan_interval).unwrap_or(true);
        if !due {
            return;
        }
        self.last_fs_scan = Some(Instant::now());
        let since = SystemTime::now().checked_sub(self.opts.stopped_window).unwrap_or(UNIX_EPOCH);
        self.recent_claude = claude::recent_transcripts(since);
        self.recent_codex =
            codex::recent_rollouts(since).into_iter().filter_map(|p| codex::read_meta(&p).map(|(cwd, ts)| (p, cwd, ts))).collect();
    }

    fn tracker_for(&mut self, path: &Path, harness: Harness) -> &mut Box<dyn SessionTracker> {
        self.trackers.entry(path.to_path_buf()).or_insert_with(|| match harness {
            Harness::Codex => Box::new(CodexTranscript::new(path)),
            _ => Box::new(ClaudeTranscript::new(path)),
        })
    }

    pub fn collect(&mut self) -> Snapshot {
        self.scanner.refresh();
        self.rescan_fs_if_due();
        let host = self.scanner.host();
        let procs = self.scanner.processes();
        let by_pid: HashMap<u32, &RawProc> = procs.iter().map(|p| (p.pid, p)).collect();
        let (roots, orphans) = build_forest(&procs);
        let registry: HashMap<u32, PidSession> = claude::read_pid_sessions().into_iter().map(|s| (s.pid, s)).collect();

        let now = SystemTime::now();
        let mut agents = Vec::new();
        let mut attached: HashSet<PathBuf> = HashSet::new();

        // Which rollouts each Codex process has open, gathered before any
        // attribution so that no process's fallback can claim a thread
        // another process is demonstrably writing.
        let held: HashMap<u32, Option<Vec<PathBuf>>> =
            roots.iter().filter(|r| r.harness == Some(Harness::Codex)).map(|r| (r.pid, codex::rollouts_open_by(r.pid))).collect();
        let all_held: HashSet<PathBuf> = held.values().flatten().flatten().cloned().collect();

        for root in roots {
            let raw = by_pid.get(&root.pid).copied();
            let harness = root.harness.unwrap_or(Harness::Unknown);
            let proc_start = raw.map(|p| UNIX_EPOCH + Duration::from_secs(p.start_time)).unwrap_or(now);
            let cwd = root.cwd.clone().or_else(|| registry.get(&root.pid).map(|r| r.cwd.clone()));

            // One process can host several conversations. Claude Code runs one
            // per process; the Codex app-server runs many.
            let (paths, attribution) = match harness {
                Harness::Claude => {
                    let (p, a) = attribute_claude(&root, raw, cwd.as_deref(), proc_start, &registry);
                    (p.into_iter().collect::<Vec<_>>(), a)
                }
                Harness::Codex => {
                    let mine: Option<Vec<PathBuf>> = held
                        .get(&root.pid)
                        .and_then(|h| h.as_ref())
                        .map(|h| h.iter().filter(|p| !attached.contains(*p)).cloned().collect());
                    let taken: HashSet<PathBuf> = attached.union(&all_held).cloned().collect();
                    attribute_codex(cwd.as_deref(), proc_start, mine.as_deref(), &self.recent_codex, &taken, now, &self.opts)
                }
                _ => (Vec::new(), Attribution::None),
            };

            let (cpu, rss, count, mcp) = root.totals();
            let reg = registry.get(&root.pid);

            // No transcript: the process still deserves a row.
            if paths.is_empty() {
                let summary = SessionSummary::default();
                let state = live_state(reg, summary.activity, None, cpu, &self.opts);
                agents.push(Agent {
                    id: format!("pid:{}", root.pid),
                    name: reg.and_then(|r| r.name.clone()).unwrap_or_else(|| display_name(harness, cwd.as_deref())),
                    harness,
                    state,
                    activity: summary.activity,
                    pid: Some(root.pid),
                    session_id: reg.map(|r| r.session_id.clone()),
                    session_path: None,
                    cwd,
                    model: None,
                    harness_version: reg.and_then(|r| r.version.clone()),
                    usage: summary.usage,
                    cost_usd: 0.0,
                    cost_breakdown: Default::default(),
                    price_source: None,
                    unpriced_tokens: 0,
                    turns: 0,
                    subagent_turns: 0,
                    tool_calls: 0,
                    web_searches: 0,
                    spans: Vec::new(),
                    age_secs: root.age_secs,
                    idle_secs: None,
                    cpu_percent: cpu,
                    rss_bytes: rss,
                    process_count: count,
                    mcp_count: mcp,
                    tree: Some(root),
                    attribution,
                    shares_process: false,
                    parse_warning: None,
                });
                continue;
            }

            for (i, path) in paths.iter().enumerate() {
                // Only the first row carries the process, so that a machine's
                // totals are not multiplied by the number of conversations.
                let owns_process = i == 0;
                let tr = self.tracker_for(path, harness);
                let _ = tr.refresh();
                let mut summary = tr.summary().clone();
                attached.insert(path.clone());

                if let Some(reg) = reg {
                    if summary.session_id.is_none() {
                        summary.session_id = Some(reg.session_id.clone());
                    }
                    if summary.harness_version.is_none() {
                        summary.harness_version = reg.version.clone();
                    }
                }

                let idle_secs = summary.last_activity.and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs());
                let state = live_state(reg, summary.activity, idle_secs, cpu, &self.opts);
                // A thread names itself after its own working directory, which
                // is the only thing distinguishing two rows on one app-server.
                let name = match (reg.and_then(|r| r.name.clone()), paths.len()) {
                    (Some(n), 1) => n,
                    _ => display_name(harness, summary.cwd.as_deref().or(cwd.as_deref())),
                };
                let id = match summary.session_id.as_deref() {
                    Some(sid) => format!("pid:{}:{}", root.pid, sid),
                    None => format!("pid:{}:{}", root.pid, path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
                };

                agents.push(Agent {
                    id,
                    name,
                    harness,
                    state,
                    activity: summary.activity,
                    pid: Some(root.pid),
                    session_id: summary.session_id.clone(),
                    session_path: Some(path.clone()),
                    cwd: summary.cwd.clone().or_else(|| cwd.clone()),
                    model: summary.model.clone(),
                    harness_version: summary.harness_version.clone(),
                    usage: summary.usage,
                    cost_usd: summary.cost_usd,
                    cost_breakdown: summary.cost_breakdown,
                    price_source: summary.model.as_deref().and_then(|m| crate::pricing::table().source_for(m)),
                    unpriced_tokens: summary.unpriced_tokens,
                    turns: summary.turns,
                    subagent_turns: summary.subagent_turns,
                    tool_calls: summary.tool_calls,
                    web_searches: summary.web_searches,
                    spans: summary.spans.to_vec(),
                    age_secs: root.age_secs,
                    idle_secs,
                    cpu_percent: if owns_process { cpu } else { 0.0 },
                    rss_bytes: if owns_process { rss } else { 0 },
                    process_count: if owns_process { count } else { 0 },
                    mcp_count: if owns_process { mcp } else { 0 },
                    tree: if owns_process { Some(root.clone()) } else { None },
                    attribution,
                    shares_process: !owns_process,
                    parse_warning: parse_warning(&summary, harness),
                });
            }
        }

        // Stopped agents: recently written transcripts nobody owns.
        let mut stopped: Vec<(PathBuf, Harness)> = Vec::new();
        for p in &self.recent_claude {
            if !attached.contains(p) && !is_subagent_transcript(p) {
                stopped.push((p.clone(), Harness::Claude));
            }
        }
        for (p, _, _) in &self.recent_codex {
            if !attached.contains(p) {
                stopped.push((p.clone(), Harness::Codex));
            }
        }
        for (p, harness) in stopped {
            let tr = self.tracker_for(&p, harness);
            let _ = tr.refresh();
            let s = tr.summary().clone();
            if s.turns == 0 && s.usage.total() == 0 {
                continue;
            }
            let idle_secs = s.last_activity.and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs());
            let id = s.session_id.clone().unwrap_or_else(|| p.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default());
            agents.push(Agent {
                id: format!("session:{id}"),
                name: display_name(harness, s.cwd.as_deref()),
                harness,
                state: AgentState::Stopped,
                activity: s.activity,
                pid: None,
                session_id: Some(id),
                session_path: Some(p),
                cwd: s.cwd.clone(),
                model: s.model.clone(),
                harness_version: s.harness_version.clone(),
                usage: s.usage,
                cost_usd: s.cost_usd,
                cost_breakdown: s.cost_breakdown,
                price_source: s.model.as_deref().and_then(|m| crate::pricing::table().source_for(m)),
                unpriced_tokens: s.unpriced_tokens,
                turns: s.turns,
                subagent_turns: s.subagent_turns,
                tool_calls: s.tool_calls,
                web_searches: s.web_searches,
                spans: s.spans.to_vec(),
                age_secs: idle_secs.unwrap_or(0),
                idle_secs,
                cpu_percent: 0.0,
                rss_bytes: 0,
                process_count: 0,
                mcp_count: 0,
                tree: None,
                attribution: Attribution::TranscriptOnly,
                shares_process: false,
                parse_warning: parse_warning(&s, harness),
            });
        }

        // Drop trackers for transcripts that fell out of the window.
        let keep: HashSet<&PathBuf> = agents.iter().filter_map(|a| a.session_path.as_ref()).collect();
        self.trackers.retain(|p, _| keep.contains(p));

        let mut snap =
            Snapshot { schema_version: SNAPSHOT_SCHEMA_VERSION, taken_at: now, host, agents, orphans, totals: Totals::default() };
        snap.compute_totals();
        snap
    }
}

/// A transcript that parsed while its usage records did not is a format change,
/// not a quiet session. Naming the harness version makes the report actionable:
/// it is the first thing anyone will ask for.
fn parse_warning(s: &SessionSummary, harness: Harness) -> Option<String> {
    if !s.health.fields_unrecognised() {
        return None;
    }
    let version = s.harness_version.as_deref().unwrap_or("unknown version");
    Some(format!(
        "usage fields not recognised in {} {}: tokens and cost are unreliable, agent-top may need updating",
        harness.label(),
        version
    ))
}

fn attribute_claude(
    root: &ProcNode,
    raw: Option<&RawProc>,
    cwd: Option<&Path>,
    proc_start: SystemTime,
    registry: &HashMap<u32, PidSession>,
) -> (Option<PathBuf>, Attribution) {
    if let Some(reg) = registry.get(&root.pid)
        && let Some(p) = claude::transcript_path(&reg.cwd, &reg.session_id)
    {
        return (Some(p), Attribution::HarnessRegistry);
    }
    if let (Some(raw), Some(cwd)) = (raw, cwd)
        && let Some(id) = session_id_from_args(&raw.cmd)
        && let Some(p) = claude::transcript_path(cwd, &id)
        && p.exists()
    {
        return (Some(p), Attribution::CommandLine);
    }
    if let Some(cwd) = cwd
        && let Some(p) = claude::guess_transcript(cwd, proc_start)
    {
        return (Some(p), Attribution::CwdHeuristic);
    }
    (None, Attribution::None)
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
fn attribute_codex(
    cwd: Option<&Path>,
    proc_start: SystemTime,
    held: Option<&[PathBuf]>,
    recent: &[(PathBuf, PathBuf, SystemTime)],
    taken: &HashSet<PathBuf>,
    now: SystemTime,
    opts: &CollectorOptions,
) -> (Vec<PathBuf>, Attribution) {
    if let Some(held) = held {
        let mut mine = held.to_vec();
        mine.sort_by_key(|p| std::cmp::Reverse(written_at(p)));
        mine.truncate(MAX_CODEX_THREADS);
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
        .filter(|(p, _, _)| written_at(p).map(|w| now.duration_since(w).unwrap_or_default() <= opts.activity_timeout).unwrap_or(false))
        .collect();
    live.sort_by_key(|(p, _, _)| std::cmp::Reverse(written_at(p)));
    live.truncate(MAX_CODEX_THREADS);
    let attribution = if live.is_empty() { Attribution::None } else { Attribution::CwdHeuristic };
    (live.into_iter().map(|(p, _, _)| p.clone()).collect(), attribution)
}

/// One process is not plausibly running more conversations than this at once,
/// and an unbounded fan-out would let a stale directory fill the table.
const MAX_CODEX_THREADS: usize = 12;

fn written_at(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Older Claude Code versions stored subagent transcripts as `agent-<id>.jsonl`
/// next to the parent session; they are not sessions of their own. Current
/// versions nest them under `<session>/subagents/`, where the directory walk
/// does not look, and `ClaudeTranscript` folds them into the parent.
fn is_subagent_transcript(p: &Path) -> bool {
    p.file_name().and_then(|f| f.to_str()).map(|f| f.starts_with("agent-")).unwrap_or(false)
}

fn live_state(reg: Option<&PidSession>, activity: Activity, idle_secs: Option<u64>, cpu: f32, opts: &CollectorOptions) -> AgentState {
    // Statuses observed in the registry so far (Claude Code 2.1.259): "busy",
    // "idle", "shell". Unknown values fall through to the transcript heuristic.
    match reg.and_then(|r| r.status.as_deref()) {
        Some("busy" | "running" | "working" | "shell" | "tool" | "thinking") => return AgentState::Running,
        Some("idle" | "waiting" | "paused" | "permission" | "blocked") => return AgentState::Idle,
        _ => {}
    }
    match activity {
        Activity::Working => {
            if idle_secs.map(|s| s > opts.activity_timeout.as_secs()).unwrap_or(false) {
                AgentState::Idle
            } else {
                AgentState::Running
            }
        }
        Activity::Waiting => AgentState::Idle,
        Activity::Unknown => {
            if cpu > 5.0 || idle_secs.map(|s| s < 10).unwrap_or(false) {
                AgentState::Running
            } else {
                AgentState::Idle
            }
        }
    }
}

fn display_name(harness: Harness, cwd: Option<&Path>) -> String {
    match cwd.and_then(|c| c.file_name()).map(|f| f.to_string_lossy().into_owned()) {
        Some(dir) => format!("{}:{}", harness.label(), dir),
        None => harness.label().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write a rollout with an explicit modification time.
    ///
    /// Ordering must not be left to how finely the filesystem happens to
    /// timestamp three writes microseconds apart: Linux gave all three the
    /// same mtime, the stable sort preserved insertion order, and the test
    /// failed there while passing on macOS.
    fn rollout(dir: &Path, name: &str, written: SystemTime) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"x").unwrap();
        let f = fs::File::options().write(true).open(&p).unwrap();
        f.set_times(fs::FileTimes::new().set_accessed(written).set_modified(written)).unwrap();
        p
    }

    /// One app-server, several conversations. Every live one must get a row:
    /// returning only the newest is what collapsed them into a single
    /// mis-attributed row.
    #[test]
    fn every_live_codex_thread_is_returned_newest_first() {
        let dir = std::env::temp_dir().join(format!("agent-top-threads-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let started = now - Duration::from_secs(600);
        let opts = CollectorOptions::default();

        // Distinct write times, oldest first, so "newest first" has a single
        // correct answer.
        let a = rollout(&dir, "a.jsonl", now - Duration::from_secs(300));
        let b = rollout(&dir, "b.jsonl", now - Duration::from_secs(200));
        let c = rollout(&dir, "c.jsonl", now - Duration::from_secs(100));
        let recent: Vec<(PathBuf, PathBuf, SystemTime)> =
            [&a, &b, &c].iter().map(|p| ((*p).clone(), PathBuf::from("/Users/dev/code/one"), started)).collect();

        // The app-server case: the process cwd matches no conversation.
        let (paths, attribution) = attribute_codex(Some(Path::new("/")), started, None, &recent, &HashSet::new(), now, &opts);
        assert_eq!(paths.len(), 3, "all three conversations get a row");
        assert_eq!(paths[0], c, "newest activity first");
        assert_eq!(attribution, Attribution::CwdHeuristic, "still a heuristic, and still labelled one");

        // A conversation already claimed by another process is not shown twice.
        let taken: HashSet<PathBuf> = [c.clone()].into_iter().collect();
        let (paths, _) = attribute_codex(Some(Path::new("/")), started, None, &recent, &taken, now, &opts);
        assert_eq!(paths.len(), 2);
        assert!(!paths.contains(&c));

        // A conversation nobody has written to for longer than the activity
        // window has finished; it belongs in the stopped list, not on this
        // process.
        let stale = now + opts.activity_timeout + Duration::from_secs(60);
        let (paths, attribution) = attribute_codex(Some(Path::new("/")), started, None, &recent, &HashSet::new(), stale, &opts);
        assert!(paths.is_empty());
        assert_eq!(attribution, Attribution::None);

        // The CLI case: one conversation, in the directory the process runs in.
        let (paths, _) = attribute_codex(Some(Path::new("/Users/dev/code/one")), started, None, &recent, &HashSet::new(), now, &opts);
        assert_eq!(paths.len(), 3, "a cwd match takes every conversation in that directory");
        assert_eq!(paths[0], c);

        // A rollout that predates the process is not this process's.
        let (paths, _) = attribute_codex(Some(Path::new("/")), now + Duration::from_secs(3600), None, &recent, &HashSet::new(), now, &opts);
        assert!(paths.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    /// Two app-servers at once, the VS Code one and a CLI-spawned one, both
    /// running from `/`. Without the open-file signal the one asked first
    /// took every live thread. The bug this guards was found live on
    /// 2026-09-04: two threads of a fresh app-server were shown on the four
    /// day old VS Code one.
    #[test]
    fn an_open_rollout_belongs_to_the_process_holding_it() {
        let dir = std::env::temp_dir().join(format!("agent-top-held-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        let started = now - Duration::from_secs(600);
        let opts = CollectorOptions::default();
        let a = rollout(&dir, "a.jsonl", now - Duration::from_secs(200));
        let b = rollout(&dir, "b.jsonl", now - Duration::from_secs(100));
        let recent: Vec<(PathBuf, PathBuf, SystemTime)> =
            [&a, &b].iter().map(|p| ((*p).clone(), PathBuf::from("/Users/dev/code/one"), started)).collect();

        // The newer app-server holds both rollouts open. It started after the
        // rollouts' recorded start, which the heuristic would reject; the open
        // file settles it.
        let held = vec![a.clone(), b.clone()];
        let (paths, attribution) = attribute_codex(Some(Path::new("/")), now, Some(&held), &recent, &HashSet::new(), now, &opts);
        assert_eq!(paths, vec![b.clone(), a.clone()], "held rollouts, newest written first");
        assert_eq!(attribution, Attribution::OpenFile);

        // The older app-server holds nothing. Its fallback would have taken
        // both live rollouts; with them marked taken it gets no row.
        let taken: HashSet<PathBuf> = held.iter().cloned().collect();
        let (paths, attribution) = attribute_codex(Some(Path::new("/")), started, None, &recent, &taken, now, &opts);
        assert!(paths.is_empty());
        assert_eq!(attribution, Attribution::None);

        let _ = fs::remove_dir_all(&dir);
    }
}
