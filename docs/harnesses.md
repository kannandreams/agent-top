# Harness support

Every harness is an adapter that produces the same row, so the table, the report, the trace and the snapshot look the same whichever tool wrote the transcript.

| Harness | Discovery | Tokens and cost | State |
|---|---|---|---|
| Claude Code | process table + `~/.claude/sessions/<pid>.json` (exact) | transcript usage, priced per model; subagent transcripts folded into their parent | harness-reported |
| Codex CLI and app-server | process table + the rollout files the process holds open (exact on macOS and Linux; `cwd` heuristic elsewhere) | transcript usage, priced per model at OpenAI list prices | transcript events |
| Gemini CLI | process table + `cwd` heuristic (the CLI keeps no registry and does not hold its transcript open) | transcript usage, priced per model; subagent transcripts folded into their parent | transcript events |
| OpenCode | process table + `cwd` heuristic; reads its SQLite session store read-only | tokens and OpenCode's own computed cost; subagent sessions folded into their parent | transcript times |
| Aider, Copilot CLI, cursor-agent | process table only | not yet | CPU heuristic |

## Attribution

Joining a process to a transcript is the one place a heuristic can be wrong, so the detail pane says how each row was matched:

- **harness registry (exact)**: Claude Code writes a per-pid file that names its session.
- **open transcript file (exact)**: Codex holds its rollout file open, and the open-file table names it.
- **command line --resume (exact)**: the session id is on the command line.
- **cwd + start time (heuristic)**: the most recent transcript in the process's working directory. Gemini CLI and OpenCode rows are matched this way.
- **transcript only (no process)**: a recent transcript nobody owns; the session has stopped.

## MCP servers

Calls are counted from the transcript's own tool names: Claude Code's `mcp__<server>__<tool>`, Gemini's `mcp_<server>_<tool>`, and the server Codex names in each `mcp_tool_call_end` event. The process is matched to the server by name in its command line, or by elimination when exactly one process and one server are left. OpenCode MCP counts are still to come.

## Drift

A harness can change its transcript format under agent-top. A session that did work while accounting for no tokens is a parser that has fallen behind, not a quiet session, and the row says so by name instead of showing a believable `$0.00`. Adding or repairing an adapter is a contained job; the [architecture](architecture.md) page describes the contract.
