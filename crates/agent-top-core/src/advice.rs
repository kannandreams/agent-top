//! Advice: the meter read for you.
//!
//! Everything else in agent-top reports a number. This module applies a few
//! rules to numbers already on the snapshot and says, in one sentence each,
//! what looks like a bad deal and what you could do about it. It changes
//! nothing: agent-top does not kill a process or edit a config, it points.
//!
//! Every rule fires only with its evidence in the sentence, because the same
//! figure is bad for one tool and fine for another: a search server that
//! cost $12 for three calls is a poor bargain, a test runner may not be. The
//! thresholds are the constants below, each with the reason it is where it is.
//!
//! Rules apply to live agents only. A stopped session's re-reads are over,
//! and its servers are gone, so there is nothing left to act on.

use crate::model::{Advice, AdviceRule, AgentState, ContextOrigin, McpMatch, Snapshot};
use std::collections::HashMap;
use std::time::SystemTime;

/// A tool result this large is a whole file or a whole page, not an answer:
/// about the size of a 1,000 line source file. Below it, results are the
/// normal cost of doing work and no rule fires.
pub const BIG_RESULT_TOKENS: u64 = 8_000;
/// A source must have added at least this much to the prompt before its
/// results are worth a sentence, whatever their size per call.
pub const MIN_SOURCE_TOKENS: u64 = 40_000;
/// An attached server that has answered nothing for this long is not warming
/// up, it is unused. Ten minutes covers a long first turn.
pub const IDLE_SERVER_SECS: u64 = 10 * 60;
/// Memory a server must gain over the window before it is called growth:
/// enough to matter, and more than a page cache warming up.
pub const GROWTH_BYTES: u64 = 64 << 20;
/// ... and that gain must be at least half its starting size, so a server
/// that was large from the start is not flagged for a small drift.
pub const GROWTH_RATIO: f64 = 1.5;
/// A trend shorter than this is a burst, not a leak.
pub const GROWTH_WINDOW_SECS: u64 = 10 * 60;

/// How one MCP process's memory has moved over the collector's window, for
/// the leak rule. Built by the collector from its per-tick samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RssTrend {
    /// When the oldest sample in the window was taken.
    pub since: SystemTime,
    pub from_bytes: u64,
    pub to_bytes: u64,
    /// No sample fell more than a few percent below the one before it: the
    /// memory climbed rather than churned.
    pub steady: bool,
    /// Bytes gained over roughly the last ten minutes: a trend that has
    /// flattened is a server that loaded something, not one that leaks.
    pub recent_growth: u64,
}

/// Every piece of advice the snapshot supports, most expensive first.
pub fn advise(snap: &Snapshot, trends: &HashMap<u32, RssTrend>) -> Vec<Advice> {
    let mut out = Vec::new();
    for a in snap.agents.iter().filter(|a| a.state != AgentState::Stopped) {
        let priced = a.price_source.is_some();
        for c in &a.context {
            if c.origin == ContextOrigin::Other || c.calls == 0 {
                continue;
            }
            let per_call = c.tokens / c.calls;
            if per_call < BIG_RESULT_TOKENS || c.tokens < MIN_SOURCE_TOKENS {
                continue;
            }
            let cost = priced.then_some(c.cost_usd);
            let paid = match cost {
                Some(usd) => format!("re-read at a cost of ${usd:.2} since"),
                None => "re-read on every response since (unpriced model)".to_string(),
            };
            let (subject, headline, action) = match c.origin {
                ContextOrigin::Mcp => (
                    c.name.clone(),
                    format!("{} MCP server: {} added {} tokens to the prompt, {}", c.name, calls(c.calls), tokens(c.tokens), paid),
                    "ask it for smaller results, or drop it from this project's MCP config".to_string(),
                ),
                _ => (
                    c.name.clone(),
                    format!(
                        "{}: {} added {} tokens to the prompt ({} each), {}",
                        c.name,
                        calls(c.calls),
                        tokens(c.tokens),
                        tokens(per_call),
                        paid
                    ),
                    "read in smaller parts, or start a fresh session so the results stop being re-sent".to_string(),
                ),
            };
            out.push(Advice {
                agent_id: a.id.clone(),
                agent_name: a.name.clone(),
                rule: AdviceRule::ExpensiveSource,
                subject,
                pid: None,
                headline,
                action,
                cost_usd: cost.unwrap_or(0.0),
                tokens: c.tokens,
                calls: c.calls,
                rss_bytes: 0,
            });
        }

        // The idle rule trusts the join: a server the transcript calls but no
        // process could be matched to (`TranscriptOnly`) means some process
        // row is that server under another name, so no process-only row of
        // this agent can be called unused with any confidence.
        let join_complete = !a.mcp_servers.iter().any(|m| m.matched_by == McpMatch::TranscriptOnly);
        for m in &a.mcp_servers {
            let Some(pid) = m.pid else { continue };
            if join_complete && m.calls == 0 && a.turns > 0 && m.age_secs.unwrap_or(0) >= IDLE_SERVER_SECS {
                out.push(Advice {
                    agent_id: a.id.clone(),
                    agent_name: a.name.clone(),
                    rule: AdviceRule::IdleMcpServer,
                    subject: m.name.clone(),
                    pid: Some(pid),
                    headline: format!(
                        "{} (pid {pid}) has answered no calls in {} and holds {}",
                        m.name,
                        age(m.age_secs.unwrap_or(0)),
                        bytes(m.rss_bytes)
                    ),
                    action: "remove it from this project's MCP config; its tool definitions are sent with every response".to_string(),
                    cost_usd: 0.0,
                    tokens: 0,
                    calls: 0,
                    rss_bytes: m.rss_bytes,
                });
            }
            if let Some(t) = trends.get(&pid)
                && is_leak_shaped(t, snap.taken_at)
                && m.last_call.is_none_or(|c| c < t.since)
            {
                let over = snap.taken_at.duration_since(t.since).map(|d| d.as_secs()).unwrap_or(0);
                out.push(Advice {
                    agent_id: a.id.clone(),
                    agent_name: a.name.clone(),
                    rule: AdviceRule::GrowingMcpServer,
                    subject: m.name.clone(),
                    pid: Some(pid),
                    headline: format!(
                        "{} (pid {pid}) grew from {} to {} over {} with no calls",
                        m.name,
                        bytes(t.from_bytes),
                        bytes(t.to_bytes),
                        age(over)
                    ),
                    action: "a server that grows while unused is likely leaking; watch it, and kill it if it outlives its agent"
                        .to_string(),
                    cost_usd: 0.0,
                    tokens: 0,
                    calls: m.calls,
                    rss_bytes: t.to_bytes,
                });
            }
        }
    }
    // Money first, then the leaks, then the merely idle; ties by size.
    out.sort_by(|x, y| {
        rank(x.rule)
            .cmp(&rank(y.rule))
            .then(y.cost_usd.partial_cmp(&x.cost_usd).unwrap_or(std::cmp::Ordering::Equal))
            .then(y.tokens.cmp(&x.tokens))
            .then(y.rss_bytes.cmp(&x.rss_bytes))
    });
    out
}

fn rank(r: AdviceRule) -> u8 {
    match r {
        AdviceRule::ExpensiveSource => 0,
        AdviceRule::GrowingMcpServer => 1,
        AdviceRule::IdleMcpServer => 2,
    }
}

/// The leak rule's shape test: a long enough, steady, still-continuing climb
/// of at least `GROWTH_BYTES` and `GROWTH_RATIO`.
fn is_leak_shaped(t: &RssTrend, now: SystemTime) -> bool {
    let window = now.duration_since(t.since).map(|d| d.as_secs()).unwrap_or(0);
    let grown = t.to_bytes.saturating_sub(t.from_bytes);
    window >= GROWTH_WINDOW_SECS
        && t.steady
        && t.recent_growth > 0
        && grown >= GROWTH_BYTES
        && (t.to_bytes as f64) >= (t.from_bytes as f64) * GROWTH_RATIO
}

fn calls(n: u64) -> String {
    if n == 1 { "1 call".into() } else { format!("{n} calls") }
}

fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= 1024.0 * MB { format!("{:.1} GB", n as f64 / (1024.0 * MB)) } else { format!("{:.0} MB", n as f64 / MB) }
}

fn age(secs: u64) -> String {
    if secs >= 3600 { format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60) } else { format!("{}m", secs / 60) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::time::Duration;

    fn agent(id: &str) -> Agent {
        Agent {
            id: format!("pid:{id}"),
            name: format!("claude:{id}"),
            harness: Harness::Claude,
            state: AgentState::Running,
            activity: Activity::Working,
            pid: Some(1),
            session_id: None,
            session_path: None,
            cwd: None,
            model: Some("claude-fable-5-1".into()),
            harness_version: None,
            usage: TokenUsage::default(),
            cost_usd: 0.0,
            cost_breakdown: Default::default(),
            price_source: Some(PriceSource::Builtin),
            unpriced_tokens: 0,
            turns: 5,
            subagent_turns: 0,
            tool_calls: 0,
            web_searches: 0,
            spans: Vec::new(),
            age_secs: 3600,
            idle_secs: None,
            cpu_percent: 0.0,
            rss_bytes: 0,
            process_count: 1,
            mcp_count: 0,
            mcp_servers: Vec::new(),
            context: Vec::new(),
            tree: None,
            attribution: Attribution::HarnessRegistry,
            shares_process: false,
            parse_warning: None,
            rate_limit: None,
        }
    }

    fn source(name: &str, origin: ContextOrigin, calls: u64, tokens: u64, cost_usd: f64) -> ContextSource {
        ContextSource { name: name.into(), origin, calls, tokens, cost_usd }
    }

    fn server(name: &str, pid: u32, calls: u64, age_secs: u64, rss: u64, matched_by: McpMatch) -> McpServer {
        McpServer {
            name: name.into(),
            pid: Some(pid),
            cmdline: None,
            cpu_percent: 0.0,
            rss_bytes: rss,
            age_secs: Some(age_secs),
            calls,
            errors: 0,
            last_call: None,
            matched_by,
        }
    }

    fn snapshot(agents: Vec<Agent>) -> Snapshot {
        Snapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            taken_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10_000),
            host: HostStats::default(),
            agents,
            orphans: Vec::new(),
            orphan_origins: Vec::new(),
            advice: Vec::new(),
            totals: Totals::default(),
        }
    }

    #[test]
    fn an_oversized_result_is_flagged_with_its_cost_and_a_small_one_is_not() {
        let mut a = agent("proj");
        a.context = vec![
            // One MCP call that dumped 40k tokens: the headline example.
            source("docs-search", ContextOrigin::Mcp, 1, 40_000, 12.03),
            // Three whole-file reads.
            source("Read", ContextOrigin::Tool, 3, 120_000, 40.10),
            // Many small results, more tokens in total, but each is a normal answer.
            source("Bash", ContextOrigin::Tool, 200, 400_000, 90.0),
            // A big result that has not been re-read enough to matter yet.
            source("Grep", ContextOrigin::Tool, 1, 9_000, 0.02),
            source("prompts & replies", ContextOrigin::Other, 0, 900_000, 200.0),
        ];
        let out = advise(&snapshot(vec![a]), &HashMap::new());
        let subjects: Vec<&str> = out.iter().map(|x| x.subject.as_str()).collect();
        assert_eq!(subjects, ["Read", "docs-search"], "most expensive first: {out:#?}");
        assert!(out.iter().all(|x| x.rule == AdviceRule::ExpensiveSource));
        let read = &out[0];
        assert_eq!(read.headline, "Read: 3 calls added 120k tokens to the prompt (40k each), re-read at a cost of $40.10 since");
        assert!(read.action.contains("smaller parts"));
        let mcp = &out[1];
        assert_eq!(mcp.headline, "docs-search MCP server: 1 call added 40k tokens to the prompt, re-read at a cost of $12.03 since");
        assert!(mcp.action.contains("MCP config"));
        assert_eq!((mcp.calls, mcp.tokens, mcp.cost_usd), (1, 40_000, 12.03));
    }

    #[test]
    fn an_unpriced_model_still_gets_the_size_advice_without_a_dollar_figure() {
        let mut a = agent("proj");
        a.price_source = None;
        a.context = vec![source("Read", ContextOrigin::Tool, 1, 50_000, 0.0)];
        let out = advise(&snapshot(vec![a]), &HashMap::new());
        assert_eq!(out.len(), 1);
        assert!(out[0].headline.ends_with("(unpriced model)"), "{}", out[0].headline);
        assert!(!out[0].headline.contains('$'));
    }

    #[test]
    fn an_attached_server_with_no_calls_is_idle_unless_the_join_is_incomplete() {
        let mut a = agent("proj");
        a.mcp_servers = vec![
            server("server-filesystem", 11, 0, 42 * 60, 180 << 20, McpMatch::ProcessOnly),
            server("git", 12, 9, 42 * 60, 50 << 20, McpMatch::Name),
            // Just started: no verdict yet.
            server("fresh", 13, 0, 30, 20 << 20, McpMatch::ProcessOnly),
        ];
        let out = advise(&snapshot(vec![a.clone()]), &HashMap::new());
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].rule, AdviceRule::IdleMcpServer);
        assert_eq!(out[0].pid, Some(11));
        assert_eq!(out[0].headline, "server-filesystem (pid 11) has answered no calls in 42m and holds 180 MB");

        // A called server with no process means one of the process-only rows
        // is really that server; nothing can be called idle.
        a.mcp_servers.push(McpServer { pid: None, ..server("scratchfs", 0, 4, 0, 0, McpMatch::TranscriptOnly) });
        assert!(advise(&snapshot(vec![a.clone()]), &HashMap::new()).is_empty());

        // A stopped session has no servers to remove.
        a.mcp_servers.pop();
        a.state = AgentState::Stopped;
        assert!(advise(&snapshot(vec![a]), &HashMap::new()).is_empty());
    }

    #[test]
    fn a_server_that_keeps_growing_while_unused_is_a_leak_candidate() {
        let snap_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let mut a = agent("proj");
        let mut chrome = server("chrome-devtools", 77, 3, 3 * 3600, 410 << 20, McpMatch::Name);
        chrome.last_call = Some(snap_at - Duration::from_secs(2 * 3600));
        a.mcp_servers = vec![chrome];
        let trend = RssTrend {
            since: snap_at - Duration::from_secs(34 * 60),
            from_bytes: 120 << 20,
            to_bytes: 410 << 20,
            steady: true,
            recent_growth: 8 << 20,
        };
        let snap = snapshot(vec![a.clone()]);
        let out = advise(&snap, &HashMap::from([(77, trend)]));
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].rule, AdviceRule::GrowingMcpServer);
        assert_eq!(out[0].headline, "chrome-devtools (pid 77) grew from 120 MB to 410 MB over 34m with no calls");

        // Called during the window: it may be caching what it was asked for.
        let mut called = a.clone();
        called.mcp_servers[0].last_call = Some(snap_at - Duration::from_secs(60));
        assert!(advise(&snapshot(vec![called]), &HashMap::from([(77, trend)])).is_empty());
        // Flattened out: it loaded something and stopped.
        let flat = RssTrend { recent_growth: 0, ..trend };
        assert!(advise(&snap, &HashMap::from([(77, flat)])).is_empty());
        // Churning up and down is not a leak.
        let churn = RssTrend { steady: false, ..trend };
        assert!(advise(&snap, &HashMap::from([(77, churn)])).is_empty());
        // Too short a window, or too little growth.
        let short = RssTrend { since: snap_at - Duration::from_secs(5 * 60), ..trend };
        assert!(advise(&snap, &HashMap::from([(77, short)])).is_empty());
        let small = RssTrend { from_bytes: 380 << 20, ..trend };
        assert!(advise(&snap, &HashMap::from([(77, small)])).is_empty());
    }
}
