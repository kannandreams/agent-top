//! Rendering. Layout, top to bottom: header (host gauges + totals), agent
//! table, optional detail pane (process tree + token breakdown), key bar.

use crate::app::{App, DetailView, Overlay, Panel};
use crate::format::{
    age, bytes, cost, cpu_cell, duration_ms, mem_cell, nested_name, session_name, short_cmd, short_model, tokens, tokens_cell, tool_calls,
    truncate,
};
use crate::theme::{Ramp, Theme};
use agent_top_core::{Agent, AgentState, Attribution, McpMatch, OrphanOrigin, ProcKind, ProcNode, SpanKind, ToolSpan};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Wrap};
use std::time::{Duration, SystemTime};

/// Column widths of the totals block in the header. The label column is wide
/// enough that the longest label still leaves a gap before the number.
const STAT_LABEL_W: usize = 10;
const STAT_VALUE_W: usize = 7;

fn state_style(s: AgentState, theme: &Theme) -> Style {
    match s {
        AgentState::Running => Style::default().fg(theme.green).add_modifier(Modifier::BOLD),
        AgentState::Idle => Style::default().fg(theme.yellow),
        AgentState::Stopped => Style::default().fg(theme.dim),
    }
}

// ── meters ──────────────────────────────────────────────────────────────────
//
// btop's meters read as magnitude before you read the number, because colour
// is interpolated along the meter's own length: a bar that gets longer gets
// hotter. The texture comes from the seven-eighths block, which most terminal
// fonts render with a one-pixel gap at the cell's right edge, so a run of them
// looks like segments rather than one slab. Unfilled cells keep the same
// texture in the theme's surface colour, making the track visible as a channel.

/// Filled cell of a meter.
const METER_FULL: &str = "▉";
/// The tip of a bar that is still growing.
const METER_TIP: &str = "▸";
/// Unfilled cell.
const METER_TRACK: &str = "▏";

// The same three glyphs as `char`, for tests and for scanning rendered rows.
#[cfg(test)]
const METER_FULL_CH: char = '▉';
#[cfg(test)]
const METER_TIP_CH: char = '▸';

/// `len` textured cells whose colour sweeps `ramp` from `t0` to `t1`. `tip`
/// draws the last cell as an arrow, for a bar that is still growing.
fn textured(len: usize, ramp: &Ramp, t0: f64, t1: f64, tip: bool) -> Vec<Span<'static>> {
    (0..len)
        .map(|i| {
            let t = t0 + (t1 - t0) * (i + 1) as f64 / len as f64;
            let symbol = if tip && i + 1 == len { METER_TIP } else { METER_FULL };
            Span::styled(symbol, Style::default().fg(ramp.at(t)))
        })
        .collect()
}

/// A meter anchored at zero: `filled` of `width`, colour ramped along the
/// meter's own length, so a fuller meter is a hotter meter.
fn meter_spans(filled: usize, width: usize, ramp: &Ramp, theme: &Theme) -> Vec<Span<'static>> {
    let filled = filled.min(width);
    let mut spans = textured(filled, ramp, 0.0, filled as f64 / width as f64, false);
    if filled < width {
        spans.push(Span::styled(METER_TRACK.repeat(width - filled), Style::default().fg(theme.track)));
    }
    spans
}

/// Where a call's duration sits on its ramp: log-scaled between 50 ms and a
/// minute. Absolute, not relative to the window on screen — in a busy session
/// most calls are one cell wide, so colour has to carry the magnitude that
/// width cannot. A 40 ms read and a 30 s test run are then obviously different
/// even when both are a single cell.
fn heat(ms: u64) -> f64 {
    const FLOOR_MS: f64 = 50.0;
    const CEIL_MS: f64 = 60_000.0;
    let ms = (ms as f64).clamp(FLOOR_MS, CEIL_MS);
    (ms.log10() - FLOOR_MS.log10()) / (CEIL_MS.log10() - FLOOR_MS.log10())
}

/// A labelled host meter: `cpu  25.1%  (12 cores) ▉▉▉▉▏▏▏▏▏`.
fn meter_line(label: String, ratio: f64, width: usize, theme: &Theme) -> Line<'static> {
    let label_w = label.chars().count();
    let bar_w = width.saturating_sub(label_w + 1);
    let mut spans = vec![Span::styled(label, Style::default().fg(theme.text)), Span::raw(" ")];
    if bar_w >= 4 {
        spans.extend(meter_spans((ratio.clamp(0.0, 1.0) * bar_w as f64).round() as usize, bar_w, &theme.ramp_load, theme));
    }
    Line::from(spans)
}

fn block<'a>(title: &'a str, theme: &Theme) -> Block<'a> {
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(theme.border)).title(Line::from(vec![
        Span::raw(" "),
        Span::styled(title, Style::default().fg(theme.accent).bold()),
        Span::raw(" "),
    ]))
}

/// One row of the totals block: a fixed-width label, a fixed-width headline
/// number, then the detail that qualifies it. The fixed columns are what make
/// the four rows read as a table rather than as ragged sentences.
fn stat_line(label: &str, value: String, value_style: Style, rest: Vec<Span<'static>>, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{label:<STAT_LABEL_W$}"), Style::default().fg(theme.dim)),
        // Right-aligned: the numbers line up on their units, which is the
        // whole point of giving them a column of their own.
        Span::styled(format!("{value:>STAT_VALUE_W$}"), value_style),
        Span::raw("  "),
    ];
    spans.extend(rest);
    Line::from(spans)
}

fn dim(text: impl Into<String>, theme: &Theme) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(theme.dim))
}

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let area = f.area();
    // Set both defaults explicitly: raw text must not inherit a terminal
    // foreground that belongs to a different palette.
    f.render_widget(Block::default().style(theme.style()), area);
    // A pinned panel takes the table's and the detail pane's place; the header
    // and footer stay, so a dedicated pane is still recognisably agent-top and
    // still shows the burn rate.
    if let Some(p) = app.pinned {
        let [header, body, footer] = Layout::vertical([Constraint::Length(6), Constraint::Min(4), Constraint::Length(1)]).areas(area);
        draw_header(f, app, header, theme);
        draw_pinned(f, body, app, p, theme);
        draw_footer(f, app, footer, theme);
        draw_overlay(f, area, app, theme);
        return;
    }
    // With the detail pane open, the agents table takes only the height its
    // rows need (a border, a title, a header and one line each, capped so a
    // long list still scrolls), and the detail pane gets the rest of the
    // screen. A handful of agents no longer leaves the table half empty while
    // the detail pane clips its own facts.
    let (table_c, detail_c) = if app.show_detail {
        let need = (app.rows.len() as u16).saturating_add(4).clamp(6, 22);
        (Constraint::Length(need), Constraint::Min(10))
    } else {
        (Constraint::Min(4), Constraint::Length(0))
    };
    let [header, table, detail, footer] = Layout::vertical([Constraint::Length(6), table_c, detail_c, Constraint::Length(1)]).areas(area);
    draw_header(f, app, header, theme);
    draw_table(f, app, table, theme);
    if app.show_detail {
        draw_detail(f, app, detail, theme);
    }
    draw_footer(f, app, footer, theme);
    draw_overlay(f, area, app, theme);
}

fn draw_overlay(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    match app.overlay {
        Overlay::Help => draw_help(f, area, theme),
        Overlay::Panel(p) => draw_peek(f, area, app, p, theme),
        Overlay::Update => draw_update(f, area, app, theme),
        Overlay::None => {}
    }
}

// ── panels ──────────────────────────────────────────────────────────────────
//
// A panel's content is built once, as lines, and drawn either as a peek (a
// centred popup, rows capped to the popup) or pinned (the whole terminal, every
// row, scrollable). The key hints differ; the content does not.

/// One panel's lines and the colour of its frame.
struct PanelBody {
    accent: Color,
    /// The popup width the peek asks for; the terminal may give less.
    width: u16,
    lines: Vec<Line<'static>>,
}

/// Build a panel's content. `cap` limits rows to what a popup can show, with
/// an "… n more" line; `None` shows everything. `width` is the room for text.
fn panel_body(p: Panel, snap: &agent_top_core::Snapshot, cap: Option<usize>, width: usize, theme: &Theme) -> PanelBody {
    match p {
        Panel::SlowTools => PanelBody { accent: theme.peach, width: 66, lines: tool_lines(snap, ToolPanel::Slow, cap, theme) },
        Panel::FailedTools => PanelBody { accent: theme.red, width: 66, lines: tool_lines(snap, ToolPanel::Failed, cap, theme) },
        Panel::Advice => PanelBody { accent: theme.advice, width: 84, lines: advice_lines(snap, width, theme) },
        Panel::Mcp => PanelBody { accent: theme.mauve, width: 92, lines: mcp_lines(snap, cap, width, theme) },
    }
}

/// The frame every panel is drawn in: its title in its colour, and the command
/// that starts agent-top on it alone, so the standalone form is learnt by
/// seeing it.
fn panel_block(p: Panel, snap: &agent_top_core::Snapshot, accent: Color, theme: &Theme) -> Block<'static> {
    let title = match p {
        Panel::Advice if !snap.advice.is_empty() => format!(" advice ({}) ", snap.advice.len()),
        _ => format!(" {} ", p.title()),
    };
    Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(title, Style::default().fg(accent).bold()))
        .title_bottom(Line::from(Span::styled(format!(" agent-top {} ", p.command()), Style::default().fg(theme.dim))).right_aligned())
}

/// A panel as a centred popup over whatever is underneath, with the ways on
/// from here on its last lines: Enter to fill the terminal, `o` to open it in
/// a pane when there is a multiplexer to ask, and the panel's key or Esc to
/// close. The `o` line shows the command that would run, before it runs.
fn draw_peek(f: &mut Frame, area: Rect, app: &App, p: Panel, theme: &Theme) {
    let snap = &app.snapshot;
    // Rows are capped to what fits a popup of the usual height; the pinned
    // form has no cap.
    let cap = (area.height.saturating_sub(12) as usize).clamp(4, 20);
    let probe = panel_body(p, snap, Some(cap), 80, theme);
    let w = probe.width.min(area.width.saturating_sub(2));
    let inner = w.saturating_sub(4) as usize;
    let body = panel_body(p, snap, Some(cap), inner, theme);
    let mut lines = body.lines;
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  Enter", Style::default().fg(body.accent).bold()),
        Span::styled(format!(" full screen · {} or Esc to close", p.key()), Style::default().fg(theme.dim)),
    ]));
    if let Some(mux) = app.multiplexer {
        lines.push(Line::from(vec![
            Span::styled("  o", Style::default().fg(body.accent).bold()),
            Span::styled(format!(" open in a {} pane:  ", mux.label()), Style::default().fg(theme.dim)),
            Span::styled(truncate(&mux.describe(p.command()), inner.saturating_sub(28)), Style::default().fg(theme.accent)),
        ]));
    }
    let h = (lines.len() as u16 + 2).clamp(6, area.height.saturating_sub(2));
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(Text::from(lines)).style(theme.style()).block(panel_block(p, snap, body.accent, theme)), popup);
}

/// A panel filling the terminal between the header and the footer: every row,
/// scrolled by the movement keys. The scroll offset is clamped here, where the
/// height is known.
fn draw_pinned(f: &mut Frame, area: Rect, app: &mut App, p: Panel, theme: &Theme) {
    let body = panel_body(p, &app.snapshot, None, area.width.saturating_sub(4) as usize, theme);
    let visible = area.height.saturating_sub(2) as usize;
    let max = body.lines.len().saturating_sub(visible) as u16;
    app.scroll = app.scroll.min(max);
    let block = panel_block(p, &app.snapshot, body.accent, theme);
    f.render_widget(Paragraph::new(Text::from(body.lines)).block(block).scroll((app.scroll, 0)), area);
}

/// Every MCP server under every agent on screen, then the orphans. The
/// per-agent rows in the detail pane show one agent's servers; this is the
/// machine's.
fn mcp_lines(snap: &agent_top_core::Snapshot, cap: Option<usize>, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let now = snap.taken_at;
    let cap = cap.unwrap_or(usize::MAX);
    let rows: Vec<(&Agent, &agent_top_core::McpServer)> =
        snap.agents.iter().flat_map(|a| a.mcp_servers.iter().map(move |m| (a, m))).collect();
    let agents_with = snap.agents.iter().filter(|a| !a.mcp_servers.is_empty()).count();
    let mut lines: Vec<Line> = Vec::new();
    if rows.is_empty() {
        lines.push(Line::styled("  no MCP servers under any agent on screen", Style::default().fg(theme.dim)));
    } else {
        lines.push(Line::styled(
            format!("  {} servers under {agents_with} agents · calls from the transcript; pid? = process guessed", rows.len()),
            Style::default().fg(theme.dim),
        ));
        lines.push(Line::styled(
            format!(
                "  {:<14} {:<14} {:>6} {:>5} {:>3} {:>9} {:>5} {:>6}",
                "agent", "server", "pid", "calls", "err", "last call", "cpu", "rss"
            ),
            Style::default().fg(theme.dim),
        ));
        for (a, m) in rows.iter().take(cap) {
            let (pid, cpu, rss) = match m.pid {
                Some(pid) if m.matched_by == McpMatch::Sole => (format!("{pid}?"), format!("{:.1}%", m.cpu_percent), bytes(m.rss_bytes)),
                Some(pid) => (pid.to_string(), format!("{:.1}%", m.cpu_percent), bytes(m.rss_bytes)),
                None => ("-".into(), "-".into(), "-".into()),
            };
            let last = match m.last_call {
                Some(t) => format!("{} ago", age(now.duration_since(t).unwrap_or_default().as_secs())),
                None => "-".into(),
            };
            let err_style = if m.errors > 0 { Style::default().fg(theme.red) } else { Style::default().fg(theme.dim) };
            lines.push(Line::from(vec![
                Span::raw(format!("  {:<14} ", truncate(&a.name, 14))),
                Span::styled(format!("{:<14} ", truncate(&m.name, 14)), Style::default().fg(theme.mauve)),
                Span::styled(format!("{pid:>6} "), Style::default().fg(theme.dim)),
                Span::raw(format!("{:>5} ", m.calls)),
                Span::styled(format!("{:>3} ", m.errors), err_style),
                Span::styled(format!("{last:>9} {cpu:>5} {rss:>6}"), Style::default().fg(theme.dim)),
            ]));
        }
        if rows.len() > cap {
            lines.push(Line::styled(format!("  … {} more", rows.len() - cap), Style::default().fg(theme.dim)));
        }
    }
    if !snap.orphans.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  orphaned mcp processes", Style::default().fg(theme.red).bold()),
            Span::styled("  (no live agent ancestor; likely leaked)", Style::default().fg(theme.dim)),
        ]));
        for o in snap.orphans.iter().take(cap) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:>6} ", o.pid), Style::default().fg(theme.red)),
                Span::styled(format!("{:>6} {:>6}  ", bytes(o.rss_bytes), age(o.age_secs)), Style::default().fg(theme.dim)),
                Span::raw(short_cmd(o, width.saturating_sub(24))),
            ]));
            if let Some(origin) = snap.orphan_origins.iter().find(|x| x.pid == o.pid) {
                lines.push(Line::styled(format!("         {}", orphan_origin(origin, now)), Style::default().fg(theme.dim)));
            }
        }
        if snap.orphans.len() > cap {
            lines.push(Line::styled(format!("  … {} more", snap.orphans.len() - cap), Style::default().fg(theme.dim)));
        }
    }
    lines
}

/// The upgrade question: which version is out, which this is, and the exact
/// command `u` would run, so nothing happens that was not shown first. When
/// the binary was not installed by an installer agent-top knows, it shows
/// the ways to upgrade by hand instead of offering to guess.
fn draw_update(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let amber = theme.peach;
    let latest = app.latest().unwrap_or_default();
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("  agent-top "),
            Span::styled(format!("v{latest}"), Style::default().fg(amber).bold()),
            Span::raw(format!(" is available; this is v{}.", crate::VERSION)),
        ]),
        Line::raw(""),
    ];
    match app.installer.command_line() {
        Some(cmd) => {
            lines.push(Line::from(vec![
                Span::styled("  u ", theme.badge_style(amber)),
                Span::raw("  upgrade now, in this terminal:  "),
                Span::styled(cmd, Style::default().fg(theme.accent)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  n ", theme.badge_style(theme.accent)),
                Span::raw("  not now: this version is not asked about again; the footer badge stays"),
            ]));
        }
        None => {
            lines.push(Line::raw("  This binary was not installed by Homebrew or cargo, so agent-top will not"));
            lines.push(Line::raw("  guess how to replace it. Upgrade with one of:"));
            lines.push(Line::styled("    brew update && brew upgrade agent-top", Style::default().fg(theme.accent)));
            lines.push(Line::styled("    cargo install agent-top", Style::default().fg(theme.accent)));
            lines.push(Line::styled("    https://github.com/kannandreams/agent-top/releases/latest", Style::default().fg(theme.accent)));
            lines.push(Line::from(vec![
                Span::styled("  n ", theme.badge_style(theme.accent)),
                Span::raw("  not now: this version is not asked about again"),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("  what's new: agent-top --whats-new · {}", crate::CHANGELOG_URL), Style::default().fg(theme.dim)));
    lines.push(Line::styled("  AGENT_TOP_NO_UPDATE_CHECK=1 turns the check off", Style::default().fg(theme.dim)));
    let w = 92.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(amber))
        .title(Span::styled(" update available ", Style::default().fg(amber).bold()));
    f.render_widget(Paragraph::new(Text::from(lines)).style(theme.style()).block(block), popup);
}

/// Every piece of advice on the snapshot: one headline with its numbers, and
/// under it, dimmed, what could be done. The rules and their thresholds live
/// in `agent_top_core::advice`.
fn advice_lines(snap: &agent_top_core::Snapshot, inner: usize, theme: &Theme) -> Vec<Line<'static>> {
    let advice = &snap.advice;
    let mut lines: Vec<Line> = Vec::new();
    if advice.is_empty() {
        lines.push(Line::styled(
            "  nothing to suggest: no oversized results, idle servers or growing servers",
            Style::default().fg(theme.dim),
        ));
    } else {
        let mut last_agent = "";
        for x in advice {
            if x.agent_name != last_agent {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}", x.agent_name), Style::default().fg(theme.accent).bold()),
                    Span::styled(format!("  {}", x.agent_id), Style::default().fg(theme.dim)),
                ]));
                last_agent = &x.agent_name;
            }
            let (mark, colour) = match x.rule {
                agent_top_core::AdviceRule::ExpensiveSource => ("$", theme.peach),
                agent_top_core::AdviceRule::GrowingMcpServer => ("↑", theme.red),
                agent_top_core::AdviceRule::IdleMcpServer => ("·", theme.dim),
            };
            for (i, part) in wrap(&x.headline, inner.saturating_sub(4)).into_iter().enumerate() {
                let lead = if i == 0 { format!("  {mark} ") } else { "    ".to_string() };
                lines.push(Line::from(vec![Span::styled(lead, Style::default().fg(colour).bold()), Span::raw(part)]));
            }
            for part in wrap(&x.action, inner.saturating_sub(6)) {
                lines.push(Line::styled(format!("      {part}"), Style::default().fg(theme.dim)));
            }
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("  read from the numbers on screen; nothing is done for you", Style::default().fg(theme.dim)));
    lines
}

/// Greedy word wrap to `width` columns; a single overlong word stands alone.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(16);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// One tool's stats, summed over every agent on screen.
#[derive(Default, Clone)]
struct ToolStat {
    name: String,
    calls: u64,
    timed: u64,
    total_ms: u64,
    max_ms: u64,
    errors: u64,
}

/// Which leaderboard a tool panel shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolPanel {
    Slow,
    Failed,
}

/// Aggregate every tool span across every agent into per-tool stats.
fn tool_stats(snap: &agent_top_core::Snapshot) -> Vec<ToolStat> {
    use std::collections::HashMap;
    let mut by: HashMap<&str, ToolStat> = HashMap::new();
    for a in &snap.agents {
        for sp in &a.spans {
            if sp.kind != SpanKind::Tool {
                continue;
            }
            let e = by.entry(sp.name.as_str()).or_default();
            if e.name.is_empty() {
                e.name = sp.name.clone();
            }
            e.calls += 1;
            if let Some(ms) = sp.duration_ms {
                e.timed += 1;
                e.total_ms += ms;
                e.max_ms = e.max_ms.max(ms);
            }
            if sp.error {
                e.errors += 1;
            }
        }
    }
    by.into_values().collect()
}

/// The slow-tools or failed-tools leaderboard, from the tool spans already on
/// screen. Amber for time, red for failures, so the panel is recognisable at
/// a glance.
fn tool_lines(snap: &agent_top_core::Snapshot, panel: ToolPanel, cap: Option<usize>, theme: &Theme) -> Vec<Line<'static>> {
    let mut stats = tool_stats(snap);
    match panel {
        ToolPanel::Slow => stats.sort_by(|a, b| b.total_ms.cmp(&a.total_ms).then(b.max_ms.cmp(&a.max_ms))),
        ToolPanel::Failed => {
            stats.retain(|s| s.errors > 0);
            stats.sort_by(|a, b| b.errors.cmp(&a.errors).then(b.calls.cmp(&a.calls)));
        }
    }

    let mut lines: Vec<Line> = Vec::new();
    if stats.is_empty() {
        let msg = match panel {
            ToolPanel::Slow => "no tool calls on screen yet",
            ToolPanel::Failed => "no failed tool calls on screen — all green",
        };
        lines.push(Line::styled(format!("  {msg}"), Style::default().fg(theme.dim)));
    } else {
        let header = match panel {
            ToolPanel::Slow => format!("  {:<22}{:>7}{:>9}{:>9}{:>7}", "tool", "calls", "total", "avg", "max"),
            ToolPanel::Failed => format!("  {:<22}{:>8}{:>8}{:>9}", "tool", "fails", "calls", "fail%"),
        };
        lines.push(Line::styled(header, Style::default().fg(theme.dim)));
        let cap = cap.unwrap_or(usize::MAX);
        for s in stats.iter().take(cap) {
            let line = match panel {
                ToolPanel::Slow => {
                    let avg = s.total_ms.checked_div(s.timed).unwrap_or(0);
                    format!(
                        "  {:<22}{:>7}{:>9}{:>9}{:>7}",
                        truncate(&s.name, 22),
                        s.calls,
                        duration_ms(s.total_ms),
                        duration_ms(avg),
                        duration_ms(s.max_ms)
                    )
                }
                ToolPanel::Failed => {
                    let pct = if s.calls > 0 { 100.0 * s.errors as f64 / s.calls as f64 } else { 0.0 };
                    format!("  {:<22}{:>8}{:>8}{:>8.0}%", truncate(&s.name, 22), s.errors, s.calls, pct)
                }
            };
            lines.push(Line::raw(line));
        }
        if stats.len() > cap {
            lines.push(Line::styled(format!("  … {} more", stats.len() - cap), Style::default().fg(theme.dim)));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("  aggregated from the tool trace on screen", Style::default().fg(theme.dim)));
    lines
}

/// The spend velocity, coloured by how fast: dim near zero, then amber and red
/// as the dollars-per-hour climbs. It reads the rate the header already keeps.
fn burn_span(per_hour: f64, theme: &Theme) -> Span<'static> {
    let colour = if per_hour >= 20.0 {
        theme.red
    } else if per_hour >= 5.0 {
        theme.peach
    } else if per_hour >= 0.005 {
        theme.green
    } else {
        theme.dim
    };
    Span::styled(format!("${per_hour:.2}/h"), Style::default().fg(colour))
}

fn draw_header(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let snap = &app.snapshot;
    let host = &snap.host;
    // The version lives in the footer now, next to quit; the header just names
    // the tool and the host.
    let title = format!(
        "agent-top{}{}",
        host.hostname.as_deref().map(|h| format!(" @ {h}")).unwrap_or_default(),
        if app.paused { "  [PAUSED]" } else { "" }
    );
    let outer = block(&title, theme);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner);
    let [cpu_row, mem_row, spark_row, _] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(left);

    let cpu = host.cpu_percent as f64;
    // Leave a gutter so the meter never runs into the totals column.
    let w = (cpu_row.width as usize).saturating_sub(3);
    f.render_widget(Paragraph::new(meter_line(format!("cpu {cpu:>5.1}% ({:>2} cores)", host.cpu_count), cpu / 100.0, w, theme)), cpu_row);
    let mem_pct = if host.mem_total_bytes > 0 { host.mem_used_bytes as f64 * 100.0 / host.mem_total_bytes as f64 } else { 0.0 };
    f.render_widget(
        Paragraph::new(meter_line(
            format!("mem {:>5.1}% {:>5}/{:>5}", mem_pct, bytes(host.mem_used_bytes), bytes(host.mem_total_bytes)),
            mem_pct / 100.0,
            w,
            theme,
        )),
        mem_row,
    );
    let [spark_label, spark] = Layout::horizontal([Constraint::Length(11), Constraint::Min(4)]).areas(spark_row);
    f.render_widget(Paragraph::new(Span::styled("out tok/s ", Style::default().fg(theme.dim))), spark_label);
    f.render_widget(Sparkline::default().data(&app.output_rate).style(Style::default().fg(theme.accent)), spark);

    let t = &snap.totals;
    let lines = vec![
        stat_line(
            "agents",
            t.agents.to_string(),
            Style::default().bold(),
            vec![
                Span::styled(format!("{:>2} running", t.running), Style::default().fg(theme.green)),
                dim("   ", theme),
                Span::styled(format!("{:>2} idle", t.idle), Style::default().fg(theme.yellow)),
                dim("   ", theme),
                Span::styled(format!("{:>2} stopped", t.stopped), Style::default().fg(theme.dim)),
            ],
            theme,
        ),
        stat_line(
            "tokens",
            tokens(t.tokens),
            Style::default().bold(),
            vec![
                dim("total cost ", theme),
                Span::styled(
                    format!("${:.2}{}", t.cost_usd, if t.unpriced_tokens > 0 { "+" } else { "" }),
                    Style::default().fg(theme.mauve).bold(),
                ),
                dim("   burn ", theme),
                burn_span(app.burn_per_hour, theme),
            ],
            theme,
        ),
        stat_line(
            "procs",
            t.processes.to_string(),
            Style::default(),
            vec![
                dim("mcp ", theme),
                Span::raw(format!("{:<4}", t.mcp_processes)),
                dim("orphaned ", theme),
                if t.orphaned_mcp > 0 {
                    Span::styled(t.orphaned_mcp.to_string(), Style::default().fg(theme.red).bold())
                } else {
                    Span::raw("0")
                },
            ],
            theme,
        ),
        // What the agents themselves are costing the machine, as opposed to
        // the whole-host meters on the left.
        stat_line(
            "agent use",
            format!("{:.1}%", t.cpu_percent),
            Style::default(),
            vec![dim("cpu · ", theme), Span::raw(bytes(t.rss_bytes)), dim(" resident", theme)],
            theme,
        ),
    ];
    f.render_widget(Paragraph::new(Text::from(lines)), right);
}

fn draw_table(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let header = Row::new(
        ["AGENT", "HARNESS", "STATE", "PID", "MODEL", "TOKENS", "COST", "CPU%", "MEM", "TOOLS", "PROCS", "MCP", "AGE"]
            .into_iter()
            .map(|h| Cell::from(h).style(Style::default().fg(theme.accent).bold())),
    )
    .bottom_margin(0);

    let rows = app.rows.iter().enumerate().map(|(i, a)| {
        let mem = mem_cell(a);
        let cpu = cpu_cell(a);
        let mcp_style = if a.mcp_count > 0 { Style::default().fg(theme.mauve) } else { Style::default() };
        Row::new(vec![
            Cell::from(table_session_name(a, app.row_depths[i])).style(Style::default().bold()),
            Cell::from(a.harness.label()),
            Cell::from(a.state.label()).style(state_style(a.state, theme)),
            Cell::from(a.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into())),
            Cell::from(short_model(a.model.as_deref())),
            Cell::from(tokens_cell(a)),
            Cell::from(cost(a)).style(Style::default().fg(theme.mauve)),
            Cell::from(cpu),
            Cell::from(mem),
            Cell::from(tool_calls(a)),
            Cell::from(if a.shares_process {
                "·".to_string()
            } else if a.pid.is_some() {
                a.process_count.to_string()
            } else {
                "-".to_string()
            }),
            Cell::from(a.mcp_count.to_string()).style(mcp_style),
            Cell::from(age(a.age_secs)),
        ])
        .style(if a.state == AgentState::Stopped { Style::default().fg(theme.dim) } else { Style::default() })
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
        .block(block(&title, theme))
        .column_spacing(1)
        .row_highlight_style(Style::default().bg(theme.selection).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(if app.rows.is_empty() { None } else { Some(app.selected) });
    f.render_stateful_widget(table, area, &mut state);

    if app.rows.is_empty() {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled("no coding agents found. ", Style::default().fg(theme.dim)),
            Span::styled(
                "start a coding agent in another terminal, or run agent-top --json to debug discovery.",
                Style::default().fg(theme.dim),
            ),
        ]))
        .wrap(Wrap { trim: true });
        let inner = Rect { x: area.x + 2, y: area.y + 2, width: area.width.saturating_sub(4), height: 2 };
        f.render_widget(msg, inner);
    }
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let Some(a) = app.selected_agent() else {
        f.render_widget(block("detail", theme), area);
        return;
    };
    let title = format!("{} · {} · {}  [{}]", session_name(a), a.harness.label(), a.state.label(), app.detail.label());
    let outer = block(&title, theme);
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner);
    f.render_widget(Paragraph::new(agent_facts(a, app.snapshot.taken_at, theme)).wrap(Wrap { trim: false }), left);
    let panel = match app.detail {
        DetailView::Tree => tree_detail(app, a, right.width as usize, theme),
        DetailView::Trace => tool_trace(a, app.snapshot.taken_at, right.width as usize, right.height as usize, theme),
    };
    f.render_widget(Paragraph::new(panel), right);
}

fn kv<'a>(k: &'a str, v: String, theme: &Theme) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<11}"), Style::default().fg(theme.dim)), Span::raw(v)])
}

fn table_session_name(a: &Agent, depth: usize) -> String {
    nested_name(a, depth, 26)
}

/// One line of the cost breakdown: tokens, the price they were charged at,
/// and what that came to. The price column is the row's current model's; the
/// cost column is exact even when the session changed model part way.
fn cost_row(label: &str, n: u64, per_m: Option<f64>, usd: f64, theme: &Theme) -> Line<'static> {
    // A harness can record the cost without preserving a per-model rate.
    let usd = if per_m.is_some() || usd > 0.0 { format!("{usd:>10.2}") } else { format!("{:>10}", "-") };
    let per_m = per_m.map(|p| format!("{p:>9.2}")).unwrap_or_else(|| format!("{:>9}", "n/a"));
    Line::from(vec![
        Span::styled(format!("{label:<13}"), Style::default().fg(theme.dim)),
        Span::raw(format!("{:>7}", tokens(n))),
        dim(per_m, theme),
        Span::raw(usd),
    ])
}

/// What the cost figure is priced at, so a user comparing it with another
/// tool's number knows which table produced it.
fn price_basis(a: &Agent) -> String {
    match a.price_source {
        Some(agent_top_core::PriceSource::Builtin) => "list price, built-in table".into(),
        Some(agent_top_core::PriceSource::UserFile) => "your price file".into(),
        Some(agent_top_core::PriceSource::Harness) => "harness-reported cost".into(),
        None if a.unpriced_tokens > 0 => "no price for this model".into(),
        None => String::new(),
    }
}

/// A window's label from its length: `5h`, `weekly`, `24h`.
pub fn window_label(minutes: u64) -> String {
    match minutes {
        0 => "window".into(),
        10080 => "weekly".into(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// One rate-limit window: how much is used and when it resets, coloured by
/// how close it is to the limit.
fn rate_window_line(kind: &str, w: &agent_top_core::RateWindow, now: SystemTime, theme: &Theme) -> Line<'static> {
    let pct = w.used_percent;
    let colour = if pct >= 90.0 {
        theme.red
    } else if pct >= 75.0 {
        theme.peach
    } else {
        theme.green
    };
    let resets = match w.resets_at {
        Some(t) => match t.duration_since(now) {
            Ok(d) => format!("resets in {}", age(d.as_secs())),
            Err(_) => "resetting now".to_string(),
        },
        None => String::new(),
    };
    Line::from(vec![
        Span::styled(format!("  {:<9}", window_label(w.window_minutes)), Style::default().fg(theme.dim)),
        Span::styled(format!("{pct:>4.0}% used"), Style::default().fg(colour)),
        dim(format!("   {kind}"), theme),
        if resets.is_empty() { Span::raw(String::new()) } else { dim(format!("   {resets}"), theme) },
    ])
}

/// How much of the prompt is being served from cache, coloured by how good
/// that is. Blank on a session too small to judge, so it does not cry "0%" on
/// a two-turn session that never built a cache.
fn cache_line(a: &Agent, theme: &Theme) -> Line<'static> {
    // Under a few thousand prompt tokens there is nothing meaningful to say.
    let prompt = a.usage.prompt();
    let Some(rate) = a.usage.cache_hit_rate().filter(|_| prompt >= 5_000) else {
        return kv("cache", "-".into(), theme);
    };
    let pct = rate * 100.0;
    let colour = if pct >= 70.0 {
        theme.green
    } else if pct >= 40.0 {
        theme.peach
    } else {
        theme.red
    };
    let note = if pct < 40.0 { "   full price most turns" } else { "" };
    Line::from(vec![
        Span::styled(format!("{:<11}", "cache"), Style::default().fg(theme.dim)),
        Span::styled(format!("{pct:.0}% from cache"), Style::default().fg(colour)),
        dim(note.to_string(), theme),
    ])
}

fn agent_facts(a: &Agent, now: SystemTime, theme: &Theme) -> Text<'static> {
    let u = &a.usage;
    let b = &a.cost_breakdown;
    let price = a
        .model
        .as_deref()
        .filter(|_| a.price_source != Some(agent_top_core::PriceSource::Harness))
        .and_then(agent_top_core::pricing::price_for);
    let attribution = match a.attribution {
        Attribution::HarnessRegistry => "harness registry (exact)",
        Attribution::CommandLine => "command line --resume (exact)",
        Attribution::OpenFile => "open transcript file (exact)",
        Attribution::CwdHeuristic => "cwd + start time (heuristic)",
        Attribution::None => "none (process only)",
        Attribution::TranscriptOnly => "transcript only (no process)",
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let tilde = |p: &std::path::Path| p.to_string_lossy().replacen(&home, "~", 1);
    let mut lines = Vec::new();
    if let Some(w) = &a.parse_warning {
        lines.push(Line::from(vec![Span::styled(format!("⚠ {w}"), Style::default().fg(theme.red).bold())]));
        lines.push(Line::raw(""));
    }
    // Identity first, then the headline numbers a glance wants (cost, cache,
    // turns, tools) high up, so a short detail pane never clips them; the
    // per-token cost breakdown, a dig-deeper detail, comes below them.
    lines.push(kv("session", a.session_id.clone().unwrap_or_else(|| "-".into()), theme));
    if let Some(info) = &a.subagent {
        lines.push(kv("parent", info.parent_session_id.clone(), theme));
        if let Some(nickname) = &info.nickname {
            lines.push(kv("nickname", nickname.clone(), theme));
        }
        if let Some(role) = &info.role {
            lines.push(kv("role", role.clone(), theme));
        }
    }
    lines.extend(vec![
        kv("cwd", a.cwd.as_deref().map(tilde).unwrap_or_else(|| "-".into()), theme),
        kv("model", a.model.clone().unwrap_or_else(|| "-".into()), theme),
        kv("version", a.harness_version.clone().unwrap_or_else(|| "-".into()), theme),
        kv(
            "activity",
            format!("{:?}{}", a.activity, a.idle_secs.map(|s| format!(", last write {} ago", age(s))).unwrap_or_default()),
            theme,
        ),
        kv("attributed", attribution.to_string(), theme),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{:<11}", "cost"), Style::default().fg(theme.dim)),
            Span::styled(cost(a), Style::default().bold()),
            dim(format!("   {}", price_basis(a)), theme),
        ]),
        cache_line(a, theme),
        kv("tokens", tokens(u.total()), theme),
        kv(
            "turns",
            if matches!(a.harness, agent_top_core::Harness::Codex | agent_top_core::Harness::Kodelet) {
                a.turns.to_string()
            } else {
                format!("{} ({} subagent)", a.turns, a.subagent_turns)
            },
            theme,
        ),
        kv("tool calls", tool_calls(a), theme),
    ]);
    if a.tool_calls_lower_bound {
        lines.push(Line::from(dim("  retained/observed calls; earlier history may be missing", theme)));
    }
    if a.web_searches > 0 {
        let priced = if b.web_search > 0.0 { format!(" (${:.2})", b.web_search) } else { " (not priced)".to_string() };
        lines.push(kv("web search", format!("{}{priced}", a.web_searches), theme));
    }
    // The per-token cost breakdown, below the headline stats.
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<13}{:>7}", "breakdown", ""), Style::default().fg(theme.accent).bold()),
        dim(format!("{:>9}{:>10}", "$/M", "cost"), theme),
    ]));
    lines.extend(vec![
        cost_row("  input", u.input, price.map(|p| p.input), b.input, theme),
        cost_row("  cache rd", u.cache_read, price.map(|p| p.cache_read), b.cache_read, theme),
        cost_row(
            if a.harness == agent_top_core::Harness::Kodelet { "  cache write" } else { "  cache wr 5m" },
            u.cache_write_5m,
            price.map(|p| p.cache_write_5m),
            b.cache_write_5m,
            theme,
        ),
    ]);
    if a.harness != agent_top_core::Harness::Kodelet {
        lines.push(cost_row("  cache wr 1h", u.cache_write_1h, price.map(|p| p.cache_write_1h), b.cache_write_1h, theme));
    }
    lines.push(cost_row("  output", u.output, price.map(|p| p.output), b.output, theme));
    if let Some(rl) = &a.rate_limit {
        lines.push(Line::raw(""));
        let head = match &rl.plan {
            Some(plan) => format!("rate limit ({plan})"),
            None => "rate limit".to_string(),
        };
        let mut spans = vec![Span::styled(format!("{head:<20}"), Style::default().fg(theme.accent).bold())];
        if rl.reached {
            spans.push(Span::styled("LIMIT REACHED", Style::default().fg(theme.red).bold()));
        }
        lines.push(Line::from(spans));
        for (label, w) in [("primary", &rl.primary), ("secondary", &rl.secondary)] {
            if let Some(w) = w {
                lines.push(rate_window_line(label, w, now, theme));
            }
        }
    }
    if let Some(p) = &a.session_path {
        lines.push(kv("transcript", tilde(p), theme));
    }
    if let Some(id) = &a.session_id {
        // Kodelet IDs begin with a date shared by all sessions that day.
        // Other harnesses use a short prefix that the command can resolve.
        let short = if a.harness == agent_top_core::Harness::Kodelet { id.clone() } else { id.chars().take(8).collect() };
        lines.push(kv("export", format!("agent-top trace --session {short} -o trace.json"), theme));
    }
    Text::from(lines)
}

/// A waterfall of the agent's recent tool calls: one row per call, positioned
/// and sized on a shared time axis, so a single long call and a storm of short
/// ones look different at a glance. Answers "why has this agent been busy for
/// eight minutes", which the table alone cannot.
fn tool_trace(a: &Agent, now: SystemTime, width: usize, height: usize, theme: &Theme) -> Text<'static> {
    let head = |extra: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled("tool trace", Style::default().fg(theme.accent).bold())];
        spans.extend(extra);
        Line::from(spans)
    };
    if a.spans.is_empty() {
        return Text::from(vec![
            head(vec![Span::styled(format!("   {} tool calls", tool_calls(a)), Style::default().fg(theme.dim))]),
            Line::styled(
                if a.tool_calls > 0 { "  (calls happened before agent-top started reading)" } else { "  (no tool calls yet)" },
                Style::default().fg(theme.dim),
            ),
        ]);
    }

    // Two header lines, then one row per span, newest at the bottom. Turns
    // contain everything else and would each be a full-width bar, so they
    // are summarised in the header rather than drawn.
    let rows = height.saturating_sub(2).max(1);
    let shown: Vec<&ToolSpan> = {
        let mut v: Vec<&ToolSpan> = a.spans.iter().rev().filter(|s| s.kind != SpanKind::Turn).take(rows).collect();
        v.reverse();
        v
    };
    let turn = a.spans.iter().rev().find(|s| s.kind == SpanKind::Turn);
    let t0 = shown.iter().map(|s| s.started_at).min().unwrap_or(now);
    let mut t1 = shown.iter().map(|s| s.ended_at(now)).max().unwrap_or(now);
    // An in-flight call is still growing, so the axis runs to the present.
    if shown.iter().any(|s| s.is_open()) && now > t1 {
        t1 = now;
    }
    let window_ms = t1.duration_since(t0).map(|d| d.as_millis() as u64).unwrap_or(0).max(1);

    const NAME_W: usize = 14;
    const DUR_W: usize = 8;
    let bar_w = width.saturating_sub(NAME_W + DUR_W + 3).max(4);
    let cell = |t: SystemTime| -> usize {
        let off = t.duration_since(t0).map(|d| d.as_millis() as u64).unwrap_or(0);
        ((off as f64 / window_ms as f64) * bar_w as f64).round() as usize
    };

    let calls = shown.iter().filter(|s| s.kind == SpanKind::Tool).count();
    let mut lines = vec![
        head(vec![Span::styled(
            format!("   {} of {} calls · window {}", calls, tool_calls(a), duration_ms(window_ms)),
            Style::default().fg(theme.dim),
        )]),
        Line::from(trace_summary(&shown, turn, now, window_ms, theme)),
    ];
    let track = Style::default().fg(theme.track);
    for s in &shown {
        let elapsed = s.elapsed_ms(now);
        let start = cell(s.started_at).min(bar_w.saturating_sub(1));
        let end = cell(s.ended_at(now)).clamp(start + 1, bar_w);
        // The ramp says how long the call took; the marker and the name colour
        // say what kind of call it was. Keeping those on separate channels
        // means a slow call and a failed call never compete for one colour.
        let inference = s.kind == SpanKind::Inference;
        let (ramp, mark) = if s.error {
            (&theme.ramp_error, "!")
        } else if s.is_open() {
            (&theme.ramp_open, "…")
        } else if inference {
            (&theme.ramp_inference, " ")
        } else if s.sidechain {
            (&theme.ramp_subagent, " ")
        } else {
            (&theme.ramp_ok, " ")
        };
        let name_style = match (s.error, s.is_open(), inference, s.sidechain) {
            (true, _, _, _) => Style::default().fg(theme.ramp_error.at(1.0)),
            (_, true, _, _) => Style::default().fg(theme.ramp_open.at(0.8)),
            (_, _, true, _) => Style::default().fg(theme.dim),
            (_, _, _, true) => Style::default().fg(theme.ramp_subagent.at(0.0)),
            _ => Style::default().fg(theme.text),
        };
        let label = if inference { "model" } else { s.name.as_str() };
        let name = format!("{}{}", if s.sidechain { "↳" } else { "" }, label);
        let mut row = vec![
            Span::styled(format!("{:<NAME_W$}", truncate(&name, NAME_W)), name_style),
            Span::styled(format!("{:>DUR_W$}", format!("{}{mark}", duration_ms(elapsed))), Style::default().fg(theme.dim)),
            Span::raw(" "),
        ];
        // The bar sits in a full-width track, so an empty stretch reads as
        // "nothing was running then" rather than as the panel ending early.
        if start > 0 {
            row.push(Span::styled(METER_TRACK.repeat(start), track));
        }
        // Width is the call's share of the window; colour is how long it took.
        // Two channels rather than one, because at this zoom most bars are a
        // single cell and width alone would say nothing.
        let h = heat(elapsed);
        row.extend(textured(end - start, ramp, h * 0.7, h, s.is_open()));
        if end < bar_w {
            row.push(Span::styled(METER_TRACK.repeat(bar_w - end), track));
        }
        lines.push(Line::from(row));
    }
    Text::from(lines)
}

/// The line under the trace header: where the window actually went. Tool
/// time and model time are measured separately, each with overlaps merged,
/// so together they say how much of the window was accounted for and how
/// much was neither (waiting on the human, mostly).
fn trace_summary(shown: &[&ToolSpan], turn: Option<&ToolSpan>, now: SystemTime, window_ms: u64, theme: &Theme) -> Vec<Span<'static>> {
    let tools: Vec<&ToolSpan> = shown.iter().copied().filter(|s| s.kind == SpanKind::Tool).collect();
    let thinking: Vec<&ToolSpan> = shown.iter().copied().filter(|s| s.kind == SpanKind::Inference).collect();
    let slowest = tools.iter().max_by_key(|s| s.elapsed_ms(now));
    let errors = tools.iter().filter(|s| s.error).count();
    let open = tools.iter().filter(|s| s.is_open()).count();
    let share = busy_ms(&tools, now) * 100 / window_ms;
    let mut spans = vec![
        Span::styled("  in tools ", Style::default().fg(theme.dim)),
        Span::styled(format!("{share}%"), Style::default().fg(theme.ramp_ok.at(share as f64 / 100.0)).bold()),
    ];
    if !thinking.is_empty() {
        let share = busy_ms(&thinking, now) * 100 / window_ms;
        spans.push(Span::styled("  model ", Style::default().fg(theme.dim)));
        spans.push(Span::styled(format!("{share}%"), Style::default().fg(theme.ramp_inference.at(1.0)).bold()));
    }
    if let Some(t) = turn {
        let mark = if t.is_open() { "…" } else { "" };
        spans.push(Span::styled(format!("  turn {}{mark}", duration_ms(t.elapsed_ms(now))), Style::default().fg(theme.dim)));
    }
    if let Some(s) = slowest {
        spans.push(Span::styled(
            format!("  slowest {} {}", truncate(&s.name, 14), duration_ms(s.elapsed_ms(now))),
            Style::default().fg(theme.dim),
        ));
    }
    if open > 0 {
        spans.push(Span::styled(format!("  {open} in flight"), Style::default().fg(theme.yellow)));
    }
    if errors > 0 {
        spans.push(Span::styled(format!("  {errors} failed"), Style::default().fg(theme.red)));
    }
    spans
}

/// Wall-clock milliseconds covered by at least one call. Agents run tools in
/// parallel, so summing durations would happily exceed the window; overlapping
/// intervals are merged instead.
fn busy_ms(shown: &[&ToolSpan], now: SystemTime) -> u64 {
    let mut iv: Vec<(SystemTime, SystemTime)> = shown.iter().map(|s| (s.started_at, s.ended_at(now))).collect();
    iv.sort_by_key(|(a, _)| *a);
    let mut total = Duration::ZERO;
    let mut cur: Option<(SystemTime, SystemTime)> = None;
    for (start, end) in iv {
        match cur {
            Some((cs, ce)) if start <= ce => cur = Some((cs, ce.max(end))),
            Some((cs, ce)) => {
                total += ce.duration_since(cs).unwrap_or_default();
                cur = Some((start, end));
            }
            None => cur = Some((start, end)),
        }
    }
    if let Some((cs, ce)) = cur {
        total += ce.duration_since(cs).unwrap_or_default();
    }
    total.as_millis() as u64
}

/// The process tree of the selected row. Sessions that share a process show
/// the tree of the row that owns it; the table carries the session hierarchy.
fn tree_detail(app: &App, selected: &Agent, width: usize, theme: &Theme) -> Text<'static> {
    let mut lines = Vec::new();
    let process = selected
        .pid
        .and_then(|pid| app.snapshot.agents.iter().find(|a| a.pid == Some(pid) && a.harness == selected.harness && a.tree.is_some()))
        .unwrap_or(selected);
    let shared = selected.shares_process
        || (selected.pid.is_some()
            && app.snapshot.agents.iter().filter(|a| a.pid == selected.pid && a.harness == selected.harness).count() > 1);
    if shared {
        lines.push(Line::styled("CPU/RSS shared by sessions", Style::default().fg(theme.dim)));
    }
    lines.extend(
        process_tree(selected, process, &app.snapshot.orphans, &app.snapshot.orphan_origins, app.snapshot.taken_at, width, theme).lines,
    );
    Text::from(lines)
}

fn process_tree(
    a: &Agent,
    process: &Agent,
    orphans: &[ProcNode],
    origins: &[OrphanOrigin],
    now: SystemTime,
    width: usize,
    theme: &Theme,
) -> Text<'static> {
    let mut lines = vec![Line::from(vec![
        Span::styled("process tree", Style::default().fg(theme.accent).bold()),
        Span::styled(
            format!(
                "   {} procs · {} mcp · cpu {:.1}% · rss {}",
                process.process_count,
                process.mcp_count,
                process.cpu_percent,
                bytes(process.rss_bytes)
            ),
            Style::default().fg(theme.dim),
        ),
    ])];
    match &process.tree {
        None if a.shares_process => lines
            .push(Line::styled("  shares its process with another conversation; see the row that owns it", Style::default().fg(theme.dim))),
        None => lines.push(Line::styled("  (no live process)", Style::default().fg(theme.dim))),
        Some(root) => render_node(root, "", true, true, width, &mut lines, theme),
    }
    if !a.mcp_servers.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("mcp servers", Style::default().fg(theme.mauve).bold()),
            Span::styled("   calls from the transcript; pid? = process guessed", Style::default().fg(theme.dim)),
        ]));
        lines.push(Line::styled(
            format!("  {:<14} {:>6} {:>5} {:>3} {:>9} {:>5} {:>6}", "server", "pid", "calls", "err", "last call", "cpu", "rss"),
            Style::default().fg(theme.dim),
        ));
        for m in &a.mcp_servers {
            // A `?` after the pid marks a process paired with the server by
            // elimination rather than by name; `-` is a server with no
            // process, an HTTP one or one that has exited.
            let (pid, cpu, rss) = match m.pid {
                Some(pid) if m.matched_by == McpMatch::Sole => (format!("{pid}?"), format!("{:.1}%", m.cpu_percent), bytes(m.rss_bytes)),
                Some(pid) => (pid.to_string(), format!("{:.1}%", m.cpu_percent), bytes(m.rss_bytes)),
                None => ("-".into(), "-".into(), "-".into()),
            };
            let last = match m.last_call {
                Some(t) => format!("{} ago", age(now.duration_since(t).unwrap_or_default().as_secs())),
                None => "-".into(),
            };
            let err_style = if m.errors > 0 { Style::default().fg(theme.red) } else { Style::default().fg(theme.dim) };
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14} ", truncate(&m.name, 14)), Style::default().fg(theme.mauve)),
                Span::styled(format!("{pid:>6} "), Style::default().fg(theme.dim)),
                Span::raw(format!("{:>5} ", m.calls)),
                Span::styled(format!("{:>3} ", m.errors), err_style),
                Span::styled(format!("{last:>9} {cpu:>5} {rss:>6}"), Style::default().fg(theme.dim)),
            ]));
        }
    }
    if !a.context.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(context_by_source(a, theme));
    }
    if !orphans.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("orphaned mcp processes", Style::default().fg(theme.red).bold()),
            Span::styled("  (no live agent ancestor; likely leaked)", Style::default().fg(theme.dim)),
        ]));
        for o in orphans.iter().take(8) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:>6} ", o.pid), Style::default().fg(theme.red)),
                Span::styled(format!("{:>6} {:>6}  ", bytes(o.rss_bytes), age(o.age_secs)), Style::default().fg(theme.dim)),
                Span::raw(short_cmd(o, width.saturating_sub(24))),
            ]));
            if let Some(origin) = origins.iter().find(|x| x.pid == o.pid) {
                lines.push(Line::styled(format!("         {}", orphan_origin(origin, now)), Style::default().fg(theme.dim)));
            }
        }
        if orphans.len() > 8 {
            lines.push(Line::styled(format!("  … {} more", orphans.len() - 8), Style::default().fg(theme.dim)));
        }
    }
    Text::from(lines)
}

/// Rows shown before the section folds the rest into "… n more".
const CONTEXT_ROWS: usize = 6;

/// What each tool's results added to the prompt and what carrying it has
/// cost, largest first. MCP servers are named as such; the row for the
/// system prompt, the user's messages and the model's replies is last in
/// spirit but sorts with the rest, since on a fresh session it is the
/// biggest thing there.
fn context_by_source(a: &Agent, theme: &Theme) -> Vec<Line<'static>> {
    use agent_top_core::ContextOrigin;
    let mut lines = vec![
        Line::from(vec![
            Span::styled("context", Style::default().fg(theme.accent).bold()),
            Span::styled("   tokens each tool added to the prompt, and their cost since", Style::default().fg(theme.dim)),
        ]),
        Line::styled(format!("  {:<18} {:>5} {:>7} {:>8}", "source", "calls", "added", "cost"), Style::default().fg(theme.dim)),
    ];
    for c in a.context.iter().take(CONTEXT_ROWS) {
        let (label, style) = match c.origin {
            ContextOrigin::Mcp => (format!("mcp {}", c.name), Style::default().fg(theme.mauve)),
            ContextOrigin::Tool => (c.name.clone(), Style::default().fg(theme.text)),
            ContextOrigin::Other => ("prompts & replies".to_string(), Style::default().fg(theme.dim)),
        };
        let calls = if c.origin == ContextOrigin::Other { "-".to_string() } else { c.calls.to_string() };
        // No price for the model: the tokens are real, the cost is not knowable.
        let cost = if a.price_source.is_none() { "-".to_string() } else { format!("${:.2}", c.cost_usd) };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<18} ", truncate(&label, 18)), style),
            Span::styled(format!("{calls:>5} "), Style::default().fg(theme.dim)),
            Span::raw(format!("{:>7} ", tokens(c.tokens))),
            Span::raw(format!("{cost:>8}")),
        ]));
    }
    if a.context.len() > CONTEXT_ROWS {
        lines.push(Line::styled(format!("  … {} more", a.context.len() - CONTEXT_ROWS), Style::default().fg(theme.dim)));
    }
    lines
}

/// Where an orphan came from, in one line: the agent it was under and how
/// long ago it lost it, or, when agent-top never saw a parent, how long it has
/// been watching.
pub fn orphan_origin(o: &OrphanOrigin, now: SystemTime) -> String {
    let ago = |t: SystemTime| age(now.duration_since(t).unwrap_or_default().as_secs());
    match (&o.parent, o.orphaned_at) {
        (Some(p), Some(at)) => format!("orphaned from {} (pid {}) {} ago", p.name, p.pid, ago(at)),
        (Some(p), None) => format!("was under {} (pid {})", p.name, p.pid),
        (None, _) => format!("parent unknown; already an orphan when first seen {} ago", ago(o.first_seen)),
    }
}

fn render_node(n: &ProcNode, prefix: &str, last: bool, root: bool, width: usize, out: &mut Vec<Line<'static>>, theme: &Theme) {
    let branch = if root {
        ""
    } else if last {
        "└─ "
    } else {
        "├─ "
    };
    let (tag, style) = match n.kind {
        ProcKind::Agent => ("agent", Style::default().fg(theme.green).bold()),
        ProcKind::Mcp => ("mcp", Style::default().fg(theme.mauve).bold()),
        ProcKind::Shell => ("shell", Style::default().fg(theme.blue)),
        ProcKind::Tool => ("tool", Style::default().fg(theme.text)),
    };
    let head = format!("{prefix}{branch}");
    let stats = format!(" {:>6} {:>5.1}% {:>6} {:>5} ", n.pid, n.cpu_percent, bytes(n.rss_bytes), age(n.age_secs));
    let used = head.chars().count() + tag.len() + stats.len() + 2;
    let cmd = short_cmd(n, width.saturating_sub(used).max(8));
    out.push(Line::from(vec![
        Span::styled(head, Style::default().fg(theme.dim)),
        Span::styled(format!("[{tag}]"), style),
        Span::styled(stats, Style::default().fg(theme.dim)),
        Span::raw(cmd),
    ]));
    let child_prefix = if root { String::new() } else { format!("{prefix}{}", if last { "   " } else { "│  " }) };
    let n_children = n.children.len();
    for (i, c) in n.children.iter().enumerate() {
        render_node(c, &child_prefix, i + 1 == n_children, false, width, out, theme);
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    // A key badge: the letter on a coloured ground, its label dimmed beside it.
    let key = |k: &str, d: &str, bg: Color| -> Vec<Span<'static>> {
        vec![Span::styled(k.to_string(), theme.badge_style(bg)), Span::styled(format!(" {d} "), Style::default().fg(theme.dim))]
    };
    let sep = || Span::styled("│ ", Style::default().fg(theme.dim));

    let amber = theme.peach;
    let mut left = Vec::new();
    // The panels, each in the colour of its panel; the pinned one is named
    // as the way back.
    let n = app.snapshot.advice.len();
    let advice_label = if n > 0 { format!("advice ({n})") } else { "advice".to_string() };
    let panel_keys = |left: &mut Vec<Span<'static>>| {
        for p in Panel::ALL {
            let (label, colour) = match p {
                Panel::SlowTools => ("slow tools".to_string(), amber),
                Panel::FailedTools => ("fails".to_string(), theme.red),
                Panel::Advice => (advice_label.clone(), theme.advice),
                Panel::Mcp => ("mcp".to_string(), theme.mauve),
            };
            let label = if app.pinned == Some(p) && !app.standalone { format!("{label} ▸ table") } else { label };
            left.extend(key(&p.key().to_string(), &label, colour));
        }
    };
    if let Some(notice) = app.current_notice() {
        // A fresh notice takes the bar for a few seconds: it is the answer to
        // the key the user just pressed.
        left.push(Span::styled(format!(" {notice}"), Style::default().fg(theme.accent)));
    } else if app.pinned.is_some() {
        left.extend(key("↑↓/jk", "scroll", theme.accent));
        if !app.standalone {
            left.extend(key("Esc", "table", theme.accent));
        }
        if app.multiplexer.is_some() {
            left.extend(key("o", "open in pane", theme.accent));
        }
        left.extend(key("p", if app.paused { "resume" } else { "pause" }, theme.accent));
        left.push(sep());
        panel_keys(&mut left);
        left.push(sep());
        left.extend(key("?", "help", theme.accent));
    } else {
        // Navigation and view.
        left.extend(key("↑↓/jk", "select", theme.accent));
        left.extend(key("s", format!("sort:{}{}", app.sort.label(), if app.sort_desc { "↑" } else { "↓" }).as_str(), theme.accent));
        left.extend(key("r", "reverse", theme.accent));
        left.extend(key("t", if app.show_detail { "hide detail" } else { "show detail" }, theme.accent));
        left.extend(key("Tab", if app.detail == DetailView::Tree { "trace" } else { "tree" }, theme.accent));
        // `x` only matters when there are stopped sessions to hide; hiding it
        // otherwise keeps a key that does nothing off the bar.
        if app.snapshot.totals.stopped > 0 {
            left.extend(key("x", if app.show_stopped { "hide stopped" } else { "show stopped" }, theme.accent));
        }
        left.extend(key("p", if app.paused { "resume" } else { "pause" }, theme.accent));
        left.push(sep());
        panel_keys(&mut left);
        left.push(sep());
        left.extend(key("?", "help", theme.accent));
    }

    // The version badge and quit sit together at the right end. When the update
    // check has found a newer version, the badge turns amber and shows the
    // arrow to it.
    let latest = app.latest();
    let (badge, badge_bg) = match &latest {
        Some(newer) => (format!(" v{} → v{} ", crate::VERSION, newer), theme.peach),
        None => (format!(" v{} ", crate::VERSION), theme.accent),
    };
    let mut right = vec![Span::styled(badge, theme.badge_style(badge_bg)), Span::raw("  ")];
    right.extend(key("q", "quit", theme.accent));
    let right_w = right.iter().map(|s| s.content.chars().count()).sum::<usize>() as u16;
    let [left_area, right_area] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)]).areas(area);
    f.render_widget(Paragraph::new(Line::from(left)), left_area);
    f.render_widget(Paragraph::new(Line::from(right)), right_area);
}

fn draw_help(f: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::from(vec![Span::styled("keys", Style::default().fg(theme.accent).bold())]),
        Line::raw("  ↑ ↓ j k      move selection      g G     first / last"),
        Line::raw("  s            cycle sort column   r       reverse sort"),
        Line::raw("  t / Enter    toggle detail pane  x       toggle stopped rows"),
        Line::raw("  Tab / v      detail: tree ⇄ trace"),
        Line::raw("  p / Space    pause refresh       q / Esc quit"),
        Line::from(vec![
            Span::raw("  l            "),
            Span::styled("slowest tools", Style::default().fg(theme.peach)),
            Span::raw("       f       "),
            Span::styled("failed tool calls", Style::default().fg(theme.red)),
        ]),
        Line::from(vec![
            Span::raw("  a            "),
            Span::styled("advice", Style::default().fg(theme.advice)),
            Span::raw(": oversized results, idle and growing MCP servers"),
        ]),
        Line::from(vec![
            Span::raw("  m            "),
            Span::styled("mcp servers", Style::default().fg(theme.mauve)),
            Span::raw(": every server under every agent, and the orphans"),
        ]),
        Line::raw(""),
        Line::from(vec![Span::styled("panels", Style::default().fg(theme.accent).bold())]),
        Line::raw("  Each of l f a m is a popup over the table. On the popup:"),
        Line::raw("  Enter        fill the terminal with it; j k scroll, Esc back"),
        Line::raw("  o            open it in a new pane of the tmux, zellij,"),
        Line::raw("               WezTerm or kitty this runs in (shown only then)"),
        Line::raw("  agent-top slow | fails | advice | mcp   start on that panel"),
        Line::raw(""),
        Line::from(vec![Span::styled(format!("agent-top {}", crate::VERSION), Style::default().fg(theme.accent).bold())]),
        Line::raw("  upgrade   asked once when a newer version is out (u runs the"),
        Line::raw("            installer that put agent-top here, n declines); or by hand:"),
        Line::raw("            brew update && brew upgrade agent-top | cargo install agent-top"),
        Line::styled("  what's new  agent-top --whats-new", Style::default().fg(theme.dim)),
        Line::styled(format!("  changelog   {}", crate::CHANGELOG_URL), Style::default().fg(theme.dim)),
        Line::raw(""),
        Line::from(vec![Span::styled("columns", Style::default().fg(theme.accent).bold())]),
        Line::raw("  STATE   running = mid-turn, idle = waiting for you,"),
        Line::raw("          stopped = transcript with no live process"),
        Line::raw("  TOKENS  input + cache read + cache write + output"),
        Line::raw("  COST    USD at list price; '+' or '≥' = some tokens unpriced."),
        Line::raw("          The detail pane shows it per kind of token with the"),
        Line::raw("          price used, so a different figure elsewhere can be"),
        Line::raw("          traced to the one line that differs."),
        Line::raw("  PROCS   processes in the agent's tree; MCP = of those,"),
        Line::raw("          Model Context Protocol servers (name heuristic)"),
        Line::raw("  AGE     process age, or time since last write when stopped"),
        Line::raw(""),
        Line::from(vec![Span::styled("tool trace", Style::default().fg(theme.accent).bold())]),
        Line::raw("  Every tool call the harness logged, on a shared time axis."),
        Line::raw("  Width  = the call's share of the window on screen."),
        Line::raw("  Colour = how long it took: green under a second, amber a"),
        Line::raw("           few seconds, red approaching a minute."),
        Line::raw("  ↳ blue = a subagent's call,  … amber = still running,"),
        Line::raw("  ! red  = the harness reported the call as failed."),
        Line::raw("  in tools = share of the window covered by at least one call;"),
        Line::raw("           the rest of it is the model thinking."),
        Line::raw(""),
        Line::from(vec![Span::styled("context", Style::default().fg(theme.accent).bold())]),
        Line::raw("  What each tool's results added to the prompt, and what"),
        Line::raw("  re-reading them on every response since has cost. Results"),
        Line::raw("  answered together share the growth evenly: an estimate."),
        Line::raw(""),
        Line::from(vec![
            Span::styled("orphaned mcp", Style::default().fg(theme.red).bold()),
            Span::raw("  MCP-looking processes whose agent is gone."),
        ]),
        Line::raw("  Inspect with `agent-top --json`; kill with `kill <pid>`."),
    ];
    // Size the popup to its content, with margins, and cap to the screen so it
    // always fits: a fixed small box was clipping the lower half.
    let w = 80.min(area.width.saturating_sub(4));
    let h = (lines.len() as u16 + 3).min(area.height.saturating_sub(2));
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(Text::from(lines)).style(theme.style()).block(block("help", theme)).wrap(Wrap { trim: false }), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeMode;
    use agent_top_core::{HostStats, Snapshot, TokenUsage, Totals};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn span(name: &str, start_s: u64, dur_ms: Option<u64>, sidechain: bool, error: bool) -> ToolSpan {
        ToolSpan {
            id: format!("{name}{start_s}"),
            name: name.into(),
            started_at: SystemTime::UNIX_EPOCH + Duration::from_secs(start_s),
            duration_ms: dur_ms,
            sidechain,
            error,
            kind: SpanKind::Tool,
        }
    }

    fn agent(name: &str, spans: Vec<ToolSpan>) -> Agent {
        Agent {
            id: format!("pid:{}", name.len()),
            name: name.into(),
            harness: agent_top_core::Harness::Claude,
            state: AgentState::Running,
            activity: agent_top_core::Activity::Working,
            pid: Some(4242),
            session_id: Some("a29e19c3".into()),
            subagent: None,
            session_path: None,
            cwd: None,
            model: Some("claude-fable-5-1".into()),
            harness_version: Some("2.1.259".into()),
            usage: TokenUsage { input: 2, cache_write_5m: 9_900, cache_write_1h: 0, cache_read: 22_000, output: 250 },
            cost_usd: 1.42,
            cost_breakdown: agent_top_core::CostBreakdown { output: 1.42, ..Default::default() },
            price_source: Some(agent_top_core::PriceSource::Builtin),
            unpriced_tokens: 0,
            turns: 12,
            subagent_turns: 1,
            tool_calls: 71,
            tool_calls_lower_bound: false,
            web_searches: 0,
            spans,
            age_secs: 1080,
            idle_secs: Some(3),
            cpu_percent: 6.6,
            rss_bytes: 452 * 1024 * 1024,
            process_count: 4,
            mcp_count: 1,
            mcp_servers: Vec::new(),
            context: Vec::new(),
            tree: None,
            attribution: Attribution::HarnessRegistry,
            shares_process: false,
            parse_warning: None,
            rate_limit: None,
        }
    }

    #[test]
    fn old_process_subagents_render_as_agents() {
        let node = ProcNode {
            pid: 42,
            ppid: Some(41),
            name: "codex".into(),
            cmdline: "codex exec review".into(),
            kind: serde_json::from_str("\"subagent\"").unwrap(),
            harness: Some(agent_top_core::Harness::Codex),
            cpu_percent: 0.0,
            rss_bytes: 0,
            age_secs: 1,
            cwd: None,
            children: Vec::new(),
        };
        let mut lines = Vec::new();
        render_node(&node, "", true, false, 120, &mut lines, &Theme::new(ThemeMode::Dark, true));
        assert!(lines[0].to_string().contains("[agent]"));
        assert!(!lines[0].to_string().contains("subagent"));
        assert_eq!(serde_json::to_value(node.kind).unwrap(), "agent");
    }

    fn codex_family() -> Vec<Agent> {
        let mut parent = agent("main", Vec::new());
        parent.harness = agent_top_core::Harness::Codex;
        parent.session_id = Some("session-main".into());
        parent.id = "session-main".into();
        parent.subagent_turns = 0;
        parent.usage = TokenUsage { input: 100, ..Default::default() };
        parent.rss_bytes = 256 << 20;
        parent.process_count = 1;
        parent.mcp_count = 0;
        parent.tree = Some(ProcNode {
            pid: 4242,
            ppid: None,
            name: "codex".into(),
            cmdline: "codex --yolo".into(),
            kind: ProcKind::Agent,
            harness: Some(agent_top_core::Harness::Codex),
            cpu_percent: 6.6,
            rss_bytes: parent.rss_bytes,
            age_secs: 60,
            cwd: None,
            children: Vec::new(),
        });
        let mut child = parent.clone();
        child.id = "session-scout".into();
        child.session_id = Some("session-scout".into());
        child.usage.input = 200;
        child.subagent = Some(agent_top_core::SubagentInfo {
            parent_session_id: "session-main".into(),
            nickname: Some("Scout".into()),
            role: Some("explorer".into()),
        });
        child.shares_process = true;
        child.rss_bytes = 0;
        child.cpu_percent = 0.0;
        child.process_count = 0;
        child.tree = None;
        let mut grandchild = child.clone();
        grandchild.id = "session-review".into();
        grandchild.session_id = Some("session-review".into());
        grandchild.usage.input = 300;
        grandchild.subagent = Some(agent_top_core::SubagentInfo {
            parent_session_id: "session-scout".into(),
            nickname: Some("Review".into()),
            role: Some("reviewer".into()),
        });
        vec![grandchild, parent, child]
    }

    #[test]
    fn subagent_rows_nest_in_the_table_and_share_the_owners_process_tree() {
        let mut app = App::new(snapshot(codex_family()));
        app.selected_id = Some("session-scout".into());
        app.rebuild_rows();
        assert_eq!(app.row_depths, vec![0, 1, 2]);
        assert_eq!(table_session_name(&app.rows[1], 1), "↳ Scout (explorer)");
        assert_eq!(table_session_name(&app.rows[2], 2), "  ↳ Review (reviewer)");
        let selected = app.selected_agent().unwrap();
        assert_eq!(mem_cell(selected), "·");
        assert_eq!(cpu_cell(selected), "·");
        let theme = Theme::new(ThemeMode::Dark, true);
        let text = tree_detail(&app, selected, 120, &theme).to_string();
        assert!(!text.contains("session tree"), "the table carries the hierarchy: {text}");
        let (before, processes) = text.split_once("CPU/RSS shared by sessions").unwrap();
        assert!(before.is_empty(), "{text}");
        assert!(processes.contains("process tree"));
        assert!(processes.contains("rss 256M"));
        assert_eq!(processes.matches("[agent]").count(), 1, "show the owner's actual process tree once");
        assert!(!processes.contains("[subagent"));
        assert_eq!(app.snapshot.totals.tokens, 600);
        assert_eq!(app.snapshot.totals.rss_bytes, 256 << 20);
        let facts = agent_facts(selected, app.snapshot.taken_at, &theme).to_string();
        assert!(facts.contains("parent     session-main"), "{facts}");
        assert!(facts.contains("nickname   Scout"));
        assert!(!facts.contains("(0 subagent)"));
        let out = render(&mut app, 180, 48);
        assert!(out.contains("↳ Scout (explorer)"), "{out}");
        assert!(out.contains("Scout (explorer) · codex · running  [tree]"), "the detail title identifies the selected session: {out}");
        assert!(out.contains("process tree"));
    }

    #[test]
    fn kodelet_children_share_resources_and_show_recorded_costs_without_ttl_guesses() {
        let mut family = codex_family();
        for a in &mut family {
            a.harness = agent_top_core::Harness::Kodelet;
            a.price_source = Some(agent_top_core::PriceSource::Harness);
            a.tool_calls_lower_bound = true;
            a.usage.cache_write_5m = 1_000;
            a.cost_breakdown.cache_write_5m = 0.12;
            a.cost_usd = 0.12;
        }
        let mut app = App::new(snapshot(family));
        app.selected_id = Some("session-scout".into());
        app.rebuild_rows();
        assert_eq!(app.row_depths, vec![0, 1, 2]);
        assert_eq!(app.snapshot.totals.tokens, 3_600);
        assert_eq!(app.snapshot.totals.rss_bytes, 256 << 20);
        let selected = app.selected_agent().unwrap();
        let facts = agent_facts(selected, app.snapshot.taken_at, &Theme::new(ThemeMode::Dark, true)).to_string();
        assert!(facts.contains("harness-reported cost"), "{facts}");
        assert!(facts.contains("≥71") && facts.contains("earlier history may be missing"), "{facts}");
        assert!(facts.contains("cache write"), "{facts}");
        assert!(!facts.contains("cache wr 5m") && !facts.contains("cache wr 1h"), "no recorded TTL split: {facts}");
        assert!(!facts.contains("list price") && !facts.contains("subagent)"), "{facts}");
        assert!(facts.lines().any(|l| l.contains("cache write") && l.contains("0.12") && l.contains("n/a")), "{facts}");
        let out = crate::format::plain_table(&app.snapshot);
        assert!(out.contains("↳ Scout (explorer)") && out.contains("kodelet"), "{out}");
        assert!(out.contains("≥71"), "{out}");
    }

    #[test]
    fn kodelet_trace_command_keeps_the_full_timestamped_session_id() {
        let mut a = codex_family().remove(0);
        a.harness = agent_top_core::Harness::Kodelet;
        a.session_id = Some("20260919T090026-0123456789abcdef".into());
        let facts = agent_facts(&a, SystemTime::now(), &Theme::new(ThemeMode::Dark, true)).to_string();
        assert!(facts.contains("agent-top trace --session 20260919T090026-0123456789abcdef -o trace.json"), "{facts}");
    }

    #[test]
    fn orphan_session_retains_metadata_and_hidden_owner_is_still_inspectable() {
        let mut family = codex_family();
        family.iter_mut().find(|a| a.id == "session-main").unwrap().state = AgentState::Stopped;
        let mut app = App::new(snapshot(family));
        app.show_stopped = false;
        app.selected_id = Some("session-scout".into());
        app.rebuild_rows();
        assert_eq!(app.rows.len(), 2);
        let selected = app.selected_agent().unwrap();
        assert!(table_session_name(selected, 0).starts_with("[subagent]"));
        let theme = Theme::new(ThemeMode::Dark, true);
        let text = tree_detail(&app, selected, 120, &theme).to_string();
        assert!(text.contains("rss 256M"), "the hidden resource-owner row remains inspectable: {text}");
        let facts = agent_facts(selected, app.snapshot.taken_at, &theme).to_string();
        assert!(facts.contains("parent     session-main"), "the facts still name the hidden parent: {facts}");

        app.snapshot.agents.retain(|a| a.id != "session-main");
        app.rebuild_rows();
        let text = tree_detail(&app, app.selected_agent().unwrap(), 120, &theme).to_string();
        assert!(text.contains("shares its process"));
        assert!(!text.contains("[agent]"), "do not fabricate a missing process tree");
    }

    /// `--once` names and nests subagent rows the way the live table does;
    /// without it a parent and its children print the same name.
    #[test]
    fn the_plain_table_names_and_nests_subagent_rows() {
        let out = crate::format::plain_table(&snapshot(codex_family()));
        let names: Vec<&str> = out.lines().skip(1).take(3).map(|l| l[..l.find(" codex").unwrap()].trim_end()).collect();
        assert_eq!(names, ["main", "↳ Scout (explorer)", "  ↳ Review (reviewer)"], "{out}");

        let mut family = codex_family();
        family.retain(|a| a.id != "session-main");
        let out = crate::format::plain_table(&snapshot(family));
        assert!(out.lines().nth(1).unwrap().starts_with("[subagent] Scout"), "a row whose parent is not listed says so: {out}");
    }

    #[test]
    fn session_hierarchy_survives_narrow_terminals() {
        let mut app = App::new(snapshot(codex_family()));
        app.selected_id = Some("session-review".into());
        app.rebuild_rows();
        let theme = Theme::new(ThemeMode::Dark, true);
        for width in [0, 1, 8, 20, 40] {
            let _ = tree_detail(&app, app.selected_agent().unwrap(), width, &theme);
        }
        for (width, height) in [(20, 8), (40, 16), (80, 24)] {
            let _ = render(&mut app, width, height);
        }
    }

    #[test]
    fn no_lineage_keeps_the_existing_process_view() {
        let app = App::new(snapshot(vec![agent("standalone", Vec::new())]));
        let selected = app.selected_agent().unwrap();
        assert_eq!(table_session_name(selected, 0), "standalone");
        let text = tree_detail(&app, selected, 120, &Theme::new(ThemeMode::Dark, true)).to_string();
        assert!(text.starts_with("process tree"));
        assert!(!text.contains("session tree"));
        assert!(!text.contains("CPU/RSS shared"));
    }

    fn snapshot(agents: Vec<Agent>) -> Snapshot {
        let mut s = Snapshot {
            schema_version: agent_top_core::SNAPSHOT_SCHEMA_VERSION,
            // 60s after the first span starts, so an open span reads as 20s old.
            taken_at: SystemTime::UNIX_EPOCH + Duration::from_secs(160),
            host: HostStats {
                hostname: Some("test-host".into()),
                cpu_percent: 23.4,
                cpu_count: 12,
                mem_used_bytes: 19 << 30,
                mem_total_bytes: 32 << 30,
            },
            agents,
            orphans: Vec::new(),
            orphan_origins: Vec::new(),
            advice: Vec::new(),
            totals: Totals::default(),
        };
        s.compute_totals();
        s
    }

    fn render_buffer(app: &mut App, w: u16, h: u16, theme: &Theme) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app, theme)).unwrap();
        term.backend().buffer().clone()
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let buf = render_buffer(app, w, h, &Theme::new(ThemeMode::Dark, true));
        (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn text_position(buf: &Buffer, text: &str) -> (u16, u16) {
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(byte) = row.find(text) {
                return (row[..byte].chars().count() as u16, y);
            }
        }
        panic!("{text:?} not found in rendered frame");
    }

    fn assert_text_style(buf: &Buffer, text: &str, foreground: Color, background: Color) {
        let (x, y) = text_position(buf, text);
        for dx in 0..text.chars().count() as u16 {
            let cell = &buf[(x + dx, y)];
            assert_eq!((cell.fg, cell.bg), (foreground, background), "{text:?} at ({}, {y})", x + dx);
        }
    }

    #[test]
    fn both_themes_style_session_hierarchy_and_shared_processes() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            for truecolor in [true, false] {
                let theme = Theme::new(mode, truecolor);
                let mut app = App::new(snapshot(codex_family()));
                app.selected_id = Some("session-scout".into());
                app.rebuild_rows();
                let buf = render_buffer(&mut app, 180, 48, &theme);

                assert_text_style(&buf, "CPU/RSS shared by sessions", theme.dim, theme.background);
                assert_text_style(&buf, "process tree", theme.accent, theme.background);
                assert_text_style(&buf, "[agent]", theme.green, theme.background);
                assert_text_style(&buf, "nickname", theme.dim, theme.background);
            }
        }
    }

    #[test]
    fn both_themes_style_normal_text_selections_and_badges() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            for truecolor in [true, false] {
                let theme = Theme::new(mode, truecolor);
                let mut app = App::new(mcp_snapshot());
                app.show_detail = false;
                let buf = render_buffer(&mut app, 160, 40, &theme);

                assert_text_style(&buf, &app.rows[app.selected].name, theme.text, theme.selection);
                assert_text_style(&buf, &app.rows[1].name, theme.text, theme.background);
                assert_text_style(&buf, "cpu", theme.text, theme.background);
                assert_text_style(&buf, "out tok/s", theme.dim, theme.background);
                assert_text_style(&buf, "HARNESS", theme.accent, theme.background);
                assert_eq!((buf[(0, 0)].fg, buf[(0, 0)].bg), (theme.border, theme.background));
                let (x, y) = text_position(&buf, "q quit");
                let badge = theme.badge_style(theme.accent);
                assert_eq!((buf[(x, y)].fg, buf[(x, y)].bg), (badge.fg.unwrap(), theme.accent));
                assert!(buf.content.iter().all(|cell| cell.fg != Color::Reset && cell.bg != Color::Reset));
            }
        }
    }

    #[test]
    fn both_themes_restore_popup_styles_after_clear() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = Theme::new(mode, true);
            let mut app = App::new(snapshot(vec![agent("worker", Vec::new())]));
            *app.update.lock().unwrap() = Some("99.9.9".into());
            app.installer = crate::update::Installer::Homebrew;
            for (overlay, title, accent, body) in [
                (Overlay::Help, "help", theme.accent, "cycle sort column"),
                (Overlay::Update, "update available", theme.peach, "is available; this is"),
            ] {
                app.overlay = overlay;
                let buf = render_buffer(&mut app, 120, 60, &theme);
                assert_text_style(&buf, title, accent, theme.background);
                assert_text_style(&buf, body, theme.text, theme.background);
                assert!(buf.content.iter().all(|cell| cell.fg != Color::Reset && cell.bg != Color::Reset));
                if overlay == Overlay::Update {
                    assert_text_style(&buf, "  u ", theme.badge_style(theme.peach).fg.unwrap(), theme.peach);
                    assert_text_style(&buf, "  n ", theme.badge_style(theme.accent).fg.unwrap(), theme.accent);
                }
            }
            app.installer = crate::update::Installer::Unknown;
            let buf = render_buffer(&mut app, 120, 60, &theme);
            assert_text_style(&buf, "This binary was not installed", theme.text, theme.background);
            assert_text_style(&buf, "  n ", theme.badge_style(theme.accent).fg.unwrap(), theme.accent);
        }
    }

    #[test]
    fn both_themes_style_peeked_and_pinned_panels() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = Theme::new(mode, true);
            let mut snap = mcp_snapshot();
            snap.agents[0].spans.push(span("PanelTool", 100, Some(2_500), false, true));
            for panel in Panel::ALL {
                let (accent, text, foreground) = match panel {
                    Panel::SlowTools => (theme.peach, "PanelTool", theme.text),
                    Panel::FailedTools => (theme.red, "PanelTool", theme.text),
                    Panel::Advice => (theme.advice, "nothing to suggest", theme.dim),
                    Panel::Mcp => (theme.mauve, "filesystem", theme.mauve),
                };
                for pinned in [false, true] {
                    let mut app = App::new(snap.clone());
                    app.show_detail = false;
                    if pinned {
                        app.pin(panel);
                    } else {
                        app.overlay = Overlay::Panel(panel);
                    }
                    let buf = render_buffer(&mut app, 160, 44, &theme);
                    assert_text_style(&buf, panel.title(), accent, theme.background);
                    assert_text_style(&buf, text, foreground, theme.background);
                    assert!(buf.content.iter().all(|cell| cell.fg != Color::Reset && cell.bg != Color::Reset));
                    if pinned {
                        assert_eq!((buf[(1, 40)].fg, buf[(1, 40)].bg), (theme.text, theme.background));
                    }
                }
            }
        }
    }

    #[test]
    fn both_themes_use_their_meter_tracks_and_trace_ramps() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = Theme::new(mode, true);
            let mut inference = span("inference", 130, Some(5_000), false, false);
            inference.kind = SpanKind::Inference;
            let spans = vec![
                span("Clean", 100, Some(500), false, false),
                span("Sidechain", 110, Some(2_000), true, false),
                span("Failed", 120, Some(1_000), false, true),
                inference,
                span("Pending", 140, None, false, false),
            ];
            let mut app = App::new(snapshot(vec![agent("worker", spans)]));
            app.detail = DetailView::Trace;
            let buf = render_buffer(&mut app, 160, 40, &theme);

            let cpu: Vec<_> = (0..buf.area.width).map(|x| &buf[(x, 1)]).filter(|c| c.symbol() == METER_FULL).collect();
            let track = (0..buf.area.width).filter(|&x| buf[(x, 1)].symbol() == METER_TRACK).count();
            assert!(!cpu.is_empty() && track > 0);
            assert_eq!(cpu.last().unwrap().fg, theme.ramp_load.at(cpu.len() as f64 / (cpu.len() + track) as f64));

            for (label, ramp, elapsed) in [
                ("Clean", &theme.ramp_ok, 500),
                ("Sidechain", &theme.ramp_subagent, 2_000),
                ("Failed", &theme.ramp_error, 1_000),
                // Include the padding so the facts pane's model label cannot match.
                ("model         ", &theme.ramp_inference, 5_000),
                ("Pending", &theme.ramp_open, 20_000),
            ] {
                let y = (0..buf.area.height)
                    .find(|&y| {
                        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                        row.contains(label) && (row.contains(METER_FULL) || row.contains(METER_TIP))
                    })
                    .expect("trace row with a bar");
                let tip =
                    (0..buf.area.width).rev().map(|x| &buf[(x, y)]).find(|c| c.symbol() == METER_FULL || c.symbol() == METER_TIP).unwrap();
                assert_eq!((tip.fg, tip.bg), (ramp.at(heat(elapsed)), theme.background), "{mode:?}: {label}");
            }
            for cell in buf.content.iter().filter(|c| c.symbol() == METER_TRACK) {
                assert_eq!((cell.fg, cell.bg), (theme.track, theme.background));
            }
        }
    }

    /// The cost is shown one line per kind of token with the price it was
    /// charged at, so a figure that differs from another tool's can be traced
    /// to the line that differs. Run with `--nocapture` to eyeball it.
    #[test]
    fn detail_pane_breaks_the_cost_down_per_token_kind() {
        let mut a = agent("tuff", Vec::new());
        a.cost_breakdown = agent_top_core::CostBreakdown {
            input: 0.00002,
            cache_write_5m: 0.12375,
            cache_write_1h: 0.0,
            cache_read: 0.0055,
            output: 0.0125,
            web_search: 0.0,
        };
        a.cost_usd = a.cost_breakdown.total();
        let mut app = App::new(snapshot(vec![a]));
        app.detail = DetailView::Tree;
        let out = render(&mut app, 110, 60);
        println!("{out}");
        let line = |label: &str| out.lines().find(|l| l.contains(label)).unwrap_or_else(|| panic!("no {label} line in\n{out}")).to_string();
        assert!(line("$/M").contains("cost"), "column headings");
        assert!(line("cache rd").contains("22k") && line("cache rd").contains("0.25") && line("cache rd").contains("0.01"));
        assert!(line("cache wr 5m").contains("9.9k") && line("cache wr 5m").contains("12.50") && line("cache wr 5m").contains("0.12"));
        assert!(line("output").contains("50.00") && line("output").contains("0.01"));
        assert!(line("list price, built-in table").contains("$0.14"), "the total names its basis");

        // A model with no price shows the counts and says so, rather than
        // printing zeros that read as a cheap session.
        let mut a = agent("tuff", Vec::new());
        a.model = Some("gpt-9-unknown".into());
        a.price_source = None;
        a.cost_usd = 0.0;
        a.cost_breakdown = Default::default();
        a.unpriced_tokens = a.usage.total();
        let mut app = App::new(snapshot(vec![a]));
        app.detail = DetailView::Tree;
        let out = render(&mut app, 110, 60);
        let line = |label: &str| out.lines().find(|l| l.contains(label)).unwrap().to_string();
        assert!(line("cache rd").contains("n/a") && line("cache rd").contains("22k"));
        assert!(line("no price for this model").contains("n/a"));
    }

    /// Renders the whole frame with the trace panel open. Run with
    /// `cargo test -- --nocapture` to eyeball the layout.
    #[test]
    fn detail_pane_lists_mcp_servers_and_says_where_an_orphan_came_from() {
        use agent_top_core::{McpServer, OrphanParent};
        let mut a = agent("with-mcp", Vec::new());
        a.mcp_servers = vec![
            McpServer {
                name: "filesystem".into(),
                pid: Some(5001),
                cmdline: Some("npx -y @modelcontextprotocol/server-filesystem /tmp".into()),
                cpu_percent: 0.2,
                rss_bytes: 40 << 20,
                age_secs: Some(300),
                calls: 17,
                errors: 2,
                last_call: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
                matched_by: McpMatch::Name,
            },
            McpServer {
                name: "linear".into(),
                pid: None,
                cmdline: None,
                cpu_percent: 0.0,
                rss_bytes: 0,
                age_secs: None,
                calls: 3,
                errors: 0,
                last_call: None,
                matched_by: McpMatch::TranscriptOnly,
            },
        ];
        let mut snap = snapshot(vec![a]);
        snap.orphans = vec![ProcNode {
            pid: 6001,
            ppid: Some(1),
            name: "node".into(),
            cmdline: "node chrome-devtools-mcp".into(),
            kind: ProcKind::Mcp,
            harness: None,
            cpu_percent: 0.0,
            rss_bytes: 30 << 20,
            age_secs: 900,
            cwd: None,
            children: Vec::new(),
        }];
        snap.orphan_origins = vec![OrphanOrigin {
            pid: 6001,
            first_seen: SystemTime::UNIX_EPOCH,
            orphaned_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(40)),
            parent: Some(OrphanParent { pid: 4242, agent_id: "pid:4242".into(), name: "tuff-25".into() }),
        }];
        let mut app = App::new(snap);
        app.show_detail = true;
        app.detail = DetailView::Tree;
        let out = render(&mut app, 120, 40);
        assert!(out.contains("mcp servers"), "{out}");
        assert!(out.contains("filesystem"), "{out}");
        assert!(out.contains("17"), "calls column: {out}");
        assert!(out.contains("1m ago"), "last call: {out}");
        assert!(out.contains("linear"), "{out}");
        assert!(out.lines().any(|l| l.contains("linear") && l.contains("-     3   0")), "a server with no process: {out}");
        assert!(out.contains("orphaned from tuff-25 (pid 4242) 2m ago"), "{out}");
    }

    #[test]
    fn detail_pane_lists_context_by_source() {
        use agent_top_core::{ContextOrigin, ContextSource, PriceSource};
        let mut a = agent("ctx", Vec::new());
        a.price_source = Some(PriceSource::Builtin);
        let src = |name: &str, origin, calls, tokens, cost_usd| ContextSource { name: name.into(), origin, calls, tokens, cost_usd };
        a.context = vec![
            src("scratchfs", ContextOrigin::Mcp, 17, 1_200_000, 38.1),
            src("Read", ContextOrigin::Tool, 84, 812_000, 22.0),
            src("other", ContextOrigin::Other, 0, 90_000, 2.1),
        ];
        let mut app = App::new(snapshot(vec![a]));
        app.show_detail = true;
        app.detail = DetailView::Tree;
        let out = render(&mut app, 120, 40);
        assert!(out.contains("context"), "{out}");
        assert!(
            out.lines().any(|l| l.contains("mcp scratchfs") && l.contains("17") && l.contains("1.2M") && l.contains("$38.10")),
            "{out}"
        );
        assert!(out.lines().any(|l| l.contains("Read") && l.contains("84") && l.contains("812") && l.contains("$22.00")), "{out}");
        assert!(out.lines().any(|l| l.contains("prompts & replies") && l.contains("-") && l.contains("$2.10")), "{out}");

        // An unpriced model: tokens shown, cost not pretended.
        let mut a = agent("unpriced", Vec::new());
        a.price_source = None;
        a.context = vec![src("exec_command", ContextOrigin::Tool, 3, 2_524, 0.0)];
        let mut app = App::new(snapshot(vec![a]));
        app.show_detail = true;
        app.detail = DetailView::Tree;
        let out = render(&mut app, 120, 40);
        assert!(out.lines().any(|l| l.contains("exec_command") && l.contains("2.5k        -")), "{out}");
    }

    #[test]
    fn codex_nested_tools_appear_in_tree_context() {
        use agent_top_core::harness::{SessionTracker, codex::CodexTranscript};
        let path = std::env::temp_dir().join(format!("agent-top-codex-context-ui-{}.jsonl", std::process::id()));
        std::fs::write(&path, r#"{"timestamp":"1970-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"wrapper-1"}}
{"type":"event_msg","payload":{"type":"item_completed","started_at_ms":1250,"completed_at_ms":1500,"item":{"type":"CommandExecution","id":"exec-command","source":"unified_exec_startup","formatted_output":"abc"}}}
{"type":"event_msg","payload":{"type":"item_completed","started_at_ms":1600,"completed_at_ms":1700,"item":{"type":"FileChange","id":"exec-patch","stdout":"d"}}}
{"timestamp":"1970-01-01T00:00:02.000Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"wrapper-1"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000}}}}
{"timestamp":"1970-01-01T00:00:05.000Z","type":"response_item","payload":{"type":"message","role":"assistant"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1100}}}}
"#).unwrap();
        let mut transcript = CodexTranscript::new(&path);
        transcript.refresh().unwrap();
        let _ = std::fs::remove_file(path);
        let mut a = agent("code-mode", Vec::new());
        a.harness = agent_top_core::Harness::Codex;
        a.price_source = None;
        a.context = transcript.summary().context.sources();
        let mut app = App::new(snapshot(vec![a]));
        app.show_detail = true;
        app.detail = DetailView::Tree;
        let out = render(&mut app, 180, 48);
        assert!(out.contains("context"), "{out}");
        assert!(!out.contains("estimated"), "{out}");
        for (name, values) in [("apply_patch", "1      25        -"), ("exec_command", "1      75        -")] {
            assert!(out.lines().any(|l| l.contains(name) && l.contains(values)), "{out}");
        }
    }

    #[test]
    fn detail_pane_shows_the_rate_limit() {
        use agent_top_core::{RateLimit, RateWindow};
        let mut a = agent("throttled", Vec::new());
        a.rate_limit = Some(RateLimit {
            primary: Some(RateWindow {
                used_percent: 92.0,
                window_minutes: 300,
                resets_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(160 + 3600)),
            }),
            secondary: Some(RateWindow { used_percent: 27.0, window_minutes: 10080, resets_at: None }),
            plan: Some("plus".into()),
            reached: true,
        });
        let mut app = App::new(snapshot(vec![a]));
        app.show_detail = true;
        app.detail = DetailView::Tree;
        // A tall pane so the facts column shows the rate limit near its foot.
        let out = render(&mut app, 120, 110);
        assert!(out.contains("rate limit (plus)"), "{out}");
        assert!(out.contains("LIMIT REACHED"), "{out}");
        assert!(out.contains("92% used"), "{out}");
        assert!(out.contains("5h"), "{out}");
        assert!(out.contains("weekly"), "{out}");
        assert!(out.contains("resets in 1h"), "{out}");
    }

    #[test]
    fn detail_pane_shows_cache_efficiency() {
        // A wasteful session: a big prompt, almost none of it from cache.
        let mut a = agent("cold-cache", Vec::new());
        a.usage = TokenUsage { input: 90_000, cache_read: 10_000, cache_write_5m: 0, cache_write_1h: 0, output: 500 };
        let mut app = App::new(snapshot(vec![a]));
        app.show_detail = true;
        app.detail = DetailView::Tree;
        let out = render(&mut app, 120, 110);
        assert!(out.contains("10% from cache"), "{out}");
        assert!(out.contains("full price most turns"), "{out}");

        // A healthy session says the percentage without the warning.
        let mut a = agent("warm-cache", Vec::new());
        a.usage = TokenUsage { input: 10_000, cache_read: 90_000, cache_write_5m: 0, cache_write_1h: 0, output: 500 };
        let mut app = App::new(snapshot(vec![a]));
        app.show_detail = true;
        app.detail = DetailView::Tree;
        let out = render(&mut app, 120, 110);
        assert!(out.contains("90% from cache"), "{out}");
        assert!(!out.contains("full price"), "{out}");
    }

    #[test]
    fn footer_groups_features_and_hides_x_when_nothing_is_stopped() {
        // No stopped sessions: the x key is not on the bar.
        let mut app = App::new(snapshot(vec![agent("a", Vec::new())]));
        let out = render(&mut app, 120, 20);
        let footer = out.lines().last().unwrap();
        assert!(footer.contains("slow tools"), "{footer}");
        assert!(footer.contains("fails"), "{footer}");
        assert!(footer.contains("│"), "grouped with separators: {footer}");
        assert!(footer.trim_end().ends_with("quit"), "quit is at the right end: {footer:?}");
        assert!(!footer.contains("stopped"), "no x/stopped key when nothing is stopped: {footer}");

        // With a stopped session, x appears.
        let mut stopped = agent("old", Vec::new());
        stopped.state = AgentState::Stopped;
        stopped.pid = None;
        let mut app = App::new(snapshot(vec![agent("a", Vec::new()), stopped]));
        let out = render(&mut app, 120, 20);
        assert!(out.lines().last().unwrap().contains("stopped"), "x shows when something is stopped");
    }

    #[test]
    fn footer_badge_shows_the_update_when_one_is_known() {
        let mut app = App::new(snapshot(vec![agent("a", Vec::new())]));
        // No update known: the badge is just the version.
        let out = render(&mut app, 120, 20);
        assert!(out.lines().last().unwrap().contains(&format!("v{}", crate::VERSION)), "{out}");
        assert!(!out.lines().last().unwrap().contains("→"), "no arrow without an update");
        // A newer version is known: the arrow appears.
        *app.update.lock().unwrap() = Some("99.9.9".into());
        let out = render(&mut app, 120, 20);
        assert!(out.lines().last().unwrap().contains("→ v99.9.9"), "{out}");
    }

    #[test]
    fn help_popup_nudges_the_upgrade() {
        let mut app = App::new(snapshot(vec![agent("a", Vec::new())]));
        app.overlay = Overlay::Help;
        let out = render(&mut app, 90, 44);
        assert!(out.contains(&format!("agent-top {}", crate::VERSION)), "{out}");
        assert!(out.contains("brew upgrade agent-top"), "{out}");
        assert!(out.contains("--whats-new"), "{out}");
    }

    #[test]
    fn tool_panels_rank_by_time_and_by_failure() {
        // Two agents' worth of tool spans: Bash slow and sometimes failing,
        // Read fast and clean.
        let sp = |name: &str, dur: u64, err: bool| ToolSpan {
            id: name.into(),
            name: name.into(),
            started_at: SystemTime::UNIX_EPOCH,
            duration_ms: Some(dur),
            sidechain: false,
            error: err,
            kind: SpanKind::Tool,
        };
        let mut a = agent("worker", vec![sp("Bash", 20_000, false), sp("Bash", 5_000, true), sp("Read", 100, false)]);
        a.spans.push(sp("Read", 120, false));
        let mut app = App::new(snapshot(vec![a]));

        app.overlay = Overlay::Panel(Panel::SlowTools);
        let out = render(&mut app, 100, 40);
        assert!(out.contains("slowest tools"), "{out}");
        // Bash's total (25s) beats Read's, so Bash is the first data row.
        let bash_line = out.lines().find(|l| l.contains("Bash")).unwrap();
        let read_line = out.lines().find(|l| l.contains("Read")).unwrap();
        assert!(
            out.lines().position(|l| l.contains("Bash")) < out.lines().position(|l| l.contains("Read")),
            "Bash ranks above Read:\n{out}"
        );
        assert!(bash_line.contains("25.0s") || bash_line.contains("25s"), "bash total: {bash_line}");
        let _ = read_line;

        app.overlay = Overlay::Panel(Panel::FailedTools);
        let out = render(&mut app, 100, 40);
        assert!(out.contains("failed tool calls"), "{out}");
        assert!(out.lines().any(|l| l.contains("Bash")), "the failing tool is listed: {out}");
        assert!(!out.lines().any(|l| l.contains("Read")), "a clean tool is not in the failures panel: {out}");
    }

    /// The advice popup lists every piece of advice under its agent, with a
    /// mark per rule, and closes on the same key; an empty snapshot says so.
    #[test]
    fn advice_popup_lists_headlines_with_their_actions() {
        use agent_top_core::{Advice, AdviceRule};
        let mut snap = snapshot(vec![agent("worker", Vec::new())]);
        let mut app = App::new(snap.clone());
        app.on_key(ratatui::crossterm::event::KeyCode::Char('a'));
        assert_eq!(app.overlay, Overlay::Panel(Panel::Advice));
        let out = render(&mut app, 100, 40);
        assert!(out.contains("nothing to suggest"), "{out}");
        app.on_key(ratatui::crossterm::event::KeyCode::Char('a'));
        assert_eq!(app.overlay, Overlay::None);

        snap.advice = vec![
            Advice {
                agent_id: "pid:6".into(),
                agent_name: "worker".into(),
                rule: AdviceRule::ExpensiveSource,
                subject: "docs-search".into(),
                pid: None,
                headline: "docs-search MCP server: 1 call added 40k tokens to the prompt, re-read at a cost of $12.03 since".into(),
                action: "ask it for smaller results, or drop it from this project's MCP config".into(),
                cost_usd: 12.03,
                tokens: 40_000,
                calls: 1,
                rss_bytes: 0,
            },
            Advice {
                agent_id: "pid:6".into(),
                agent_name: "worker".into(),
                rule: AdviceRule::IdleMcpServer,
                subject: "server-filesystem".into(),
                pid: Some(11),
                headline: "server-filesystem (pid 11) has answered no calls in 42m and holds 180 MB".into(),
                action: "remove it from this project's MCP config; its tool definitions are sent with every response".into(),
                cost_usd: 0.0,
                tokens: 0,
                calls: 0,
                rss_bytes: 180 << 20,
            },
        ];
        let mut app = App::new(snap);
        app.overlay = Overlay::Panel(Panel::Advice);
        let out = render(&mut app, 100, 40);
        println!("{out}");
        assert!(out.contains("advice (2)"), "{out}");
        assert!(out.contains("$ docs-search MCP server: 1 call added 40k tokens"), "{out}");
        assert!(out.contains("· server-filesystem (pid 11) has answered no calls in 42m"), "{out}");
        assert!(out.contains("ask it for smaller results"), "{out}");
        // The footer badge counts it too (wide enough that the bar is not clipped).
        let wide = render(&mut app, 140, 40);
        let footer = wide.lines().last().unwrap();
        assert!(footer.contains("advice (2)"), "footer counts it: {footer}");
        // Narrow terminal: the headline wraps rather than clipping.
        let out = render(&mut app, 60, 40);
        assert!(out.contains("re-read at a cost of $12.03 since"), "{out}");
    }

    /// The update popup names both versions and the exact command `u` runs,
    /// or the manual routes when the installer is unknown.
    #[test]
    fn update_popup_shows_the_command_it_would_run() {
        let mut app = App::new(snapshot(vec![agent("worker", Vec::new())]));
        *app.update.lock().unwrap() = Some("9.9.9".into());
        app.installer = crate::update::Installer::Homebrew;
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::Update);
        let out = render(&mut app, 120, 40);
        assert!(out.contains("update available"), "{out}");
        assert!(out.contains("v9.9.9 is available; this is v"), "{out}");
        assert!(out.contains("brew update && brew upgrade agent-top"), "{out}");
        assert!(out.contains("not now"), "{out}");

        app.installer = crate::update::Installer::Unknown;
        let out = render(&mut app, 120, 40);
        assert!(out.contains("not installed by Homebrew or cargo"), "{out}");
        assert!(out.contains("releases/latest"), "{out}");
        assert!(!out.contains("upgrade now"), "no offer to run something it cannot: {out}");
    }

    #[test]
    fn draws_the_trace_waterfall() {
        let spans = vec![
            span("Bash", 100, Some(2_500), false, false),
            span("Read", 103, Some(40), false, false),
            span("Grep", 103, Some(12_000), true, false),
            span("Edit", 118, Some(300), false, true),
            span("Bash", 140, None, false, false),
        ];
        let mut app = App::new(snapshot(vec![agent("tuff-25", spans)]));
        app.detail = DetailView::Trace;
        let out = render(&mut app, 120, 26);
        println!("\n{out}\n");

        assert!(out.contains("[trace]"), "the pane says which view it is");
        assert!(out.contains("5 of 71 calls"), "header counts shown vs total: {out}");
        assert!(out.contains("↳Grep"), "subagent calls are marked");
        assert!(out.contains("12.0s"), "durations are formatted");
        assert!(out.contains("20.0s…"), "the open span is measured against taken_at");
        assert!(out.contains("300ms!"), "failed calls are flagged");
        assert!(out.contains("1 in flight"), "summary counts open calls");
        assert!(out.contains("1 failed"), "summary counts failures");
        assert!(out.contains(METER_TRACK), "bars sit in a visible track");
        assert!(out.contains(METER_TIP), "the in-flight call is tipped");
        // 100..115 and 118..118.3 and 140..160 covered out of a 60s window.
        assert!(out.contains("in tools 58%"), "busy share merges overlapping calls: {out}");
        // The waterfall is monotonic: each row starts no earlier than the one
        // above. (Filtered by tool name so the header's CPU meter is excluded.)
        let bar_start = |l: &str| l.chars().position(|c| c == METER_FULL_CH || c == METER_TIP_CH);
        let bars: Vec<usize> = out
            .lines()
            // The summary line names the slowest tool too, but has no bar.
            .filter(|l| ["Bash", "Read", "Grep", "Edit"].iter().any(|n| l.contains(n)) && bar_start(l).is_some())
            .map(|l| bar_start(l).unwrap())
            .collect();
        assert_eq!(bars.len(), 5);
        assert!(bars.windows(2).all(|w| w[0] <= w[1]), "spans are ordered along the axis: {bars:?}");
    }

    #[test]
    fn trace_panel_explains_itself_when_there_is_nothing_to_show() {
        let mut app = App::new(snapshot(vec![agent("fresh", Vec::new())]));
        app.detail = DetailView::Trace;
        let out = render(&mut app, 120, 26);
        assert!(out.contains("calls happened before agent-top started reading"), "{out}");
    }

    #[test]
    fn heat_is_log_scaled_and_bounded() {
        assert_eq!(heat(0), 0.0, "anything under the floor is the coolest colour");
        assert_eq!(heat(50), 0.0);
        assert_eq!(heat(60_000), 1.0);
        assert_eq!(heat(600_000), 1.0, "past the ceiling it saturates, it does not wrap");
        // Log scaling: each decade covers the same slice of the ramp, so the
        // interesting range (tens of ms to tens of seconds) is spread out
        // instead of being crushed against one end.
        let decade = heat(500) - heat(50);
        assert!((heat(5_000) - heat(500) - decade).abs() < 1e-9);
        assert!(heat(2_500) > heat(40), "a 2.5s call outranks a 40ms one");
    }

    /// The point of colouring by duration rather than by width: at a typical
    /// zoom almost every call is one cell wide, and two one-cell calls that
    /// took 40ms and 30s must not look identical.
    #[test]
    fn one_cell_calls_still_show_their_duration() {
        let spans = vec![span("Quick", 100, Some(40), false, false), span("Slow", 130, Some(30_000), false, false)];
        let mut app = App::new(snapshot(vec![agent("tuff-25", spans)]));
        app.detail = DetailView::Trace;
        let buf = render_buffer(&mut app, 120, 26, &Theme::new(ThemeMode::Dark, true));
        let colors: Vec<Color> = (0..buf.area.height)
            .filter_map(|y| {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                if !row.contains("Quick") && !row.contains("Slow") {
                    return None;
                }
                (0..buf.area.width).find(|x| buf[(*x, y)].symbol() == METER_FULL).map(|x| buf[(x, y)].fg)
            })
            .collect();
        assert_eq!(colors.len(), 2, "both rows drew a bar");
        assert_ne!(colors[0], colors[1], "a 40ms call and a 30s call must not share a colour");
    }

    #[test]
    fn a_longer_bar_is_a_hotter_bar() {
        // Two calls of very different length, same start, so only length differs.
        let spans = vec![span("Short", 100, Some(500), false, false), span("Long", 100, Some(59_000), false, false)];
        let mut app = App::new(snapshot(vec![agent("tuff-25", spans)]));
        app.detail = DetailView::Trace;
        let buf = render_buffer(&mut app, 120, 26, &Theme::new(ThemeMode::Dark, true));

        let tip_of = |needle: &str| -> Color {
            let row = |y: u16| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>();
            // The summary line names the slowest tool as well; the row we want
            // is the one that also has a bar on it.
            let y = (0..buf.area.height).find(|y| row(*y).contains(needle) && row(*y).contains(METER_FULL)).expect("row is on screen");
            // The colour of the bar's last filled cell is its ramp position.
            (0..buf.area.width).rev().find(|x| buf[(*x, y)].symbol() == METER_FULL).map(|x| buf[(x, y)].fg).expect("row has a bar")
        };
        assert_ne!(tip_of("Short"), tip_of("Long"), "length must change the colour, not just the width");
    }

    fn mcp_snapshot() -> Snapshot {
        use agent_top_core::{McpServer, OrphanParent};
        let server = |name: &str, pid: Option<u32>, calls: u64, by: McpMatch| McpServer {
            name: name.into(),
            pid,
            cmdline: None,
            cpu_percent: 0.2,
            rss_bytes: 40 << 20,
            age_secs: Some(300),
            calls,
            errors: 0,
            last_call: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            matched_by: by,
        };
        let mut a = agent("with-mcp", Vec::new());
        a.mcp_servers = vec![server("filesystem", Some(5001), 17, McpMatch::Name), server("linear", None, 3, McpMatch::TranscriptOnly)];
        let mut b = agent("other-repo", Vec::new());
        b.mcp_servers = vec![server("scratchfs", Some(5002), 2, McpMatch::Sole)];
        let mut snap = snapshot(vec![a, b]);
        snap.orphans = vec![ProcNode {
            pid: 6001,
            ppid: Some(1),
            name: "node".into(),
            cmdline: "node chrome-devtools-mcp".into(),
            kind: ProcKind::Mcp,
            harness: None,
            cpu_percent: 0.0,
            rss_bytes: 30 << 20,
            age_secs: 900,
            cwd: None,
            children: Vec::new(),
        }];
        snap.orphan_origins = vec![OrphanOrigin {
            pid: 6001,
            first_seen: SystemTime::UNIX_EPOCH,
            orphaned_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(40)),
            parent: Some(OrphanParent { pid: 4242, agent_id: "pid:4242".into(), name: "tuff-25".into() }),
        }];
        snap
    }

    /// The MCP panel is the machine's servers, not one agent's: every agent's
    /// rows, a guessed pid marked, and the orphans with where they came from.
    /// Pinned, it replaces the table; the footer says how to get back.
    #[test]
    fn the_pinned_mcp_panel_lists_every_agents_servers_and_the_orphans() {
        let mut app = App::new(mcp_snapshot());
        app.pin(Panel::Mcp);
        let out = render(&mut app, 120, 30);
        assert!(out.contains("3 servers under 2 agents"), "{out}");
        assert!(out.lines().any(|l| l.contains("with-mcp") && l.contains("filesystem") && l.contains("5001")), "{out}");
        assert!(
            out.lines().any(|l| l.contains("other-repo") && l.contains("scratchfs") && l.contains("5002?")),
            "a sole match is marked: {out}"
        );
        assert!(out.lines().any(|l| l.contains("linear") && l.contains(" - ")), "no process, no pid: {out}");
        assert!(out.contains("orphaned from tuff-25 (pid 4242) 2m ago"), "{out}");
        assert!(out.contains("agent-top mcp"), "the standalone command is on the frame: {out}");
        assert!(!out.contains("AGENT ") || !out.contains("HARNESS"), "the table is gone: {out}");
        assert!(out.contains("Esc") && out.contains("table"), "the footer offers the way back: {out}");
        // Scrolling past the end is clamped where the height is known.
        app.scroll = u16::MAX;
        render(&mut app, 120, 30);
        assert!(app.scroll < 30, "clamped to the content: {}", app.scroll);
    }

    /// The peek carries the ways on from it: Enter always, `o` with the exact
    /// command only inside a multiplexer.
    #[test]
    fn a_peek_offers_enter_and_the_pane_command_only_inside_a_multiplexer() {
        let mut app = App::new(mcp_snapshot());
        app.overlay = Overlay::Panel(Panel::Mcp);
        let out = render(&mut app, 120, 40);
        assert!(out.contains("full screen · m or Esc to close"), "{out}");
        assert!(!out.contains("open in a"), "no multiplexer, no o: {out}");
        assert!(out.contains("filesystem") && out.contains("AGENT"), "the table is still underneath: {out}");

        app.multiplexer = Some(crate::pane::Multiplexer::Tmux);
        let out = render(&mut app, 120, 40);
        assert!(out.contains("open in a tmux pane:  tmux split-window -h -d 'agent-top mcp'"), "{out}");

        // A standalone run keeps the peeks and drops the way back.
        app.standalone = true;
        app.pin(Panel::Mcp);
        let out = render(&mut app, 120, 30);
        assert!(!out.contains("▸ table"), "{out}");
        app.overlay = Overlay::Panel(Panel::Advice);
        let out = render(&mut app, 120, 30);
        assert!(out.contains("nothing to suggest"), "a peek over a pinned panel: {out}");
    }

    /// Every panel, pinned and peeked, at the sizes people actually use.
    #[test]
    fn every_panel_survives_a_narrow_terminal_pinned_and_peeked() {
        let spans = vec![span("SomeVeryLongToolName", 100, Some(2_500), false, true), span("Bash", 159, None, false, false)];
        let mut snap = mcp_snapshot();
        snap.agents.push(agent("tuff-25", spans));
        for p in Panel::ALL {
            let mut app = App::new(snap.clone());
            app.multiplexer = Some(crate::pane::Multiplexer::Zellij);
            for (w, h) in [(40u16, 12u16), (60, 10), (200, 60), (24, 8)] {
                app.pin(p);
                let out = render(&mut app, w, h);
                assert!(out.lines().all(|l| l.chars().count() <= w as usize), "pinned {p:?} overflows {w} columns");
                app.pinned = None;
                app.overlay = Overlay::Panel(p);
                let out = render(&mut app, w, h);
                assert!(out.lines().all(|l| l.chars().count() <= w as usize), "peeked {p:?} overflows {w} columns");
                app.overlay = Overlay::None;
            }
        }
    }

    /// The panel must not panic or overflow at the sizes people actually use.
    #[test]
    fn survives_a_narrow_terminal() {
        let spans = vec![span("SomeVeryLongToolName", 100, Some(2_500), false, false), span("Bash", 159, None, false, false)];
        let mut app = App::new(snapshot(vec![agent("tuff-25", spans)]));
        app.detail = DetailView::Trace;
        for (w, h) in [(40u16, 12u16), (60, 10), (200, 60), (24, 8)] {
            let out = render(&mut app, w, h);
            assert!(out.lines().all(|l| l.chars().count() <= w as usize), "no row overflows {w} columns");
        }
    }
}
