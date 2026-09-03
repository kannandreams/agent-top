//! Rendering. Layout, top to bottom: header (host gauges + totals), agent
//! table, optional detail pane (process tree + token breakdown), key bar.

use crate::app::{App, DetailView};
use crate::format::{age, bytes, cost, duration_ms, short_cmd, short_model, tokens, truncate};
use agent_top_core::{Agent, AgentState, Attribution, ProcKind, ProcNode, ToolSpan};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Wrap};
use std::time::{Duration, SystemTime};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

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
    let title = format!("{} · {} · {}  [{}]", a.name, a.harness.label(), a.state.label(), app.detail.label());
    let outer = block(&title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [left, right] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner);
    f.render_widget(Paragraph::new(agent_facts(a)).wrap(Wrap { trim: false }), left);
    let panel = match app.detail {
        DetailView::Tree => process_tree(a, &app.snapshot.orphans, right.width as usize),
        DetailView::Trace => tool_trace(a, app.snapshot.taken_at, right.width as usize, right.height as usize),
    };
    f.render_widget(Paragraph::new(panel), right);
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

    // Two header lines, then one row per span, newest at the bottom.
    let rows = height.saturating_sub(2).max(1);
    let shown: Vec<&ToolSpan> = {
        let mut v: Vec<&ToolSpan> = a.spans.iter().rev().take(rows).collect();
        v.reverse();
        v
    };
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

    let mut lines = vec![
        head(vec![Span::styled(
            format!("   {} of {} calls · window {}", shown.len(), a.tool_calls, duration_ms(window_ms)),
            Style::default().fg(DIM),
        )]),
        Line::from(trace_summary(&shown, now, window_ms)),
    ];
    let track = Style::default().fg(term_color(TRACK_RGB));
    for s in &shown {
        let elapsed = s.elapsed_ms(now);
        let start = cell(s.started_at).min(bar_w.saturating_sub(1));
        let end = cell(s.ended_at(now)).clamp(start + 1, bar_w);
        // The ramp says how long the call took; the marker and the name colour
        // say what kind of call it was. Keeping those on separate channels
        // means a slow call and a failed call never compete for one colour.
        let (ramp, mark) = if s.error {
            (&RAMP_ERROR, "!")
        } else if s.is_open() {
            (&RAMP_OPEN, "…")
        } else if s.sidechain {
            (&RAMP_SUBAGENT, " ")
        } else {
            (&RAMP_OK, " ")
        };
        let name_style = match (s.error, s.is_open(), s.sidechain) {
            (true, _, _) => Style::default().fg(RAMP_ERROR.at(1.0)),
            (_, true, _) => Style::default().fg(RAMP_OPEN.at(0.8)),
            (_, _, true) => Style::default().fg(RAMP_SUBAGENT.at(0.0)),
            _ => Style::default().fg(Color::White),
        };
        let name = format!("{}{}", if s.sidechain { "↳" } else { "" }, s.name);
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

/// The line under the trace header: where the window actually went.
fn trace_summary(shown: &[&ToolSpan], now: SystemTime, window_ms: u64) -> Vec<Span<'static>> {
    let slowest = shown.iter().max_by_key(|s| s.elapsed_ms(now));
    let errors = shown.iter().filter(|s| s.error).count();
    let open = shown.iter().filter(|s| s.is_open()).count();
    let share = busy_ms(shown, now) * 100 / window_ms;
    let mut spans = vec![
        Span::styled("  in tools ", Style::default().fg(DIM)),
        Span::styled(format!("{share}%"), Style::default().fg(RAMP_OK.at(share as f64 / 100.0)).bold()),
    ];
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
    spans.extend(key("Tab", if app.detail == DetailView::Tree { "trace" } else { "tree" }));
    spans.extend(key("x", if app.show_stopped { "hide stopped" } else { "show stopped" }));
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
            unpriced_tokens: 0,
            turns: 12,
            subagent_turns: 1,
            tool_calls: 71,
            spans,
            age_secs: 1080,
            idle_secs: Some(3),
            cpu_percent: 6.6,
            rss_bytes: 452 * 1024 * 1024,
            process_count: 4,
            mcp_count: 1,
            tree: None,
            attribution: Attribution::HarnessRegistry,
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

    /// Renders the whole frame with the trace panel open. Run with
    /// `cargo test -- --nocapture` to eyeball the layout.
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
