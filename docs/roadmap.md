# Roadmap

Dates are targets, not promises. Each phase ships on its own.

## v0.1 — the table (now)

- Agents, state, tokens, cost, CPU, memory, tool calls, process count, MCP count, age.
- Claude Code exact attribution; Codex heuristic attribution.
- Process tree and orphaned MCP detection.
- Tool trace: per-call spans from both harnesses, as a waterfall in the detail pane and as JSON.
- Prices as data: a `prices.toml` compiled into the binary, with `~/.config/agent-top/prices.toml` merged over it, so any model can be priced and a stale price corrected without a release.
- Drift detection: a session that did work accounting for no tokens is a parser that has fallen behind the format, not a quiet session, and the row says so by name instead of showing a believable `$0.00`. Partial renames, where some fields still read and the total is merely too low, are still not detected.
- One row per Codex conversation rather than per process, so an app-server hosting several threads no longer collapses them into one mis-attributed row. Resources stay counted once, on the row that owns the process.
- Shell completions for bash, zsh and fish, generated from the binary and installed by Homebrew.
- `--once`, `--json`.
- Trace export: `agent-top trace --session <id> --format chrome`, every tool call in a session as a file Perfetto opens. Reads the whole transcript rather than the live tracker's bounded window, so it is retroactive and harness-neutral.
- Turn and inference spans, so the gaps in the waterfall are labelled and the export nests tool calls under the turn that issued them.
- Server-side web searches priced per search, the one per-call charge Anthropic adds on top of tokens.
- `--format otlp` for the trace export, with deterministic trace and span ids and tool calls parented to their turn. Any direct push to a collector stays opt-in behind an explicit `--endpoint`, because a network call contradicts the default posture in [vision.md](vision.md), and none is built.
- Prebuilt macOS and Linux binaries, a Homebrew tap and `cargo binstall` support, cut by tag (see [releasing.md](releasing.md)).

## v0.2 — trust the numbers (Q4 2026)

- Gemini CLI and OpenCode transcript adapters.
- Per-agent history sparkline (tokens per minute) and cost rate ($/hour).
- Linux `/proc` verification against a real Linux desktop, not only CI.
- Golden transcript fixtures: small real transcripts per harness version, checked in with the exact numbers they should produce. These catch regressions in our own parsing. They cannot catch an upstream format change, because a fixture recorded at version X keeps passing forever after the harness moves to version Y.

## v0.3 — the tree (Q1 2027)

- Logical subagent tree from transcripts (Claude `isSidechain` and `agent-*.jsonl`, Codex `close_agent` events), merged with the process tree.
- Per-MCP-server rows: which agent started it, how many tool calls went through it, idle time.
- Orphan lifecycle: first-seen time, parent-of-record, one-key copy of the `kill` command. Still no signalling from agent-top.

## v0.4 — signals (Q2 2027)

- Optional `agent-top hook` subcommand that harnesses with hook support can call on session start/stop to register pid, session id and transcript path exactly.
- Rate-limit view for harnesses that log it (Codex `rate_limits` is already in the transcript).
- Configurable thresholds and a non-TUI `agent-top watch --alert` for orphan or cost spikes.
- Detecting a partial format change, where a rename leaves some fields readable and the totals merely wrong. Needs a notion of which fields ought to be present that does not cry wolf every time a harness adds one.

## Out of scope, deliberately

- Killing or restarting agents from inside agent-top.
- Any network call.
- Reading transcript content (prompts, code). agent-top reads metadata fields only.
