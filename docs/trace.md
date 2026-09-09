# Tool trace and export

Every tool call, model response and turn in a session, reconstructed from the transcript. Nothing has to be switched on in the harness: the timestamps are already in the file it writes. That makes the trace retroactive, so it works on a session that ended last week, and harness-neutral, so it works the same for Claude Code, Codex, Gemini CLI and OpenCode.

In the terminal the trace is the waterfall in the detail pane (`Tab`). Out of the terminal it is a file.

```sh
agent-top trace --session 9f2c1a4e -o trace.json            # Chrome trace format
agent-top trace --session 9f2c1a4e --format otlp -o t.json  # OTLP/JSON
agent-top trace --session ~/.claude/projects/.../abc.jsonl  # a path works too
```

`--session` takes a session id, a unique prefix of one, or a path to a transcript. The detail pane prints the exact command for the selected session next to **export**.

## Chrome trace format

The default. Open the file in [Perfetto](https://ui.perfetto.dev) or `chrome://tracing`. Tool calls nest under the turn that issued them, model responses sit beside them, and subagent calls are on their own track, so a session reads like a profile: where the time went, what ran in parallel, what waited on what.

## OpenTelemetry

`--format otlp` writes the body of an OTLP/HTTP traces request, with deterministic trace and span ids so the same session always produces the same trace. Any collector that speaks OTLP/JSON accepts it: Jaeger, Tempo, the OpenTelemetry Collector.

```sh
agent-top trace --session 9f2c1a4e --format otlp --endpoint http://localhost:4318/v1/traces
```

`--endpoint` posts the document to the address you type as well as writing it. It is the only network call agent-top ever makes with your data in it, and it happens only when you type the address. Nothing is sent by default.

## What is in a span

| Kind | What it is |
|---|---|
| turn | one prompt from you to the agent's final reply |
| inference | one model response: from the request to its last token |
| tool | one tool call, from the request to its result; failures are marked |

Subagent spans carry a flag so a viewer can put them on their own track. Tool names are the harness's own (`Read`, `exec_command`, `run_shell_command`) and MCP calls keep their server name.
