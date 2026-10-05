---
description: "agent-top sync keeps sessions, spans and MCP calls in a local SQLite file, so cost history survives a harness deleting its transcripts."
---

# History store

Harnesses delete their own transcripts. Claude Code removes sessions older than `cleanupPeriodDays`, 30 by default, and every number agent-top computed from them goes too: `agent-top report --since all` shows less than it did a month earlier.

`agent-top sync` keeps those numbers in a SQLite file on your machine.

```sh
agent-top sync                  # read every transcript that changed since the last sync
agent-top sync --since 7d       # only transcripts written in the last week
agent-top sync --db ./mine.db   # a different file
agent-top sync --json           # what the sync did, structured
```

The first sync reads everything on disk. Later ones read only transcripts whose size or modification time changed, so a sync with nothing new takes a fraction of a second. A session stays in the store after its transcript is deleted. A transcript that reads as empty where the store already has a session does not replace it.

Run it from cron, `launchd` or a shell hook, as often as you like. A long-running `agent-top serve` is planned.

## Where the file is

The first of these that is set: `--db`, `AGENT_TOP_DB`, `$XDG_DATA_HOME/agent-top/agent-top.db`, `~/.local/share/agent-top/agent-top.db`. It is created with mode `0600`. Nothing else in agent-top creates or writes it.

## What is in it

What `--json` shows, and nothing else: no prompt, reply or tool content is ever read, so none can be stored. The file does hold working-directory paths and project names.

Any SQLite client opens it:

```sh
sqlite3 ~/.local/share/agent-top/agent-top.db \
  "select project, round(sum(cost_usd), 2) from sessions group by 1 order by 2 desc limit 10"
```

| Table | One row per | Main columns |
|---|---|---|
| `sessions` | session, keyed by `harness`, `session_id` | `cwd`, `project`, `model`, `started_at`, `last_activity`, `turns`, `tool_calls`, token columns (`input`, `cache_write_5m`, `cache_write_1h`, `cache_write_unsplit`, `cache_read`, `output`), `cost_usd`, `unpriced_tokens`, `price_source`, `harness_cost_usd`, `parent_session_id` for a subagent session |
| `spans` | tool call, inference or turn, keyed by `harness`, `session_id`, `seq` | `kind`, `name`, `started_at`, `duration_ms` (null while open), `error`, `sidechain`, `parent_seq` (the turn it ran in) |
| `mcp_calls` | MCP server a session called | `server`, `calls`, `errors`, `last_call_at` |
| `sources` | transcript synced | `path`, `size`, `mtime_ms`, `synced_by_version` |

Times are milliseconds since the Unix epoch, UTC. A session is dated by its last activity, as in the [cost report](report.md). Spans carry no tokens.

Cost is computed with the price table of the agent-top that synced the row. After an upgrade, the next sync re-reads every transcript still on disk, so a price or parser fix reaches it. A session whose transcript is gone keeps the cost it was stored with, and `synced_by_version` in `sessions` says which version that was.

The schema version is `PRAGMA user_version`. Columns are added, not renamed, and an older agent-top refuses to open a file a newer one has upgraded.
