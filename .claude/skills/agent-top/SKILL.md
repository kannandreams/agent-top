---
name: agent-top
description: Use agent-top to see what coding-agent sessions (Claude Code, Codex, Gemini CLI, OpenCode) are running on this machine, or to debug one after the fact: cost, token burn, why a turn is slow, a tool call that never came back, or an MCP server process left running with no agent above it. Trigger on "why is this agent slow/expensive", "what has this session cost", "check for a leaked/orphaned MCP process", "trace this session", or before assuming a hang is the model thinking rather than a tool call stuck.
---

# Using agent-top

agent-top is a read-only process/transcript viewer for coding-agent sessions. It never signals or kills a process and never writes to a transcript, so it is always safe to run alongside whatever is being debugged. If the command is missing: `brew install kannandreams/tap/agent-top` or `cargo binstall agent-top`.

## Right now, on this machine

```sh
agent-top            # live table, one row per session, refreshed every second
agent-top --once     # the same information, printed once and exit, for a quick look or a script
agent-top --json      # one snapshot as JSON, to pipe or attach to a bug report
```

STATE and COST are the two columns to read first: `running` is mid-turn, `idle` is waiting on you, `stopped` is a transcript with no live process. Cost is priced from the harness's own usage records, not estimated. Select a row to see its detail pane: process tree, MCP servers, and context-by-source (which tool's results are filling the prompt and what re-reading them has cost since).

## What has this cost so far

```sh
agent-top report --since 7d --by harness   # or --by day, --by model, --by project
agent-top report --since all --json        # every session on disk, machine-readable
```

Reads the transcripts already on disk; no daemon, nothing has to have been running.

## Why is this session slow

Select the session and press `Tab` to switch the detail pane to the tool-call waterfall, or export it:

```sh
agent-top trace --session <id-or-prefix> -o trace.json            # Chrome trace, open in ui.perfetto.dev
agent-top trace --session <id-or-prefix> --format otlp -o t.json  # OTLP/JSON, for Jaeger/Tempo/a collector
```

The header line above the waterfall gives the split between tool time and inference time (overlapping calls merged, not summed). A single wide bar on the tools track is one slow call; a row of narrow bars back to back is the model calling a cheap tool repeatedly instead of batching. A bar that starts and never closes is a tool call that never got a result back. Check that before trusting whatever the agent did next. Subagent calls sit on their own track, so a fan-out that should have run in parallel but didn't is visible as bars with gaps instead of bars stacked on top of each other.

`--session` takes a session id, a unique prefix of one, or a path to the transcript file directly; it works on a session that already ended, since the trace is reconstructed from the transcript rather than from live telemetry.

## Is a process leaking

The detail pane lists orphaned MCP servers in red: a process with no live agent above it, its pid, memory, and (when known) which agent it was orphaned from and how long ago. agent-top only reports the pid. Killing it is a manual `kill`, on purpose.

## Everything above works on someone else's machine too

```sh
agent-top --json > snap.json
agent-top --replay snap.json   # the same screen, reconstructed, no local data read
```

Ask for the `--json` snapshot instead of a description when a report doesn't make sense. It reproduces exactly.
