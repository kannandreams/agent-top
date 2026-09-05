//! Joins the process table with the transcripts into a `Snapshot`.
//!
//! The collector knows no harness by name. Each one is a `HarnessAdapter`
//! (RFC-101): it lists its transcripts, says which belong to which process,
//! and opens a tracker for one. The collector walks the process forest, asks
//! the adapter for each root, and builds the rows.

use crate::harness::{self, AttributeContext, HarnessAdapter, RegistryHints, SessionSummary, SessionTracker, SpanRetention};
use crate::model::*;
use crate::process::{ProcessScanner, RawProc, build_forest};
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
    adapters: Vec<Box<dyn HarnessAdapter>>,
    trackers: HashMap<PathBuf, Box<dyn SessionTracker>>,
    last_fs_scan: Option<Instant>,
}

impl Collector {
    pub fn new(opts: CollectorOptions) -> Self {
        Collector { opts, scanner: ProcessScanner::new(), adapters: harness::adapters(), trackers: HashMap::new(), last_fs_scan: None }
    }

    fn rescan_fs_if_due(&mut self) {
        let due = self.last_fs_scan.map(|t| t.elapsed() >= self.opts.fs_scan_interval).unwrap_or(true);
        if !due {
            return;
        }
        self.last_fs_scan = Some(Instant::now());
        let since = SystemTime::now().checked_sub(self.opts.stopped_window).unwrap_or(UNIX_EPOCH);
        for a in &mut self.adapters {
            a.rescan(since);
        }
    }

    fn adapter(&self, harness: Harness) -> Option<&dyn HarnessAdapter> {
        self.adapters.iter().find(|a| a.harness() == harness).map(|a| a.as_ref())
    }

    /// The tracker for a transcript, opened on first sight.
    fn tracker_for(&mut self, path: &Path, harness: Harness) -> Option<&mut Box<dyn SessionTracker>> {
        if !self.trackers.contains_key(path) {
            let tracker = self.adapter(harness)?.open(path, SpanRetention::Recent);
            self.trackers.insert(path.to_path_buf(), tracker);
        }
        self.trackers.get_mut(path)
    }

    pub fn collect(&mut self) -> Snapshot {
        self.scanner.refresh();
        self.rescan_fs_if_due();
        let host = self.scanner.host();
        let procs = self.scanner.processes();
        let by_pid: HashMap<u32, &RawProc> = procs.iter().map(|p| (p.pid, p)).collect();
        let (roots, orphans) = build_forest(&procs);

        // Each adapter sees all of its processes before any is attributed.
        for a in &mut self.adapters {
            let mine: Vec<&ProcNode> = roots.iter().filter(|r| r.harness == Some(a.harness())).collect();
            a.prepare(&mine);
        }

        let now = SystemTime::now();
        let mut agents = Vec::new();
        let mut attached: HashSet<PathBuf> = HashSet::new();

        for root in roots {
            let raw = by_pid.get(&root.pid).copied();
            let harness = root.harness.unwrap_or(Harness::Unknown);
            let proc_start = raw.map(|p| UNIX_EPOCH + Duration::from_secs(p.start_time)).unwrap_or(now);
            let hints = self.adapter(harness).and_then(|a| a.hints(root.pid));
            let cwd = root.cwd.clone().or_else(|| hints.as_ref().and_then(|h| h.cwd.clone()));

            // One process can host several conversations. Claude Code runs one
            // per process; the Codex app-server runs many.
            let (paths, attribution) = match self.adapter(harness) {
                Some(a) => {
                    let ctx = AttributeContext {
                        cwd: cwd.as_deref(),
                        proc_start,
                        now,
                        attached: &attached,
                        activity_timeout: self.opts.activity_timeout,
                    };
                    a.attribute(&root, raw, &ctx)
                }
                None => (Vec::new(), Attribution::None),
            };

            let (cpu, rss, count, mcp) = root.totals();
            let hints = hints.unwrap_or_default();

            // No transcript: the process still deserves a row.
            if paths.is_empty() {
                let summary = SessionSummary::default();
                let state = live_state(&hints, summary.activity, None, cpu, &self.opts);
                agents.push(Agent {
                    id: format!("pid:{}", root.pid),
                    name: hints.name.clone().unwrap_or_else(|| display_name(harness, cwd.as_deref())),
                    harness,
                    state,
                    activity: summary.activity,
                    pid: Some(root.pid),
                    session_id: hints.session_id.clone(),
                    session_path: None,
                    cwd,
                    model: None,
                    harness_version: hints.version.clone(),
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
                let Some(tr) = self.tracker_for(path, harness) else { continue };
                let _ = tr.refresh();
                let mut summary = tr.summary().clone();
                attached.insert(path.clone());

                if summary.session_id.is_none() {
                    summary.session_id = hints.session_id.clone();
                }
                if summary.harness_version.is_none() {
                    summary.harness_version = hints.version.clone();
                }

                let idle_secs = summary.last_activity.and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs());
                let state = live_state(&hints, summary.activity, idle_secs, cpu, &self.opts);
                // A thread names itself after its own working directory, which
                // is the only thing distinguishing two rows on one app-server.
                let name = match (hints.name.clone(), paths.len()) {
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
        let stopped: Vec<(PathBuf, Harness)> =
            self.adapters.iter().flat_map(|a| a.unowned(&attached).into_iter().map(move |p| (p, a.harness()))).collect();
        for (p, harness) in stopped {
            let Some(tr) = self.tracker_for(&p, harness) else { continue };
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

fn live_state(hints: &RegistryHints, activity: Activity, idle_secs: Option<u64>, cpu: f32, opts: &CollectorOptions) -> AgentState {
    // Statuses observed in the registry so far (Claude Code 2.1.259): "busy",
    // "idle", "shell". Unknown values fall through to the transcript heuristic.
    match hints.status.as_deref() {
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

    #[test]
    fn the_registry_status_beats_the_transcript_heuristic() {
        let opts = CollectorOptions::default();
        let busy = RegistryHints { status: Some("busy".into()), ..Default::default() };
        assert_eq!(live_state(&busy, Activity::Waiting, Some(0), 0.0, &opts), AgentState::Running);
        let idle = RegistryHints { status: Some("idle".into()), ..Default::default() };
        assert_eq!(live_state(&idle, Activity::Working, Some(0), 90.0, &opts), AgentState::Idle);
        // No registry, which is every harness but Claude Code: the transcript decides.
        let none = RegistryHints::default();
        assert_eq!(live_state(&none, Activity::Working, Some(1), 0.0, &opts), AgentState::Running);
        assert_eq!(live_state(&none, Activity::Working, Some(opts.activity_timeout.as_secs() + 1), 0.0, &opts), AgentState::Idle);
        assert_eq!(live_state(&none, Activity::Waiting, Some(1), 90.0, &opts), AgentState::Idle);
        assert_eq!(live_state(&none, Activity::Unknown, Some(3), 0.0, &opts), AgentState::Running);
        assert_eq!(live_state(&none, Activity::Unknown, Some(300), 0.0, &opts), AgentState::Idle);
    }

    #[test]
    fn every_adapter_is_a_distinct_harness_and_recognises_its_own_fixture() {
        let adapters = harness::adapters();
        let mut seen = HashSet::new();
        for a in &adapters {
            assert!(seen.insert(a.harness()), "two adapters for {:?}", a.harness());
        }
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for (file, want) in
            [("claude-2.1.226.jsonl", Harness::Claude), ("codex-0.130.jsonl", Harness::Codex), ("gemini-0.58.jsonl", Harness::Gemini)]
        {
            assert_eq!(harness::detect(&fixtures.join(file)), Some(want), "{file}");
        }
    }
}
