# Vision

**agent-top is htop for local coding agents.**

## The problem

Coding agents are now long-running processes on a developer's machine, several at a time: a couple of Claude Code sessions in different repos, a Codex thread inside VS Code, a Gemini CLI, subagents those agents spawn, and the Model Context Protocol (MCP) servers each of them starts. They consume three scarce things at once: CPU and memory like any process, tokens that cost real money, and the developer's attention (an idle agent is waiting for a human).

Nothing on the machine shows those three together. `htop` shows a `node` process and no idea whose it is or what it is spending. The harnesses each show their own session and nothing about the others. When an agent dies and leaves its MCP servers behind, nobody notices until the machine swaps. That failure is not hypothetical: Codex has a series of bug reports where completed subagents kept whole stdio MCP process trees alive (openai/codex #17574, #25015, #12491, #16256), fixed only in April 2026 for one path (openai/codex #19753). Every harness that spawns helper processes has the same shape of bug waiting.

## What agent-top is

A single terminal view, in the spirit of `btop`, that lists every coding agent on the machine with its state, tokens, cost, resource use, process tree and age, refreshed every second, and that points at leaked helper processes. Read-only, dependency-light, one static binary, no daemon.

## What agent-top is not

- **Not a harness.** It does not run models, hold API keys, or talk to any provider.
- **Not a controller.** It never signals, restarts, or configures an agent. Killing an orphaned MCP server is the user's decision with the user's `kill`.
- **Not a cloud dashboard.** Local machine, local files, no telemetry.
- **Not a billing system.** Cost is computed from list prices on the transcript's own usage numbers and is labelled a floor whenever a price is unknown.

## Principles

1. **Ground truth over inference.** When a harness publishes its own state (Claude Code's per-pid registry), use it. Heuristics are labelled as such in the data and the UI.
2. **Count, never guess.** Tokens come from the harness's usage objects. Prices come from a dated table. Unknown models are "unpriced", not estimated.
3. **Cheap enough to leave running.** Incremental transcript tailing, one process scan per tick, no async runtime.
4. **Every harness, one shape.** Claude, Codex, Gemini and the rest are adapters that produce the same `Agent` row.
