# agent-top-core

The engine behind [`agent-top`](https://crates.io/crates/agent-top), *htop for
local coding agents*. **If you want the tool, install `agent-top` instead** —
this crate is its library half, published because a crate on crates.io cannot
depend on an unpublished one.

It answers one question, with no terminal dependency at all: *which coding
agents are on this machine right now, what are they doing, and what have they
spent?*

```rust
use agent_top_core::{Collector, CollectorOptions};

let mut collector = Collector::new(CollectorOptions::default());
let snapshot = collector.collect();

for agent in &snapshot.agents {
    println!("{} {} {} tokens ${:.2}", agent.name, agent.state.label(), agent.usage.total(), agent.cost_usd);
}
```

A `Snapshot` is exactly what `agent-top --json` prints, and it serialises and
deserialises, so it can be stored, shipped in a bug report and replayed.

## What it does

- **Discovery.** Walks the process table with `sysinfo`, marks Claude Code,
  Codex, Gemini CLI, OpenCode, Aider, Copilot CLI and cursor-agent roots, folds
  their children into a tree labelled `subagent` / `mcp` / `shell` / `tool`, and
  flags MCP servers whose agent has died as orphans.
- **Attribution.** Joins each process to its transcript — exactly where the
  harness publishes a registry, by heuristic otherwise, and the result says
  which it was so a caller never has to guess how much to trust a row.
- **Accounting.** Tails transcripts incrementally, counting tokens, cost, turns
  and tool calls from the harness's own usage records. Tokens are counted, never
  estimated; a model with no known price is reported as unpriced rather than
  guessed at.
- **Tracing.** Pairs each tool call with its result to produce `ToolSpan`s with
  real durations, from harnesses that log no telemetry of their own.

Everything is read-only and local: no network calls, no signalling of agents,
no reading of prompt or tool content — metadata fields only.

## Stability

Pre-1.0, and the version moves in lockstep with `agent-top`. The `harness`
module in particular will change as the adapter trait lands. Pin an exact
version if you build on it.

MIT. Source, roadmap and the tool itself:
<https://github.com/kannandreams/agent-top>
