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

        for root in roots {
            let raw = by_pid.get(&root.pid).copied();
            let harness = root.harness.unwrap_or(Harness::Unknown);
            let proc_start = raw.map(|p| UNIX_EPOCH + Duration::from_secs(p.start_time)).unwrap_or(now);
            let cwd = root.cwd.clone().or_else(|| registry.get(&root.pid).map(|r| r.cwd.clone()));

            let (path, attribution) = match harness {
                Harness::Claude => attribute_claude(&root, raw, cwd.as_deref(), proc_start, &registry),
                Harness::Codex => attribute_codex(cwd.as_deref(), proc_start, &self.recent_codex),
                _ => (None, Attribution::None),
            };

            let mut summary = SessionSummary::default();
            if let Some(p) = &path {
                let tr = self.tracker_for(p, harness);
                let _ = tr.refresh();
                summary = tr.summary().clone();
                attached.insert(p.clone());
            }
            if let Some(reg) = registry.get(&root.pid) {
                if summary.session_id.is_none() {
                    summary.session_id = Some(reg.session_id.clone());
                }
                if summary.harness_version.is_none() {
                    summary.harness_version = reg.version.clone();
                }
            }

            let (cpu, rss, count, mcp) = root.totals();
            let idle_secs = summary.last_activity.and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs());
            let state = live_state(registry.get(&root.pid), summary.activity, idle_secs, cpu, &self.opts);
            let name = registry.get(&root.pid).and_then(|r| r.name.clone()).unwrap_or_else(|| display_name(harness, cwd.as_deref()));

            agents.push(Agent {
                id: format!("pid:{}", root.pid),
                name,
                harness,
                state,
                activity: summary.activity,
                pid: Some(root.pid),
                session_id: summary.session_id.clone(),
                session_path: path,
                cwd: summary.cwd.clone().or(cwd),
                model: summary.model.clone(),
                harness_version: summary.harness_version.clone(),
                usage: summary.usage,
                cost_usd: summary.cost_usd,
                unpriced_tokens: summary.unpriced_tokens,
                turns: summary.turns,
                subagent_turns: summary.subagent_turns,
                tool_calls: summary.tool_calls,
                age_secs: root.age_secs,
                idle_secs,
                cpu_percent: cpu,
                rss_bytes: rss,
                process_count: count,
                mcp_count: mcp,
                tree: Some(root),
                attribution,
            });
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
                unpriced_tokens: s.unpriced_tokens,
                turns: s.turns,
                subagent_turns: s.subagent_turns,
                tool_calls: s.tool_calls,
                age_secs: idle_secs.unwrap_or(0),
                idle_secs,
                cpu_percent: 0.0,
                rss_bytes: 0,
                process_count: 0,
                mcp_count: 0,
                tree: None,
                attribution: Attribution::TranscriptOnly,
            });
        }

        // Drop trackers for transcripts that fell out of the window.
        let keep: HashSet<&PathBuf> = agents.iter().filter_map(|a| a.session_path.as_ref()).collect();
        self.trackers.retain(|p, _| keep.contains(p));

        let mut snap = Snapshot { taken_at: now, host, agents, orphans, totals: Totals::default() };
        snap.compute_totals();
        snap
    }
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

fn attribute_codex(
    cwd: Option<&Path>,
    proc_start: SystemTime,
    recent: &[(PathBuf, PathBuf, SystemTime)],
) -> (Option<PathBuf>, Attribution) {
    let slack = Duration::from_secs(60);
    let started_after = |ts: &SystemTime| *ts + slack >= proc_start;
    // Prefer a rollout whose cwd matches the process. The VS Code app-server
    // runs from an unrelated directory and hosts many threads, so fall back to
    // the newest rollout started after the process did. Both are heuristics.
    let best = cwd
        .and_then(|cwd| recent.iter().filter(|(_, c, ts)| c == cwd && started_after(ts)).max_by_key(|(_, _, ts)| *ts))
        .or_else(|| recent.iter().filter(|(_, _, ts)| started_after(ts)).max_by_key(|(_, _, ts)| *ts))
        .map(|(p, _, _)| p.clone());
    match best {
        Some(p) => (Some(p), Attribution::CwdHeuristic),
        None => (None, Attribution::None),
    }
}

/// Claude Code stores subagent transcripts as `agent-<id>.jsonl` next to the
/// parent session; they are not sessions of their own.
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
