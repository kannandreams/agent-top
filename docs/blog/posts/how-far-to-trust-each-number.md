---
date: 2026-09-24
authors:
  - kannan
slug: how-far-to-trust-each-number
description: "Every figure agent-top shows is one of four kinds: exact, a floor, an estimate or a guess. What each marker on screen means, where each kind comes from, and how the tests keep the labels honest."
---

# Exact, floor, estimate, guess: how far to trust each number

agent-top reads files it did not write. Some of them record exactly what happened, some record part of it, and some leave agent-top to work the rest out. The figures on screen come from all three, and they sit next to each other in the same table.

So each figure carries a mark for which kind it is. This post lists the marks, what produces each one, and what to do when a number does not match the one your harness shows.

<!-- more -->

## The four kinds

| Kind | Mark on screen | Example |
|---|---|---|
| exact | none, or `(exact)` | tokens from the harness's usage records |
| floor | `≥` or `+` | a cost that includes a model with no price |
| estimate | `(est.)` | what each tool's results added to the prompt |
| guess | `(heuristic)`, `pid?` | a session matched to its process by working directory |

A figure with no mark is exact. The rest of this post is the marked ones.

## Exact: tokens, and most attributions

Tokens are counted from the usage records the harness writes after each model response. agent-top never estimates them from text length. A response split across several transcript lines is counted once.

The link between a process and its transcript is usually exact too. The detail pane says how it was made on the `attributed` line:

| `attributed` | How |
|---|---|
| `harness registry (exact)` | Claude Code publishes which pid owns which session |
| `open transcript file (exact)` | the process holds the transcript file open, which agent-top reads on macOS and Linux |
| `command line --resume (exact)` | the session id is in the process's arguments |
| `cwd + start time (heuristic)` | none of the above worked, so the row was matched by directory and start time |
| `transcript only (no process)` | a recent transcript with nothing running behind it |

The demo snapshot used for the [screenshots](../../replay.md) has two heuristic rows. Replay it to see how one is labelled.

## Floor: `≥` and `+`

A floor is a real number that is known to be short.

**Cost.** agent-top prices tokens from a table you can read with `agent-top --prices`. A model with no entry in it has its tokens counted as `unpriced_tokens` and priced at nothing. The row's cost is then the priced part only, and it is shown as `≥$4.10` in the live view or `$4.10+` in the report. If none of a row's tokens could be priced, the cost reads `n/a`.

![agent-top report by harness, with a + on the Codex total](../../screenshots/report-by-harness.png)

The Codex row in this report ends in `+`. Some of those sessions ran on a model the table did not price when the report was taken. The UNPRICED column says how many tokens that was.

**Tool calls.** Kodelet deletes old tool calls when it compacts a conversation and keeps no lifetime count. agent-top counts the calls it can still see, plus the ones it watched happen while it was running, and shows the result as `≥71` with a line under it: `retained/observed calls; earlier history may be missing`. In `--json` the same row has `tool_calls_lower_bound: true`.

## Estimate: `(est.)`

The `context` section of the detail pane says which tool's results are filling the prompt, and what re-reading them has cost since. Its header ends in `(est.)`.

The token sizes are derived. The prompt of one response is the prompt of the previous one plus the previous reply plus the tool results in between. Subtract the first two and the remainder belongs to the tools. When one response answered several tool calls at once, that remainder is split evenly between them, because the usage record has one number for all of them. Codex code mode, where one call wraps several tools, divides its share by the length of each tool's output.

The rows add up to the session's prompt-side cost, which is shown above the section, so the total is checked. The split between rows is the estimate. [Accounting](../../accounting.md#context-by-source) has every rule.

## Guess: `(heuristic)` and `pid?`

The MCP panel (`m`) matches each server a transcript calls to the process running it. It first looks for the server's name in a process's command line. A server configured as `scratchfs` and started as `npx @modelcontextprotocol/server-filesystem` shares no text with its process, so that match fails. When exactly one unmatched server and one unmatched MCP process are left under the same agent, agent-top pairs them and marks the pid `pid?`. The panel's header says so: `pid? = process guessed`.

On a machine with one MCP server per agent, most rows are `pid?`. The mark stays because the pairing is by elimination, and a second server makes a wrong pair possible.

## One more: `?`

If a transcript parses but its usage records do not, the harness has probably changed its format. The row's cost shows `?` and the detail pane opens with a red warning. `$0.00` would read as a cheap session, which is the wrong conclusion.

## Where the price came from

The `cost` line in the detail pane names its source:

| Label | Meaning |
|---|---|
| `list price, built-in table` | the vendor's published price, from agent-top's table |
| `your price file` | an entry in `~/.config/agent-top/prices.toml` |
| `harness-reported cost` | OpenCode and Kodelet record their own cost, and agent-top uses it as recorded |
| `no price for this model` | the tokens are counted and the cost is `n/a` |

This matters most when your harness shows a different total. In one real Claude Code session agent-top showed $44.12 and Claude Code showed $51.91. Input, cache write and output agreed line for line. Cache read did not: the pricing page lists $0.25 per million, and Claude Code 2.1.259 charged $0.50. A long session re-reads its whole context from cache on every response, so that one line was 31.2M tokens and most of the gap.

The detail pane shows the per-line breakdown for every row, so the line that differs can be found without arithmetic. To see your harness's figure, override the one price in your own file and the row will say `your price file`. The built-in table stays at the published price.

## How the labels are tested

`crates/agent-top-core/tests/fixtures` holds real transcripts, reduced to the fields the parser reads, each with the exact output it should produce. Until [#50](https://github.com/kannandreams/agent-top/pull/50) those expected files recorded the numbers and none of the labels. A change that marked every Claude row as a floor, or dropped the `≥` from Kodelet's tool count, would have passed every test.

They now also record `price_source`, `tool_calls_lower_bound`, `cache_write_unsplit` and `folds_child_usage`. A change to any label fails the same test as a change to a number.

## Try it

```sh
brew install kannandreams/tap/agent-top   # or: cargo binstall agent-top
agent-top
```

Select a row and read the `attributed` and `cost` lines in the detail pane. `agent-top --prices` shows the table the costs come from. The [Accounting](../../accounting.md) page has the full rules.
