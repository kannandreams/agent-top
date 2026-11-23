//! The context timeline (`c`, RFC-110 D4): one agent's prompt size at every
//! model response, with a rule where each compaction happened and a table of
//! them underneath. Full width, between the header and the footer.
//!
//! The x axis is the response number, not the clock: a session idles for
//! hours between turns, and a time axis would spend the width on the gaps.
//! The prompt line and the compactions are the harness's figures; the split
//! by source (`s`) is the context ledger's estimate and says so.

use crate::format::{tokens, truncate, utc_minute};
use crate::theme::Theme;
use agent_top_core::{Agent, Compaction, CompactionSource, CompactionTrigger, ContextPoint};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Clear, Dataset, GraphType, Paragraph};

/// One agent's timeline: the main agent's, or one subagent's, which starts
/// from an empty context of its own.
struct Timeline<'a> {
    who: String,
    points: &'a [ContextPoint],
    compactions: &'a [Compaction],
    /// The model's window, when the harness wrote it. The main agent's only:
    /// a subagent may run another model.
    window: Option<u64>,
}

/// The main agent's timeline, then each subagent's that has a response, in
/// start order.
fn timelines(a: &Agent) -> Vec<Timeline<'_>> {
    let main = Timeline { who: "main agent".into(), points: &a.context_points, compactions: &a.compactions, window: a.context_window };
    let subs = a.subagent_runs.iter().filter(|r| !r.context_points.is_empty()).map(|r| Timeline {
        who: format!("{} {}", r.agent_type.as_deref().unwrap_or("subagent"), truncate(&r.id, 8)),
        points: &r.context_points,
        compactions: &r.compactions,
        window: None,
    });
    std::iter::once(main).chain(subs).collect()
}

/// How many timelines `Tab` steps through for this agent.
pub fn timeline_count(a: &Agent) -> usize {
    timelines(a).len()
}

/// Draw the overlay over `area`, the space between the header and footer.
/// `which` picks the timeline: 0 is the main agent, then its subagents.
pub fn draw(f: &mut Frame, area: Rect, agent: Option<&Agent>, which: usize, split: bool, theme: &Theme) {
    f.render_widget(Clear, area);
    let all = agent.map(timelines).unwrap_or_default();
    let t = (!all.is_empty()).then(|| &all[which % all.len()]);
    let mut title = vec![Span::styled(" context ", Style::default().fg(theme.accent).bold())];
    if let (Some(a), Some(t)) = (agent, t) {
        let n = t.points.len();
        let c = t.compactions.len();
        let who = if all.len() > 1 { format!("{} {}/{} · ", t.who, which % all.len() + 1, all.len()) } else { String::new() };
        title.push(Span::styled(
            format!(
                "· {} · {who}{} responses · {} {} ",
                crate::format::session_name(a),
                n,
                c,
                if c == 1 { "compaction" } else { "compactions" }
            ),
            Style::default().fg(theme.dim),
        ));
        if let Some(w) = t.window {
            title.push(Span::styled(format!("· window {} ", tokens(w)), Style::default().fg(theme.dim)));
        }
        if split {
            title.push(Span::styled("· by source, estimated: ", Style::default().fg(theme.dim)));
            title.push(Span::styled("tools ", Style::default().fg(theme.accent)));
            title.push(Span::styled("mcp ", Style::default().fg(theme.mauve)));
            title.push(Span::styled("prompts & replies ", Style::default().fg(theme.dim)));
        }
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::from(title))
        .title_top(Line::from(Span::styled(" Esc close ", Style::default().fg(theme.accent))).right_aligned())
        .style(theme.style());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(t) = t.filter(|t| !t.points.is_empty()) else {
        let msg = match (agent, all.len()) {
            (None, _) => "No agent selected.",
            (Some(a), 1) if a.context_now.is_some() => {
                let msg = format!(
                    " {} records only the context's current size ({}{}), not one per response, so there is no timeline.",
                    a.harness.label(),
                    tokens(a.context_now.unwrap_or(0)),
                    a.context_window.map(|w| format!(" of {}", tokens(w))).unwrap_or_default()
                );
                f.render_widget(Paragraph::new(Line::styled(msg, Style::default().fg(theme.dim))), inner);
                return;
            }
            (_, 1) => "No model response with token usage has been read for this agent yet.",
            _ => "No model response with token usage has been read for the main agent yet. Tab shows its subagents.",
        };
        f.render_widget(Paragraph::new(Line::styled(format!(" {msg}"), Style::default().fg(theme.dim))), inner);
        return;
    };

    let rows = t.compactions.len() as u16 + 2; // header, compactions, now
    let table_h = rows.min(inner.height / 2).max(3);
    let [chart_area, table_area] = Layout::vertical([Constraint::Min(6), Constraint::Length(table_h)]).areas(inner);
    draw_chart(f, chart_area, t, split, theme);
    f.render_widget(Paragraph::new(table_lines(t, table_h as usize, theme)), table_area);
}

/// Keep at most `buckets` points, each the largest in its stretch, so a
/// thousand responses still show every peak at terminal resolution.
fn downsample(points: &[ContextPoint], buckets: usize, value: impl Fn(&ContextPoint) -> u64) -> Vec<(f64, f64)> {
    let n = points.len();
    let buckets = buckets.max(1);
    if n <= buckets {
        return points.iter().enumerate().map(|(i, p)| ((i + 1) as f64, value(p) as f64)).collect();
    }
    (0..buckets)
        .filter_map(|b| {
            let (lo, hi) = (b * n / buckets, ((b + 1) * n / buckets).max(b * n / buckets + 1));
            (lo..hi.min(n)).max_by_key(|&i| value(&points[i])).map(|i| ((i + 1) as f64, value(&points[i]) as f64))
        })
        .collect()
}

fn draw_chart(f: &mut Frame, area: Rect, a: &Timeline, split: bool, theme: &Theme) {
    let pts = a.points;
    let n = pts.len();
    let peak = pts.iter().map(|p| p.prompt).chain(a.compactions.iter().map(|c| c.before)).chain(a.window).max().unwrap_or(0);
    let y_max = (peak as f64 * 1.08).max(1.0);
    let buckets = area.width.saturating_sub(9) as usize * 2;

    // A rule between the last response before a compaction and the first
    // after it: responses are numbered from 1, so `seq` + 0.5.
    let rules: Vec<(Vec<(f64, f64)>, &Compaction)> = a
        .compactions
        .iter()
        .map(|c| {
            let x = c.seq as f64 + 0.5;
            (vec![(x, 0.0), (x, y_max)], c)
        })
        .collect();
    let mut sets: Vec<Dataset> = rules
        .iter()
        .map(|(d, c)| {
            // A rule the transcript recorded is solid; an inferred one is dotted.
            let marker = if c.detected_by == CompactionSource::Harness { Marker::Braille } else { Marker::Dot };
            Dataset::default().marker(marker).graph_type(GraphType::Line).style(Style::default().fg(theme.peach)).data(d)
        })
        .collect();

    // The window, as the harness wrote it, dotted across the top.
    let window: Vec<(f64, f64)> = a.window.map(|w| vec![(1.0, w as f64), ((n as f64).max(2.0), w as f64)]).unwrap_or_default();
    if !window.is_empty() {
        sets.push(Dataset::default().marker(Marker::Dot).graph_type(GraphType::Line).style(Style::default().fg(theme.dim)).data(&window));
    }
    let lines: Vec<(Vec<(f64, f64)>, Style)> = if split {
        vec![
            (downsample(pts, buckets, |p| p.other), Style::default().fg(theme.dim)),
            (downsample(pts, buckets, |p| p.other + p.mcp), Style::default().fg(theme.mauve)),
            (downsample(pts, buckets, |p| p.other + p.mcp + p.tool), Style::default().fg(theme.accent)),
        ]
    } else {
        vec![(downsample(pts, buckets, |p| p.prompt), Style::default().fg(theme.accent))]
    };
    sets.extend(lines.iter().map(|(d, s)| Dataset::default().marker(Marker::Braille).graph_type(GraphType::Line).style(*s).data(d)));

    let x_max = (n as f64).max(2.0);
    let dim = Style::default().fg(theme.dim);
    let x_labels =
        vec![Span::styled("1", dim), Span::styled(format!("{}", n.div_ceil(2)), dim), Span::styled(format!("{n} responses"), dim)];
    let y_labels = vec![Span::styled("0", dim), Span::styled(tokens((y_max / 2.0) as u64), dim), Span::styled(tokens(y_max as u64), dim)];
    let chart = Chart::new(sets)
        .style(theme.style())
        .x_axis(Axis::default().bounds([1.0, x_max]).labels(x_labels).style(dim))
        .y_axis(Axis::default().bounds([0.0, y_max]).labels(y_labels).style(dim));
    f.render_widget(chart, area);
}

/// The compactions, newest last, then where the context is now. When they do
/// not all fit, the oldest go.
fn table_lines(a: &Timeline, height: usize, theme: &Theme) -> Vec<Line<'static>> {
    let dim = Style::default().fg(theme.dim);
    let mut out = vec![Line::styled(
        format!(
            " {:<3} {:<12} {:<9} {:>8} {:>8} {:>8} {:>9}   {}",
            "#", "at (UTC)", "trigger", "before", "summary", "after", "responses", "filled by (est.)"
        ),
        dim,
    )];
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut prev_seq = 0;
    for (i, c) in a.compactions.iter().enumerate() {
        let trigger = match (c.detected_by, c.trigger) {
            (CompactionSource::PromptHalved, _) => "inferred",
            (_, CompactionTrigger::Auto) => "auto",
            (_, CompactionTrigger::Manual) => "manual",
            (_, CompactionTrigger::Unknown) => "-",
        };
        let opt = |v: Option<u64>| v.map(tokens).unwrap_or_else(|| "-".into());
        let filled = c
            .sources_before
            .iter()
            .filter(|s| s.origin != agent_top_core::ContextOrigin::Other && c.before > 0 && s.tokens * 100 >= c.before)
            .take(2)
            .map(|s| format!("{} {}%", truncate(&s.name, 14), s.tokens * 100 / c.before))
            .collect::<Vec<_>>()
            .join("  ");
        rows.push(Line::from(vec![
            Span::styled(format!(" {:<3} ", i + 1), Style::default().fg(theme.peach)),
            Span::raw(format!(
                "{:<12} {:<9} {:>8} {:>8} {:>8} {:>9}   ",
                c.at.map(utc_minute).unwrap_or_else(|| "-".into()),
                trigger,
                tokens(c.before),
                opt(c.summary),
                opt(c.after),
                c.seq - prev_seq
            )),
            Span::styled(filled, dim),
        ]));
        prev_seq = c.seq;
    }
    if let Some(last) = a.points.last() {
        let share = |t: u64| (t * 100).checked_div(last.prompt).unwrap_or(0);
        rows.push(Line::from(vec![
            Span::styled(format!(" {:<3} ", "now"), Style::default().fg(theme.green)),
            Span::raw(format!(
                "{:<12} {:<9} {:>8} {:>8} {:>8} {:>9}   ",
                last.at.map(utc_minute).unwrap_or_else(|| "-".into()),
                "",
                tokens(last.prompt),
                "",
                "",
                a.points.len() - prev_seq
            )),
            Span::styled(format!("tools {}%  mcp {}%  prompts & replies {}%", share(last.tool), share(last.mcp), share(last.other)), dim),
        ]));
    }
    let keep = height.saturating_sub(1);
    let skip = rows.len().saturating_sub(keep);
    out.extend(rows.into_iter().skip(skip));
    out
}

/// One line for the detail pane: where the context is now, and the key that
/// opens the timeline.
pub fn summary_line(a: &Agent, theme: &Theme) -> Option<Line<'static>> {
    let Some(last) = a.context_points.last() else {
        // Only the current size, with no series behind it: say so, and offer
        // no timeline and no claim about compactions.
        let now = a.context_now?;
        let s = match a.context_window.filter(|w| *w > 0) {
            Some(w) => format!("  {} now of {} ({}%)", tokens(now), tokens(w), now * 100 / w),
            None => format!("  {} now", tokens(now)),
        };
        return Some(Line::from(vec![
            Span::raw(s),
            Span::styled(" · the harness keeps no size per response", Style::default().fg(theme.dim)),
        ]));
    };
    let n = a.compactions.len();
    let mut s = match a.context_window.filter(|w| *w > 0) {
        Some(w) => format!("  {} now of {} ({}%)", tokens(last.prompt), tokens(w), last.prompt * 100 / w),
        None => format!("  {} now", tokens(last.prompt)),
    };
    if let Some(c) = a.compactions.last() {
        s.push_str(&format!(
            " · {n} {} · last {} responses ago",
            if n == 1 { "compaction" } else { "compactions" },
            a.context_points.len() - c.seq
        ));
    } else {
        s.push_str(" · no compactions");
    }
    match timeline_count(a) - 1 {
        0 => {}
        1 => s.push_str(" · 1 subagent"),
        n => s.push_str(&format!(" · {n} subagents")),
    }
    Some(Line::from(vec![Span::raw(s), Span::styled("   c timeline", Style::default().fg(theme.peach))]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(prompt: u64) -> ContextPoint {
        ContextPoint { prompt, other: prompt, ..Default::default() }
    }

    #[test]
    fn downsampling_keeps_every_peak() {
        let pts: Vec<_> = (0..1_000).map(|i| pt(if i == 637 { 900 } else { i % 50 })).collect();
        let d = downsample(&pts, 40, |p| p.prompt);
        assert_eq!(d.len(), 40);
        assert!(d.contains(&(638.0, 900.0)), "the peak survives at its own response number");
        assert_eq!(downsample(&pts[..10], 40, |p| p.prompt).len(), 10, "nothing to drop");
    }
}
