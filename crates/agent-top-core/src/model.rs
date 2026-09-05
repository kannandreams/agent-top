//! The data model shared by discovery, the TUI and the JSON output.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

/// Which agent harness a process or transcript belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
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

/// What a span measures. Tool calls came first and gave the type its name;
/// the other two label the time between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum SpanKind {
    /// One tool call, from the harness issuing it to the result coming back.
    #[default]
    Tool,
    /// The model producing a response: from a prompt or tool results being
    /// submitted to the last block of the reply being written. The gap in a
    /// waterfall that is not a tool call is almost always one of these.
    Inference,
    /// One human turn: from the prompt to the model ending its reply.
    /// Contains every tool call and inference span issued in between.
    Turn,
}

impl SpanKind {
    pub const ALL: [SpanKind; 3] = [SpanKind::Tool, SpanKind::Inference, SpanKind::Turn];

    pub fn label(self) -> &'static str {
        match self {
            SpanKind::Tool => "tool",
            SpanKind::Inference => "inference",
            SpanKind::Turn => "turn",
        }
    }
}

/// One span of an agent trace: a tool call, an inference, or a turn.
///
/// Every harness writes the same shape in its own vocabulary — Claude pairs a
/// `tool_use` block with a `tool_result` block by `tool_use_id`, Codex pairs a
/// `function_call` with a `function_call_output` by `call_id` — and both stamp
/// each line with a timestamp. That is a span: a name, a start and a duration.
/// Only the call's metadata is kept; arguments and output are never read.
///
/// The name predates `kind`: the type carried only tool calls until 0.3.1 and
/// is kept for the sake of the published API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpan {
    /// The harness's own call id, so a span survives across refreshes. For
    /// inference and turn spans, a counter the parser assigns.
    pub id: String,
    /// Tool name as the harness reports it (`Bash`, `exec_command`, ...), or
    /// `inference` / `turn`.
    pub name: String,
    pub started_at: SystemTime,
    /// Wall-clock duration, or `None` while the call is still in flight.
    pub duration_ms: Option<u64>,
    /// The call was issued by a subagent (Claude's `isSidechain`).
    pub sidechain: bool,
    /// The harness reported the result as an error.
    pub error: bool,
    /// Absent in snapshots written before 0.3.1, which held tool calls only.
    #[serde(default)]
    pub kind: SpanKind,
}

impl ToolSpan {
    pub fn is_open(&self) -> bool {
        self.duration_ms.is_none()
    }

    /// Duration if closed, otherwise how long it has been running as of `now`.
    pub fn elapsed_ms(&self, now: SystemTime) -> u64 {
        match self.duration_ms {
            Some(ms) => ms,
            None => now.duration_since(self.started_at).map(|d| d.as_millis() as u64).unwrap_or(0),
        }
    }

    pub fn ended_at(&self, now: SystemTime) -> SystemTime {
        self.started_at + std::time::Duration::from_millis(self.elapsed_ms(now))
    }
}

/// One MCP server an agent uses, seen from either side or both: the process
/// table has the server's pid, CPU and memory; the transcript has how often
/// the agent called it. Claude Code names an MCP tool `mcp__<server>__<tool>`,
/// which is where the server name and the call count come from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServer {
    /// The server's name as the harness configured it, or, for a process the
    /// transcript never named, the program it is running.
    pub name: String,
    pub pid: Option<u32>,
    pub cmdline: Option<String>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub age_secs: Option<u64>,
    /// Tool calls the agent made to this server, from the transcript.
    pub calls: u64,
    /// Of those, how many the harness reported as errors.
    pub errors: u64,
    pub last_call: Option<SystemTime>,
    /// How the process and the transcript's server were put together, for
    /// the UI to label a guess as one.
    pub matched_by: McpMatch,
}

/// How an `McpServer` row was formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpMatch {
    /// A process under the agent that no transcript server names. Either it
    /// has not been called yet, or its name does not appear in its command.
    ProcessOnly,
    /// A server the transcript calls with no process under the agent: an HTTP
    /// server, or one that has exited.
    TranscriptOnly,
    /// The server's name appears in the process's command line.
    Name,
    /// One unmatched process and one unmatched server were left; they are
    /// taken to be the same. A guess.
    Sole,
}

/// Role of a process inside an agent's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// CPU, RSS, process count and MCP server count summed over the whole
    /// subtree. An MCP process's own children (an `npx` wrapper's `node`) are
    /// the same server, so they add to the process count and not to the
    /// server count.
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
            if self.kind != ProcKind::Mcp {
                mcp += cmcp;
            }
        }
        (cpu, rss, count, mcp)
    }

    /// The MCP servers in this tree: each `Mcp` node whose parent is not one.
    /// A server started through `npx` or `uvx` is two or three processes; the
    /// top one stands for the server.
    pub fn mcp_roots(&self) -> Vec<&ProcNode> {
        let mut out = Vec::new();
        self.collect_mcp_roots(&mut out);
        out
    }

    fn collect_mcp_roots<'a>(&'a self, out: &mut Vec<&'a ProcNode>) {
        if self.kind == ProcKind::Mcp {
            out.push(self);
            return;
        }
        for c in &self.children {
            c.collect_mcp_roots(out);
        }
    }

    pub fn walk<'a>(&'a self, depth: usize, f: &mut dyn FnMut(&'a ProcNode, usize)) {
        f(self, depth);
        for c in &self.children {
            c.walk(depth + 1, f);
        }
    }
}

/// A single row in the agent table.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// `cost_usd` by kind of token, so a figure that differs from another
    /// tool's can be traced to the one line that differs.
    #[serde(default)]
    pub cost_breakdown: CostBreakdown,
    /// Where the price of this row's model came from; `None` when it has none.
    #[serde(default)]
    pub price_source: Option<PriceSource>,
    /// Tokens on messages whose model had no known price (so `cost_usd` is a floor).
    pub unpriced_tokens: u64,
    pub turns: u64,
    pub subagent_turns: u64,
    pub tool_calls: u64,
    /// Server-side web searches the model ran, billed per search on top of
    /// tokens. Counted for every harness; priced only where the price table
    /// has a rate (Anthropic's, for Claude Code).
    #[serde(default)]
    pub web_searches: u64,
    /// The most recent spans, oldest first: tool calls, inferences and turns.
    /// Bounded; see `harness::MAX_SPANS`.
    pub spans: Vec<ToolSpan>,
    /// Seconds since the process started (live) or since the last transcript write (stopped).
    pub age_secs: u64,
    /// Seconds since the transcript was last written.
    pub idle_secs: Option<u64>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub process_count: usize,
    pub mcp_count: usize,
    /// The MCP servers this agent uses, one row each, from the process tree
    /// and the transcript. See `McpServer`.
    #[serde(default)]
    pub mcp_servers: Vec<McpServer>,
    pub tree: Option<ProcNode>,
    /// How the session was attributed to the process, for debugging attribution.
    pub attribution: Attribution,
    /// True when another row shares this pid and carries its CPU, memory and
    /// process counts. One Codex app-server hosts many conversations, so its
    /// threads each get a row while the process is only counted once.
    /// Absent in snapshots written before 0.2.0.
    #[serde(default)]
    pub shares_process: bool,
    /// Set when the transcript parsed but its usage records did not, which
    /// means this row's tokens and cost are not to be believed. Almost always a
    /// harness that changed its format under us.
    pub parse_warning: Option<String>,
}

/// USD by kind of token, accumulated message by message at each message's
/// own model price, so a session that changed model part way is still exact.
/// The lines sum to `Agent::cost_usd`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct CostBreakdown {
    pub input: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
    pub output: f64,
    /// Server-side web searches, billed per search on top of the tokens.
    pub web_search: f64,
}

impl CostBreakdown {
    pub fn total(&self) -> f64 {
        self.input + self.cache_write_5m + self.cache_write_1h + self.cache_read + self.output + self.web_search
    }

    pub fn add(&mut self, o: &CostBreakdown) {
        self.input += o.input;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
        self.cache_read += o.cache_read;
        self.output += o.output;
        self.web_search += o.web_search;
    }

    pub fn sub(&mut self, o: &CostBreakdown) {
        self.input -= o.input;
        self.cache_write_5m -= o.cache_write_5m;
        self.cache_write_1h -= o.cache_write_1h;
        self.cache_read -= o.cache_read;
        self.output -= o.output;
        self.web_search -= o.web_search;
    }
}

/// Where a model's price came from. The built-in table carries list prices;
/// a user's file is whatever they chose to write, and the UI says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PriceSource {
    Builtin,
    UserFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attribution {
    /// The harness told us (Claude's `~/.claude/sessions/<pid>.json`).
    HarnessRegistry,
    /// A `--resume <id>` style argument on the command line.
    CommandLine,
    /// The process has the transcript open (Codex keeps every live thread's
    /// rollout open); exact.
    OpenFile,
    /// Matched by working directory and start time; may be wrong with concurrent sessions.
    CwdHeuristic,
    /// No transcript found; process only.
    None,
    /// No process; transcript only.
    TranscriptOnly,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostStats {
    pub hostname: Option<String>,
    pub cpu_percent: f32,
    pub cpu_count: usize,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// Version of the `--json` document. Bumped when a field changes meaning or
/// disappears; new fields alone do not bump it.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// What the collector remembers about an orphaned MCP process: when it first
/// saw it, and, if it watched the process lose its parent, which agent that
/// was. Memory lasts for the run; a process that was already an orphan when
/// agent-top started has no parent on record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrphanOrigin {
    pub pid: u32,
    pub first_seen: SystemTime,
    /// When the process was first seen without its parent, if the parent was
    /// seen before that.
    pub orphaned_at: Option<SystemTime>,
    pub parent: Option<OrphanParent>,
}

/// The agent an orphan used to belong to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrphanParent {
    pub pid: u32,
    pub agent_id: String,
    pub name: String,
}

/// Everything the UI needs for one frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub taken_at: SystemTime,
    pub host: HostStats,
    pub agents: Vec<Agent>,
    /// MCP-looking processes with no live agent ancestor: leak candidates.
    pub orphans: Vec<ProcNode>,
    /// One entry per orphan, saying where it came from when that is known.
    #[serde(default)]
    pub orphan_origins: Vec<OrphanOrigin>,
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
