//! agent-top: htop for local coding agents.

mod app;
mod format;
mod report;
mod trace;
mod ui;

use agent_top_core::{Collector, CollectorOptions, Snapshot};
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where to read the full, current changelog, printed by the upgrade nudge and
/// `--whats-new` so a stale binary can still point at what is new.
pub(crate) const CHANGELOG_URL: &str = "https://github.com/kannandreams/agent-top/blob/main/CHANGELOG.md";
/// This build's version.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The changelog embedded at build time (see build.rs).
const CHANGELOG: &str = include_str!(concat!(env!("OUT_DIR"), "/changelog.md"));

/// Print the most recent changelog sections baked into this build, then the
/// link for anything newer. Honest about the limit: a binary cannot know the
/// latest version without a network call it will not make, so it shows what it
/// shipped with and points at the online changelog for the rest.
fn print_whats_new() {
    println!("agent-top {VERSION}\n");
    // The changelog is newest-first; show the first few `## [` sections.
    let mut shown = 0;
    for line in CHANGELOG.lines() {
        if line.starts_with("## [") {
            shown += 1;
            if shown > 4 {
                break;
            }
        }
        if shown >= 1 {
            println!("{line}");
        }
    }
    println!("\nNewer versions may exist; this is only what this build shipped with.");
    println!("Full and current changelog: {CHANGELOG_URL}");
}

#[derive(Parser, Debug)]
#[command(name = "agent-top", version, about = "htop for local coding agents", long_about = None,
    after_help = "Upgrade: brew upgrade agent-top | cargo install agent-top\nWhat's new: agent-top --whats-new")]
struct Cli {
    /// Print one snapshot as JSON and exit.
    #[arg(long)]
    json: bool,
    /// Print one plain-text table and exit.
    #[arg(long)]
    once: bool,
    /// Refresh interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,
    /// Keep showing stopped sessions for this many minutes after their last write.
    #[arg(long, default_value_t = 30)]
    stopped_window_min: u64,
    /// Print the effective price table, showing which entries a user file
    /// overrode, and exit.
    #[arg(long)]
    prices: bool,
    /// Print a shell completion script and exit. Homebrew installs these for
    /// you; otherwise source the output from your shell's startup file.
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
    /// Render a snapshot saved by `--json` instead of scanning this machine.
    /// Every key still works, so a bug report can be inspected as the reporter
    /// saw it. Nothing on the local machine is read.
    #[arg(long, value_name = "FILE")]
    replay: Option<PathBuf>,
    /// Print what is new in this build's changelog and how to upgrade, then
    /// exit. Reads only the changelog compiled into the binary; makes no
    /// network call.
    #[arg(long)]
    whats_new: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Export one session's tool calls as a trace file, for Perfetto and the
    /// like. Reads the whole transcript, so it works on sessions that ended
    /// long ago and on harnesses with no telemetry of their own. Writes a
    /// file; never contacts a collector.
    Trace {
        /// A session id, a unique prefix of one, or a path to a transcript.
        #[arg(long, value_name = "ID|FILE")]
        session: String,
        /// Output format.
        #[arg(long, value_enum, default_value_t = trace::Format::Chrome)]
        format: trace::Format,
        /// Write here instead of standard output.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Also POST the document to this OTLP/HTTP traces URL, for example
        /// http://localhost:4318/v1/traces. The only network call agent-top
        /// ever makes, and only when you type the address. OTLP format only.
        #[arg(long, value_name = "URL")]
        endpoint: Option<String>,
    },
    /// What the agents have cost, across every harness, from the transcripts
    /// already on disk. Reads history, not the live snapshot; nothing is
    /// written and nothing leaves the machine.
    Report {
        /// How far back to look: `all`, a duration like `7d` / `12h` / `2w`,
        /// or a date `YYYY-MM-DD`.
        #[arg(long, default_value = "30d", value_name = "WHEN")]
        since: String,
        /// What to group the rows by.
        #[arg(long, value_enum, default_value_t = report::GroupBy::Harness)]
        by: report::GroupBy,
        /// Print the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

/// Where frames come from: this machine, or a snapshot someone saved earlier.
enum Source {
    Live(Box<Collector>),
    Replay(Box<Snapshot>),
}

impl Source {
    fn collect(&mut self) -> Snapshot {
        match self {
            Source::Live(c) => c.collect(),
            // A replayed snapshot is one moment in time: re-serving it keeps
            // ages, durations and the trace axis stable while keys are pressed.
            Source::Replay(s) => (**s).clone(),
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }
    if cli.prices {
        print!("{}", format::price_table(agent_top_core::pricing::table()));
        return Ok(());
    }
    if cli.whats_new {
        print_whats_new();
        return Ok(());
    }
    if let Some(Command::Trace { session, format, output, endpoint }) = &cli.command {
        return export_trace(session, *format, output.as_deref(), endpoint.as_deref());
    }
    if let Some(Command::Report { since, by, json }) = &cli.command {
        for w in &agent_top_core::pricing::table().warnings {
            eprintln!("agent-top: {w}");
        }
        let since = report::parse_since(since)?;
        let rep = report::build(since, *by);
        if *json {
            println!("{}", serde_json::to_string_pretty(&rep.to_json())?);
        } else {
            print!("{}", rep.to_plain());
        }
        return Ok(());
    }
    // A user's price file that could not be read is the difference between a
    // real cost and a wrong one, so say so rather than quietly using defaults.
    for w in &agent_top_core::pricing::table().warnings {
        eprintln!("agent-top: {w}");
    }
    let mut source = match &cli.replay {
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            let snap: Snapshot =
                serde_json::from_str(&text).with_context(|| format!("{} is not an agent-top --json snapshot", path.display()))?;
            Source::Replay(Box::new(snap))
        }
        None => {
            let opts = CollectorOptions { stopped_window: Duration::from_secs(cli.stopped_window_min * 60), ..Default::default() };
            Source::Live(Box::new(Collector::new(opts)))
        }
    };

    // A live first pass is thrown away so per-process CPU has an interval to
    // measure over; a replay has nothing to settle.
    let mut settled = || {
        if let Source::Live(_) = source {
            let _ = source.collect();
            std::thread::sleep(Duration::from_millis(250));
        }
        source.collect()
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&settled())?);
        return Ok(());
    }
    if cli.once {
        print!("{}", format::plain_table(&settled()));
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut source, Duration::from_millis(cli.interval_ms.max(100)));
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, source: &mut Source, interval: Duration) -> Result<()> {
    let mut app = app::App::new(source.collect());
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        let timeout = interval.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Char('q') => {
                    if app.overlay != app::Overlay::None {
                        app.overlay = app::Overlay::None;
                    } else {
                        return Ok(());
                    }
                }
                KeyCode::Esc => {
                    if app.overlay != app::Overlay::None {
                        app.overlay = app::Overlay::None;
                    } else {
                        return Ok(());
                    }
                }
                KeyCode::Char('c') if ctrl => return Ok(()),
                _ => app.on_key(key.code),
            }
        }
        if last_tick.elapsed() >= interval {
            if !app.paused {
                app.update(source.collect());
            }
            last_tick = Instant::now();
        }
    }
}

fn export_trace(session: &str, format: trace::Format, output: Option<&std::path::Path>, endpoint: Option<&str>) -> Result<()> {
    if endpoint.is_some() && format != trace::Format::Otlp {
        anyhow::bail!("--endpoint posts OTLP; add --format otlp");
    }
    let src = trace::resolve(session)?;
    let summary = trace::read(&src)?;
    let doc = serde_json::to_string(&trace::render(&src, &summary, format))?;
    if let Some(url) = endpoint {
        let status = trace::post(url, &doc)?;
        eprintln!("agent-top: {url} accepted the trace ({status})");
    }
    let count = |k: agent_top_core::SpanKind| summary.spans.iter().filter(|s| s.kind == k).count();
    let open = summary.spans.iter().filter(|s| s.kind == agent_top_core::SpanKind::Tool && s.is_open()).count();
    let still_open = if open > 0 { format!(" ({open} never returned)") } else { String::new() };
    match output {
        Some(path) if path != std::path::Path::new("-") => {
            std::fs::write(path, doc).with_context(|| format!("writing {}", path.display()))?;
            eprintln!(
                "agent-top: wrote {} tool calls{still_open}, {} inferences, {} turns from {} {} to {}",
                count(agent_top_core::SpanKind::Tool),
                count(agent_top_core::SpanKind::Inference),
                count(agent_top_core::SpanKind::Turn),
                src.harness.label(),
                summary.session_id.as_deref().unwrap_or("?"),
                path.display()
            );
        }
        // Posted somewhere and no file asked for: the terminal need not get
        // the document too.
        None if endpoint.is_some() => {}
        _ => println!("{doc}"),
    }
    Ok(())
}
