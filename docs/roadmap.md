# Roadmap

Dates are targets, not promises. Each phase ships on its own.

## v0.1 — the table (now)

- Agents, state, tokens, cost, CPU, memory, tool calls, process count, MCP count, age.
- Claude Code exact attribution; Codex heuristic attribution.
- Process tree and orphaned MCP detection.
- Tool trace: per-call spans from both harnesses, as a waterfall in the detail pane and as JSON.
- Prices as data: a `prices.toml` compiled into the binary, with `~/.config/agent-top/prices.toml` merged over it, so any model can be priced and a stale price corrected without a release.
- `--once`, `--json`.
- Prebuilt macOS and Linux binaries, a Homebrew tap and `cargo binstall` support, cut by tag (see [releasing.md](releasing.md)).

## v0.2 — trust the numbers (Q4 2026)

- Exact Codex attribution (per-thread rows for the app-server, matched through the rollout's `originator` and pid where Codex exposes it).
- Gemini CLI and OpenCode transcript adapters.
- Per-agent history sparkline (tokens per minute) and cost rate ($/hour).
- Linux `/proc` verification against a real Linux desktop, not only CI.
- Golden transcript fixtures: small real transcripts per harness version, checked in with the exact numbers they should produce. These catch regressions in our own parsing. They cannot catch an upstream format change, because a fixture recorded at version X keeps passing forever after the harness moves to version Y.
- Drift detection, which is the half that does catch it. Every field read from a transcript falls back to zero when it is missing, so a renamed field shows a user 0 tokens and $0.00 rather than an error. A session with assistant messages, a usage object, and no recognised fields in it is not a quiet session, it is a broken parser, and the UI should say so by name: "usage fields not recognised for claude 2.4.x". Probably an `agent-top doctor` that reports the same thing against the newest local transcript.

## v0.3 — the tree (Q1 2027)

- Logical subagent tree from transcripts (Claude `isSidechain` and `agent-*.jsonl`, Codex `close_agent` events), merged with the process tree.
- Per-MCP-server rows: which agent started it, how many tool calls went through it, idle time.
- Orphan lifecycle: first-seen time, parent-of-record, one-key copy of the `kill` command. Still no signalling from agent-top.
- Trace export: `agent-top trace --session <id> --format otlp|chrome`, written to a file so it can be opened in Perfetto or fed to Jaeger. Retroactive and harness-neutral, unlike a harness's own telemetry, which has to be enabled before the session starts. Any direct push to a collector stays opt-in behind an explicit `--endpoint`, because a network call contradicts the default posture in [vision.md](vision.md).
- Spans beyond tool calls: inference spans from `requestId` and turn boundaries, so the gaps in the waterfall are labelled rather than blank.

## v0.4 — signals (Q2 2027)

- Optional `agent-top hook` subcommand that harnesses with hook support can call on session start/stop to register pid, session id and transcript path exactly.
- Rate-limit view for harnesses that log it (Codex `rate_limits` is already in the transcript).
- Configurable thresholds and a non-TUI `agent-top watch --alert` for orphan or cost spikes.

## Out of scope, deliberately

- Killing or restarting agents from inside agent-top.
- Any network call.
- Reading transcript content (prompts, code). agent-top reads metadata fields only.
