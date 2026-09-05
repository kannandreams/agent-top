//! UI state: selection, sort, toggles and short histories for sparklines.

use agent_top_core::{Agent, AgentState, Snapshot};
use ratatui::crossterm::event::KeyCode;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    State,
    Name,
    Tokens,
    Cost,
    Cpu,
    Mem,
    Age,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::State => "state",
            SortKey::Name => "name",
            SortKey::Tokens => "tokens",
            SortKey::Cost => "cost",
            SortKey::Cpu => "cpu",
            SortKey::Mem => "mem",
            SortKey::Age => "age",
        }
    }
    fn next(self) -> SortKey {
        match self {
            SortKey::State => SortKey::Name,
            SortKey::Name => SortKey::Tokens,
            SortKey::Tokens => SortKey::Cost,
            SortKey::Cost => SortKey::Cpu,
            SortKey::Cpu => SortKey::Mem,
            SortKey::Mem => SortKey::Age,
            SortKey::Age => SortKey::State,
        }
    }
}

/// Which panel the right half of the detail pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailView {
    /// The live process tree, plus orphaned MCP servers.
    Tree,
    /// A waterfall of the agent's recent tool calls.
    Trace,
}

impl DetailView {
    pub fn label(self) -> &'static str {
        match self {
            DetailView::Tree => "tree",
            DetailView::Trace => "trace",
        }
    }

    fn next(self) -> DetailView {
        match self {
            DetailView::Tree => DetailView::Trace,
            DetailView::Trace => DetailView::Tree,
        }
    }
}

const HISTORY: usize = 120;

/// Output tokens per second are measured over this window rather than per
/// tick. A harness reports usage once per assistant message, so tick-to-tick
/// deltas are zeros with spikes between them; ten seconds smooths a turn into
/// a rate and still drops back to zero soon after the agents go quiet.
const RATE_WINDOW: Duration = Duration::from_secs(10);

/// Which full-screen popup, if any, is over the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    /// Tool calls ranked by how much time they took.
    SlowTools,
    /// Tool calls ranked by how often they failed.
    FailedTools,
}

pub struct App {
    pub snapshot: Snapshot,
    pub rows: Vec<Agent>,
    pub selected_id: Option<String>,
    pub selected: usize,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub show_detail: bool,
    pub detail: DetailView,
    pub overlay: Overlay,
    pub show_stopped: bool,
    pub paused: bool,
    pub cpu_history: Vec<u64>,
    /// Output tokens per second across every agent, one entry per tick.
    pub output_rate: Vec<u64>,
    pub cost_history: Vec<u64>,
    /// (when, output tokens across every agent) for the last `RATE_WINDOW`.
    rate_samples: VecDeque<(Instant, u64)>,
    /// The latest published version when it is newer than this build, filled by
    /// the update check; `None` otherwise. The footer reads it each frame.
    pub update: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        let mut app = App {
            snapshot,
            rows: Vec::new(),
            selected_id: None,
            selected: 0,
            sort: SortKey::State,
            sort_desc: false,
            show_detail: true,
            detail: DetailView::Tree,
            overlay: Overlay::None,
            show_stopped: true,
            paused: false,
            cpu_history: Vec::new(),
            output_rate: Vec::new(),
            cost_history: Vec::new(),
            rate_samples: VecDeque::new(),
            update: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        app.rebuild_rows();
        app
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        self.update_at(snapshot, Instant::now());
    }

    /// `update` with the clock injected, so the rate can be tested.
    pub fn update_at(&mut self, snapshot: Snapshot, now: Instant) {
        push(&mut self.cpu_history, snapshot.host.cpu_percent.round() as u64);
        // Output only: cache reads and prompt tokens are the context being
        // re-sent, not work being produced, and they dwarf the output by a
        // hundred to one on a long session.
        let output: u64 = snapshot.agents.iter().map(|a| a.usage.output).sum();
        self.rate_samples.push_back((now, output));
        // Keep the newest sample that is at least a window old as the anchor,
        // so the rate always spans a full window once there is that much
        // history.
        while self.rate_samples.len() > 2 && self.rate_samples[1].0 + RATE_WINDOW <= now {
            self.rate_samples.pop_front();
        }
        push(&mut self.output_rate, output_per_second(&self.rate_samples));
        push(&mut self.cost_history, (snapshot.totals.cost_usd * 100.0) as u64);
        self.snapshot = snapshot;
        self.rebuild_rows();
    }

    pub fn rebuild_rows(&mut self) {
        let mut rows: Vec<Agent> =
            self.snapshot.agents.iter().filter(|a| self.show_stopped || a.state != AgentState::Stopped).cloned().collect();
        let key = self.sort;
        rows.sort_by(|a, b| {
            let ord = match key {
                SortKey::State => a.state.cmp(&b.state).then_with(|| b.usage.total().cmp(&a.usage.total())),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Tokens => b.usage.total().cmp(&a.usage.total()),
                SortKey::Cost => b.cost_usd.partial_cmp(&a.cost_usd).unwrap_or(std::cmp::Ordering::Equal),
                SortKey::Cpu => b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap_or(std::cmp::Ordering::Equal),
                SortKey::Mem => b.rss_bytes.cmp(&a.rss_bytes),
                SortKey::Age => b.age_secs.cmp(&a.age_secs),
            };
            if self.sort_desc { ord.reverse() } else { ord }
        });
        self.rows = rows;
        // Keep the cursor on the same agent across refreshes.
        if let Some(id) = &self.selected_id
            && let Some(i) = self.rows.iter().position(|a| &a.id == id)
        {
            self.selected = i;
        }
        if self.rows.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.rows.len() {
            self.selected = self.rows.len() - 1;
        }
        self.selected_id = self.rows.get(self.selected).map(|a| a.id.clone());
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.rows.get(self.selected)
    }

    fn select(&mut self, i: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = i.min(self.rows.len() - 1);
        self.selected_id = Some(self.rows[self.selected].id.clone());
    }

    pub fn on_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::PageDown => self.select(self.selected + 10),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(10)),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.rebuild_rows();
            }
            KeyCode::Char('r') => {
                self.sort_desc = !self.sort_desc;
                self.rebuild_rows();
            }
            KeyCode::Char('t') | KeyCode::Enter => self.show_detail = !self.show_detail,
            // Cycling the view opens the pane rather than switching a panel
            // nobody can see.
            KeyCode::Tab | KeyCode::Char('v') => {
                if self.show_detail {
                    self.detail = self.detail.next();
                } else {
                    self.show_detail = true;
                }
            }
            KeyCode::Char('x') => {
                self.show_stopped = !self.show_stopped;
                self.rebuild_rows();
            }
            KeyCode::Char('p') | KeyCode::Char(' ') => self.paused = !self.paused,
            KeyCode::Char('h') | KeyCode::Char('?') | KeyCode::F(1) => self.toggle(Overlay::Help),
            KeyCode::Char('l') => self.toggle(Overlay::SlowTools),
            KeyCode::Char('f') => self.toggle(Overlay::FailedTools),
            KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
    }

    /// Open the given overlay, or close it if it is already the one showing.
    fn toggle(&mut self, o: Overlay) {
        self.overlay = if self.overlay == o { Overlay::None } else { o };
    }
}

/// Output tokens per second between the oldest and newest sample. An agent
/// leaving the snapshot can make the total fall; that reads as zero, not as
/// a negative rate.
fn output_per_second(samples: &VecDeque<(Instant, u64)>) -> u64 {
    let (Some((t0, n0)), Some((t1, n1))) = (samples.front(), samples.back()) else { return 0 };
    let secs = t1.duration_since(*t0).as_secs_f64();
    if secs <= 0.0 {
        return 0;
    }
    (n1.saturating_sub(*n0) as f64 / secs).round() as u64
}

fn push(v: &mut Vec<u64>, x: u64) {
    v.push(x);
    if v.len() > HISTORY {
        let drop = v.len() - HISTORY;
        v.drain(..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_top_core::{Activity, Attribution, Harness, HostStats, TokenUsage, Totals};
    use std::time::SystemTime;

    fn snapshot(output: u64) -> Snapshot {
        let agent = Agent {
            id: "pid:1".into(),
            name: "claude".into(),
            harness: Harness::Claude,
            state: AgentState::Running,
            activity: Activity::Working,
            pid: Some(1),
            session_id: None,
            session_path: None,
            cwd: None,
            model: None,
            harness_version: None,
            usage: TokenUsage { input: 10, cache_write_5m: 0, cache_write_1h: 0, cache_read: 500_000, output },
            cost_usd: 0.0,
            cost_breakdown: Default::default(),
            price_source: None,
            unpriced_tokens: 0,
            turns: 1,
            subagent_turns: 0,
            tool_calls: 0,
            web_searches: 0,
            spans: Vec::new(),
            age_secs: 0,
            idle_secs: None,
            cpu_percent: 0.0,
            rss_bytes: 0,
            process_count: 1,
            mcp_count: 0,
            mcp_servers: Vec::new(),
            tree: None,
            attribution: Attribution::HarnessRegistry,
            shares_process: false,
            parse_warning: None,
            rate_limit: None,
        };
        let mut s = Snapshot {
            schema_version: agent_top_core::SNAPSHOT_SCHEMA_VERSION,
            taken_at: SystemTime::UNIX_EPOCH,
            host: HostStats::default(),
            agents: vec![agent],
            orphans: Vec::new(),
            orphan_origins: Vec::new(),
            totals: Totals::default(),
        };
        s.compute_totals();
        s
    }

    #[test]
    fn output_rate_is_per_second_over_the_window_not_per_tick() {
        let t0 = Instant::now();
        let mut app = App::new(snapshot(0));
        // 1000 output tokens landing in one tick, at a one second interval.
        app.update_at(snapshot(0), t0);
        app.update_at(snapshot(1000), t0 + Duration::from_secs(1));
        assert_eq!(app.output_rate.last(), Some(&1000), "one second, one thousand tokens");
        // Nine quiet ticks: the burst is spread over the window, not forgotten.
        for i in 2..=10 {
            app.update_at(snapshot(1000), t0 + Duration::from_secs(i));
        }
        assert_eq!(app.output_rate.last(), Some(&100), "1000 tokens over the 10 s window");
        // Past the window the burst has aged out and the rate is zero again.
        for i in 11..=21 {
            app.update_at(snapshot(1000), t0 + Duration::from_secs(i));
        }
        assert_eq!(app.output_rate.last(), Some(&0));
        assert!(app.rate_samples.len() <= 12, "samples outside the window are dropped");
    }

    #[test]
    fn a_falling_total_reads_as_zero_and_a_half_second_tick_is_scaled() {
        let t0 = Instant::now();
        let mut app = App::new(snapshot(0));
        app.update_at(snapshot(500), t0);
        app.update_at(snapshot(0), t0 + Duration::from_secs(1));
        assert_eq!(app.output_rate.last(), Some(&0));
        let mut app = App::new(snapshot(0));
        app.update_at(snapshot(0), t0);
        app.update_at(snapshot(50), t0 + Duration::from_millis(500));
        assert_eq!(app.output_rate.last(), Some(&100), "50 tokens in half a second");
    }
}
