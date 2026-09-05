//! Joins the process table with the transcripts into a `Snapshot`.
//!
//! The collector knows no harness by name. Each one is a `HarnessAdapter`
//! (RFC-101): it lists its transcripts, says which belong to which process,
//! and opens a tracker for one. The collector walks the process forest, asks
//! the adapter for each root, and builds the rows.

use crate::harness::{self, AttributeContext, HarnessAdapter, McpUsage, RegistryHints, SessionSummary, SessionTracker, SpanRetention};
use crate::model::*;
use crate::process::{ProcessScanner, RawProc, build_forest};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// What is known about every MCP process seen this run, by pid, so that
    /// an orphan can say which agent it used to belong to. See `OrphanOrigin`.
    mcp_memory: HashMap<u32, McpMemory>,
}

/// One MCP process's history for the run.
#[derive(Debug, Clone)]
struct McpMemory {
    /// The process start time, so a reused pid is not mistaken for the same process.
    start_time: u64,
    first_seen: SystemTime,
    parent: Option<OrphanParent>,
    orphaned_at: Option<SystemTime>,
}

impl Collector {
    pub fn new(opts: CollectorOptions) -> Self {
        Collector {
            opts,
            scanner: ProcessScanner::new(),
            adapters: harness::adapters(),
            trackers: HashMap::new(),
            last_fs_scan: None,
            mcp_memory: HashMap::new(),
        }
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
                    mcp_servers: mcp_rows(Some(&root), &BTreeMap::new()),
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
                    mcp_servers: mcp_rows(if owns_process { Some(&root) } else { None }, &summary.mcp),
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
                mcp_servers: mcp_rows(None, &s.mcp),
                tree: None,
                attribution: Attribution::TranscriptOnly,
                shares_process: false,
                parse_warning: parse_warning(&s, harness),
            });
        }

        // Drop trackers for transcripts that fell out of the window.
        let keep: HashSet<&PathBuf> = agents.iter().filter_map(|a| a.session_path.as_ref()).collect();
        self.trackers.retain(|p, _| keep.contains(p));

        let orphan_origins = self.remember_mcp(&agents, &orphans, &by_pid, now);

        let mut snap = Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            taken_at: now,
            host,
            agents,
            orphans,
            orphan_origins,
            totals: Totals::default(),
        };
        snap.compute_totals();
        snap
    }

    /// Note which agent each MCP process is under this tick, and which of the
    /// orphans used to be under one. A process seen under an agent at one
    /// tick and among the orphans at the next is reported as orphaned from
    /// that agent; one that was already an orphan when the run started has
    /// no parent on record. Memory is per run and per process start time, so
    /// a reused pid starts over.
    fn remember_mcp(
        &mut self,
        agents: &[Agent],
        orphans: &[ProcNode],
        by_pid: &HashMap<u32, &RawProc>,
        now: SystemTime,
    ) -> Vec<OrphanOrigin> {
        let start_of = |pid: u32| by_pid.get(&pid).map(|p| p.start_time).unwrap_or(0);
        for a in agents {
            let Some(tree) = &a.tree else { continue };
            let parent = OrphanParent { pid: tree.pid, agent_id: a.id.clone(), name: a.name.clone() };
            let under: Vec<u32> = tree.mcp_roots().iter().map(|n| n.pid).collect();
            for pid in under {
                let m = touch(&mut self.mcp_memory, pid, start_of(pid), now);
                m.parent = Some(parent.clone());
                m.orphaned_at = None;
            }
        }
        let mut origins = Vec::with_capacity(orphans.len());
        for o in orphans {
            let m = touch(&mut self.mcp_memory, o.pid, start_of(o.pid), now);
            if m.parent.is_some() && m.orphaned_at.is_none() {
                m.orphaned_at = Some(now);
            }
            origins.push(OrphanOrigin { pid: o.pid, first_seen: m.first_seen, orphaned_at: m.orphaned_at, parent: m.parent.clone() });
        }
        // A process that has exited is forgotten, so the map does not grow
        // with every server ever started.
        self.mcp_memory.retain(|pid, _| by_pid.contains_key(pid));
        origins
    }
}

/// The memory entry for a process, fresh if the pid is new or has been
/// reused by a process with a different start time.
fn touch(memory: &mut HashMap<u32, McpMemory>, pid: u32, start_time: u64, now: SystemTime) -> &mut McpMemory {
    let entry = memory.entry(pid).or_insert(McpMemory { start_time, first_seen: now, parent: None, orphaned_at: None });
    if entry.start_time != start_time {
        *entry = McpMemory { start_time, first_seen: now, parent: None, orphaned_at: None };
    }
    entry
}

/// One row per MCP server, from the processes under the agent and the servers
/// its transcript names, joined where they can be.
///
/// The transcript knows a server by the name the harness configured
/// (`filesystem`, `chrome-devtools`); the process table knows a command line
/// (`npx -y @modelcontextprotocol/server-filesystem /tmp`). Neither side
/// carries the other's key, so the join is a name test: a server whose
/// normalised name appears in a process's normalised command line is that
/// process. When exactly one process and one server are left over, they are
/// taken to be the same, and the row says so. Anything else stays a row of
/// its own: a process the agent has not called yet, or a server with no
/// process, which is an HTTP server or one that has exited.
pub fn mcp_rows(tree: Option<&ProcNode>, usage: &BTreeMap<String, McpUsage>) -> Vec<McpServer> {
    let procs: Vec<&ProcNode> = tree.map(|t| t.mcp_roots()).unwrap_or_default();
    let mut rows = Vec::new();
    let mut unmatched_procs: Vec<&ProcNode> = Vec::new();
    let mut unmatched_servers: Vec<(&String, &McpUsage)> = Vec::new();
    let mut claimed: HashSet<u32> = HashSet::new();

    for (name, u) in usage {
        let key = normalise(name);
        let hit = procs.iter().find(|p| !claimed.contains(&p.pid) && !key.is_empty() && subtree_mentions(p, &key));
        match hit {
            Some(p) => {
                claimed.insert(p.pid);
                rows.push(row(name.clone(), Some(p), Some(u), McpMatch::Name));
            }
            None => unmatched_servers.push((name, u)),
        }
    }
    for p in &procs {
        if !claimed.contains(&p.pid) {
            unmatched_procs.push(p);
        }
    }
    if let ([p], [(name, u)]) = (unmatched_procs.as_slice(), unmatched_servers.as_slice()) {
        rows.push(row((*name).clone(), Some(p), Some(u), McpMatch::Sole));
        return rows;
    }
    for (name, u) in unmatched_servers {
        rows.push(row(name.clone(), None, Some(u), McpMatch::TranscriptOnly));
    }
    for p in unmatched_procs {
        rows.push(row(p.name.clone(), Some(p), None, McpMatch::ProcessOnly));
    }
    rows
}

/// Whether the server's normalised name appears in the command line of the
/// process or of anything under it: `npx` names the package, its `node`
/// child names the binary, and the configured name may match either.
fn subtree_mentions(p: &ProcNode, key: &str) -> bool {
    let mut found = false;
    p.walk(0, &mut |n, _| found |= normalise(&n.cmdline).contains(key));
    found
}

fn row(name: String, p: Option<&ProcNode>, u: Option<&McpUsage>, matched_by: McpMatch) -> McpServer {
    let u = u.copied().unwrap_or_default();
    // CPU and memory are the server's whole subtree, as the process count is.
    let totals = p.map(|p| p.totals());
    McpServer {
        name,
        pid: p.map(|p| p.pid),
        cmdline: p.map(|p| p.cmdline.clone()),
        cpu_percent: totals.map(|t| t.0).unwrap_or(0.0),
        rss_bytes: totals.map(|t| t.1).unwrap_or(0),
        age_secs: p.map(|p| p.age_secs),
        calls: u.calls,
        errors: u.errors,
        last_call: u.last_call,
        matched_by,
    }
}

/// Lowercase, letters and digits only, so `chrome-devtools` finds
/// `chrome-devtools-mcp` and `server_filesystem` finds `server-filesystem`.
fn normalise(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
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

    fn node(pid: u32, kind: ProcKind, cmd: &str) -> ProcNode {
        ProcNode {
            pid,
            ppid: Some(1),
            name: cmd.split(' ').next().unwrap().to_string(),
            cmdline: cmd.to_string(),
            kind,
            harness: None,
            cpu_percent: 0.5,
            rss_bytes: 10 << 20,
            age_secs: 60,
            cwd: None,
            children: Vec::new(),
        }
    }

    fn used(calls: u64) -> McpUsage {
        McpUsage { calls, errors: 0, last_call: Some(UNIX_EPOCH) }
    }

    #[test]
    fn mcp_rows_join_processes_to_servers_by_name_then_by_elimination() {
        let mut root = node(10, ProcKind::Agent, "claude");
        root.children = vec![
            node(11, ProcKind::Mcp, "npx -y @modelcontextprotocol/server-filesystem /tmp"),
            node(12, ProcKind::Mcp, "node /opt/chrome-devtools-mcp/build/index.js"),
            node(13, ProcKind::Shell, "zsh -c cargo test"),
        ];
        let mut usage = BTreeMap::new();
        usage.insert("chrome-devtools".to_string(), used(3));
        usage.insert("filesystem".to_string(), used(7));
        usage.insert("linear".to_string(), used(1));
        let rows = mcp_rows(Some(&root), &usage);
        assert_eq!(rows.len(), 3);
        let by_name: HashMap<&str, &McpServer> = rows.iter().map(|r| (r.name.as_str(), r)).collect();
        assert_eq!(by_name["filesystem"].pid, Some(11));
        assert_eq!(by_name["filesystem"].calls, 7);
        assert_eq!(by_name["filesystem"].matched_by, McpMatch::Name);
        assert_eq!(by_name["chrome-devtools"].pid, Some(12));
        // An HTTP server, or one that exited: called, but no process.
        assert_eq!(by_name["linear"].pid, None);
        assert_eq!(by_name["linear"].matched_by, McpMatch::TranscriptOnly);

        // One process whose command never mentions its configured name, and
        // one server: taken to be the same, and labelled as a guess.
        root.children = vec![node(14, ProcKind::Mcp, "uvx some-tool serve --stdio")];
        let mut usage = BTreeMap::new();
        usage.insert("tickets".to_string(), used(2));
        let rows = mcp_rows(Some(&root), &usage);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].name.as_str(), rows[0].pid, rows[0].calls, rows[0].matched_by), ("tickets", Some(14), 2, McpMatch::Sole));

        // An npx wrapper and its node child are one server, found through
        // the child's command line, with the wrapper's pid.
        let mut wrapper = node(16, ProcKind::Mcp, "npm exec @modelcontextprotocol/server-filesystem /tmp");
        wrapper.children = vec![node(17, ProcKind::Mcp, "node /x/.bin/mcp-server-filesystem /tmp")];
        root.children = vec![wrapper];
        let mut usage = BTreeMap::new();
        usage.insert("filesystem".to_string(), used(4));
        let rows = mcp_rows(Some(&root), &usage);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].pid, rows[0].matched_by), (Some(16), McpMatch::Name));
        assert_eq!(rows[0].rss_bytes, 20 << 20, "the subtree's memory");
        assert_eq!(root.totals().3, 1, "one server, two processes");

        // Two such processes: no guessing, each stays its own row.
        root.children = vec![node(14, ProcKind::Mcp, "uvx some-tool serve --stdio")];
        root.children.push(node(15, ProcKind::Mcp, "uvx other-tool serve"));
        let rows = mcp_rows(Some(&root), &usage);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().filter(|r| r.matched_by == McpMatch::ProcessOnly).count() == 2);
        assert!(mcp_rows(None, &BTreeMap::new()).is_empty());
    }

    fn raw(pid: u32, start_time: u64) -> RawProc {
        RawProc {
            pid,
            ppid: Some(1),
            name: "x".into(),
            exe: None,
            cmd: vec!["x".into()],
            cwd: None,
            cpu_percent: 0.0,
            rss_bytes: 0,
            start_time,
            run_time: 1,
        }
    }

    /// The RFC-104 success test, in miniature: a server seen under an agent
    /// at one tick and among the orphans at the next says which agent it
    /// came from; one that was an orphan from the start does not pretend to.
    #[test]
    fn an_orphan_remembers_the_agent_it_was_under() {
        let mut c = Collector::new(CollectorOptions::default());
        let t0 = UNIX_EPOCH + Duration::from_secs(1_000);
        let t1 = t0 + Duration::from_secs(10);
        let mut root = node(10, ProcKind::Agent, "claude");
        root.children = vec![node(11, ProcKind::Mcp, "npx server-filesystem")];
        let mut agent = Agent {
            id: "pid:10".into(),
            name: "claude:proj".into(),
            harness: Harness::Claude,
            state: AgentState::Running,
            activity: Activity::Working,
            pid: Some(10),
            session_id: None,
            session_path: None,
            cwd: None,
            model: None,
            harness_version: None,
            usage: TokenUsage::default(),
            cost_usd: 0.0,
            cost_breakdown: Default::default(),
            price_source: None,
            unpriced_tokens: 0,
            turns: 0,
            subagent_turns: 0,
            tool_calls: 0,
            web_searches: 0,
            spans: Vec::new(),
            age_secs: 0,
            idle_secs: None,
            cpu_percent: 0.0,
            rss_bytes: 0,
            process_count: 2,
            mcp_count: 1,
            mcp_servers: Vec::new(),
            tree: Some(root),
            attribution: Attribution::HarnessRegistry,
            shares_process: false,
            parse_warning: None,
        };
        let procs = [raw(10, 5), raw(11, 6), raw(99, 7)];
        let by_pid: HashMap<u32, &RawProc> = procs.iter().map(|p| (p.pid, p)).collect();

        // Tick 0: the server is under its agent; 99 is an orphan from the start.
        let origins = c.remember_mcp(std::slice::from_ref(&agent), &[node(99, ProcKind::Mcp, "uvx mcp-server-git")], &by_pid, t0);
        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0].pid, 99);
        assert!(origins[0].parent.is_none());
        assert_eq!(origins[0].first_seen, t0);

        // Tick 1: the agent is gone and the server is an orphan.
        agent.tree = None;
        let procs = [raw(11, 6), raw(99, 7)];
        let by_pid: HashMap<u32, &RawProc> = procs.iter().map(|p| (p.pid, p)).collect();
        let orphans = [node(11, ProcKind::Mcp, "npx server-filesystem"), node(99, ProcKind::Mcp, "uvx mcp-server-git")];
        let origins = c.remember_mcp(&[], &orphans, &by_pid, t1);
        let fs = origins.iter().find(|o| o.pid == 11).unwrap();
        assert_eq!(fs.parent.as_ref().map(|p| (p.pid, p.name.as_str())), Some((10, "claude:proj")));
        assert_eq!(fs.orphaned_at, Some(t1));
        assert_eq!(fs.first_seen, t0);
        let git = origins.iter().find(|o| o.pid == 99).unwrap();
        assert!(git.parent.is_none() && git.orphaned_at.is_none());

        // Tick 2: pid 11 is reused by a different process. The memory starts over.
        let procs = [raw(11, 900)];
        let by_pid: HashMap<u32, &RawProc> = procs.iter().map(|p| (p.pid, p)).collect();
        let origins = c.remember_mcp(&[], &orphans[..1], &by_pid, t1 + Duration::from_secs(10));
        assert!(origins[0].parent.is_none());
        assert!(!c.mcp_memory.contains_key(&99), "an exited process is forgotten");
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
