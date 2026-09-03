//! Number and text formatting shared by the TUI and `--once`.

use agent_top_core::pricing::{Origin, Table};
use agent_top_core::{Agent, ProcNode, Snapshot};

/// CPU and memory belong to the process, and several conversations can share
/// one. Showing 0.0% on the rows that do not own it would read as an idle
/// agent rather than as "counted on the row above".
pub fn cpu_cell(a: &Agent) -> String {
    if a.shares_process {
        "·".to_string()
    } else if a.pid.is_some() {
        format!("{:.1}", a.cpu_percent)
    } else {
        "-".to_string()
    }
}

pub fn mem_cell(a: &Agent) -> String {
    if a.shares_process {
        "·".to_string()
    } else if a.rss_bytes > 0 {
        bytes(a.rss_bytes)
    } else {
        "-".to_string()
    }
}

/// The TOKENS cell: `?` when the parse is not to be trusted.
pub fn tokens_cell(a: &Agent) -> String {
    if a.parse_warning.is_some() { "?".to_string() } else { tokens(a.usage.total()) }
}

pub fn tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f >= K * K * K {
        format!("{:.1}G", f / (K * K * K))
    } else if f >= K * K {
        format!("{:.0}M", f / (K * K))
    } else if f >= K {
        format!("{:.0}K", f / K)
    } else {
        format!("{n}B")
    }
}

pub fn age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{:02}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// A tool call's duration, at the precision a human reads at a glance.
pub fn duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{}h{:02}m", ms / 3_600_000, (ms % 3_600_000) / 60_000)
    }
}

pub fn cost(a: &Agent) -> String {
    // Never print a number for a row we know is wrong: "$0.00" reads as a cheap
    // session, which is exactly the wrong conclusion.
    if a.parse_warning.is_some() {
        "?".to_string()
    } else if a.unpriced_tokens > 0 && a.cost_usd == 0.0 {
        "n/a".to_string()
    } else if a.unpriced_tokens > 0 {
        format!("≥${:.2}", a.cost_usd)
    } else {
        format!("${:.2}", a.cost_usd)
    }
}

pub fn short_model(m: Option<&str>) -> String {
    match m {
        None => "-".to_string(),
        Some(m) => {
            let m = m.strip_prefix("claude-").unwrap_or(m);
            let mut s = m.to_string();
            if s.len() > 14 {
                s.truncate(14);
            }
            s
        }
    }
}

pub fn short_cmd(node: &ProcNode, width: usize) -> String {
    let mut s = node.cmdline.replace('\n', " ");
    if let Some(home) = std::env::var_os("HOME") {
        s = s.replace(&*home.to_string_lossy(), "~");
    }
    if s.chars().count() > width {
        s = s.chars().take(width.saturating_sub(1)).collect::<String>() + "…";
    }
    s
}

/// Plain table for `--once` and for non-tty use.
pub fn plain_table(snap: &Snapshot) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<24} {:<8} {:<8} {:>7} {:<14} {:>8} {:>8} {:>6} {:>7} {:>5} {:>5} {:>4} {:>7}\n",
        "AGENT", "HARNESS", "STATE", "PID", "MODEL", "TOKENS", "COST", "CPU%", "MEM", "TOOLS", "PROCS", "MCP", "AGE"
    ));
    for a in &snap.agents {
        out.push_str(&format!(
            "{:<24} {:<8} {:<8} {:>7} {:<14} {:>8} {:>8} {:>6} {:>7} {:>5} {:>5} {:>4} {:>7}\n",
            truncate(&a.name, 24),
            a.harness.label(),
            a.state.label(),
            a.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            short_model(a.model.as_deref()),
            tokens_cell(a),
            cost(a),
            cpu_cell(a),
            mem_cell(a),
            a.tool_calls,
            if a.shares_process { "·".into() } else { a.process_count.to_string() },
            a.mcp_count,
            age(a.age_secs),
        ));
    }
    let warned: Vec<&Agent> = snap.agents.iter().filter(|a| a.parse_warning.is_some()).collect();
    if !warned.is_empty() {
        out.push_str("\nWARNING\n");
        for a in warned {
            out.push_str(&format!("  {}: {}\n", a.name, a.parse_warning.as_deref().unwrap_or_default()));
        }
    }
    if !snap.orphans.is_empty() {
        out.push_str("\nORPHANED MCP PROCESSES (no live agent ancestor)\n");
        for o in &snap.orphans {
            out.push_str(&format!("  {:>7} {:>7} {:>7}  {}\n", o.pid, bytes(o.rss_bytes), age(o.age_secs), short_cmd(o, 100)));
        }
    }
    let t = &snap.totals;
    out.push_str(&format!(
        "\n{} agents ({} running, {} idle, {} stopped) · {} tokens · ${:.2}{} · {} procs · {} mcp · {} orphaned\n",
        t.agents,
        t.running,
        t.idle,
        t.stopped,
        tokens(t.tokens),
        t.cost_usd,
        if t.unpriced_tokens > 0 { "+" } else { "" },
        t.processes,
        t.mcp_processes,
        t.orphaned_mcp
    ));
    out
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "…" }
}

/// `--prices`: the effective table, longest prefix first, so a user can see at
/// a glance which of their overrides took effect and what is still unpriced.
pub fn price_table(t: &Table) -> String {
    let mut out = String::new();
    out.push_str("USD per million tokens\n\n");
    out.push_str(&format!(
        "{:<22} {:>8} {:>8} {:>10} {:>10} {:>10}  {}\n",
        "MODEL PREFIX", "INPUT", "OUTPUT", "CACHE RD", "CACHE 5m", "CACHE 1h", "SOURCE"
    ));
    let mut entries: Vec<_> = t.entries.iter().collect();
    entries.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()).then_with(|| a.prefix.cmp(&b.prefix)));
    for e in entries {
        let p = &e.price;
        out.push_str(&format!(
            "{:<22} {:>8.2} {:>8.2} {:>10.3} {:>10.3} {:>10.3}  {}\n",
            e.prefix,
            p.input,
            p.output,
            p.cache_read,
            p.cache_write_5m,
            p.cache_write_1h,
            match e.origin {
                Origin::Builtin => "built-in",
                Origin::User => "your file",
            }
        ));
    }
    out.push_str(&format!("\nbuilt-in table checked {}\n", t.updated.as_deref().unwrap_or("(undated)")));
    match &t.user_path {
        Some(p) if t.warnings.is_empty() => out.push_str(&format!("overrides from {}\n", p.display())),
        Some(p) => out.push_str(&format!("no overrides applied from {}\n", p.display())),
        None => out.push_str("no user price file; set one at ~/.config/agent-top/prices.toml\n"),
    }
    for w in &t.warnings {
        out.push_str(&format!("warning: {w}\n"));
    }
    out.push_str("\nA model with no entry here is counted but reported as unpriced, never guessed at.\n");
    out
}
