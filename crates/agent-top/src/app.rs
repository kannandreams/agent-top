//! UI state: selection, sort, toggles and short histories for sparklines.

use agent_top_core::{Agent, AgentState, Snapshot};
use ratatui::crossterm::event::KeyCode;
use std::collections::{HashMap, VecDeque};
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
    /// Explicit session lineage, the live process tree, and orphaned MCP servers.
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

/// A session forest built only from explicit transcript lineage. Input order
/// determines root/sibling order; PIDs and working directories are irrelevant.
pub(crate) struct SessionTree {
    /// Row index and depth, parents before their children.
    pub order: Vec<(usize, usize)>,
}

impl SessionTree {
    pub fn new(agents: &[Agent]) -> Self {
        let mut sessions = HashMap::new();
        for (i, agent) in agents.iter().enumerate() {
            if let Some(id) = agent.session_id.as_deref() {
                // An ambiguous session ID is not enough evidence to pick one
                // of several parents. Keep its children visible as roots.
                sessions.entry((agent.harness, id)).and_modify(|index| *index = None).or_insert(Some(i));
            }
        }
        let mut parents: Vec<Option<usize>> = agents
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let info = agent.subagent.as_ref()?;
                sessions.get(&(agent.harness, info.parent_session_id.as_str())).copied().flatten().filter(|&parent| parent != i)
            })
            .collect();

        // Break corrupt/replayed cycles before traversal. Every row remains
        // visible once, and iteration avoids overflowing on a deep hierarchy.
        let mut state = vec![0; agents.len()];
        for start in 0..agents.len() {
            let mut path = Vec::new();
            let mut current = Some(start);
            while let Some(i) = current {
                if state[i] == 2 {
                    break;
                }
                if state[i] == 1 {
                    parents[i] = None;
                    break;
                }
                state[i] = 1;
                path.push(i);
                current = parents[i];
            }
            for i in path {
                state[i] = 2;
            }
        }

        let mut children = vec![Vec::new(); agents.len()];
        let mut roots = Vec::new();
        for (i, parent) in parents.iter().enumerate() {
            if let Some(parent) = parent {
                children[*parent].push(i);
            } else {
                roots.push(i);
            }
        }
        let mut pending: Vec<_> = roots.into_iter().rev().map(|i| (i, 0)).collect();
        let mut order = Vec::with_capacity(agents.len());
        while let Some((i, depth)) = pending.pop() {
            order.push((i, depth));
            pending.extend(children[i].iter().rev().map(|&child| (child, depth + 1)));
        }
        Self { order }
    }
}

const HISTORY: usize = 120;

/// Output tokens per second are measured over this window rather than per
/// tick. A harness reports usage once per assistant message, so tick-to-tick
/// deltas are zeros with spikes between them; ten seconds smooths a turn into
/// a rate and still drops back to zero soon after the agents go quiet.
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Burn rate is smoothed over a longer window than the token rate, because
/// cost arrives in per-turn lumps that a short window would make jump around.
const BURN_WINDOW: Duration = Duration::from_secs(60);

/// A panel that can be peeked at as a popup over the table, pinned to the
/// whole terminal with Enter, started on its own with `agent-top <command>`,
/// or opened in a multiplexer pane with `o`. One key per panel, wherever it
/// is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// Tool calls ranked by how much time they took.
    SlowTools,
    /// Tool calls ranked by how often they failed.
    FailedTools,
    /// What looks like a bad deal right now, and what to do about it.
    Advice,
    /// Every MCP server under every agent, and the orphans.
    Mcp,
}

impl Panel {
    pub const ALL: [Panel; 4] = [Panel::SlowTools, Panel::FailedTools, Panel::Advice, Panel::Mcp];

    /// The key that shows the panel, in every mode.
    pub fn key(self) -> char {
        match self {
            Panel::SlowTools => 'l',
            Panel::FailedTools => 'f',
            Panel::Advice => 'a',
            Panel::Mcp => 'm',
        }
    }

    /// The subcommand that starts agent-top on this panel: `agent-top mcp`.
    pub fn command(self) -> &'static str {
        match self {
            Panel::SlowTools => "slow",
            Panel::FailedTools => "fails",
            Panel::Advice => "advice",
            Panel::Mcp => "mcp",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Panel::SlowTools => "slowest tools",
            Panel::FailedTools => "failed tool calls",
            Panel::Advice => "advice",
            Panel::Mcp => "mcp servers",
        }
    }

    fn from_key(c: char) -> Option<Panel> {
        Panel::ALL.into_iter().find(|p| p.key() == c)
    }
}

/// Which popup, if any, floats over the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    /// A panel peeked at over whatever is underneath: the table, or a
    /// pinned panel.
    Panel(Panel),
    /// A newer agent-top is available: upgrade now, or not now.
    Update,
}

/// How long a footer notice stays on screen.
const NOTICE_FOR: Duration = Duration::from_secs(5);

pub struct App {
    pub snapshot: Snapshot,
    pub rows: Vec<Agent>,
    /// Depth in the explicit session hierarchy, parallel to `rows`.
    pub row_depths: Vec<usize>,
    pub selected_id: Option<String>,
    pub selected: usize,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub show_detail: bool,
    pub detail: DetailView,
    pub overlay: Overlay,
    /// A panel filling the terminal instead of the table and detail pane.
    pub pinned: Option<Panel>,
    /// Started with `agent-top <panel>`: there is no table to go back to, so
    /// Esc with nothing open does nothing, and the update question is left to
    /// the main view.
    pub standalone: bool,
    /// Lines scrolled off the top of the pinned panel. The renderer clamps it
    /// to the panel's height each frame.
    pub scroll: u16,
    /// The multiplexer this run is inside, if any; decides whether `o` is offered.
    pub multiplexer: Option<crate::pane::Multiplexer>,
    /// Set when the user pressed `o`: the main loop opens that panel in a pane.
    pub open_requested: Option<Panel>,
    /// A one-line message for the footer, with when it was set.
    pub notice: Option<(String, Instant)>,
    pub show_stopped: bool,
    pub paused: bool,
    pub cpu_history: Vec<u64>,
    /// Output tokens per second across every agent, one entry per tick.
    pub output_rate: Vec<u64>,
    pub cost_history: Vec<u64>,
    /// (when, output tokens across every agent) for the last `RATE_WINDOW`.
    rate_samples: VecDeque<(Instant, u64)>,
    /// (when, cumulative cost in micro-dollars) for the last `BURN_WINDOW`.
    cost_samples: VecDeque<(Instant, u64)>,
    /// Current spend velocity across every agent, US dollars per hour.
    pub burn_per_hour: f64,
    /// The latest published version when it is newer than this build, filled by
    /// the update check; `None` otherwise. The footer reads it each frame.
    pub update: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// The version the user has already said "not now" to, from the cache.
    pub update_dismissed: Option<String>,
    /// The upgrade question is asked at most once per run.
    update_prompted: bool,
    /// Which installer would run an upgrade; the popup shows its command.
    pub installer: crate::update::Installer,
    /// Set when the user pressed `u` in the update popup: the main loop
    /// leaves the TUI and runs the upgrade.
    pub upgrade_requested: Option<String>,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        let mut app = App {
            snapshot,
            rows: Vec::new(),
            row_depths: Vec::new(),
            selected_id: None,
            selected: 0,
            sort: SortKey::State,
            sort_desc: false,
            show_detail: true,
            detail: DetailView::Tree,
            overlay: Overlay::None,
            pinned: None,
            standalone: false,
            scroll: 0,
            multiplexer: None,
            open_requested: None,
            notice: None,
            show_stopped: true,
            paused: false,
            cpu_history: Vec::new(),
            output_rate: Vec::new(),
            cost_history: Vec::new(),
            rate_samples: VecDeque::new(),
            cost_samples: VecDeque::new(),
            burn_per_hour: 0.0,
            update: std::sync::Arc::new(std::sync::Mutex::new(None)),
            update_dismissed: None,
            update_prompted: false,
            installer: crate::update::Installer::Unknown,
            upgrade_requested: None,
        };
        app.rebuild_rows();
        app
    }

    /// The newer version the update check knows of, if any.
    pub fn latest(&self) -> Option<String> {
        self.update.lock().ok().and_then(|g| g.clone())
    }

    /// Open the update question once a newer version is known, unless the
    /// user already declined that version or another popup is open. Called
    /// after the check starts and on every tick, since the answer can arrive
    /// from the network a moment after start.
    pub fn maybe_prompt_update(&mut self) {
        // A dedicated pane leaves the question to the main view, so two
        // agent-top windows do not ask it twice.
        if self.update_prompted || self.overlay != Overlay::None || self.standalone {
            return;
        }
        let Some(latest) = self.latest() else { return };
        if self.update_dismissed.as_deref() == Some(latest.as_str()) {
            return;
        }
        self.update_prompted = true;
        self.overlay = Overlay::Update;
    }

    /// Close whatever popup is open. Closing the update question counts as
    /// "not now": that version is not asked about again.
    pub fn close_overlay(&mut self) {
        if self.overlay == Overlay::Update
            && let Some(latest) = self.latest()
        {
            self.update_dismissed = Some(latest.clone());
            crate::update::dismiss(&latest);
        }
        self.overlay = Overlay::None;
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
        // Spend velocity: the cost added over the burn window, projected to an
        // hour. Cost only climbs, so an idle machine reads zero.
        let cost_micros = (snapshot.totals.cost_usd * 1_000_000.0) as u64;
        self.cost_samples.push_back((now, cost_micros));
        while self.cost_samples.len() > 2 && self.cost_samples[1].0 + BURN_WINDOW <= now {
            self.cost_samples.pop_front();
        }
        self.burn_per_hour = burn_per_hour(&self.cost_samples);
        self.snapshot = snapshot;
        self.rebuild_rows();
        self.maybe_prompt_update();
    }

    pub fn rebuild_rows(&mut self) {
        let mut rows: Vec<Agent> =
            self.snapshot.agents.iter().filter(|a| self.show_stopped || a.state != AgentState::Stopped).cloned().collect();
        let key = self.sort;
        rows.sort_by(|a, b| {
            let ord = match key {
                SortKey::State => a.state.cmp(&b.state).then_with(|| b.usage.total().cmp(&a.usage.total())),
                SortKey::Name => crate::format::session_name(a).to_lowercase().cmp(&crate::format::session_name(b).to_lowercase()),
                SortKey::Tokens => b.usage.total().cmp(&a.usage.total()),
                SortKey::Cost => b.cost_usd.partial_cmp(&a.cost_usd).unwrap_or(std::cmp::Ordering::Equal),
                SortKey::Cpu => b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap_or(std::cmp::Ordering::Equal),
                SortKey::Mem => b.rss_bytes.cmp(&a.rss_bytes),
                SortKey::Age => b.age_secs.cmp(&a.age_secs),
            };
            if self.sort_desc { ord.reverse() } else { ord }
        });
        let hierarchy = SessionTree::new(&rows);
        self.row_depths = hierarchy.order.iter().map(|&(_, depth)| depth).collect();
        self.rows = hierarchy.order.into_iter().map(|(i, _)| rows[i].clone()).collect();
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

    /// Fill the terminal with a panel. The peek that led here closes: it is
    /// the same panel at a bigger size, not a second copy.
    pub fn pin(&mut self, p: Panel) {
        self.pinned = Some(p);
        self.overlay = Overlay::None;
        self.scroll = 0;
    }

    /// Back to the table. A standalone run has no table, so it stays put.
    fn unpin(&mut self) {
        if !self.standalone {
            self.pinned = None;
            self.scroll = 0;
        }
    }

    /// Show a line in the footer for a few seconds.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    /// The footer notice, if it is still fresh.
    pub fn current_notice(&self) -> Option<&str> {
        self.notice.as_ref().filter(|(_, at)| at.elapsed() < NOTICE_FOR).map(|(t, _)| t.as_str())
    }

    /// A panel's key: closes its peek, switches a peek to it, returns from it
    /// when it is pinned, or opens it as a peek.
    fn panel_key(&mut self, p: Panel) {
        match self.overlay {
            Overlay::Panel(open) if open == p => self.overlay = Overlay::None,
            Overlay::Panel(_) => self.overlay = Overlay::Panel(p),
            _ if self.pinned == Some(p) => self.unpin(),
            _ => self.overlay = Overlay::Panel(p),
        }
    }

    /// The panel `o` would open in a pane: the peek if one is open, else the
    /// pinned panel.
    fn panel_in_front(&self) -> Option<Panel> {
        match self.overlay {
            Overlay::Panel(p) => Some(p),
            Overlay::None => self.pinned,
            _ => None,
        }
    }

    /// Movement keys scroll the pinned panel when one fills the screen, and
    /// move the table's selection otherwise.
    fn scroll_or_select(&mut self, code: KeyCode) {
        let page: u16 = 10;
        if self.pinned.is_some() && self.overlay == Overlay::None {
            self.scroll = match code {
                KeyCode::Char('j') | KeyCode::Down => self.scroll.saturating_add(1),
                KeyCode::Char('k') | KeyCode::Up => self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll.saturating_add(page),
                KeyCode::PageUp => self.scroll.saturating_sub(page),
                KeyCode::Char('g') | KeyCode::Home => 0,
                KeyCode::Char('G') | KeyCode::End => u16::MAX,
                _ => self.scroll,
            };
            return;
        }
        match code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::PageDown => self.select(self.selected + page as usize),
            KeyCode::PageUp => self.select(self.selected.saturating_sub(page as usize)),
            _ => {}
        }
    }

    /// Handle one keypress. Returns `true` when the app should exit: `q` or
    /// `Esc` with nothing open, or `q` on the update popup specifically. That
    /// popup asks a real question, so the generic "close whatever's open"
    /// key must not read as a quiet decline the way it does for every other
    /// overlay; quitting leaves the version un-declined for next launch,
    /// and only `n`/`Esc`, below, actually says no.
    pub fn on_key(&mut self, code: KeyCode) -> bool {
        // The update question takes `u`, `n` and `q` for itself while it is open.
        if self.overlay == Overlay::Update {
            match code {
                KeyCode::Char('u') | KeyCode::Char('y') | KeyCode::Enter => {
                    if self.installer.steps().is_some() {
                        self.upgrade_requested = self.latest();
                    }
                    return false;
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.close_overlay();
                    return false;
                }
                KeyCode::Char('q') => return true,
                _ => {}
            }
        }
        match code {
            KeyCode::Char('j' | 'k' | 'g' | 'G')
            | KeyCode::Down
            | KeyCode::Up
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageDown
            | KeyCode::PageUp => self.scroll_or_select(code),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.rebuild_rows();
            }
            KeyCode::Char('r') => {
                self.sort_desc = !self.sort_desc;
                self.rebuild_rows();
            }
            // Enter on a peek pins it: the same panel, the whole terminal.
            KeyCode::Enter if matches!(self.overlay, Overlay::Panel(_)) => {
                if let Overlay::Panel(p) = self.overlay {
                    self.pin(p);
                }
            }
            KeyCode::Char('t') | KeyCode::Enter if self.pinned.is_none() => self.show_detail = !self.show_detail,
            // `o` asks the main loop to open the panel in front in a pane of
            // the multiplexer; without one there is nothing to ask. The peek
            // closes, since the pane now shows it; a pinned panel goes back
            // to the table for the same reason.
            KeyCode::Char('o') => {
                if let (Some(_), Some(p)) = (self.multiplexer, self.panel_in_front()) {
                    self.open_requested = Some(p);
                    if self.overlay == Overlay::Panel(p) {
                        self.overlay = Overlay::None;
                    } else {
                        self.unpin();
                    }
                }
            }
            // Cycling the view opens the pane rather than switching a panel
            // nobody can see.
            KeyCode::Tab | KeyCode::Char('v') if self.pinned.is_none() => {
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
            KeyCode::Char(c) if Panel::from_key(c).is_some() => {
                if let Some(p) = Panel::from_key(c) {
                    self.panel_key(p);
                }
            }
            // Esc walks back one step: close the popup, then leave the pinned
            // panel, then quit. `q` closes a popup and otherwise quits from
            // wherever it is, so a dedicated pane closes on one key.
            KeyCode::Esc => {
                if self.overlay != Overlay::None {
                    self.close_overlay();
                } else if self.pinned.is_some() && !self.standalone {
                    self.unpin();
                } else if self.pinned.is_none() {
                    return true;
                }
            }
            KeyCode::Char('q') => {
                if self.overlay != Overlay::None {
                    self.close_overlay();
                } else {
                    return true;
                }
            }
            _ => {}
        }
        false
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

/// US dollars per hour from cumulative-cost samples: the cost added across the
/// window, scaled to an hour. Needs a few seconds of history so a cold start
/// does not read a wild rate off a one-second window.
fn burn_per_hour(samples: &VecDeque<(Instant, u64)>) -> f64 {
    let (Some((t0, c0)), Some((t1, c1))) = (samples.front(), samples.back()) else { return 0.0 };
    let secs = t1.duration_since(*t0).as_secs_f64();
    if secs < 3.0 {
        return 0.0;
    }
    let micros = c1.saturating_sub(*c0) as f64;
    micros / 1_000_000.0 / secs * 3600.0
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
            subagent: None,
            session_path: None,
            cwd: None,
            model: None,
            harness_version: None,
            usage: TokenUsage { input: 10, cache_read: 500_000, output, ..Default::default() },
            cost_usd: 0.0,
            cost_breakdown: Default::default(),
            price_source: None,
            unpriced_tokens: 0,
            turns: 1,
            subagent_turns: 0,
            tool_calls: 0,
            tool_calls_lower_bound: false,
            web_searches: 0,
            spans: Vec::new(),
            age_secs: 0,
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
        };
        let mut s = Snapshot {
            schema_version: agent_top_core::SNAPSHOT_SCHEMA_VERSION,
            taken_at: SystemTime::UNIX_EPOCH,
            host: HostStats::default(),
            agents: vec![agent],
            orphans: Vec::new(),
            orphan_origins: Vec::new(),
            advice: Vec::new(),
            totals: Totals::default(),
        };
        s.compute_totals();
        s
    }

    fn session(id: &str, parent: Option<&str>, output: u64) -> Agent {
        let mut a = snapshot(output).agents.remove(0);
        a.id = id.into();
        a.name = id.into();
        a.session_id = Some(id.into());
        a.harness = Harness::Codex;
        a.subagent = parent.map(|id| agent_top_core::SubagentInfo { parent_session_id: id.into(), nickname: None, role: None });
        a
    }

    fn session_app(agents: Vec<Agent>) -> App {
        let mut s = snapshot(0);
        s.agents = agents;
        s.compute_totals();
        App::new(s)
    }

    #[test]
    fn session_hierarchy_preserves_sibling_sort_and_selection() {
        let mut app = session_app(vec![
            session("child-b", Some("root-a"), 30),
            session("root-z", None, 80),
            session("grandchild", Some("child-a"), 100),
            session("root-a", None, 10),
            session("child-a", Some("root-a"), 20),
        ]);
        app.sort = SortKey::Name;
        app.rebuild_rows();
        let ids = |app: &App| app.rows.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(",");
        assert_eq!(ids(&app), "root-a,child-a,grandchild,child-b,root-z");
        assert_eq!(app.row_depths, vec![0, 1, 2, 1, 0]);

        app.select(2);
        app.sort_desc = true;
        app.rebuild_rows();
        assert_eq!(ids(&app), "root-z,root-a,child-b,child-a,grandchild");
        assert_eq!(app.selected_agent().unwrap().id, "grandchild");

        app.sort = SortKey::Tokens;
        app.sort_desc = false;
        app.rebuild_rows();
        assert_eq!(ids(&app), "root-z,root-a,child-b,child-a,grandchild");
        let tokens_before = app.snapshot.totals.tokens;
        app.snapshot.agents.reverse();
        app.rebuild_rows();
        assert_eq!(app.selected_agent().unwrap().id, "grandchild", "a refresh must keep the selected session");
        assert_eq!(app.rows.iter().map(|a| a.usage.total()).sum::<u64>(), tokens_before, "grouping does not fold or duplicate usage");
    }

    #[test]
    fn down_keeps_the_selected_subagent_across_refreshes_with_shared_pid_and_name() {
        let parent = session("parent", None, 100);
        let mut child = session("child", Some("parent"), 20);
        child.name = parent.name.clone();
        child.shares_process = true;
        assert_eq!(child.pid, parent.pid);
        let mut app = session_app(vec![child, parent]);
        assert_eq!(app.row_depths, vec![0, 1]);
        assert_eq!(app.selected_agent().unwrap().id, "parent");

        app.on_key(KeyCode::Down);
        for tick in 1..=3 {
            let mut next = app.snapshot.clone();
            next.agents.reverse();
            app.update_at(next, Instant::now() + Duration::from_secs(tick));
            assert_eq!(app.selected_agent().unwrap().id, "child", "refresh must not jump to the shared process owner");
            assert_eq!(app.row_depths, vec![0, 1]);
        }
        app.on_key(KeyCode::Up);
        assert_eq!(app.selected_agent().unwrap().id, "parent");
    }

    #[test]
    fn session_name_sort_uses_displayed_nicknames() {
        let mut scout = session("child-a", Some("parent"), 10);
        let mut review = session("child-b", Some("parent"), 10);
        scout.name = "codex:project".into();
        review.name = scout.name.clone();
        scout.subagent.as_mut().unwrap().nickname = Some("Scout".into());
        review.subagent.as_mut().unwrap().nickname = Some("Review".into());
        let mut app = session_app(vec![scout, review, session("parent", None, 0)]);
        app.sort = SortKey::Name;
        app.rebuild_rows();
        assert_eq!(app.rows.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["parent", "child-b", "child-a"]);
        app.sort_desc = true;
        app.rebuild_rows();
        assert_eq!(app.rows.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["parent", "child-a", "child-b"]);
    }

    #[test]
    fn sessions_with_missing_or_hidden_parents_stay_visible() {
        let mut parent = session("parent", None, 10);
        parent.state = AgentState::Stopped;
        let mut app = session_app(vec![parent, session("child", Some("parent"), 20), session("missing", Some("unknown"), 30)]);
        app.sort = SortKey::Name;
        app.rebuild_rows();
        assert_eq!(app.rows.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["missing", "parent", "child"]);
        assert_eq!(app.row_depths, vec![0, 0, 1]);
        app.select(2);
        app.show_stopped = false;
        app.rebuild_rows();
        assert_eq!(app.rows.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["child", "missing"]);
        assert_eq!(app.row_depths, vec![0, 0]);
        assert_eq!(app.selected_agent().unwrap().id, "child");
        assert_eq!(app.rows[0].subagent.as_ref().unwrap().parent_session_id, "parent");
    }

    #[test]
    fn session_hierarchy_uses_harness_and_identity_not_pid() {
        let mut other_harness = session("parent", None, 0);
        other_harness.id = "claude-parent".into();
        other_harness.harness = Harness::Claude;
        let agents = vec![other_harness, session("child", Some("parent"), 0), session("parent", None, 0), session("unrelated", None, 0)];
        assert!(agents.iter().all(|a| a.pid == Some(1)), "the fixture deliberately shares a PID");
        let hierarchy = SessionTree::new(&agents);
        assert_eq!(hierarchy.order, vec![(0, 0), (2, 0), (1, 1), (3, 0)]);
    }

    #[test]
    fn malformed_session_lineage_never_drops_or_repeats_rows() {
        let mut duplicate = session("duplicate", None, 0);
        duplicate.id = "second-copy".into();
        let agents = vec![
            session("a", Some("b"), 0),
            session("b", Some("a"), 0),
            session("self", Some("self"), 0),
            session("duplicate", None, 0),
            duplicate,
            session("ambiguous", Some("duplicate"), 0),
        ];
        let hierarchy = SessionTree::new(&agents);
        let mut indexes: Vec<_> = hierarchy.order.iter().map(|&(i, _)| i).collect();
        indexes.sort_unstable();
        assert_eq!(indexes, (0..agents.len()).collect::<Vec<_>>());
        let depth = |i| hierarchy.order.iter().find(|&&(row, _)| row == i).unwrap().1;
        assert_eq!(depth(2), 0, "a session is not its own parent");
        assert_eq!(depth(5), 0, "never choose an arbitrary duplicate parent");
        assert!(depth(0) == 0 || depth(1) == 0, "a cycle is broken, not followed");
    }

    /// The question opens once when a newer version is known, not when that
    /// version was already declined, and not over another popup; `n` declines
    /// and `u` asks the main loop to upgrade.
    #[test]
    fn the_upgrade_question_is_asked_once_and_remembers_no() {
        let mut app = App::new(snapshot(0));
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::None, "nothing known yet");
        *app.update.lock().unwrap() = Some("9.9.9".into());
        app.overlay = Overlay::Help;
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::Help, "never over another popup");
        app.overlay = Overlay::None;
        app.update_at(snapshot(0), Instant::now());
        assert_eq!(app.overlay, Overlay::Update, "a tick asks");
        // `u` with an unknown installer does nothing: there is no command to run.
        app.on_key(KeyCode::Char('u'));
        assert_eq!(app.upgrade_requested, None);
        app.installer = crate::update::Installer::CargoInstall;
        app.on_key(KeyCode::Char('u'));
        assert_eq!(app.upgrade_requested.as_deref(), Some("9.9.9"));

        // Declining closes it and is remembered in memory for this version.
        let mut app = App::new(snapshot(0));
        *app.update.lock().unwrap() = Some("9.9.9".into());
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::Update);
        app.update_dismissed = None;
        app.overlay = Overlay::Update;
        // Simulate the decline without touching the real cache file.
        app.update_dismissed = Some("9.9.9".into());
        app.overlay = Overlay::None;
        app.update_prompted = false;
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::None, "a declined version is not asked again");
        // A newer release than the declined one is asked about.
        *app.update.lock().unwrap() = Some("10.0.0".into());
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::Update);
    }

    /// `q` on the update popup quits instead of quietly declining the
    /// version: the popup is a real question, and `q` closing it the way it
    /// closes every other overlay would record a "not now" nobody chose.
    /// Only `n`/`Esc` records that. Regression for the v0.15.1 live test,
    /// where a `q` press was read back as a decline.
    #[test]
    fn q_on_the_update_popup_quits_without_declining_the_version() {
        let mut app = App::new(snapshot(0));
        *app.update.lock().unwrap() = Some("9.9.9".into());
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::Update);

        assert!(app.on_key(KeyCode::Char('q')), "q quits");
        assert_eq!(app.update_dismissed, None, "quitting is not a decline");

        // Esc, unlike q, is a deliberate decline and is remembered.
        app.overlay = Overlay::Update;
        assert!(!app.on_key(KeyCode::Esc));
        assert_eq!(app.update_dismissed.as_deref(), Some("9.9.9"));

        // q still closes every other overlay without quitting.
        let mut app = App::new(snapshot(0));
        app.overlay = Overlay::Help;
        assert!(!app.on_key(KeyCode::Char('q')));
        assert_eq!(app.overlay, Overlay::None);

        // q with nothing open quits, same as before.
        assert!(app.on_key(KeyCode::Char('q')));
    }

    /// The four ways to a panel share one key. A peek opens and closes on
    /// it; Enter pins the peek; the same key, or Esc, returns to the table;
    /// another panel's key over a pinned one is a peek, not a switch.
    #[test]
    fn a_panel_is_peeked_pinned_and_left_with_the_same_key() {
        let mut app = App::new(snapshot(0));
        app.on_key(KeyCode::Char('m'));
        assert_eq!(app.overlay, Overlay::Panel(Panel::Mcp), "m peeks");
        app.on_key(KeyCode::Char('m'));
        assert_eq!(app.overlay, Overlay::None, "m again closes the peek");

        app.on_key(KeyCode::Char('m'));
        assert!(!app.on_key(KeyCode::Enter));
        assert_eq!(app.pinned, Some(Panel::Mcp), "Enter pins the peek");
        assert_eq!(app.overlay, Overlay::None, "the peek closed: it is the same panel, bigger");

        app.on_key(KeyCode::Char('l'));
        assert_eq!(app.overlay, Overlay::Panel(Panel::SlowTools), "another key peeks over the pinned panel");
        assert_eq!(app.pinned, Some(Panel::Mcp));
        assert!(!app.on_key(KeyCode::Esc), "Esc closes the peek first");
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.pinned, Some(Panel::Mcp));

        assert!(!app.on_key(KeyCode::Esc), "Esc then leaves the pinned panel");
        assert_eq!(app.pinned, None);
        assert!(app.on_key(KeyCode::Esc), "Esc with nothing open quits");

        // The pinned panel's own key goes back to the table too.
        app.pin(Panel::Advice);
        app.on_key(KeyCode::Char('a'));
        assert_eq!(app.pinned, None);

        // Movement keys scroll a pinned panel and move the selection otherwise.
        app.pin(Panel::Mcp);
        app.on_key(KeyCode::Char('j'));
        app.on_key(KeyCode::PageDown);
        assert_eq!(app.scroll, 11);
        app.on_key(KeyCode::Char('g'));
        assert_eq!(app.scroll, 0);
        assert_eq!(app.selected, 0, "the table did not move");
        // Enter and Tab are table keys; pinned, they do nothing.
        let detail = app.show_detail;
        app.on_key(KeyCode::Enter);
        app.on_key(KeyCode::Tab);
        assert_eq!(app.show_detail, detail);
        assert!(app.on_key(KeyCode::Char('q')), "q quits from a pinned panel");
    }

    /// `agent-top mcp` has no table behind it: Esc stays, q quits, the
    /// panel's key stays, and the upgrade question is left to the main view.
    #[test]
    fn a_standalone_panel_has_nowhere_to_go_back_to() {
        let mut app = App::new(snapshot(0));
        app.standalone = true;
        app.pin(Panel::Mcp);
        assert!(!app.on_key(KeyCode::Esc));
        assert_eq!(app.pinned, Some(Panel::Mcp));
        app.on_key(KeyCode::Char('m'));
        assert_eq!(app.pinned, Some(Panel::Mcp));
        app.on_key(KeyCode::Char('a'));
        assert_eq!(app.overlay, Overlay::Panel(Panel::Advice), "other panels still peek");
        app.on_key(KeyCode::Esc);
        *app.update.lock().unwrap() = Some("9.9.9".into());
        app.maybe_prompt_update();
        assert_eq!(app.overlay, Overlay::None, "no upgrade question in a pane");
        assert!(app.on_key(KeyCode::Char('q')));
    }

    /// `o` is an ask to the main loop, made only inside a multiplexer, for
    /// the panel in front; the peek closes because the pane now shows it.
    #[test]
    fn o_asks_for_a_pane_only_inside_a_multiplexer() {
        let mut app = App::new(snapshot(0));
        app.on_key(KeyCode::Char('l'));
        app.on_key(KeyCode::Char('o'));
        assert_eq!(app.open_requested, None, "a plain terminal has no panes");
        assert_eq!(app.overlay, Overlay::Panel(Panel::SlowTools), "and the peek stays");

        app.multiplexer = Some(crate::pane::Multiplexer::Tmux);
        app.on_key(KeyCode::Char('o'));
        assert_eq!(app.open_requested.take(), Some(Panel::SlowTools));
        assert_eq!(app.overlay, Overlay::None, "the peek moved into the pane");

        app.on_key(KeyCode::Char('o'));
        assert_eq!(app.open_requested, None, "nothing in front, nothing to open");

        app.pin(Panel::Mcp);
        app.on_key(KeyCode::Char('o'));
        assert_eq!(app.open_requested.take(), Some(Panel::Mcp));
        assert_eq!(app.pinned, None, "a pinned panel moves into the pane and the table returns");

        app.notify("opened");
        assert_eq!(app.current_notice(), Some("opened"));
    }

    #[test]
    fn burn_rate_is_dollars_per_hour_over_the_window() {
        use std::collections::VecDeque;
        let t0 = Instant::now();
        // $0.60 spent over 60 seconds → $36/hour.
        let mut s: VecDeque<(Instant, u64)> = VecDeque::new();
        s.push_back((t0, 1_000_000)); // $1.00
        s.push_back((t0 + Duration::from_secs(60), 1_600_000)); // $1.60
        assert!((burn_per_hour(&s) - 36.0).abs() < 1e-6, "{}", burn_per_hour(&s));
        // Too little history reads zero rather than a wild number.
        let mut s: VecDeque<(Instant, u64)> = VecDeque::new();
        s.push_back((t0, 1_000_000));
        s.push_back((t0 + Duration::from_secs(1), 2_000_000));
        assert_eq!(burn_per_hour(&s), 0.0);
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
