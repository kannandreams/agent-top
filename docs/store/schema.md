---
description: "The tables and columns of the agent-top local store, how times are stored, and how stored cost relates to the price table."
---

# Schema

The [local store](index.md) has four tables. The saved views built on them are on the [query](query.md) page, and `agent-top sql --schema` prints every column with its type.

| Table | One row per | Main columns |
|---|---|---|
| `sessions` | session, keyed by `harness`, `session_id` | `cwd`, `project`, `model`, `started_at`, `last_activity`, `turns`, `tool_calls`, token columns (`input`, `cache_write_5m`, `cache_write_1h`, `cache_write_unsplit`, `cache_read`, `output`), `cost_usd`, `unpriced_tokens`, `price_source`, `harness_cost_usd`, `parent_session_id` for a subagent session |
| `spans` | tool call, inference or turn, keyed by `harness`, `session_id`, `seq` | `kind`, `name`, `started_at`, `duration_ms` (null while open), `error`, `sidechain`, `parent_seq` (the turn it ran in) |
| `mcp_calls` | MCP server a session called | `server`, `calls`, `errors`, `last_call_at` |
| `sources` | transcript synced | `path`, `size`, `mtime_ms`, `synced_by_version` |

Times are milliseconds since the Unix epoch, UTC. A session is dated by its last activity, as in the [cost report](../report.md). Spans carry no tokens.

Cost is computed with the price table of the agent-top that synced the row. After an upgrade, the next sync re-reads every transcript still on disk, so a price or parser fix reaches it. A session whose transcript is gone keeps the cost it was stored with, and `synced_by_version` in `sessions` says which version that was.

The schema version is `PRAGMA user_version`. Columns are added, not renamed, and an older agent-top refuses to open a file a newer one has upgraded. `sync` upgrades a file an older agent-top wrote; `sql` asks you to run it first.
