---
description: "The agent-top local store: a SQLite file on your machine that keeps sessions, spans and MCP calls after a harness deletes its transcripts, and that you can query."
---

# Local store

Harnesses delete their own transcripts. Claude Code removes sessions older than `cleanupPeriodDays`, 30 by default, and every number agent-top computed from them goes too: `agent-top report --since all` shows less than it did a month earlier.

The local store is a SQLite file on your machine that keeps those numbers. `agent-top sync` fills it from the transcripts on disk, `agent-top sql` queries it, and [`agent-top report`](../report.md) counts the sessions in it whose transcripts are gone.

```sh
agent-top sync                                       # keep what is on disk now
agent-top sql "select * from cost_by_day order by day desc limit 7"
```

| Page | What it covers |
|---|---|
| [Sync](sync.md) | `agent-top sync`: what it reads, what it skips, running it on a schedule |
| [Query](query.md) | `agent-top sql`: output formats, the saved views, other SQLite clients |
| [Schema](schema.md) | the tables and columns, times, and how stored cost relates to the price table |

## Where the file is

The first of these that is set: `--db`, `AGENT_TOP_DB`, `$XDG_DATA_HOME/agent-top/agent-top.db`, `~/.local/share/agent-top/agent-top.db`. It is created with mode `0600`. Only `agent-top sync` creates or writes it; `sql`, `report` and the live view never do.

## What is in it

What `--json` shows, and nothing else. agent-top never reads prompt, reply or tool content, so none can be stored. The file does hold working-directory paths and project names.

Nothing leaves the machine. To delete the store, delete the file.
