//! UI state: selection, sort, toggles and short histories for sparklines.

use agent_top_core::{Agent, AgentState, Snapshot};
use ratatui::crossterm::event::KeyCode;

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

pub struct App {
    pub snapshot: Snapshot,
    pub rows: Vec<Agent>,
    pub selected_id: Option<String>,
    pub selected: usize,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub show_detail: bool,
    pub detail: DetailView,
    pub show_help: bool,
    pub show_stopped: bool,
    pub paused: bool,
    pub cpu_history: Vec<u64>,
    pub tokens_per_tick: Vec<u64>,
    pub cost_history: Vec<u64>,
    last_total_tokens: Option<u64>,
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
            show_help: false,
            show_stopped: true,
            paused: false,
            cpu_history: Vec::new(),
            tokens_per_tick: Vec::new(),
            cost_history: Vec::new(),
            last_total_tokens: None,
        };
        app.rebuild_rows();
        app
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        push(&mut self.cpu_history, snapshot.host.cpu_percent.round() as u64);
        let total = snapshot.totals.tokens;
        let delta = self.last_total_tokens.map(|t| total.saturating_sub(t)).unwrap_or(0);
        push(&mut self.tokens_per_tick, delta);
        push(&mut self.cost_history, (snapshot.totals.cost_usd * 100.0) as u64);
        self.last_total_tokens = Some(total);
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
            KeyCode::Char('h') | KeyCode::Char('?') | KeyCode::F(1) => self.show_help = !self.show_help,
            _ => {}
        }
    }
}

fn push(v: &mut Vec<u64>, x: u64) {
    v.push(x);
    if v.len() > HISTORY {
        let drop = v.len() - HISTORY;
        v.drain(..drop);
    }
}
