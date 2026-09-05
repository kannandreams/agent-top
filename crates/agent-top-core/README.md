# agent-top-core

This is the library behind [`agent-top`](https://crates.io/crates/agent-top), a
terminal tool that shows which coding agents are running on your machine, what
they are doing, and what they have spent.

**If you want the tool, install `agent-top` instead.** This crate is on
crates.io because Cargo requires it: `agent-top` depends on it, and a published
crate cannot depend on an unpublished one.

Use this crate directly only if you want the discovery and accounting logic
without a terminal interface, for example in a script, a status bar, or a UI of
your own.

## Usage

```rust
use agent_top_core::{Collector, CollectorOptions};

let mut collector = Collector::new(CollectorOptions::default());
let snapshot = collector.collect();

for agent in &snapshot.agents {
    println!("{} {} {} tokens ${:.2}", agent.name, agent.state.label(), agent.usage.total(), agent.cost_usd);
}
```

Call `collect()` on a timer to refresh. It reads only the bytes appended to each
transcript since the previous call, so polling once a second is cheap even when
a session has grown to tens of megabytes.

A `Snapshot` is exactly what `agent-top --json` prints. It both serialises and
deserialises, so you can store one, attach it to a bug report, and replay it
later.

## What it does

**Discovery.** Walks the process table with `sysinfo` and identifies Claude
Code, Codex, Gemini CLI, OpenCode, Aider, Copilot CLI and cursor-agent
processes. Child processes are folded into a tree and labelled as subagents, MCP
servers, shells or tools. An MCP server whose agent has exited is reported as an
orphan, which is a common way for these tools to leak memory.

**Attribution.** Matches each process to its transcript file. Where a harness
publishes a registry of its own sessions or holds its transcript open, that is
used and the result is exact. Otherwise the match is made on working directory
and start time. Each harness is a `HarnessAdapter` in `harness::adapters()`;
Claude Code, Codex, Gemini CLI and OpenCode have one. OpenCode keeps its
history in a SQLite database rather than a JSONL log, which the adapter reads
read-only. Every agent
records which method was used, so a caller never has to guess how much to trust
a row.

**Accounting.** Tails transcripts incrementally and counts tokens, cost, turns
and tool calls from the usage records the harness writes. Tokens are counted
rather than estimated. A model with no known price is reported as unpriced
instead of being guessed at, so a cost is never quietly invented.

**Tracing.** Pairs each tool call with its result to produce spans with real
durations. This works for harnesses that emit no telemetry of their own, and it
works retroactively on sessions that have already finished. The live tracker
keeps the newest 128 spans; to read a whole transcript, open it with
`harness::open_transcript(path, harness, SpanRetention::All)` and call
`refresh_all`, which is what `agent-top trace` does.

Everything is read only and stays on the machine. The library makes no network
calls, never signals or writes to an agent, and reads metadata fields only,
never the content of prompts or tool output.

## Stability

This crate is pre-1.0 and its version tracks `agent-top` exactly. The `harness`
module will change when the adapter trait lands. Pin an exact version if you
build on it.

Licensed under MIT. Source, roadmap and the tool itself are at
<https://github.com/kannandreams/agent-top>.
