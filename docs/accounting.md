---
description: "Where agent-top's numbers come from: tokens counted from usage records, cost from a price table, cache efficiency, and context by source."
---

# Accounting

The whole point of this tool is that its numbers are right, so it is explicit
about which ones are exact and which are inferred.

- **Tokens are counted, never estimated.** They come from the usage records the
  harness writes itself, deduplicated per API message so a response split across
  several transcript lines is counted once.
- **Costs come from a table you can read and change.** `agent-top --prices`
  shows it. It carries Anthropic, OpenAI and Google list prices, so Claude Code,
  Codex and Gemini rows price from it. OpenCode and Kodelet record their own
  costs, which agent-top uses directly and does not reprice at the current model's
  rate, and a recorded cost is that harness's own estimate rather than proof of
  an invoice. A model with no price anywhere is reported as unpriced rather than
  guessed at, which is why a total containing one is shown as a floor (`≥`, `+`)
  instead of a number that looks more precise than it is. A cache write whose
  TTL the harness did not record is counted on its own (`cache_write_unsplit`),
  because the TTL is what picks between the five-minute and one-hour rate, and
  only the harness that recorded it can put a cost against it.
- **A tool count says when it is a floor.** Most harnesses keep every call in
  the transcript, so the count is exact. Where compaction discards old calls
  without leaving a lifetime counter, agent-top counts the calls it retained or
  watched happen and marks the figure `≥N` (`tool_calls_lower_bound` in JSON)
  rather than presenting a snapshot as a total.
- **Attribution says how confident it is.** Claude Code publishes a per-pid
  registry, so a session is matched to its process exactly. Codex has no
  registry, but it keeps every live thread's rollout file open, and on macOS and
  Linux agent-top reads which files a process holds, which is just as exact. On
  a platform where it cannot, the match falls back to working directory and
  start time, and the detail pane labels that row a heuristic rather than
  presenting it as fact.
- **Metadata and output sizes only.** Token counts, model ids, tool names,
  timestamps, and local byte lengths of Codex nested-tool output text.
  Prompts and tool inputs are not inspected. Output text is measured, never
  retained or exported; only numeric weights survive parsing
  (see [Context by source](#context-by-source)).
- **Your data never leaves the machine.** `agent-top` never kills or writes to an
  agent, and it never sends your sessions, prompts, costs or process list
  anywhere. It makes exactly two network calls, neither carrying any of your
  data: a once-a-day update check that asks crates.io for the latest version
  number (nothing about you is sent; turn it off with
  `AGENT_TOP_NO_UPDATE_CHECK=1`), and the `trace --endpoint <url>` you type
  yourself. Killing an orphaned MCP server is your decision, with your own
  `kill`.

## Context by source

The detail pane's `context` section says what each tool's results added to the
prompt and what carrying that has cost. Token sizes are derived from usage
records, not counted from result text.

**Tokens.** A response's prompt is the previous response's prompt plus
everything appended since: the previous reply, and the tool results that
answered it. So `prompt(n) − prompt(n−1)` is the new material; the previous
reply's `output` is the part the model wrote itself; the remainder is the tool
results submitted in between, and it is filed under the tools that produced
them. When several results were answered by one response the growth is split
evenly between them, so per-source figures are not exact. The first response's
whole prompt, the replies, and any growth that no result explains (your own
messages) go to one row, `prompts & replies`.

**Cost.** Every response re-reads the whole context, so each source's tokens
are charged at every response that read them, at that response's own prompt
rate: its prompt-side cost divided by its prompt tokens, which on a well-cached
session is close to the cache-read price with some fresh input mixed in. The
first read is charged the same way. The rows therefore sum to the session's
prompt-side cost (input + cache read + cache write), which the cost breakdown
above the section shows, so the two can be checked against each other.

**Compaction.** A compaction replaces the context with a summary, after which
the old results are no longer being re-read. Claude Code writes a
`compact_boundary` line, and OpenCode writes the summary as a reply marked
`summary`; either resets the ledger exactly. For a harness that
writes no marker, a prompt that halves between two responses is taken as a
compaction; nothing else shrinks it by that much. Thinking blocks a harness
drops between turns shrink it by less, and that shrink is taken off the
`prompts & replies` row, whose replies they were.

**What it cannot see.** A model with no price shows tokens and a `-` for cost.
And the split is per response, so two tools answered together are assumed the
same size; if that matters, the `agent-top trace` export has each call's
duration, which is often a fair proxy.

### By harness

Only what differs. Where a harness is not named here, the rules above apply as
written.

**Codex response ordering.** Some versions log tool results before the usage
of the response that requested them. Those results wait for the following,
consuming response; they do not receive the initial prompt's tokens. Repeated
usage snapshots do not consume queued results or add cost.

**Codex code mode.** Nested-tool output sizes provide relative weights, not
additional tokens. Recognised nested tools whose timing fits entirely inside
one wrapper share that wrapper's allocation. This link is a timing heuristic;
unknown calls, invalid timing or overlapping wrappers keep `exec` (or `wait`).
Each wrapper output contributes once, regardless of its number of children.

Children are weighted by decoded UTF-8 output-text bytes: formatted command
output (falling back to aggregate output or stdout/stderr), patch stdout/stderr,
and text-only MCP or dynamic-tool results. Arguments, patch diffs, JSON envelope
keys and duplicate output fields do not contribute. If any size is unavailable
(including structured or non-text results), or all outputs are empty, children
split evenly. Rounding preserves the wrapper's exact allocation; the `calls`
column counts the nested calls, not an additional wrapper call.

This is a heuristic, not exact per-tool token usage. A wrapper can filter,
combine or discard child output, and byte lengths are not tokenizer counts.
No output text is retained or exported, and session token and cost totals are
unchanged.

**OpenCode.** It records one cost per reply rather than one per kind of token,
so the price table divides that figure between input, cache and output, and the
rows add up to OpenCode's own prompt-side share. A model the table does not
price gets tokens only.

**Kodelet.** It records cumulative usage rather than per-response usage, so
there is no ledger to build and the `context` section is empty for its rows.
Its Responses input counter includes cache reads; the adapter separates those
once, while Chat Completions and Anthropic input counters already exclude them.
Child sessions contribute their own usage once, never both separately and
folded into a parent.

Its tool count is a lower bound. Compaction removes previous calls and results
without keeping a lifetime counter, so each distinct retained or observed call
ID counts once, including failures, and child calls stay on the child's row.
The live tracker remembers observed IDs across compactions, but a cold start or
an export cannot restore deleted history. For forks, only completed results
whose timestamps establish new work are counted, not pending calls that may
have been inherited.

## If the cost does not match your harness

It usually will not match to the cent, and that is not a bug in either tool.
agent-top prices the harness's own token counts at the vendor's published list
price. Harnesses keep their own price tables, and those can differ from the
published page for a model, or lag behind a price change. Neither number is a
bill: on a subscription plan nothing is charged per token, and both figures are
"what this would have cost on the API".

A real example. One Claude Code session, read at the same moment by both tools:

| Line | Tokens | agent-top | Claude Code |
|---|---|---|---|
| input | 8.7k | $10 / M, $0.09 | $10 / M, $0.09 |
| cache write (1h) | 1.0M | $20 / M, $20.30 | $20 / M, $20.30 |
| cache read | 31.2M | $0.25 / M, $7.80 | $0.50 / M, $15.59 |
| output | 318k | $50 / M, $15.90 | $50 / M, $15.90 |
| total | | $44.12 | $51.91 |

Three lines agree, the cache read line does not: the pricing page lists Fable
5.1 cache hits at $0.25 per million, and Claude Code 2.1.259 charged $0.50.
Because every turn re-sends the whole conversation from cache, that one line
is most of a long session's cost, and a small difference on it becomes a large
gap in the total.

The detail pane shows this breakdown for every row, so the differing line can
be found without arithmetic. If you would rather see the same figure as your
harness, override that one price in your own table and the row will say
"your price file":

```toml
# ~/.config/agent-top/prices.toml
[[model]]
prefix = "claude-fable-5-1"
input = 10.0
output = 50.0
cache_read = 0.5
```

The built-in table stays at the published price. It is never adjusted to match
a harness, because the harness's table is not published and changes without
notice.

[architecture.md](architecture.md) has the mechanism underneath: the process
walk, the incremental transcript tail, and how a snapshot is assembled on each
tick.
