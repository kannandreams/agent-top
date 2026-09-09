# Snapshots and replay

Everything the screen shows comes from one data structure, the snapshot. `--json` prints it. `--replay` renders one back, with every key working, without reading anything on the local machine.

```sh
agent-top --json > snap.json        # save what agent-top sees right now
agent-top --replay snap.json        # look at it again, or on another machine
```

## What it is for

**Bug reports.** Attach a snapshot to an issue and the reader can inspect it exactly as you saw it: select rows, open the trace, read the process tree. A description of a wrong number is hard to act on; the snapshot that produced it is not.

**Screenshots and demos.** Every picture of the live view in these docs, and the animation on the home page, is a replay of [`docs/demo-snapshot.json`](demo-snapshot.json), a synthetic snapshot with made-up project names, paths and session ids. A recording of a real machine would publish real ones. You can replay it yourself:

```sh
curl -LO https://raw.githubusercontent.com/kannandreams/agent-top/main/docs/demo-snapshot.json
agent-top --replay demo-snapshot.json
```

**Scripts.** The JSON is stable, versioned (`schema_version`) and complete: every agent with its usage, cost breakdown, spans, MCP servers, context rows and process tree, plus the orphans, the advice and the totals. `jq` does the rest.

```sh
agent-top --json | jq '.totals.cost_usd'
agent-top --json | jq '.agents[] | select(.state == "running") | .name'
agent-top --json | jq '.orphans[] | {pid, cmdline, rss_bytes}'
```

## What is in a snapshot

Before you share one, know what it carries. It is metadata only: no prompt text, no code, no tool output. It does include working directories, transcript paths, session ids, command lines of the processes under each agent, and your hostname. Those are what make a bug report reproducible, and also what you may want to redact before posting one publicly.

| Field | What it is |
|---|---|
| `host` | hostname, CPU count, CPU and memory use |
| `agents[]` | one per row: identity, state, model, usage, `cost_breakdown`, `spans`, `mcp_servers`, `context`, `tree`, `attribution`, `rate_limit` |
| `orphans[]` | MCP processes with no live agent ancestor |
| `orphan_origins[]` | which agent each orphan was under, and when it lost it |
| `advice[]` | the sentences behind the `a` popup, with their numbers |
| `totals` | what the header shows |

Fields added in later versions are optional on the way back in, so an old snapshot still replays on a new agent-top.

## The plain-text form

`agent-top --once` prints the same snapshot as text and exits. `--once` and `--replay` combine, which is how the text screenshot in [The live view](live-view.md#the-plain-text-view) was made.
