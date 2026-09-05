# agent-top

**htop for local coding agents.**

[![CI](https://github.com/kannandreams/agent-top/actions/workflows/ci.yml/badge.svg)](https://github.com/kannandreams/agent-top/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/agent-top.svg)](https://crates.io/crates/agent-top)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-edition%202024-orange.svg)](Cargo.toml)

You have three Claude Code sessions, a Codex thread in VS Code, and a Gemini CLI you forgot about. Which one is burning tokens right now? Which one is waiting on you? Which MCP server is still alive after the agent that started it died? `agent-top` answers that in one terminal view, the way `htop` answers it for processes and `btop` answers it for the whole machine.

![agent-top](https://raw.githubusercontent.com/kannandreams/agent-top/main/docs/demo.gif)

<sub>Recorded from a synthetic snapshot (`docs/demo-snapshot.json`, replayed with `--replay`) rather than a live machine, because a recording of real sessions would publish real project names, working directories and session ids. Regenerate with `vhs docs/demo.tape`.</sub>

## Quick start

```sh
brew install kannandreams/tap/agent-top   # or: cargo binstall agent-top
agent-top
```

That is the whole setup. There is nothing to configure and nothing to enable
in your agents: `agent-top` reads the transcripts the harnesses already write
and the process table the OS already keeps. Start it in any terminal while
your agents run.

What to look at first:

- **STATE** tells you who is working and who is waiting for you.
- **COST** is what each session has spent so far, at list price.
- **Red rows in the detail pane** are MCP servers whose agent has gone. They are the leak this tool exists to catch.

Keys: `j`/`k` move, `Tab` switches the detail pane between the process tree and the tool trace, `s` sorts, `x` hides stopped sessions, `?` shows the rest, `q` quits.

Other ways to run it:

```sh
agent-top --once             # print the table once and exit
agent-top --json             # one snapshot as JSON, for scripts and bug reports
agent-top trace --session 662cda1f -o trace.json   # one session as a trace file for Perfetto
agent-top --prices           # the price table in use, and where each row came from
```

## What the table shows

| Column | Meaning |
|---|---|
| **STATE** | `running` = mid-turn (inference or tool execution), `idle` = alive and waiting for you, `stopped` = transcript with no live process (kept for 30 minutes) |
| **TOKENS** | input + cache read + cache write + output, from the harness's own transcript |
| **COST** | USD at list price, from [the price table](#prices). `+` or `≥` means some tokens had no known price and the number is a floor; `n/a` means none of them did |
| **CPU% / MEM** | summed over the agent's whole process tree |
| **TOOLS** | tool calls in the session |
| **PROCS / MCP** | processes in the tree, and how many of them look like Model Context Protocol servers |
| **AGE** | process age, or time since the last transcript write for stopped sessions |

A Claude Code session's subagents are folded into its row, the way Claude
Code's own cost display counts them. Web searches the model ran are counted
and, for Claude Code, priced.

## The detail pane

Press `t` to open it and `Tab` to switch between two views.

**Process tree.** Every process under the agent, labelled `agent`, `subagent`,
`mcp`, `shell` or `tool`, with the token breakdown beside it. Below it, one
line per **MCP server** the agent uses: the server's pid, how many times the
agent has called it, how many of those calls failed, when it was last called,
and its CPU and memory. The calls are counted from the transcript (Claude Code
names an MCP tool `mcp__<server>__<tool>`, Gemini `mcp_<server>_<tool>`, and
Codex records the server name directly), the process comes from the tree,
and the two are joined by name; when the join is a guess the pid carries a `?`.
A server the agent calls but that has no process, an HTTP server or one that
has exited, shows with no pid.

```
 mcp servers   calls from the transcript; pid? = process guessed
   server            pid calls err last call   cpu    rss
   filesystem       5001    17   2    1m ago  0.2%    40M
   linear              -     3   0         -     -      -
```

**Orphaned MCP processes**, servers with no live agent above them, are listed
in red, each with where it came from: "orphaned from `tuff-25` (pid 4242) 3m
ago" when agent-top watched the agent go, or how long it has been an orphan
when it was one already at startup.

The facts on the left include the cost one line per kind of token, with the
price each was charged at and what it came to, and the total names the table
it was priced from:

```
 tokens                    $/M      cost
   input         8.7k    10.00      0.09
   cache rd     31.2M     0.25      7.80
   cache wr 5m      0    12.50      0.00
   cache wr 1h   1.0M    20.00     20.30
   output        318k    50.00     15.90
   total        32.0M
 cost       $44.12   list price, built-in table
```

If another tool shows a different figure for the same session, this is where
to look: three lines will match and one will not. See
[If the cost does not match your harness](#if-the-cost-does-not-match-your-harness).

**Tool trace.** A waterfall of the session's recent activity on a shared time
axis:

```
 tool trace   5 of 71 calls · window 1m00s
   in tools 58%  model 31%  turn 3m20s…  slowest Bash 20.0s  1 in flight  1 failed
 model            4.2s  ▉▉▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 Bash             2.5s  ▏▏▏▉▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 ↳Grep           12.0s  ▏▏▏▏▉▉▉▉▉▉▉▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 Edit            300ms! ▏▏▏▏▏▏▏▏▏▏▏▏▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 Bash            20.0s… ▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▉▉▉▉▉▉▉▉▉▉▉▉▸
```

Each row is one tool call or one stretch of the model thinking (`model`).
Width is its share of the window; colour is how long it took, green under a
second through amber to red near a minute. `↳` marks a subagent's call, `…` one
still running, `!` one the harness reported as failed. **in tools** and
**model** say how much of the window went to each; what is left is usually
waiting on you. **turn** is how long the current human turn has been going.

None of this needs telemetry switched on. The spans are reconstructed from the
transcript by pairing each tool call with its result and each prompt with its
reply, reading only names, ids and timestamps.

### Exporting a trace

```sh
agent-top trace --session 662cda1f -o trace.json                       # Chrome trace format, for Perfetto
agent-top trace --session 662cda1f --format otlp -o trace.otlp.json    # OTLP, for Jaeger and OpenTelemetry
```

Both write the whole session, every tool call, inference and turn, and both
work on sessions that ended long ago, not only the ones on screen. `--session`
takes a session id, a unique prefix of one, or a path to a transcript file.
The detail pane shows the exact command for the selected row; a row with no
session id is a process agent-top found no transcript for, so there is
nothing to export.

**Chrome trace format** writes a plain file. To look at it, open
[ui.perfetto.dev](https://ui.perfetto.dev) in a browser and drop `trace.json`
onto the page, or in Chrome open `chrome://tracing` and click Load. Turns,
tool calls and model time sit on separate tracks, main agent and subagents
apart, so a turn shows as a bar with its calls beneath it.

**OTLP** is the OpenTelemetry trace request as JSON. Each tool call and
inference is parented to the turn it happened in, so a backend that draws
trees draws the right one. Trace and span ids are derived from the session and
call ids, so exporting the same session twice gives the same trace rather than
a duplicate.

### OTLP to Jaeger

[`examples/jaeger/compose.yaml`](examples/jaeger/compose.yaml) runs a local
Jaeger with the OTLP port open:

```sh
docker compose -f examples/jaeger/compose.yaml up -d
agent-top trace --session 662cda1f --format otlp --endpoint http://localhost:4318/v1/traces
open http://localhost:16686
```

`--endpoint` posts the document to the address you give and prints the
response status. It is the one network call agent-top can make, it happens
only when you type the address on the command line, and there is no default,
config key or environment variable that turns it on. Without `--endpoint`
nothing leaves the machine, and you can post the file yourself later with
`curl`.

## How a session becomes a trace

Nothing has to be switched on in the agent. The harness already writes a
transcript; agent-top reads it, live for the table and again in full for an
export.

```mermaid
flowchart LR
    H["Claude Code / Codex<br/>writes transcript.jsonl<br/>as the session runs"] --> T["agent-top<br/>tails the file<br/>once a second"]
    T --> UI["terminal table<br/>and waterfall"]
    T --> J["--json snapshot"]
    H --> X["agent-top trace<br/>reads the whole file<br/>pairs calls, results,<br/>prompts and replies"]
    X -->|"--format chrome"| P["trace.json"]
    X -->|"--format otlp"| O["trace.otlp.json"]
    P --> PF["ui.perfetto.dev<br/>chrome://tracing"]
    O --> JG["Jaeger, Tempo,<br/>any OTel collector"]
    X -.->|"--endpoint URL"| JG
```

Everything left of the files runs on your machine and reads only local files.
The files are yours to open in a browser or send on; the dotted line is the
one case where agent-top sends one itself, because you gave it the address.

## Prices

Prices are data, not code. The table shipped in the binary lives in
[`crates/agent-top-core/prices.toml`](crates/agent-top-core/prices.toml) and
carries the vendors' published list prices. `agent-top --prices` prints it.

You can override any price, or add a model that is missing, without waiting for
a release: write a file of your own at `~/.config/agent-top/prices.toml`. It is
merged over the built-in table at startup, one model at a time, so list only
what you want to change. A model in your file replaces the built-in row for
that model; every other row stays as shipped. `--prices` marks the rows that
came from your file, and the detail pane says "your price file" next to a cost
that used one.

```toml
# ~/.config/agent-top/prices.toml   (USD per million tokens)

[[model]]
prefix = "gpt-5-codex"
input = 1.25
output = 10.0
cache_read = 0.125
```

An entry whose `prefix` matches a built-in one replaces it, so a price that has
gone stale can be corrected without waiting for a release. A new prefix is
added, which is how the models this project does not ship prices for get costed
at all. Cache writes default to Anthropic's multipliers of the input price
(1.25x for the 5 minute TTL, 2x for the hour) and can be set explicitly with
`cache_write_5m` and `cache_write_1h`.

The longest matching prefix wins, so `claude-fable-5-1` beats `claude-fable-5`,
and a date-suffixed id like `claude-sonnet-4-6-20251114` resolves to its base
model. `agent-top --prices` prints the effective table with the source of every
row, which is the quickest way to find out why something is showing `n/a`. A
price file that cannot be parsed is reported on stderr and ignored; the
built-in prices still apply.

A model with no entry anywhere is never guessed at. Its tokens are counted and
reported as unpriced, and any total containing them is shown as a floor.

Web searches are billed per search, on top of tokens, and the rate is in the
same file under `[server_tools]`. Codex and Gemini web searches are counted but
not priced, because OpenAI's and Google's rates are not in the table.

Gemini models are priced at Google's paid-tier rate for prompts under 200k
tokens, with thinking tokens counted as output the way Google bills them. A
Gemini CLI signed in with a Google account rather than an API key is on a free
quota, so its cost here is what the same session would have cost at list price,
not a bill.

## Supported harnesses

| Harness | Discovery | Tokens and cost | State |
|---|---|---|---|
| Claude Code | process table + `~/.claude/sessions/<pid>.json` (exact) | transcript usage, priced per model, subagent transcripts folded into their parent | harness-reported |
| Codex CLI / app-server | process table + the rollout files the process holds open (exact on macOS and Linux; `cwd` heuristic elsewhere) | transcript usage; priced once you add the model to [your price table](#prices) | transcript events |
| Gemini CLI | process table + `cwd` heuristic (the CLI keeps no registry and does not hold its transcript open) | transcript usage, priced per model, subagent transcripts folded into their parent | transcript events |
| OpenCode | process table + `cwd` heuristic; reads its SQLite session store read-only | tokens and OpenCode's own computed cost, subagent sessions folded into their parent | transcript times |
| Aider, Copilot CLI, cursor-agent | process table only | not yet | CPU heuristic |

## Install

| | | |
|---|---|---|
| **Homebrew** | `brew install kannandreams/tap/agent-top` | macOS and Linux, prebuilt; installs shell completions |
| **Cargo, prebuilt** | `cargo binstall agent-top` | downloads the release binary, no compiler needed |
| **Cargo, from source** | `cargo install --locked agent-top` | builds from crates.io; needs Rust 1.85 or newer |
| **By hand** | [the releases page](https://github.com/kannandreams/agent-top/releases) | tarballs and `sha256` for macOS and Linux, x86\_64 and arm64 |

Every route ends at the same single binary: no Python, no Node, no daemon. To
upgrade, `brew upgrade agent-top` or re-run the `cargo install` command.
Without Homebrew, `agent-top --completions zsh` (or `bash`, `fish`) prints a
completion script to source from your shell's startup file.

## All the flags

```sh
agent-top                        # interactive, refreshes every second
agent-top --interval-ms 500      # faster refresh
agent-top --stopped-window-min 120   # keep stopped sessions visible for two hours
agent-top --replay snap.json     # render a saved --json snapshot, keys and all, reading nothing local
agent-top trace --session <id|prefix|path> [--format chrome|otlp] [-o FILE] [--endpoint URL]
```

`--replay` is for bug reports: attach a `--json` snapshot and the reader can
inspect it exactly as you saw it.

## Why this exists

The failure that motivated the tool is not hypothetical. Codex has a run of
reports about leaked MCP process trees, three of them still open:

| Report | State | What it describes |
|---|---|---|
| [#12491](https://github.com/openai/codex/issues/12491) | open | MCP children not reaped after a task completes: 1300+ zombies, 37 GB leaked |
| [#17574](https://github.com/openai/codex/issues/17574) | open | Subagents leak stdio MCP helper trees, which accumulate indefinitely |
| [#25015](https://github.com/openai/codex/issues/25015) | open | The app-server leaks a process stack per subagent, so memory grows linearly |
| [#16256](https://github.com/openai/codex/issues/16256) | closed | MCP subagent processes never terminated when a session is stopped or suspended |
| [#19753](https://github.com/openai/codex/pull/19753) | merged Apr 2026 | The fix for one of those paths: terminate stdio MCP servers on shutdown |

Nothing about this is specific to Codex. Every harness that spawns helper
processes has the same shape of bug available to it, which is why `agent-top`
looks for the symptom rather than for one vendor's bug.

## Where the numbers come from

The whole point of this tool is that its numbers are right, so it is explicit
about which ones are exact and which are inferred.

- **Tokens are counted, never estimated.** They come from the usage records the
  harness writes itself, deduplicated per API message so a response split across
  several transcript lines is counted once.
- **Costs come from a table you can read and change.** `agent-top --prices`
  shows it. A model with no price is reported as unpriced rather than guessed
  at, which is why a total containing one is shown as a floor (`≥`, `+`) instead
  of a number that looks more precise than it is.
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

### If the cost does not match your harness

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

[docs/architecture.md](docs/architecture.md) has the mechanism underneath: the
process walk, the incremental transcript tail, and how a snapshot is assembled
on each tick.

## Roadmap

See [docs/roadmap.md](docs/roadmap.md). Next: an Aider adapter, and MCP counts for OpenCode. [docs/releasing.md](docs/releasing.md) is the release runbook.

## Development

```sh
cargo test
cargo run -- --once
cargo clippy --all-targets
```

Two crates, split by dependency rather than by size: `crates/agent-top-core`
is discovery, transcript parsing, pricing and the process model, with no
terminal dependency, so all of it is testable without a TTY and it is exactly
what `--json` prints; `crates/agent-top` is the ratatui front end and the CLI.
Both are published, because a crate on crates.io cannot depend on an
unpublished one: `agent-top-core` exists on the registry so that `agent-top`
can. The internal engineering handbook (PRD, RFCs, ADRs, decisions) lives in the sibling `agent-top-internal-docs` repository.

## License

MIT
