---
description: "agent-top is htop for local coding agents: the problem of several long-running agent processes on one machine, what the tool is, and what it deliberately is not."
---

# Vision

**agent-top is htop for local coding agents.**

## The problem

Coding agents are now long-running processes on a developer's machine, several at a time: a couple of Claude Code sessions in different repositories, a Codex thread, a Gemini CLI, an OpenCode session, the subagents those agents spawn, and the MCP servers each of them starts.

They consume three scarce things at once. CPU and memory, like any process. Tokens, which cost real money. And the developer's attention, because an idle agent is an agent waiting for a human.

Nothing on the machine shows those three together. `htop` shows a `node` process with no idea whose it is or what it is spending. Each harness shows its own session and nothing about the others. And when an agent dies and leaves helper processes behind, nobody notices until the machine starts to swap. That one deserves its own explanation: [The MCP server that outlives its agent](blog/posts/the-server-that-outlives-its-agent.md).

## What agent-top is

A single terminal view, in the spirit of `btop`, that lists every coding agent on the machine with its state, tokens, cost, resource use, process tree and age, refreshed every second, and that points at leaked helper processes. Read-only, one static binary, no daemon, no configuration.

Around the live view, the things a monitoring tool is expected to have: a [cost report](report.md) over time, a [tool trace](trace.md) that exports to Perfetto and OpenTelemetry, [snapshots](replay.md) that replay anywhere, and the advice, leaderboard and rate-limit views described in [The live view](live-view.md).

## What agent-top is not

- **Not a harness.** It does not run models, hold API keys, or talk to any provider.
- **Not a controller.** It never signals, restarts, or configures an agent. Killing an orphaned MCP server is the user's decision with the user's `kill`.
- **Not a cloud dashboard.** Local machine, local files, no telemetry. The one network call it can make with your data is posting a trace file to an address you typed after `--endpoint`; nothing is sent by default and nothing is sent about you.
- **Not a billing system.** Cost is computed from list prices on the transcript's own usage numbers and is labelled a floor whenever a price is unknown. [Accounting](accounting.md) says exactly how.
