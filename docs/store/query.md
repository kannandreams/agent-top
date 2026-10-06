---
description: "agent-top sql runs one read-only SQL statement against the local store and prints a table, JSON or CSV, with saved views for cost, tool latency and MCP errors."
---

# Query

`agent-top sql` runs one statement against the [local store](index.md) and prints the rows. It opens the file read-only, and a statement that would change anything is refused.

```sh
agent-top sql "select * from cost_by_project order by cost_usd desc limit 10"
agent-top sql "select * from tool_latency order by p95_ms desc limit 10"
agent-top sql "select day, cost_usd from cost_by_day where day >= '2026-09-01'" --csv
agent-top sql "select model, sum(cost_usd) from sessions group by 1" --json
agent-top sql --schema          # every table and view with its columns
```

```text
harness   sessions      tokens    cost_usd  unpriced_tokens
--------  --------  ----------  ----------  ---------------
claude          54  3089042092  1700.22193                0
codex          124  1562078731  647.923571         39590487
opencode        48   704256191    8.456063                0
(3 rows)
```

The default output is an aligned table with numbers on the right. `--json` prints an array of objects keyed by column name. `--csv` prints a header row and RFC 4180 quoting.

A store written by an older agent-top needs one `agent-top sync` before `sql` or `report` reads it; the sync brings the schema up to date.

## Views

| View | One row per | Columns |
|---|---|---|
| `counted_sessions` | session with tokens or turns, what `report` counts | every `sessions` column, plus `tokens` |
| `cost_by_day` | UTC day of last activity | `day`, `sessions`, `tokens`, `cost_usd`, `unpriced_tokens` |
| `cost_by_harness` | harness | the same |
| `cost_by_model` | model | the same |
| `cost_by_project` | project | the same |
| `tool_latency` | tool name | `calls`, `errors`, `p50_ms`, `p95_ms`, `max_ms` (finished calls only) |
| `mcp_errors` | MCP server | `sessions`, `calls`, `errors`, `error_rate`, `last_call_at` |

The cost views add up to the same totals as `agent-top report --since all` for the sessions in the store. The tables behind them are on the [schema](schema.md) page.

## Other clients

Any SQLite client opens the same file:

```sh
sqlite3 ~/.local/share/agent-top/agent-top.db "select * from cost_by_model"
```

DuckDB attaches it with `ATTACH 'agent-top.db' (TYPE sqlite)`.
