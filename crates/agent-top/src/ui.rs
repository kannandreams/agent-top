//! Rendering. Layout, top to bottom: header (host gauges + totals), agent
//! table, optional detail pane (process tree + token breakdown), key bar.

use crate::app::{App, DetailView, Overlay};
use crate::format::{age, bytes, cost, cpu_cell, duration_ms, mem_cell, short_cmd, short_model, tokens, tokens_cell, truncate};
use agent_top_core::{Agent, AgentState, Attribution, McpMatch, OrphanOrigin, ProcKind, ProcNode, SpanKind, ToolSpan};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Wrap};
use std::time::{Duration, SystemTime};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
/// Panel borders. Bright enough to actually divide the screen; DarkGray reads
/// as noise next to the meters rather than as structure.
const BORDER_RGB: (u8, u8, u8) = (0x8b, 0x96, 0xa8);

/// Column widths of the totals block in the header. The label column is wide
/// enough that the longest label still leaves a gap before the number.
const STAT_LABEL_W: usize = 10;
const STAT_VALUE_W: usize = 7;

fn state_style(s: AgentState) -> Style {
    match s {
        AgentState::Running => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        AgentState::Idle => Style::default().fg(Color::Yellow),
        AgentState::Stopped => Style::default().fg(DIM),
    }
}

// ── meters ──────────────────────────────────────────────────────────────────
//
// btop's meters read as magnitude before you read the number, because colour
// is interpolated along the meter's own length: a bar that gets longer gets
// hotter. The texture comes from the seven-eighths block, which most terminal
// fonts render with a one-pixel gap at the cell's right edge, so a run of them
// looks like segments rather than one slab. Unfilled cells keep the same
// texture in near-black, which is what makes the track visible as a channel.

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

/// A three-stop colour ramp, interpolated across a meter's length.
struct Ramp([(u8, u8, u8); 3]);

/// Duration of a finished tool call: cool when short, hot when it eats the window.
const RAMP_OK: Ramp = Ramp([(0x4c, 0xc3, 0x8a), (0xd8, 0xc0, 0x4a), (0xe0, 0x7b, 0x39)]);
/// A subagent's call. A different hue family so a sidechain is obvious at a
/// glance, still ramped by duration.
const RAMP_SUBAGENT: Ramp = Ramp([(0x4a, 0x8f, 0xe0), (0x5a, 0xc8, 0xd8), (0xb0, 0x6a, 0xe0)]);
/// A call that has not come back yet.
const RAMP_OPEN: Ramp = Ramp([(0xb0, 0x8a, 0x2a), (0xe0, 0xc0, 0x40), (0xf5, 0xe8, 0x8a)]);
/// A call the harness reported as failed. Red family, so it reads as wrong and
/// not merely slow.
const RAMP_ERROR: Ramp = Ramp([(0x9c, 0x2b, 0x4e), (0xdc, 0x3c, 0x50), (0xff, 0x77, 0x94)]);
/// The model thinking: deliberately muted, so tool calls stay the foreground
/// and the gaps between them read as labelled rather than loud.
const RAMP_INFERENCE: Ramp = Ramp([(0x4e, 0x4e, 0x62), (0x6e, 0x6e, 0x8c), (0x90, 0x90, 0xb4)]);
/// Host CPU and memory, btop's classic green-amber-red.
const RAMP_LOAD: Ramp = Ramp([(0x4c, 0xc3, 0x8a), (0xd8, 0xc0, 0x4a), (0xe0, 0x45, 0x45)]);

const TRACK_RGB: (u8, u8, u8) = (0x3a, 0x3a, 0x3a);

impl Ramp {
    /// Colour at `t` in 0..=1, linear between the three stops.
    fn rgb_at(&self, t: f64) -> (u8, u8, u8) {
        let t = t.clamp(0.0, 1.0);
        let (a, b, local) = if t < 0.5 { (self.0[0], self.0[1], t * 2.0) } else { (self.0[1], self.0[2], (t - 0.5) * 2.0) };
        let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * local).round() as u8;
        (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
    }

    fn at(&self, t: f64) -> Color {
        term_color(self.rgb_at(t))
    }
}

/// True colour where the terminal advertises it, the closest xterm-256 cube
/// entry everywhere else. Without the fallback a 256-colour terminal renders
/// the whole ramp as one flat approximation and the gradient disappears.
fn truecolor() -> bool {
    use std::sync::OnceLock;
    static TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *TRUECOLOR.get_or_init(|| std::env::var("COLORTERM").map(|v| v.contains("truecolor") || v.contains("24bit")).unwrap_or(false))
}

fn term_color(rgb: (u8, u8, u8)) -> Color {
    if truecolor() { Color::Rgb(rgb.0, rgb.1, rgb.2) } else { Color::Indexed(xterm256(rgb)) }
}

/// Nearest entry in the xterm-256 palette: the 24-step grey ramp for colours
/// that are near-grey, the 6x6x6 cube otherwise.
fn xterm256((r, g, b): (u8, u8, u8)) -> u8 {
    if r.abs_diff(g) < 12 && g.abs_diff(b) < 12 && r.abs_diff(b) < 12 {
        let level = (r as u16 + g as u16 + b as u16) / 3;
        return 232 + (level * 23 / 255) as u8;
    }
    let axis = |v: u8| (v as u16 * 5 / 255) as u8;
    16 + 36 * axis(r) + 6 * axis(g) + axis(b)
}

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
fn meter_spans(filled: usize, width: usize, ramp: &Ramp) -> Vec<Span<'static>> {
    let filled = filled.min(width);
    let mut spans = textured(filled, ramp, 0.0, filled as f64 / width as f64, false);
    if filled < width {
        spans.push(Span::styled(METER_TRACK.repeat(width - filled), Style::default().fg(term_color(TRACK_RGB))));
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
fn meter_line(label: String, ratio: f64, width: usize) -> Line<'static> {
    let label_w = label.chars().count();
    let bar_w = width.saturating_sub(label_w + 1);
    let mut spans = vec![Span::styled(label, Style::default().fg(Color::White)), Span::raw(" ")];
    if bar_w >= 4 {
        spans.extend(meter_spans((ratio.clamp(0.0, 1.0) * bar_w as f64).round() as usize, bar_w, &RAMP_LOAD));
    }
    Line::from(spans)
}

fn block(title: &str) -> Block<'_> {
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(term_color(BORDER_RGB))).title(Line::from(vec![
        Span::raw(" "),
        Span::styled(title, Style::default().fg(ACCENT).bold()),
        Span::raw(" "),
    ]))
}

/// One row of the totals block: a fixed-width label, a fixed-width headline
/// number, then the detail that qualifies it. The fixed columns are what make
/// the four rows read as a table rather than as ragged sentences.
fn stat_line(label: &str, value: String, value_style: Style, rest: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{label:<STAT_LABEL_W$}"), Style::default().fg(DIM)),
        // Right-aligned: the numbers line up on their units, which is the
        // whole point of giving them a column of their own.
        Span::styled(format!("{value:>STAT_VALUE_W$}"), value_style),
        Span::raw("  "),
    ];
    spans.extend(rest);
    Line::from(spans)
}

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(DIM))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
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
    draw_header(f, app, header);
    draw_table(f, app, table);
    if app.show_detail {
        draw_detail(f, app, detail);
    }
    draw_footer(f, app, footer);
    match app.overlay {
        Overlay::Help => draw_help(f, area),
        Overlay::SlowTools => draw_tool_panel(f, area, &app.snapshot, ToolPanel::Slow),
        Overlay::FailedTools => draw_tool_panel(f, area, &app.snapshot, ToolPanel::Failed),
        Overlay::None => {}
    }
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

/// A centred popup: the slow-tools or failed-tools leaderboard, from the tool
/// spans already on screen. Amber for time, red for failures, so the panel is
/// recognisable at a glance.
fn draw_tool_panel(f: &mut Frame, area: Rect, snap: &agent_top_core::Snapshot, panel: ToolPanel) {
    let (title, accent, key) = match panel {
        ToolPanel::Slow => ("slowest tools", Color::Rgb(220, 160, 40), "l"),
        ToolPanel::Failed => ("failed tool calls", Color::Red, "f"),
    };
    let mut stats = tool_stats(snap);
    match panel {
        ToolPanel::Slow => stats.sort_by(|a, b| b.total_ms.cmp(&a.total_ms).then(b.max_ms.cmp(&a.max_ms))),
        ToolPanel::Failed => {
            stats.retain(|s| s.errors > 0);
            stats.sort_by(|a, b| b.errors.cmp(&a.errors).then(b.calls.cmp(&a.calls)));
        }
    }

    let w = 66.min(area.width.saturating_sub(2));
    let h = (stats.len() as u16 + 6).clamp(8, 26).min(area.height.saturating_sub(2));
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);

    let mut lines: Vec<Line> = Vec::new();
    if stats.is_empty() {
        let msg = match panel {
            ToolPanel::Slow => "no tool calls on screen yet",
            ToolPanel::Failed => "no failed tool calls on screen — all green",
        };
        lines.push(Line::styled(format!("  {msg}"), Style::default().fg(DIM)));
    } else {
        let header = match panel {
            ToolPanel::Slow => format!("  {:<22}{:>7}{:>9}{:>9}{:>7}", "tool", "calls", "total", "avg", "max"),
            ToolPanel::Failed => format!("  {:<22}{:>8}{:>8}{:>9}", "tool", "fails", "calls", "fail%"),
        };
        lines.push(Line::styled(header, Style::default().fg(DIM)));
        let cap = h.saturating_sub(5) as usize;
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
            lines.push(Line::styled(format!("  … {} more", stats.len() - cap), Style::default().fg(DIM)));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(format!("  aggregated from the tool trace on screen · {key} or Esc to close"), Style::default().fg(DIM)));

    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(format!(" {title} "), Style::default().fg(accent).bold()));
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), popup);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let snap = &app.snapshot;
    let host = &snap.host;
    let title = format!(
        "agent-top {}{}{}",
        crate::VERSION,
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
    // Leave a gutter so the meter never runs into the totals column.
    let w = (cpu_row.width as usize).saturating_sub(3);
    f.render_widget(Paragraph::new(meter_line(format!("cpu {cpu:>5.1}% ({:>2} cores)", host.cpu_count), cpu / 100.0, w)), cpu_row);
    let mem_pct = if host.mem_total_bytes > 0 { host.mem_used_bytes as f64 * 100.0 / host.mem_total_bytes as f64 } else { 0.0 };
    f.render_widget(
        Paragraph::new(meter_line(
            format!("mem {:>5.1}% {:>5}/{:>5}", mem_pct, bytes(host.mem_used_bytes), bytes(host.mem_total_bytes)),
            mem_pct / 100.0,
            w,
        )),
        mem_row,
    );
    let [spark_label, spark] = Layout::horizontal([Constraint::Length(11), Constraint::Min(4)]).areas(spark_row);
    f.render_widget(Paragraph::new(Span::styled("out tok/s ", Style::default().fg(DIM))), spark_label);
    f.render_widget(Sparkline::default().data(&app.output_rate).style(Style::default().fg(ACCENT)), spark);

    let t = &snap.totals;
    let lines = vec![
        stat_line(
            "agents",
            t.agents.to_string(),
            Style::default().bold(),
            vec![
                Span::styled(format!("{:>2} running", t.running), Style::default().fg(Color::Green)),
                dim("   "),
                Span::styled(format!("{:>2} idle", t.idle), Style::default().fg(Color::Yellow)),
                dim("   "),
                Span::styled(format!("{:>2} stopped", t.stopped), Style::default().fg(DIM)),
            ],
        ),
        stat_line(
            "tokens",
            tokens(t.tokens),
            Style::default().bold(),
            vec![
                dim("total cost "),
                Span::styled(
                    format!("${:.2}{}", t.cost_usd, if t.unpriced_tokens > 0 { "+" } else { "" }),
                    Style::default().fg(Color::Magenta).bold(),
                ),
                if t.unpriced_tokens > 0 { dim(format!("  ({} unpriced)", tokens(t.unpriced_tokens))) } else { Span::raw("") },
            ],
        ),
        stat_line(
            "procs",
            t.processes.to_string(),
            Style::default(),
            vec![
                dim("mcp "),
                Span::raw(format!("{:<4}", t.mcp_processes)),
                dim("orphaned "),
                if t.orphaned_mcp > 0 {
                    Span::styled(t.orphaned_mcp.to_string(), Style::default().fg(Color::Red).bold())
                } else {
                    Span::raw("0")
                },
            ],
        ),
        // What the agents themselves are costing the machine, as opposed to
        // the whole-host meters on the left.
        stat_line(
            "agent use",
            format!("{:.1}%", t.cpu_percent),
            Style::default(),
            vec![dim("cpu · "), Span::raw(bytes(t.rss_bytes)), dim(" resident")],
        ),
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
        let mem = mem_cell(a);
        let cpu = cpu_cell(a);
        let mcp_style = if a.mcp_count > 0 { Style::default().fg(Color::Magenta) } else { Style::default() };
        Row::new(vec![
            Cell::from(truncate(&a.name, 26)).style(Style::default().bold()),
            Cell::from(a.harness.label()),
            Cell::from(a.state.label()).style(state_style(a.state)),
            Cell::from(a.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into())),
            Cell::from(short_model(a.model.as_deref())),
            Cell::from(tokens_cell(a)),
            Cell::from(cost(a)).style(Style::default().fg(Color::Magenta)),
            Cell::from(cpu),
            Cell::from(mem),
            Cell::from(a.tool_calls.to_string()),
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
            Span::styled(
                "start claude, codex or gemini in another terminal, or run agent-top --json to debug discovery.",
                Style::default().fg(DIM),
            ),
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
    let title = format!("{} · {} · {}  [{}]", a.name, a.harness.label(), a.state.label(), app.detail.label());
    let outer = block(&title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner);
    f.render_widget(Paragraph::new(agent_facts(a, app.snapshot.taken_at)).wrap(Wrap { trim: false }), left);
    let panel = match app.detail {
        DetailView::Tree => {
            process_tree(a, &app.snapshot.orphans, &app.snapshot.orphan_origins, app.snapshot.taken_at, right.width as usize)
        }
        DetailView::Trace => tool_trace(a, app.snapshot.taken_at, right.width as usize, right.height as usize),
    };
    f.render_widget(Paragraph::new(panel), right);
}

fn kv<'a>(k: &'a str, v: String) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<11}"), Style::default().fg(DIM)), Span::raw(v)])
}

/// One line of the cost breakdown: tokens, the price they were charged at,
/// and what that came to. The price column is the row's current model's; the
/// cost column is exact even when the session changed model part way.
fn cost_row(label: &str, n: u64, per_m: Option<f64>, usd: f64) -> Line<'static> {
    let (per_m, usd) = match per_m {
        Some(p) => (format!("{p:>9.2}"), format!("{usd:>10.2}")),
        None => (format!("{:>9}", "n/a"), format!("{:>10}", "-")),
    };
    Line::from(vec![
        Span::styled(format!("{label:<13}"), Style::default().fg(DIM)),
        Span::raw(format!("{:>7}", tokens(n))),
        dim(per_m),
        Span::raw(usd),
    ])
}

/// What the cost figure is priced at, so a user comparing it with another
/// tool's number knows which table produced it.
fn price_basis(a: &Agent) -> String {
    match a.price_source {
        Some(agent_top_core::PriceSource::Builtin) => "list price, built-in table".into(),
        Some(agent_top_core::PriceSource::UserFile) => "your price file".into(),
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
fn rate_window_line(kind: &str, w: &agent_top_core::RateWindow, now: SystemTime) -> Line<'static> {
    let pct = w.used_percent;
    let colour = if pct >= 90.0 {
        Color::Red
    } else if pct >= 75.0 {
        Color::Rgb(220, 160, 40)
    } else {
        Color::Green
    };
    let resets = match w.resets_at {
        Some(t) => match t.duration_since(now) {
            Ok(d) => format!("resets in {}", age(d.as_secs())),
            Err(_) => "resetting now".to_string(),
        },
        None => String::new(),
    };
    Line::from(vec![
        Span::styled(format!("  {:<9}", window_label(w.window_minutes)), Style::default().fg(DIM)),
        Span::styled(format!("{pct:>4.0}% used"), Style::default().fg(colour)),
        dim(format!("   {kind}")),
        if resets.is_empty() { Span::raw(String::new()) } else { dim(format!("   {resets}")) },
    ])
}

/// How much of the prompt is being served from cache, coloured by how good
/// that is. Blank on a session too small to judge, so it does not cry "0%" on
/// a two-turn session that never built a cache.
fn cache_line(a: &Agent) -> Line<'static> {
    // Under a few thousand prompt tokens there is nothing meaningful to say.
    let prompt = a.usage.prompt();
    let Some(rate) = a.usage.cache_hit_rate().filter(|_| prompt >= 5_000) else {
        return kv("cache", "-".into());
    };
    let pct = rate * 100.0;
    let colour = if pct >= 70.0 {
        Color::Green
    } else if pct >= 40.0 {
        Color::Rgb(220, 160, 40)
    } else {
        Color::Red
    };
    let note = if pct < 40.0 { "   full price most turns" } else { "" };
    Line::from(vec![
        Span::styled(format!("{:<11}", "cache"), Style::default().fg(DIM)),
        Span::styled(format!("{pct:.0}% from cache"), Style::default().fg(colour)),
        dim(note.to_string()),
    ])
}

fn agent_facts(a: &Agent, now: SystemTime) -> Text<'static> {
    let u = &a.usage;
    let b = &a.cost_breakdown;
    let price = a.model.as_deref().and_then(agent_top_core::pricing::price_for);
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
        lines.push(Line::from(vec![Span::styled(format!("⚠ {w}"), Style::default().fg(Color::Red).bold())]));
        lines.push(Line::raw(""));
    }
    // Identity first, then the headline numbers a glance wants (cost, cache,
    // turns, tools) high up, so a short detail pane never clips them; the
    // per-token cost breakdown, a dig-deeper detail, comes below them.
    lines.extend(vec![
        kv("session", a.session_id.clone().unwrap_or_else(|| "-".into())),
        kv("cwd", a.cwd.as_deref().map(tilde).unwrap_or_else(|| "-".into())),
        kv("model", a.model.clone().unwrap_or_else(|| "-".into())),
        kv("version", a.harness_version.clone().unwrap_or_else(|| "-".into())),
        kv("activity", format!("{:?}{}", a.activity, a.idle_secs.map(|s| format!(", last write {} ago", age(s))).unwrap_or_default())),
        kv("attributed", attribution.to_string()),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{:<11}", "cost"), Style::default().fg(DIM)),
            Span::styled(cost(a), Style::default().bold()),
            dim(format!("   {}", price_basis(a))),
        ]),
        cache_line(a),
        kv("tokens", tokens(u.total())),
        kv("turns", format!("{} ({} subagent)", a.turns, a.subagent_turns)),
        kv("tool calls", a.tool_calls.to_string()),
    ]);
    if a.web_searches > 0 {
        let priced = if b.web_search > 0.0 { format!(" (${:.2})", b.web_search) } else { " (not priced)".to_string() };
        lines.push(kv("web searches", format!("{}{priced}", a.web_searches)));
    }
    // The per-token cost breakdown, below the headline stats.
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<13}{:>7}", "breakdown", ""), Style::default().fg(ACCENT).bold()),
        dim(format!("{:>9}{:>10}", "$/M", "cost")),
    ]));
    lines.extend(vec![
        cost_row("  input", u.input, price.map(|p| p.input), b.input),
        cost_row("  cache rd", u.cache_read, price.map(|p| p.cache_read), b.cache_read),
        cost_row("  cache wr 5m", u.cache_write_5m, price.map(|p| p.cache_write_5m), b.cache_write_5m),
        cost_row("  cache wr 1h", u.cache_write_1h, price.map(|p| p.cache_write_1h), b.cache_write_1h),
        cost_row("  output", u.output, price.map(|p| p.output), b.output),
    ]);
    if let Some(rl) = &a.rate_limit {
        lines.push(Line::raw(""));
        let head = match &rl.plan {
            Some(plan) => format!("rate limit ({plan})"),
            None => "rate limit".to_string(),
        };
        let mut spans = vec![Span::styled(format!("{head:<20}"), Style::default().fg(ACCENT).bold())];
        if rl.reached {
            spans.push(Span::styled("LIMIT REACHED", Style::default().fg(Color::Red).bold()));
        }
        lines.push(Line::from(spans));
        for (label, w) in [("primary", &rl.primary), ("secondary", &rl.secondary)] {
            if let Some(w) = w {
                lines.push(rate_window_line(label, w, now));
            }
        }
    }
    if let Some(p) = &a.session_path {
        lines.push(kv("transcript", tilde(p)));
    }
    if let Some(id) = &a.session_id {
        // The first eight characters are almost always unique on one machine,
        // and are what a user can type. The command resolves a prefix.
        let short: String = id.chars().take(8).collect();
        lines.push(kv("export", format!("agent-top trace --session {short} -o trace.json")));
    }
    Text::from(lines)
}

/// A waterfall of the agent's recent tool calls: one row per call, positioned
/// and sized on a shared time axis, so a single long call and a storm of short
/// ones look different at a glance. Answers "why has this agent been busy for
/// eight minutes", which the table alone cannot.
fn tool_trace(a: &Agent, now: SystemTime, width: usize, height: usize) -> Text<'static> {
    let head = |extra: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled("tool trace", Style::default().fg(ACCENT).bold())];
        spans.extend(extra);
        Line::from(spans)
    };
    if a.spans.is_empty() {
        return Text::from(vec![
            head(vec![Span::styled(format!("   {} tool calls", a.tool_calls), Style::default().fg(DIM))]),
            Line::styled(
                if a.tool_calls > 0 { "  (calls happened before agent-top started reading)" } else { "  (no tool calls yet)" },
                Style::default().fg(DIM),
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
            format!("   {} of {} calls · window {}", calls, a.tool_calls, duration_ms(window_ms)),
            Style::default().fg(DIM),
        )]),
        Line::from(trace_summary(&shown, turn, now, window_ms)),
    ];
    let track = Style::default().fg(term_color(TRACK_RGB));
    for s in &shown {
        let elapsed = s.elapsed_ms(now);
        let start = cell(s.started_at).min(bar_w.saturating_sub(1));
        let end = cell(s.ended_at(now)).clamp(start + 1, bar_w);
        // The ramp says how long the call took; the marker and the name colour
        // say what kind of call it was. Keeping those on separate channels
        // means a slow call and a failed call never compete for one colour.
        let inference = s.kind == SpanKind::Inference;
        let (ramp, mark) = if s.error {
            (&RAMP_ERROR, "!")
        } else if s.is_open() {
            (&RAMP_OPEN, "…")
        } else if inference {
            (&RAMP_INFERENCE, " ")
        } else if s.sidechain {
            (&RAMP_SUBAGENT, " ")
        } else {
            (&RAMP_OK, " ")
        };
        let name_style = match (s.error, s.is_open(), inference, s.sidechain) {
            (true, _, _, _) => Style::default().fg(RAMP_ERROR.at(1.0)),
            (_, true, _, _) => Style::default().fg(RAMP_OPEN.at(0.8)),
            (_, _, true, _) => Style::default().fg(DIM),
            (_, _, _, true) => Style::default().fg(RAMP_SUBAGENT.at(0.0)),
            _ => Style::default().fg(Color::White),
        };
        let label = if inference { "model" } else { s.name.as_str() };
        let name = format!("{}{}", if s.sidechain { "↳" } else { "" }, label);
        let mut row = vec![
            Span::styled(format!("{:<NAME_W$}", truncate(&name, NAME_W)), name_style),
            Span::styled(format!("{:>DUR_W$}", format!("{}{mark}", duration_ms(elapsed))), Style::default().fg(DIM)),
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
fn trace_summary(shown: &[&ToolSpan], turn: Option<&ToolSpan>, now: SystemTime, window_ms: u64) -> Vec<Span<'static>> {
    let tools: Vec<&ToolSpan> = shown.iter().copied().filter(|s| s.kind == SpanKind::Tool).collect();
    let thinking: Vec<&ToolSpan> = shown.iter().copied().filter(|s| s.kind == SpanKind::Inference).collect();
    let slowest = tools.iter().max_by_key(|s| s.elapsed_ms(now));
    let errors = tools.iter().filter(|s| s.error).count();
    let open = tools.iter().filter(|s| s.is_open()).count();
    let share = busy_ms(&tools, now) * 100 / window_ms;
    let mut spans = vec![
        Span::styled("  in tools ", Style::default().fg(DIM)),
        Span::styled(format!("{share}%"), Style::default().fg(RAMP_OK.at(share as f64 / 100.0)).bold()),
    ];
    if !thinking.is_empty() {
        let share = busy_ms(&thinking, now) * 100 / window_ms;
        spans.push(Span::styled("  model ", Style::default().fg(DIM)));
        spans.push(Span::styled(format!("{share}%"), Style::default().fg(RAMP_INFERENCE.at(1.0)).bold()));
    }
    if let Some(t) = turn {
        let mark = if t.is_open() { "…" } else { "" };
        spans.push(Span::styled(format!("  turn {}{mark}", duration_ms(t.elapsed_ms(now))), Style::default().fg(DIM)));
    }
    if let Some(s) = slowest {
        spans.push(Span::styled(
            format!("  slowest {} {}", truncate(&s.name, 14), duration_ms(s.elapsed_ms(now))),
            Style::default().fg(DIM),
        ));
    }
    if open > 0 {
        spans.push(Span::styled(format!("  {open} in flight"), Style::default().fg(Color::Yellow)));
    }
    if errors > 0 {
        spans.push(Span::styled(format!("  {errors} failed"), Style::default().fg(Color::Red)));
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

fn process_tree(a: &Agent, orphans: &[ProcNode], origins: &[OrphanOrigin], now: SystemTime, width: usize) -> Text<'static> {
    let mut lines = vec![Line::from(vec![
        Span::styled("process tree", Style::default().fg(ACCENT).bold()),
        Span::styled(
            format!("   {} procs · {} mcp · cpu {:.1}% · rss {}", a.process_count, a.mcp_count, a.cpu_percent, bytes(a.rss_bytes)),
            Style::default().fg(DIM),
        ),
    ])];
    match &a.tree {
        None if a.shares_process => {
            lines.push(Line::styled("  shares its process with another conversation; see the row that owns it", Style::default().fg(DIM)))
        }
        None => lines.push(Line::styled("  (no live process)", Style::default().fg(DIM))),
        Some(root) => render_node(root, "", true, true, width, &mut lines),
    }
    if !a.mcp_servers.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("mcp servers", Style::default().fg(Color::Magenta).bold()),
            Span::styled("   calls from the transcript; pid? = process guessed", Style::default().fg(DIM)),
        ]));
        lines.push(Line::styled(
            format!("  {:<14} {:>6} {:>5} {:>3} {:>9} {:>5} {:>6}", "server", "pid", "calls", "err", "last call", "cpu", "rss"),
            Style::default().fg(DIM),
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
            let err_style = if m.errors > 0 { Style::default().fg(Color::Red) } else { Style::default().fg(DIM) };
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14} ", truncate(&m.name, 14)), Style::default().fg(Color::Magenta)),
                Span::styled(format!("{pid:>6} "), Style::default().fg(DIM)),
                Span::raw(format!("{:>5} ", m.calls)),
                Span::styled(format!("{:>3} ", m.errors), err_style),
                Span::styled(format!("{last:>9} {cpu:>5} {rss:>6}"), Style::default().fg(DIM)),
            ]));
        }
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
            if let Some(origin) = origins.iter().find(|x| x.pid == o.pid) {
                lines.push(Line::styled(format!("         {}", orphan_origin(origin, now)), Style::default().fg(DIM)));
            }
        }
        if orphans.len() > 8 {
            lines.push(Line::styled(format!("  … {} more", orphans.len() - 8), Style::default().fg(DIM)));
        }
    }
    Text::from(lines)
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
    // The sort direction used to live in the header; it belongs next to the
    // key that changes it.
    spans.extend(key("s", format!("sort:{}{}", app.sort.label(), if app.sort_desc { "↑" } else { "↓" }).as_str()));
    spans.extend(key("r", "reverse"));
    spans.extend(key("t", if app.show_detail { "hide detail" } else { "show detail" }));
    spans.extend(key("Tab", if app.detail == DetailView::Tree { "trace" } else { "tree" }));
    spans.extend(key("x", if app.show_stopped { "hide stopped" } else { "show stopped" }));
    spans.extend(key("l", "slow tools"));
    spans.extend(key("f", "fails"));
    spans.extend(key("p", if app.paused { "resume" } else { "pause" }));
    spans.extend(key("?", "help"));
    spans.extend(key("q", "quit"));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let w = 62.min(area.width.saturating_sub(2));
    let h = 30.min(area.height.saturating_sub(2));
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);
    let text = Text::from(vec![
        Line::from(vec![Span::styled("keys", Style::default().fg(ACCENT).bold())]),
        Line::raw("  ↑ ↓ j k      move selection      g G     first / last"),
        Line::raw("  s            cycle sort column   r       reverse sort"),
        Line::raw("  t / Enter    toggle detail pane  x       toggle stopped rows"),
        Line::raw("  Tab / v      detail: tree ⇄ trace"),
        Line::raw("  p / Space    pause refresh       q / Esc quit"),
        Line::from(vec![
            Span::raw("  l            "),
            Span::styled("slowest tools", Style::default().fg(Color::Rgb(220, 160, 40))),
            Span::raw("       f       "),
            Span::styled("failed tool calls", Style::default().fg(Color::Red)),
        ]),
        Line::raw(""),
        Line::from(vec![Span::styled(format!("agent-top {}", crate::VERSION), Style::default().fg(ACCENT).bold())]),
        Line::raw("  upgrade   brew upgrade agent-top   |   cargo install agent-top"),
        Line::styled("  what's new  agent-top --whats-new", Style::default().fg(DIM)),
        Line::styled(format!("  changelog   {}", crate::CHANGELOG_URL), Style::default().fg(DIM)),
        Line::raw(""),
        Line::from(vec![Span::styled("columns", Style::default().fg(ACCENT).bold())]),
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
        Line::from(vec![Span::styled("tool trace", Style::default().fg(ACCENT).bold())]),
        Line::raw("  Every tool call the harness logged, on a shared time axis."),
        Line::raw("  Width  = the call's share of the window on screen."),
        Line::raw("  Colour = how long it took: green under a second, amber a"),
        Line::raw("           few seconds, red approaching a minute."),
        Line::raw("  ↳ blue = a subagent's call,  … amber = still running,"),
        Line::raw("  ! red  = the harness reported the call as failed."),
        Line::raw("  in tools = share of the window covered by at least one call;"),
        Line::raw("           the rest of it is the model thinking."),
        Line::raw(""),
        Line::from(vec![
            Span::styled("orphaned mcp", Style::default().fg(Color::Red).bold()),
            Span::raw("  MCP-looking processes whose agent is gone."),
        ]),
        Line::raw("  Inspect with `agent-top --json`; kill with `kill <pid>`."),
    ]);
    f.render_widget(Paragraph::new(text).block(block("help")).wrap(Wrap { trim: false }), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_top_core::{HostStats, Snapshot, TokenUsage, Totals};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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
            web_searches: 0,
            spans,
            age_secs: 1080,
            idle_secs: Some(3),
            cpu_percent: 6.6,
            rss_bytes: 452 * 1024 * 1024,
            process_count: 4,
            mcp_count: 1,
            mcp_servers: Vec::new(),
            tree: None,
            attribution: Attribution::HarnessRegistry,
            shares_process: false,
            parse_warning: None,
            rate_limit: None,
        }
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
            totals: Totals::default(),
        };
        s.compute_totals();
        s
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
                row.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
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

        app.overlay = Overlay::SlowTools;
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

        app.overlay = Overlay::FailedTools;
        let out = render(&mut app, 100, 40);
        assert!(out.contains("failed tool calls"), "{out}");
        assert!(out.lines().any(|l| l.contains("Bash")), "the failing tool is listed: {out}");
        assert!(!out.lines().any(|l| l.contains("Read")), "a clean tool is not in the failures panel: {out}");
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
    fn the_ramp_runs_cool_to_hot() {
        // Ends are the stops themselves, the middle is the middle stop.
        assert_eq!(RAMP_OK.rgb_at(0.0), (0x4c, 0xc3, 0x8a));
        assert_eq!(RAMP_OK.rgb_at(0.5), (0xd8, 0xc0, 0x4a));
        assert_eq!(RAMP_OK.rgb_at(1.0), (0xe0, 0x7b, 0x39));
        // Out-of-range input is clamped, not wrapped: a bar cannot go cold
        // again by being longer than the meter.
        assert_eq!(RAMP_OK.rgb_at(4.0), RAMP_OK.rgb_at(1.0));
        assert_eq!(RAMP_OK.rgb_at(-1.0), RAMP_OK.rgb_at(0.0));
        // Green channel falls and red rises as a call takes longer.
        let (r0, g0, _) = RAMP_OK.rgb_at(0.1);
        let (r1, g1, _) = RAMP_OK.rgb_at(0.9);
        assert!(r1 > r0 && g1 < g0, "{r0},{g0} -> {r1},{g1}");
        // The error ramp stays in the red family at every point, so a failure
        // never reads as a merely slow call.
        for i in 0..=10 {
            let (r, g, b) = RAMP_ERROR.rgb_at(i as f64 / 10.0);
            assert!(r > g && r > b, "error ramp went off-hue at {i}: {r},{g},{b}");
        }
    }

    #[test]
    fn falls_back_to_the_256_colour_palette() {
        // Cube entries: index 16 is black, 231 is white.
        assert_eq!(xterm256((0, 0, 0)), 232, "near-black lands on the grey ramp");
        assert_eq!(xterm256((255, 0, 0)), 16 + 36 * 5, "pure red");
        assert_eq!(xterm256((0, 255, 0)), 16 + 6 * 5, "pure green");
        assert_eq!(xterm256((0x3a, 0x3a, 0x3a)), 232 + (0x3a_u16 * 23 / 255) as u8, "the track is a grey");
        // Every ramp position must map into the palette's valid range.
        for ramp in [&RAMP_OK, &RAMP_SUBAGENT, &RAMP_OPEN, &RAMP_ERROR, &RAMP_LOAD] {
            for i in 0..=20 {
                let idx = xterm256(ramp.rgb_at(i as f64 / 20.0));
                assert!(idx >= 16, "index {idx} collides with the terminal's own ANSI colours");
            }
        }
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
        let mut term = Terminal::new(TestBackend::new(120, 26)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
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
        let mut term = Terminal::new(TestBackend::new(120, 26)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();

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
