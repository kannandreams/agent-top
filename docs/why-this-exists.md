# Why agent-top exists

You have three Claude Code sessions, a Codex thread in VS Code, and a Gemini
CLI you forgot about. Which one is burning tokens right now? Which one is
waiting on you? Which MCP server is still alive after the agent that started it
died? Each harness answers only for itself, in its own window. `agent-top`
answers across all of them in one terminal view, the way `htop` does for
processes and `btop` does for the whole machine.

## The leak this tool exists to catch

The failure that motivated the tool is not hypothetical. Codex has a run of
reports about leaked MCP process trees, several of them still open:

| Report | State | What it describes |
|---|---|---|
| [#12491](https://github.com/openai/codex/issues/12491) | open | MCP children not reaped after a task completes: 1300+ zombies, 37 GB leaked |
| [#17574](https://github.com/openai/codex/issues/17574) | open | Subagents leak stdio MCP helper trees, which accumulate indefinitely |
| [#25015](https://github.com/openai/codex/issues/25015) | open | The app-server leaks a process stack per subagent, so memory grows linearly |
| [#16256](https://github.com/openai/codex/issues/16256) | closed | MCP subagent processes never terminated when a session is stopped or suspended |
| [#19753](https://github.com/openai/codex/pull/19753) | merged Apr 2026 | The fix for one of those paths: terminate stdio MCP servers on shutdown |

Nothing about this is specific to Codex. Every harness that spawns helper
processes has the same shape of bug available to it, which is why `agent-top`
looks for the symptom, a Model Context Protocol server with no live agent above
it, rather than for any one vendor's bug. Those show as red rows in the detail
pane, each labelled with the agent it was orphaned from.

## What it will not do

`agent-top` observes; it never acts on an agent. It does not kill or restart a
process, write to a transcript, or send any of your data anywhere. Killing an
orphaned MCP server is your decision, with your own `kill`. It makes two network
calls that carry none of your data: a daily update check (a version lookup you
can disable with `AGENT_TOP_NO_UPDATE_CHECK=1`) and the `trace --endpoint <url>`
you type. See [where the numbers come from](accounting.md) for how the reading
stays read-only and metadata-only.
