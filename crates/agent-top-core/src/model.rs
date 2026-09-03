//! The data model shared by discovery, the TUI and the JSON output.

use serde::Serialize;
use std::path::PathBuf;
use std::time::SystemTime;

/// Which agent harness a process or transcript belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Harness {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Aider,
    Copilot,
    Cursor,
    Unknown,
}

impl Harness {
    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Gemini => "gemini",
            Harness::OpenCode => "opencode",
            Harness::Aider => "aider",
            Harness::Copilot => "copilot",
            Harness::Cursor => "cursor",
            Harness::Unknown => "unknown",
        }
    }
}

/// Coarse lifecycle state, in the htop sense.
///
/// `Running` means the agent is mid-turn (inference or tool execution),
/// `Idle` means the process is alive but waiting for a human, `Stopped` means
/// the transcript exists and was recently active but no process owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum AgentState {
    Running,
    Idle,
    Stopped,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Running => "running",
            AgentState::Idle => "idle",
            AgentState::Stopped => "stopped",
        }
    }
}

/// What the transcript says the agent was last doing. Harness-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Activity {
    /// Mid-turn: a prompt or tool result was just submitted, or a tool call is pending.
    Working,
    /// The last thing that happened was the assistant ending its turn.
    Waiting,
    #[default]
    Unknown,
}

/// Token counts, split the way the Anthropic and OpenAI usage objects split them.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct TokenUsage {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl TokenUsage {
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    /// Everything the model consumed or produced. This is the "TOKENS" column.
    pub fn total(&self) -> u64 {
        self.input + self.cache_write() + self.cache_read + self.output
    }

    pub fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
        self.cache_read += other.cache_read;
        self.output += other.output;
    }

    pub fn sub(&mut self, other: &TokenUsage) {
        self.input = self.input.saturating_sub(other.input);
        self.cache_write_5m = self.cache_write_5m.saturating_sub(other.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.saturating_sub(other.cache_write_1h);
        self.cache_read = self.cache_read.saturating_sub(other.cache_read);
        self.output = self.output.saturating_sub(other.output);
    }
}

/// Role of a process inside an agent's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcKind {
    /// The agent's root process (the harness itself).
    Agent,
    /// Another agent process nested under an agent (e.g. `claude -p` run from a tool).
    Subagent,
    /// A Model Context Protocol server or helper.
    Mcp,
    /// A shell spawned to run a tool call.
    Shell,
    /// Anything else the agent launched (test runners, sleep, caffeinate, ...).
    Tool,
}

impl ProcKind {
    pub fn label(self) -> &'static str {
        match self {
            ProcKind::Agent => "agent",
            ProcKind::Subagent => "subagent",
            ProcKind::Mcp => "mcp",
            ProcKind::Shell => "shell",
            ProcKind::Tool => "tool",
        }
    }
}

/// One process, with its descendants.
#[derive(Debug, Clone, Serialize)]
pub struct ProcNode {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub cmdline: String,
    pub kind: ProcKind,
    pub harness: Option<Harness>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub age_secs: u64,
    pub cwd: Option<PathBuf>,
    pub children: Vec<ProcNode>,
}

impl ProcNode {
    /// CPU, RSS and process count summed over the whole subtree.
    pub fn totals(&self) -> (f32, u64, usize, usize) {
        let mut cpu = self.cpu_percent;
        let mut rss = self.rss_bytes;
        let mut count = 1;
        let mut mcp = usize::from(self.kind == ProcKind::Mcp);
        for c in &self.children {
            let (ccpu, crss, ccount, cmcp) = c.totals();
            cpu += ccpu;
            rss += crss;
            count += ccount;
            mcp += cmcp;
        }
        (cpu, rss, count, mcp)
    }

    pub fn walk<'a>(&'a self, depth: usize, f: &mut dyn FnMut(&'a ProcNode, usize)) {
        f(self, depth);
        for c in &self.children {
            c.walk(depth + 1, f);
        }
    }
}

/// A single row in the agent table.
#[derive(Debug, Clone, Serialize)]
pub struct Agent {
    /// Stable identity across refreshes: `pid:<n>` for live agents, `session:<id>` for stopped ones.
    pub id: String,
    pub name: String,
    pub harness: Harness,
    pub state: AgentState,
    pub activity: Activity,
    pub pid: Option<u32>,
    pub session_id: Option<String>,
    pub session_path: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub harness_version: Option<String>,
    pub usage: TokenUsage,
    /// USD spent on messages whose model had a known price.
    pub cost_usd: f64,
    /// Tokens on messages whose model had no known price (so `cost_usd` is a floor).
    pub unpriced_tokens: u64,
    pub turns: u64,
    pub subagent_turns: u64,
    pub tool_calls: u64,
    /// Seconds since the process started (live) or since the last transcript write (stopped).
    pub age_secs: u64,
    /// Seconds since the transcript was last written.
    pub idle_secs: Option<u64>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub process_count: usize,
    pub mcp_count: usize,
    pub tree: Option<ProcNode>,
    /// How the session was attributed to the process, for debugging attribution.
    pub attribution: Attribution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attribution {
    /// The harness told us (Claude's `~/.claude/sessions/<pid>.json`).
    HarnessRegistry,
    /// A `--resume <id>` style argument on the command line.
    CommandLine,
    /// Matched by working directory and start time; may be wrong with concurrent sessions.
    CwdHeuristic,
    /// No transcript found; process only.
    None,
    /// No process; transcript only.
    TranscriptOnly,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HostStats {
    pub hostname: Option<String>,
    pub cpu_percent: f32,
    pub cpu_count: usize,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub agents: usize,
    pub running: usize,
    pub idle: usize,
    pub stopped: usize,
    pub tokens: u64,
    pub cost_usd: f64,
    pub unpriced_tokens: u64,
    pub processes: usize,
    pub mcp_processes: usize,
    pub orphaned_mcp: usize,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
}

/// Everything the UI needs for one frame.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub taken_at: SystemTime,
    pub host: HostStats,
    pub agents: Vec<Agent>,
    /// MCP-looking processes with no live agent ancestor: leak candidates.
    pub orphans: Vec<ProcNode>,
    pub totals: Totals,
}

impl Snapshot {
    pub fn compute_totals(&mut self) {
        let mut t = Totals::default();
        for a in &self.agents {
            t.agents += 1;
            match a.state {
                AgentState::Running => t.running += 1,
                AgentState::Idle => t.idle += 1,
                AgentState::Stopped => t.stopped += 1,
            }
            t.tokens += a.usage.total();
            t.cost_usd += a.cost_usd;
            t.unpriced_tokens += a.unpriced_tokens;
            t.processes += a.process_count;
            t.mcp_processes += a.mcp_count;
            t.cpu_percent += a.cpu_percent;
            t.rss_bytes += a.rss_bytes;
        }
        t.orphaned_mcp = self.orphans.len();
        self.totals = t;
    }
}
