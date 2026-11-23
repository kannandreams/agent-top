---
description: "The tables and columns of the agent-top local store, how times are stored, and how stored cost relates to the price table."
---

# Schema

The [local store](index.md) has nine tables. The saved views built on them are on the [query](query.md) page, and `agent-top sql --schema` prints every column with its type.

| Table | One row per | Main columns |
|---|---|---|
| `sessions` | session, keyed by `harness`, `session_id`; `attribution` is `transcript`, or `telemetry` for a [received](telemetry.md) one | `cwd`, `project`, `model`, `started_at`, `last_activity`, `turns`, `tool_calls`, token columns (`input`, `cache_write_5m`, `cache_write_1h`, `cache_write_unsplit`, `cache_read`, `output`), `cost_usd`, `unpriced_tokens`, `price_source`, `harness_cost_usd`, `tool_calls_lower_bound` (earlier calls were missing from the transcript), `parent_session_id` for a subagent session |
| `spans` | tool call, inference or turn, keyed by `harness`, `session_id`, `seq` | `kind`, `name`, `started_at`, `duration_ms` (null while open), `error`, `sidechain`, `parent_seq` (the turn it ran in), `agent` (the subagent that issued it; null for the main agent) |
| `subagents` | subagent run folded into a session, keyed by `harness`, `session_id`, `agent_id` | `agent_type`, `spawned_by` (the `span_id` of the call that started it), `model`, `started_at`, `last_activity`, `tokens`, `cost_usd`, `unpriced_tokens`, `tool_calls` |
| `mcp_calls` | MCP server a session called | `server`, `calls`, `errors`, `last_call_at` |
| `context_points` | model response of a session's main agent, keyed by `harness`, `session_id`, `agent`, `seq` | `at`, `prompt` (the tokens it read), `output`, `cache_read`, and `tool_tokens`, `mcp_tokens`, `other_tokens`, an estimated split of `prompt` |
| `compactions` | compaction, keyed like `context_points`; `seq` is the first response after it | `at`, `triggered_by` (`auto`, `manual` or `unknown`), `detected_by` (`harness`, or `prompt-halved` when inferred), `before_tokens`, `summary_tokens`, `after_tokens` |
| `compaction_sources` | source in the context when a compaction cleared it | `seq`, `origin` (`tool`, `mcp` or `other`), `name`, `tokens` (estimated) |
| `sources` | transcript synced | `path`, `size`, `mtime_ms`, `synced_by_version` |
| `telemetry_spans` | span received by `serve --listen`, keyed by `trace_id`, `span_id` | `harness`, `session_id`, `service`, `kind`, `name`, `model`, `started_at`, `duration_ms`, `error`, token columns, `received_at` |

Times are milliseconds since the Unix epoch, UTC. A session is dated by its last activity, as in the [cost report](../report.md). Spans carry no tokens.

Cost is computed with the price table of the agent-top that synced the row. After an upgrade, the next sync re-reads every transcript still on disk, so a price or parser fix reaches it. A session whose transcript is gone keeps the cost it was stored with, and `synced_by_version` in `sessions` says which version that was.

`agent` in the context tables is empty for the main agent and the subagent's id for a subagent, the same id as `subagents.agent_id`. No compaction summary text is stored.

`spans.agent` and `subagents` are filled for Claude Code and OpenCode, which both record which subagent issued a call and which call started the subagent. For OpenCode the `agent_id` is the child session's id and the cost is OpenCode's own figure for it. Other harnesses leave them empty. Upgrading a store to this schema makes the next sync re-read every transcript still on disk.

The schema version is `PRAGMA user_version`. Columns are added, not renamed, and an older agent-top refuses to open a file a newer one has upgraded. `sync` upgrades a file an older agent-top wrote; `sql` asks you to run it first.
