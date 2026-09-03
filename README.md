# agent-top

**htop for local coding agents.**

[![CI](https://github.com/kannandreams/agent-top/actions/workflows/ci.yml/badge.svg)](https://github.com/kannandreams/agent-top/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 2024](https://img.shields.io/badge/rust-edition%202024-orange.svg)](Cargo.toml)

You have three Claude Code sessions, a Codex thread in VS Code, and a Gemini CLI you forgot about. Which one is burning tokens right now? Which one is waiting on you? Which MCP server is still alive after the agent that started it died? `agent-top` answers that in one terminal view, the way `htop` answers it for processes and `btop` answers it for the whole machine.

```
┌ agent-top @ atlas-mbp ──────────────────────────────────────────────────────────┐
│ cpu  23.4% (12 cores) ▉▉▉▉▉▏▏▏▏▏▏▏▏▏▏   agents 4   2 running  1 idle  1 stopped │
│ mem  61.0% 19.5G/32.0G ▉▉▉▉▉▉▉▉▉▉▏▏▏▏   tokens 68k   cost $2.10                 │
│ tokens/s ▁▂▃▅▂▁▁▃▇▂▁                    procs 9   mcp 2   orphaned mcp 1        │
├ agents (4) ─────────────────────────────────────────────────────────────────────┤
│  AGENT         HARNESS STATE    PID    MODEL       TOKENS  COST   CPU%  MEM  MCP AGE│
│▶ tuff-25       claude  running  6980   fable-5-1   41k     $1.42  6.6   452M 1   18m│
│  reviewer      claude  running  7161   sonnet-5    8k      $0.21  1.0   409M 0    3m│
│  codex:secchi  codex   idle     57429  gpt-5-codex 13k     n/a    0.7    29M 1    7m│
│  claude:glyf   claude  stopped  -      opus-5      6k      $0.12  -     -    -    2m│
├ tuff-25 · claude · running  [tree] ─────────────────────────────────────────────┤
│ session   a29e19c3-…   process tree   4 procs · 1 mcp · cpu 7.3% · rss 490M     │
│ model     claude-fable-5-1   [agent]  6980  6.6% 452M 18m  claude --resume a29e…│
│ tokens    input 2 · cache rd 22k · ├─ [shell] 57276 0.0% 2.6M 19s /bin/zsh -c … │
│           cache wr 9.9k · out 250  │  └─ [tool] 57278 0.7% 37M 19s python3 -    │
│ cost      $1.42                    └─ [mcp]   58102 0.1% 61M 18m npx -y @model…│
└─────────────────────────────────────────────────────────────────────────────────┘
 ↑↓/jk select  s sort  r reverse  t detail  Tab trace  x hide stopped  p pause  ? help  q quit
```

## What it shows

| Column | Meaning |
|---|---|
| **STATE** | `running` = mid-turn (inference or tool execution), `idle` = alive and waiting for you, `stopped` = transcript with no live process (kept for 30 minutes) |
| **TOKENS** | input + cache read + cache write + output, from the harness's own transcript |
| **COST** | USD at list price. `+` or `≥` means some tokens had no known price and the number is a floor |
| **CPU% / MEM** | summed over the agent's whole process tree |
| **TOOLS** | tool calls in the session |
| **PROCS / MCP** | processes in the tree, and how many of them look like Model Context Protocol servers |
| **AGE** | process age, or time since the last transcript write for stopped sessions |

The detail pane shows the process tree (`agent`, `subagent`, `mcp`, `shell`, `tool`) and the token breakdown. **Orphaned MCP processes**, servers with no live agent above them, are listed in red. That is the failure mode reported repeatedly against Codex (openai/codex #17574, #25015, #12491, #16256) and it is not specific to Codex.

## Supported harnesses

| Harness | Discovery | Tokens and cost | State |
|---|---|---|---|
| Claude Code | process table + `~/.claude/sessions/<pid>.json` (exact) | transcript usage, priced per model | harness-reported |
| Codex CLI / app-server | process table + rollout `cwd` match (heuristic) | transcript usage; unpriced until a price table exists | transcript events |
| Gemini CLI, OpenCode, Aider, Copilot CLI, cursor-agent | process table only | not yet | CPU heuristic |

## Install

```sh
brew install kannandreams/tap/agent-top
```

Or, without Homebrew:

```sh
cargo binstall agent-top          # prebuilt binary, no compiler needed
cargo install agent-top           # build from crates.io
cargo install --path crates/agent-top   # build from a clone
```

Prebuilt binaries for macOS and Linux (x86_64 and arm64) are attached to every
[release](https://github.com/kannandreams/agent-top/releases) with checksums.
Building from source needs Rust 1.85+ (edition 2024). Either way: one static
binary, no Python, no Node, no daemon.

## Usage

```sh
agent-top                    # interactive, refreshes every second
agent-top --once             # print the table once and exit
agent-top --json             # one snapshot as JSON, for scripts and bug reports
agent-top --interval-ms 500  # faster refresh
agent-top --stopped-window-min 120
```

Keys: `j`/`k` move, `s` cycle sort, `r` reverse, `t` toggle the detail pane,
`Tab` switch that pane between the process tree and the tool trace, `x` hide
stopped sessions, `p` pause, `?` help, `q` quit.

## Tool trace

`Tab` turns the detail pane into a waterfall of the selected agent's recent
tool calls, on a shared time axis:

```
 tool trace   5 of 71 calls · window 1m00s
   in tools 58%  slowest Bash 20.0s  1 in flight  1 failed
 Bash             2.5s  ▉▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 Read             40ms  ▏▏▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 ↳Grep           12.0s  ▏▏▉▉▉▉▉▉▉▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 Edit            300ms! ▏▏▏▏▏▏▏▏▏▏▏▏▉▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏
 Bash            20.0s… ▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▏▉▉▉▉▉▉▉▉▉▉▉▉▸
```

Width is the call's share of the window; **colour is how long it took**, on a
log scale from green under a second, through amber, to red approaching a
minute. Those are two channels on purpose: at a typical zoom most calls are one
cell wide, so width alone would say nothing about a 40 ms read next to a 30 s
test run. `↳` and blue mark a subagent's call, `…` and amber a call still
running, `!` and red one the harness reported as failed.

**in tools** is the share of the window covered by at least one call
(overlapping calls merged, not summed) — the rest is the model thinking, which
is usually the answer to "why has this agent been busy for eight minutes".

No configuration and no telemetry opt-in: the spans are reconstructed from the
transcript the harness already writes, by pairing each call with its result
(Claude's `tool_use` / `tool_result` on `tool_use_id`, Codex's `function_call` /
`function_call_output` on `call_id`) and reading the timestamps that bracket
them. Only the call's name, id and timing are read, never its arguments or
output. The spans are in `--json` as well, so they can be fed to a real tracing
tool.

## How it works

1. Enumerate processes with `sysinfo`. Anything whose program is `claude`, `codex`, `gemini`, `opencode`, `aider`, `copilot` or `cursor-agent` (or a Node script under the corresponding npm package) is an agent root. Harness processes nested under a root are subagents of that root.
2. Attribute a transcript to each root. Claude Code writes `~/.claude/sessions/<pid>.json` with the session id, cwd, a derived name and a busy/idle status, so attribution is exact. Codex is matched by working directory and start time.
3. Tail the transcript incrementally (byte offset kept between refreshes) to accumulate usage, tool calls, turns and the last event, which decides `running` vs `idle`.
4. Price each message by its model from a static table. Unknown models are counted as unpriced tokens rather than guessed.
5. Any MCP-looking process with no agent ancestor is an orphan.

Everything is read-only. `agent-top` never signals, writes to, or talks to an agent.

## Roadmap

See [docs/roadmap.md](docs/roadmap.md). Short version: exact Codex attribution, a logical subagent tree from transcripts, trace export to OTLP, user-supplied price tables, and a `hook` subcommand for harnesses that support it. [docs/releasing.md](docs/releasing.md) is the release runbook.

## Development

```sh
cargo test
cargo run -- --once
cargo clippy --all-targets
```

`crates/agent-top-core` is discovery and accounting with no terminal dependency; `crates/agent-top` is the ratatui front end. The internal engineering handbook (PRD, RFCs, ADRs, decisions) lives in the sibling `agent-top-internal-docs` repository.

## License

MIT
