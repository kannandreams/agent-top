//! Number and text formatting shared by the TUI and `--once`.

use agent_top_core::{Agent, ProcNode, Snapshot};

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

pub fn cost(a: &Agent) -> String {
    if a.unpriced_tokens > 0 && a.cost_usd == 0.0 {
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
            "{:<24} {:<8} {:<8} {:>7} {:<14} {:>8} {:>8} {:>6.1} {:>7} {:>5} {:>5} {:>4} {:>7}\n",
            truncate(&a.name, 24),
            a.harness.label(),
            a.state.label(),
            a.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            short_model(a.model.as_deref()),
            tokens(a.usage.total()),
            cost(a),
            a.cpu_percent,
            bytes(a.rss_bytes),
            a.tool_calls,
            a.process_count,
            a.mcp_count,
            age(a.age_secs),
        ));
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
