---
date: 2026-09-10
authors:
  - kannan
slug: what-tracing-means-for-an-agent-session
description: "What a trace and a span mean once the workload is an agent loop instead of a web request, why the transcript already has everything one needs, and a walk through debugging a slow turn with it."
---

# What tracing means for an agent session

Tracing is an old idea from distributed systems: a request comes in, it fans out into work on several services, and if you want to know where the time went you need to see all of that work on one shared timeline, with the causal links between the pieces intact. A **span** is one piece of work with a start, an end, and a name. A **trace** is a tree of spans that all belong to the same request. The reason the idea generalized past HTTP services is that it doesn't actually care what the work is — only that it has a beginning, an end, and a parent.

An agent turn has exactly that shape. You send a prompt. The model thinks, then calls a tool, then thinks again about the result, maybe calls three tools in parallel, spins up a subagent that does the same thing one level down, and eventually replies. That's a trace. The model's thinking is a span. Each tool call is a span. The subagent's whole turn is a span containing its own spans. Nothing about applying tracing to this is a stretch; the only question is where the spans come from.

<!-- more -->

## The transcript is already the trace

The usual way to get a trace is instrumentation: you put OpenTelemetry calls around the code, ship spans to a collector, and only requests that happened after you did that are visible. Agent harnesses don't need that step, because the transcript they write for their own purposes already contains the trace, just not shaped like one.

Take Claude Code's JSONL. A tool call is a `tool_use` block; the answer is a `tool_result` a few lines later carrying the same `tool_use_id`. Codex pairs `function_call` and `function_call_output` by `call_id`. Both harnesses stamp the surrounding lines with a timestamp. That's a span: an id to pair begin and end, a start time, an end time once the pair completes. The file was never trying to be a trace — it's an append-only record of a conversation — but a trace is just what you get once you fold matching begin/end pairs into intervals.

This has one consequence worth dwelling on: the trace is retroactive. You don't decide to trace a session before it starts. You point at a transcript, today, for a session that ended last Tuesday, and get the same trace you'd have gotten if OpenTelemetry had been wired in from the first line. Nothing has to be switched on in the harness, which also means it works the same for every harness that writes a transcript with paired call/result records and a timestamp — Claude Code, Codex, Gemini CLI, OpenCode — without four different SDKs to integrate.

## Building the tree

agent-top's reconstruction (`crates/agent-top/src/trace.rs`) keeps it to three span kinds:

| Kind | What it is |
|---|---|
| turn | one prompt from you to the agent's final reply |
| inference | one model response: from the request to its last token |
| tool | one tool call, from the request to its result |

Parenting a tool or inference span to its turn isn't stored anywhere in the transcript; there's no `turn_id` on a tool call. It's recovered by interval containment — a turn opens at your prompt and closes at the reply, so anything that started while that window was open and hadn't already been claimed by a narrower open turn belongs to it:

```rust
fn parent_turn<'a>(spans: &[&'a ToolSpan], i: usize) -> Option<&'a ToolSpan> {
    let sp = spans[i];
    if sp.kind == SpanKind::Turn {
        return None;
    }
    let contains = |t: &ToolSpan| {
        t.kind == SpanKind::Turn
            && t.started_at <= sp.started_at
            && t.duration_ms.map(|ms| t.started_at + Duration::from_millis(ms) >= sp.started_at).unwrap_or(true)
    };
    spans[..i].iter().rev().find(|t| t.sidechain == sp.sidechain && contains(t))
        .or_else(|| spans[..i].iter().rev().find(|t| !t.sidechain && contains(t)))
        .copied()
}
```

Subagent spans prefer a subagent turn of their own if the transcript carries one, and fall back to the main turn otherwise — which is the whole rule for keeping a subagent's fan-out on its own branch of the tree instead of flattening it into the parent's.

The other thing worth being honest about is spans that never close. A tool call issued right as the transcript ends, or a session that's still running when you export it, has no end timestamp. Inventing one — "assume it took as long as the last call" — would make a trace that lies. So an open span keeps its start and gets no end: in Chrome trace format that's a begin event (`"ph": "B"`) with no matching end, which Perfetto draws as a slice with no closing edge instead of a slice with a fabricated width; in OTLP, where an end time is mandatory, it gets one equal to its start plus an `agent_top.open` attribute, so a reader can tell "zero duration" from "we don't know."

## Two shapes, two audiences

`agent-top trace --session <id>` writes either format:

```sh
agent-top trace --session 9f2c1a4e -o trace.json            # Chrome trace format
agent-top trace --session 9f2c1a4e --format otlp -o t.json  # OTLP/JSON
```

Chrome trace format needs no server — `ui.perfetto.dev` or `chrome://tracing` opens the file directly, which makes it the right first look for one session. OTLP is the interchange format if you already run Jaeger, Tempo, or a collector, and want the agent's turn to show up next to the rest of your system's traces rather than in a separate tool. `--endpoint` posts it there directly, and it's the only network call in agent-top that carries session data anywhere — no default endpoint, no config file, it only happens when you type the URL on the command line.

One detail that matters if you ever diff two exports: span and trace ids are derived from the session id and each span's own id (FNV-1a, not random), so exporting the same session twice produces byte-identical ids. Export a session before and after changing a system prompt and the tool spans that didn't change keep the same id across both files — useful if you're trying to isolate which turn got slower rather than eyeballing two unrelated-looking traces.

## Reading the waterfall without leaving the terminal

You don't need Perfetto to get the first read. `Tab` in the detail pane switches from the process tree to the same spans as a waterfall, newest at the bottom, refreshed once a second:

![The tool trace: every tool call and model response on one time axis](../../screenshots/tool-trace.png)

The header line above the waterfall is the one number that answers the question people actually ask, which is "why has this agent been busy for eight minutes": the share of the window that was tool time versus inference time, with overlapping tool calls merged rather than summed so five parallel calls that each took four seconds don't get counted as twenty seconds of tool time. If that share is mostly tool, one call is the long pole and it's usually the widest bar on the tools track. If it's mostly the gap, the model was thinking — and no span log fixes a slow model, but at least you've ruled out the tool.

## A debugging session, not a demo

Say a turn took nine minutes and you want to know why before you decide whether to file it as a bug against the harness, the MCP server, or your own prompt.

```sh
agent-top trace --session 9f2c1a4e -o slow-turn.json
```

Open it in Perfetto. Three things to check, roughly in order of how often each one turns out to be the answer:

**Is it one call or many?** A single wide bar on the tools track is a tool that's slow, full stop — an MCP server making a network round trip, a shell command that's actually doing nine minutes of work. A row of narrow bars back to back is a chattier pattern: the model calling a cheap tool repeatedly instead of batching, which is a prompting problem, not an infrastructure one. The waterfall makes the two look different at a glance in a way a duration number in a log line doesn't.

**Did anything not come back?** A begin event with no end — a slice that starts and never closes — is the same signature you'd get from [an MCP server that stopped answering](the-server-that-outlives-its-agent.md). If the transcript ended mid-call, that's expected. If it's mid-session and the agent moved on to something else anyway, the harness gave up waiting on that call, and the tool result the model based its next step on either arrived late or never arrived — worth knowing before you trust the turn after it.

**Did the subagent track run serially when it should have run in parallel?** Subagent spans sit on their own tracks specifically so a fan-out that should overlap and doesn't is visible as five bars stacked with gaps between them instead of five bars stacked on top of each other. That shape — sequential when the intent was parallel — is easy to miss in a transcript and impossible to miss in a waterfall.

None of this requires you to have decided, ahead of time, that this session might need tracing. That's the actual point: the same file that exists because the harness needed to remember what happened is the input, so the debugging tool is available after the fact, for every session you already ran, not just the ones you had the foresight to watch live.

## Try it

```sh
brew install kannandreams/tap/agent-top   # or: cargo binstall agent-top
agent-top trace --session <id> -o trace.json
```

No id handy? `agent-top` lists every session on the machine, and the detail pane prints the exact `trace` command for whichever row is selected. Full reference: [Tool trace and export](../../trace.md).
