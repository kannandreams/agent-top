# The MCP leak

A harness starts a Model Context Protocol server for a session. The session ends, or a subagent finishes, and the server is never told. It keeps running, holding memory. Repeat that a few hundred times and the machine starts to swap. This is the failure that motivated agent-top.

## The reports

Codex has a run of issues about leaked MCP process trees, several of them still open:

| Report | State | What it describes |
|---|---|---|
| [#12491](https://github.com/openai/codex/issues/12491) | open | MCP children not reaped after a task completes: 1300+ zombies, 37 GB leaked |
| [#17574](https://github.com/openai/codex/issues/17574) | open | Subagents leak stdio MCP helper trees, which accumulate indefinitely |
| [#25015](https://github.com/openai/codex/issues/25015) | open | The app-server leaks a process stack per subagent, so memory grows linearly |
| [#16256](https://github.com/openai/codex/issues/16256) | closed | MCP subagent processes never terminated when a session is stopped or suspended |
| [#19753](https://github.com/openai/codex/pull/19753) | merged Apr 2026 | The fix for one of those paths: terminate stdio MCP servers on shutdown |

Nothing about this is specific to Codex. Every harness that spawns helper processes has the same shape of bug available to it.

## How agent-top looks for it

It watches for the symptom rather than any one vendor's bug: an MCP server process with no live agent above it in the process tree. Those are listed in red in the detail pane as **orphaned mcp processes**, each with its pid, memory, age, and the agent it was orphaned from and when. If agent-top only started after the leak, the line says the parent is unknown and how long it has been watching.

There is an earlier signal too. The advice panel (`a`) names an MCP server attached to a live agent whose memory has climbed steadily with no calls to explain it, which is the shape of a leak before it becomes an orphan.

Both are described in [The live view](live-view.md#the-process-tree).

## What agent-top does about it

Nothing, on purpose. It never signals a process. It tells you the pid and the `kill` is yours. See [Vision](vision.md#what-agent-top-is-not).
