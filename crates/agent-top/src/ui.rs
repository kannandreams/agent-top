//! Rendering. Layout, top to bottom: header (host gauges + totals), agent
//! table, optional detail pane (process tree + token breakdown), key bar.

use crate::app::App;
use crate::format::{age, bytes, cost, short_cmd, short_model, tokens, truncate};
use agent_top_core::{Agent, AgentState, Attribution, ProcKind, ProcNode};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Clear, Gauge, Paragraph, Row, Sparkline, Table, TableState, Wrap};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

fn state_style(s: AgentState) -> Style {
    match s {
        AgentState::Running => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        AgentState::Idle => Style::default().fg(Color::Yellow),
        AgentState::Stopped => Style::default().fg(DIM),
    }
}

fn level_color(pct: f64) -> Color {
    if pct >= 85.0 {
        Color::Red
    } else if pct >= 60.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn block(title: &str) -> Block<'_> {
    Block::bordered().border_style(Style::default().fg(DIM)).title(Line::from(vec![
        Span::raw(" "),
        Span::styled(title, Style::default().fg(ACCENT).bold()),
        Span::raw(" "),
    ]))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let detail_h = if app.show_detail { Constraint::Percentage(40) } else { Constraint::Length(0) };
    let [header, table, detail, footer] =
        Layout::vertical([Constraint::Length(6), Constraint::Min(4), detail_h, Constraint::Length(1)]).areas(area);
    draw_header(f, app, header);
    draw_table(f, app, table);
    if app.show_detail {
        draw_detail(f, app, detail);
    }
    draw_footer(f, app, footer);
    if app.show_help {
        draw_help(f, area);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let snap = &app.snapshot;
    let host = &snap.host;
    let title = format!(
        "agent-top{}{}",
        host.hostname.as_deref().map(|h| format!(" @ {h}")).unwrap_or_default(),
        if app.paused { "  [PAUSED]" } else { "" }
    );
    let outer = block(&title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner);
    let [cpu_row, mem_row, spark_row, _] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(left);

    let cpu = host.cpu_percent as f64;
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(level_color(cpu)).bg(Color::Black))
            .ratio((cpu / 100.0).clamp(0.0, 1.0))
            .label(format!("cpu {cpu:>5.1}%  ({} cores)", host.cpu_count)),
        cpu_row,
    );
    let mem_pct = if host.mem_total_bytes > 0 { host.mem_used_bytes as f64 * 100.0 / host.mem_total_bytes as f64 } else { 0.0 };
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(level_color(mem_pct)).bg(Color::Black))
            .ratio((mem_pct / 100.0).clamp(0.0, 1.0))
            .label(format!("mem {:>5.1}%  {} / {}", mem_pct, bytes(host.mem_used_bytes), bytes(host.mem_total_bytes))),
        mem_row,
    );
    let [spark_label, spark] = Layout::horizontal([Constraint::Length(11), Constraint::Min(4)]).areas(spark_row);
    f.render_widget(Paragraph::new(Span::styled("tokens/s ", Style::default().fg(DIM))), spark_label);
    f.render_widget(Sparkline::default().data(&app.tokens_per_tick).style(Style::default().fg(ACCENT)), spark);

    let t = &snap.totals;
    let agents_cpu = t.cpu_percent;
    let lines = vec![
        Line::from(vec![
            Span::styled("agents ", Style::default().fg(DIM)),
            Span::styled(t.agents.to_string(), Style::default().bold()),
            Span::raw("   "),
            Span::styled(format!("{} running", t.running), Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(format!("{} idle", t.idle), Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled(format!("{} stopped", t.stopped), Style::default().fg(DIM)),
        ]),
        Line::from(vec![
            Span::styled("tokens ", Style::default().fg(DIM)),
            Span::styled(tokens(t.tokens), Style::default().bold()),
            Span::raw("   "),
            Span::styled("cost ", Style::default().fg(DIM)),
            Span::styled(
                format!("${:.2}{}", t.cost_usd, if t.unpriced_tokens > 0 { "+" } else { "" }),
                Style::default().fg(Color::Magenta).bold(),
            ),
            if t.unpriced_tokens > 0 {
                Span::styled(format!("  ({} tokens unpriced)", tokens(t.unpriced_tokens)), Style::default().fg(DIM))
            } else {
                Span::raw("")
            },
        ]),
        Line::from(vec![
            Span::styled("procs ", Style::default().fg(DIM)),
            Span::raw(t.processes.to_string()),
            Span::raw("   "),
            Span::styled("mcp ", Style::default().fg(DIM)),
            Span::raw(t.mcp_processes.to_string()),
            Span::raw("   "),
            Span::styled("orphaned mcp ", Style::default().fg(DIM)),
            if t.orphaned_mcp > 0 {
                Span::styled(t.orphaned_mcp.to_string(), Style::default().fg(Color::Red).bold())
            } else {
                Span::raw("0")
            },
            Span::raw("   "),
            Span::styled("agent cpu ", Style::default().fg(DIM)),
            Span::raw(format!("{agents_cpu:.1}%")),
            Span::raw("  "),
            Span::styled("agent rss ", Style::default().fg(DIM)),
            Span::raw(bytes(t.rss_bytes)),
        ]),
        Line::from(vec![
            Span::styled("sort ", Style::default().fg(DIM)),
            Span::styled(app.sort.label(), Style::default().fg(ACCENT)),
            Span::raw(if app.sort_desc { " ↑" } else { " ↓" }),
            Span::raw("   "),
            Span::styled("stopped ", Style::default().fg(DIM)),
            Span::raw(if app.show_stopped { "shown" } else { "hidden" }),
        ]),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), right);
}

fn draw_table(f: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(
        ["AGENT", "HARNESS", "STATE", "PID", "MODEL", "TOKENS", "COST", "CPU%", "MEM", "TOOLS", "PROCS", "MCP", "AGE"]
            .into_iter()
            .map(|h| Cell::from(h).style(Style::default().fg(ACCENT).bold())),
    )
    .bottom_margin(0);

    let rows = app.rows.iter().map(|a| {
        let mem = if a.rss_bytes > 0 { bytes(a.rss_bytes) } else { "-".into() };
        let cpu = if a.pid.is_some() { format!("{:.1}", a.cpu_percent) } else { "-".into() };
        let mcp_style = if a.mcp_count > 0 { Style::default().fg(Color::Magenta) } else { Style::default() };
        Row::new(vec![
            Cell::from(truncate(&a.name, 26)).style(Style::default().bold()),
            Cell::from(a.harness.label()),
            Cell::from(a.state.label()).style(state_style(a.state)),
            Cell::from(a.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into())),
            Cell::from(short_model(a.model.as_deref())),
            Cell::from(tokens(a.usage.total())),
            Cell::from(cost(a)).style(Style::default().fg(Color::Magenta)),
            Cell::from(cpu),
            Cell::from(mem),
            Cell::from(a.tool_calls.to_string()),
            Cell::from(if a.pid.is_some() { a.process_count.to_string() } else { "-".into() }),
            Cell::from(a.mcp_count.to_string()).style(mcp_style),
            Cell::from(age(a.age_secs)),
        ])
        .style(if a.state == AgentState::Stopped { Style::default().fg(DIM) } else { Style::default() })
    });

    let widths = [
        Constraint::Min(14),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(7),
    ];
    let title = format!("agents ({})", app.rows.len());
    let table = Table::new(rows, widths)
        .header(header)
        .block(block(&title))
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(Color::Rgb(40, 50, 70)).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(if app.rows.is_empty() { None } else { Some(app.selected) });
    f.render_stateful_widget(table, area, &mut state);

    if app.rows.is_empty() {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled("no coding agents found. ", Style::default().fg(DIM)),
            Span::styled("start claude / codex in another terminal, or run agent-top --json to debug discovery.", Style::default().fg(DIM)),
        ]))
        .wrap(Wrap { trim: true });
        let inner = Rect { x: area.x + 2, y: area.y + 2, width: area.width.saturating_sub(4), height: 2 };
        f.render_widget(msg, inner);
    }
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(a) = app.selected_agent() else {
        f.render_widget(block("detail"), area);
        return;
    };
    let title = format!("{} · {} · {}", a.name, a.harness.label(), a.state.label());
    let outer = block(&title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner);
    f.render_widget(Paragraph::new(agent_facts(a)).wrap(Wrap { trim: false }), left);
    f.render_widget(Paragraph::new(process_tree(a, &app.snapshot.orphans, right.width as usize)), right);
}

fn kv<'a>(k: &'a str, v: String) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<11}"), Style::default().fg(DIM)), Span::raw(v)])
}

fn agent_facts(a: &Agent) -> Text<'static> {
    let u = &a.usage;
    let attribution = match a.attribution {
        Attribution::HarnessRegistry => "harness registry (exact)",
        Attribution::CommandLine => "command line --resume (exact)",
        Attribution::CwdHeuristic => "cwd + start time (heuristic)",
        Attribution::None => "none (process only)",
        Attribution::TranscriptOnly => "transcript only (no process)",
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let tilde = |p: &std::path::Path| p.to_string_lossy().replacen(&home, "~", 1);
    let mut lines = vec![
        kv("session", a.session_id.clone().unwrap_or_else(|| "-".into())),
        kv("cwd", a.cwd.as_deref().map(tilde).unwrap_or_else(|| "-".into())),
        kv("model", a.model.clone().unwrap_or_else(|| "-".into())),
        kv("version", a.harness_version.clone().unwrap_or_else(|| "-".into())),
        kv("activity", format!("{:?}{}", a.activity, a.idle_secs.map(|s| format!(", last write {} ago", age(s))).unwrap_or_default())),
        kv("attributed", attribution.to_string()),
        Line::raw(""),
        Line::from(vec![Span::styled("tokens", Style::default().fg(ACCENT).bold())]),
        kv("  input", tokens(u.input)),
        kv("  cache rd", tokens(u.cache_read)),
        kv("  cache wr", format!("{} (5m {}, 1h {})", tokens(u.cache_write()), tokens(u.cache_write_5m), tokens(u.cache_write_1h))),
        kv("  output", tokens(u.output)),
        kv("  total", tokens(u.total())),
        kv(
            "cost",
            format!(
                "{}{}",
                cost(a),
                if a.unpriced_tokens > 0 { format!("  ({} tokens unpriced)", tokens(a.unpriced_tokens)) } else { String::new() }
            ),
        ),
        kv("turns", format!("{} ({} subagent)", a.turns, a.subagent_turns)),
        kv("tool calls", a.tool_calls.to_string()),
    ];
    if let Some(p) = &a.session_path {
        lines.push(kv("transcript", tilde(p)));
    }
    Text::from(lines)
}

fn process_tree(a: &Agent, orphans: &[ProcNode], width: usize) -> Text<'static> {
    let mut lines = vec![Line::from(vec![
        Span::styled("process tree", Style::default().fg(ACCENT).bold()),
        Span::styled(
            format!("   {} procs · {} mcp · cpu {:.1}% · rss {}", a.process_count, a.mcp_count, a.cpu_percent, bytes(a.rss_bytes)),
            Style::default().fg(DIM),
        ),
    ])];
    match &a.tree {
        None => lines.push(Line::styled("  (no live process)", Style::default().fg(DIM))),
        Some(root) => render_node(root, "", true, true, width, &mut lines),
    }
    if !orphans.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("orphaned mcp processes", Style::default().fg(Color::Red).bold()),
            Span::styled("  (no live agent ancestor; likely leaked)", Style::default().fg(DIM)),
        ]));
        for o in orphans.iter().take(8) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:>6} ", o.pid), Style::default().fg(Color::Red)),
                Span::styled(format!("{:>6} {:>6}  ", bytes(o.rss_bytes), age(o.age_secs)), Style::default().fg(DIM)),
                Span::raw(short_cmd(o, width.saturating_sub(24))),
            ]));
        }
        if orphans.len() > 8 {
            lines.push(Line::styled(format!("  … {} more", orphans.len() - 8), Style::default().fg(DIM)));
        }
    }
    Text::from(lines)
}

fn render_node(n: &ProcNode, prefix: &str, last: bool, root: bool, width: usize, out: &mut Vec<Line<'static>>) {
    let branch = if root {
        ""
    } else if last {
        "└─ "
    } else {
        "├─ "
    };
    let (tag, style) = match n.kind {
        ProcKind::Agent => ("agent", Style::default().fg(Color::Green).bold()),
        ProcKind::Subagent => ("subagent", Style::default().fg(Color::Green)),
        ProcKind::Mcp => ("mcp", Style::default().fg(Color::Magenta).bold()),
        ProcKind::Shell => ("shell", Style::default().fg(Color::Blue)),
        ProcKind::Tool => ("tool", Style::default().fg(Color::White)),
    };
    let head = format!("{prefix}{branch}");
    let stats = format!(" {:>6} {:>5.1}% {:>6} {:>5} ", n.pid, n.cpu_percent, bytes(n.rss_bytes), age(n.age_secs));
    let used = head.chars().count() + tag.len() + stats.len() + 2;
    let cmd = short_cmd(n, width.saturating_sub(used).max(8));
    out.push(Line::from(vec![
        Span::styled(head, Style::default().fg(DIM)),
        Span::styled(format!("[{tag}]"), style),
        Span::styled(stats, Style::default().fg(DIM)),
        Span::raw(cmd),
    ]));
    let child_prefix = if root { String::new() } else { format!("{prefix}{}", if last { "   " } else { "│  " }) };
    let n_children = n.children.len();
    for (i, c) in n.children.iter().enumerate() {
        render_node(c, &child_prefix, i + 1 == n_children, false, width, out);
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let key = |k: &str, d: &str| -> Vec<Span<'static>> {
        vec![
            Span::styled(k.to_string(), Style::default().fg(Color::Black).bg(ACCENT)),
            Span::styled(format!(" {d} "), Style::default().fg(DIM)),
        ]
    };
    let mut spans = Vec::new();
    spans.extend(key("↑↓/jk", "select"));
    spans.extend(key("s", format!("sort:{}", app.sort.label()).as_str()));
    spans.extend(key("r", "reverse"));
    spans.extend(key("t", if app.show_detail { "hide detail" } else { "show detail" }));
    spans.extend(key("x", if app.show_stopped { "hide stopped" } else { "show stopped" }));
    spans.extend(key("p", if app.paused { "resume" } else { "pause" }));
    spans.extend(key("?", "help"));
    spans.extend(key("q", "quit"));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let w = 62.min(area.width.saturating_sub(2));
    let h = 20.min(area.height.saturating_sub(2));
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);
    let text = Text::from(vec![
        Line::from(vec![Span::styled("keys", Style::default().fg(ACCENT).bold())]),
        Line::raw("  ↑ ↓ j k      move selection      g G     first / last"),
        Line::raw("  s            cycle sort column   r       reverse sort"),
        Line::raw("  t / Enter    toggle detail pane  x       toggle stopped rows"),
        Line::raw("  p / Space    pause refresh       q / Esc quit"),
        Line::raw(""),
        Line::from(vec![Span::styled("columns", Style::default().fg(ACCENT).bold())]),
        Line::raw("  STATE   running = mid-turn, idle = waiting for you,"),
        Line::raw("          stopped = transcript with no live process"),
        Line::raw("  TOKENS  input + cache read + cache write + output"),
        Line::raw("  COST    USD at list price; '+' or '≥' = some tokens unpriced"),
        Line::raw("  PROCS   processes in the agent's tree; MCP = of those,"),
        Line::raw("          Model Context Protocol servers (name heuristic)"),
        Line::raw("  AGE     process age, or time since last write when stopped"),
        Line::raw(""),
        Line::from(vec![
            Span::styled("orphaned mcp", Style::default().fg(Color::Red).bold()),
            Span::raw("  MCP-looking processes whose agent is gone."),
        ]),
        Line::raw("  Inspect with `agent-top --json`; kill with `kill <pid>`."),
    ]);
    f.render_widget(Paragraph::new(text).block(block("help")).wrap(Wrap { trim: false }), popup);
}
