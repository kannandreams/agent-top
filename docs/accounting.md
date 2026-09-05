# Where agent-top's numbers come from

The whole point of this tool is that its numbers are right, so it is explicit
about which ones are exact and which are inferred.

- **Tokens are counted, never estimated.** They come from the usage records the
  harness writes itself, deduplicated per API message so a response split across
  several transcript lines is counted once.
- **Costs come from a table you can read and change.** `agent-top --prices`
  shows it. It carries Anthropic, OpenAI and Google list prices, so Claude Code,
  Codex, Gemini and OpenCode rows all price from it (OpenCode is the exception:
  it runs third-party models and computes its own cost, which agent-top uses
  directly). A model with no price anywhere is reported as unpriced rather than
  guessed at, which is why a total containing one is shown as a floor (`≥`, `+`)
  instead of a number that looks more precise than it is.
- **Attribution says how confident it is.** Claude Code publishes a per-pid
  registry, so a session is matched to its process exactly. Codex has no
  registry, but it keeps every live thread's rollout file open, and on macOS and
  Linux agent-top reads which files a process holds, which is just as exact. On
  a platform where it cannot, the match falls back to working directory and
  start time, and the detail pane labels that row a heuristic rather than
  presenting it as fact.
- **Only metadata is read.** Token counts, model ids, tool names, timestamps.
  Never a prompt, a tool input, or a tool result.
- **Nothing is written, signalled, or sent anywhere.** `agent-top` never kills or
  writes to an agent, and the only network call it can make is the one you ask
  for by typing an address after `--endpoint`. Killing an orphaned MCP server
  is your decision, with your own `kill`.

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
