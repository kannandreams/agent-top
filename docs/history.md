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

## Querying it

`agent-top sql` runs one statement against the store and prints the rows. It opens the file read-only, and a statement that would change anything is refused.

```sh
agent-top sql "select * from cost_by_project order by cost_usd desc limit 10"
agent-top sql "select * from tool_latency order by p95_ms desc limit 10"
agent-top sql "select day, cost_usd from cost_by_day where day >= '2026-09-01'" --csv
agent-top sql "select model, sum(cost_usd) from sessions group by 1" --json
agent-top sql --schema          # every table and view with its columns
```

Any SQLite client opens the same file, and DuckDB attaches it with `ATTACH 'agent-top.db' (TYPE sqlite)`.

| View | One row per | Columns |
|---|---|---|
| `counted_sessions` | session with tokens or turns, what `report` counts | every `sessions` column, plus `tokens` |
| `cost_by_day` | UTC day of last activity | `day`, `sessions`, `tokens`, `cost_usd`, `unpriced_tokens` |
| `cost_by_harness` | harness | the same |
| `cost_by_model` | model | the same |
| `cost_by_project` | project | the same |
| `tool_latency` | tool name | `calls`, `errors`, `p50_ms`, `p95_ms`, `max_ms` (finished calls only) |
| `mcp_errors` | MCP server | `sessions`, `calls`, `errors`, `error_rate`, `last_call_at` |

## Tables

| Table | One row per | Main columns |
|---|---|---|
| `sessions` | session, keyed by `harness`, `session_id` | `cwd`, `project`, `model`, `started_at`, `last_activity`, `turns`, `tool_calls`, token columns (`input`, `cache_write_5m`, `cache_write_1h`, `cache_write_unsplit`, `cache_read`, `output`), `cost_usd`, `unpriced_tokens`, `price_source`, `harness_cost_usd`, `parent_session_id` for a subagent session |
| `spans` | tool call, inference or turn, keyed by `harness`, `session_id`, `seq` | `kind`, `name`, `started_at`, `duration_ms` (null while open), `error`, `sidechain`, `parent_seq` (the turn it ran in) |
| `mcp_calls` | MCP server a session called | `server`, `calls`, `errors`, `last_call_at` |
| `sources` | transcript synced | `path`, `size`, `mtime_ms`, `synced_by_version` |

Times are milliseconds since the Unix epoch, UTC. A session is dated by its last activity, as in the [cost report](report.md). Spans carry no tokens.

Cost is computed with the price table of the agent-top that synced the row. After an upgrade, the next sync re-reads every transcript still on disk, so a price or parser fix reaches it. A session whose transcript is gone keeps the cost it was stored with, and `synced_by_version` in `sessions` says which version that was.

The schema version is `PRAGMA user_version`. Columns are added, not renamed, and an older agent-top refuses to open a file a newer one has upgraded. `sync` upgrades a file an older agent-top wrote; `sql` asks you to run it first.
