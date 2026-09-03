//! agent-top: htop for local coding agents.

mod app;
mod format;
mod ui;

use agent_top_core::{Collector, CollectorOptions};
use anyhow::Result;
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "agent-top", version, about = "htop for local coding agents", long_about = None)]
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let opts = CollectorOptions { stopped_window: Duration::from_secs(cli.stopped_window_min * 60), ..Default::default() };
    let mut collector = Collector::new(opts);

    if cli.json {
        // Two passes so per-process CPU has an interval to measure over.
        let _ = collector.collect();
        std::thread::sleep(Duration::from_millis(250));
        let snap = collector.collect();
        println!("{}", serde_json::to_string_pretty(&snap)?);
        return Ok(());
    }
    if cli.once {
        let _ = collector.collect();
        std::thread::sleep(Duration::from_millis(250));
        let snap = collector.collect();
        print!("{}", format::plain_table(&snap));
        return Ok(());
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut collector, Duration::from_millis(cli.interval_ms.max(100)));
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, collector: &mut Collector, interval: Duration) -> Result<()> {
    let mut app = app::App::new(collector.collect());
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
                KeyCode::Char('q') | KeyCode::Esc => {
                    if app.show_help {
                        app.show_help = false;
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
                app.update(collector.collect());
            }
            last_tick = Instant::now();
        }
    }
}
