# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.18.0] - 2026-09-13

### Added
- OpenCode: MCP call counts per server, matched against the server names in OpenCode's config.
- OpenCode: context by source in the detail pane, `--once` and `--json`.
- Prices for `deepseek-v4-pro` and `deepseek-flash` (also `deepseek-v4-flash`), at DeepSeek's peak list rate.

## [0.17.0] - 2026-09-13

### Added
- `Enter` expands a popup to fill the terminal, with scrolling; `Esc` returns to the table.
- `agent-top slow`, `fails`, `advice` and `mcp` start directly on one panel.
- `o` opens the current panel in a split pane under tmux, zellij, WezTerm or kitty.
- `m` opens the MCP servers panel: every server under every agent, plus orphaned MCP processes and where they came from.

### Changed
- The help popup lists the panels, and the footer shows `m mcp`.

## [0.16.0] - 2026-09-11

### Added
- A documentation site with a guide to every panel, the cost report, trace export, replay and prices, plus a blog.

### Fixed
- `q` on the update popup quits agent-top; before, it declined the update.
- The detail pane's `web search` label no longer runs into its value.

## [0.15.2] - 2026-09-07

### Changed
- Shorter wording on the update popup.

## [0.15.1] - 2026-09-07

### Fixed
- Timestamps with a numeric zone offset (`+01:00`, `+0100`) now parse.

## [0.15.0] - 2026-09-07

### Added
- When a newer version exists, a popup offers `u` to upgrade with the installer that installed agent-top, then restarts it. `n` or `Esc` skips that version.
- `a` opens the advice panel: expensive context sources, idle MCP servers and MCP servers whose memory keeps growing. Also in `--once` and in `--json` as `advice`.

## [0.14.0] - 2026-09-07

### Added
- Context by source: the detail pane shows which tools and MCP servers add the most tokens to the prompt, and what re-reading them has cost. Claude Code, Codex and Gemini CLI.
- `CONTEXT BY SOURCE` in `--once`, and `context` in `--json`.

## [0.13.0] - 2026-09-05

### Added
- Burn rate: the header shows spend per hour across all agents, averaged over the last minute.
- A daily update check against crates.io. The footer version badge turns amber when a newer version exists. `AGENT_TOP_NO_UPDATE_CHECK=1` turns the check off.

### Changed
- The version badge moved from the header to the footer.
- The help popup is sized to its content.

## [0.12.1] - 2026-09-05

### Changed
- Footer keys are grouped, with `q quit` at the right. `x` appears only when there are stopped sessions to hide.

## [0.12.0] - 2026-09-05

### Added
- `agent-top --whats-new` prints the changelog shipped with the binary.
- The header, help popup and `--help` show the running version and the upgrade command.

## [0.11.0] - 2026-09-05

### Added
- `l` opens the slowest tools panel and `f` opens the failed tool calls panel, both across all agents.

## [0.10.2] - 2026-09-05

### Changed
- The detail pane lists cost, cache, tokens, turns and tool calls before the cost breakdown.
- With the detail pane open, the agents table takes only the height its rows need.

## [0.10.1] - 2026-09-05

### Changed
- No change to the binary; the README gains a key features section.

## [0.10.0] - 2026-09-05

### Added
- Cache efficiency: the detail pane shows the share of the prompt served from cache, and `agent-top report` has a `CACHE` column.

## [0.9.2] - 2026-09-05

### Changed
- No change to the binary; the README introduction is rewritten.

## [0.9.1] - 2026-09-05

### Added
- OpenAI prices for the GPT-5 family, so Codex sessions show a cost.

### Changed
- The README is reorganised, with background moved to [docs/why-this-exists.md](docs/why-this-exists.md) and [docs/accounting.md](docs/accounting.md).

## [0.9.0] - 2026-09-05

### Added
- `agent-top report` totals cost and tokens from transcripts on disk, grouped with `--by harness|model|project|day` over a `--since` window. Supports `--json`.

## [0.8.0] - 2026-09-05

### Added
- Codex rate limits (5-hour and weekly windows) in the detail pane and in `--json` as `rate_limit`.
- `--once` lists agents at 75% or more of a rate limit under `RATE LIMITS`.

## [0.7.1] - 2026-09-05

### Added
- OpenCode: turn and inference spans in the tool trace.

## [0.7.0] - 2026-09-05

### Added
- OpenCode support: tokens, cost, model, tool calls, subagents and tool trace, read from OpenCode's database without writing to it.

## [0.6.0] - 2026-09-05

### Added
- Codex and Gemini CLI: MCP call counts per server.

## [0.5.0] - 2026-09-05

### Added
- Claude Code: the detail pane has one row per MCP server with pid, calls, errors, last call, CPU and memory. Also `MCP SERVERS` in `--once` and `mcp_servers` in `--json`.
- Orphaned MCP processes name the agent they were orphaned from, and how long ago. In `--json` as `orphan_origins`.
- An `npx` or `uvx` wrapper and the server it starts count as one MCP server.

### Fixed
- MCP servers were labelled `tool`, and `--resume` and working-directory attribution never matched, because process command lines were never read.
- `--json` snapshots from before 0.2.0 replay again.

## [0.4.0] - 2026-09-05

### Added
- Gemini CLI support: tokens, cost, turns, tool calls, web searches, tool trace and trace export.
- Prices for Gemini 2.5, 3, 3.1, 3.5, 3.6, 3.7 and 3.8 models.

### Changed
- `agent-top-core`: each harness is a `HarnessAdapter`, and `harness::open_transcript` returns `None` for a harness without one.

## [0.3.5] - 2026-09-04

### Added
- The detail pane breaks cost down by token type with the rate applied, and names the price table used. In `--json` as `cost_breakdown` and `price_source`.

## [0.3.4] - 2026-09-04

### Fixed
- Codex sessions started after the first day of a month were not found.
- Two Codex app-servers running at once no longer swap conversations.
- The header sparkline plots output tokens per second, labelled `out tok/s`.

## [0.3.3] - 2026-09-04

### Added
- `agent-top trace --endpoint URL` posts an OTLP trace to a collector.
- A local Jaeger setup in `examples/jaeger/`.
- The detail pane shows the `agent-top trace` command for the selected session.

## [0.3.2] - 2026-09-04

### Added
- `agent-top trace --format otlp` writes OpenTelemetry JSON for Jaeger, Tempo or any OTLP collector.

## [0.3.1] - 2026-09-04

### Added
- Claude Code web searches are counted and priced; Codex web searches are counted.
- Turn and inference spans in the waterfall, and `kind` on each span in `--json`.
- Chrome trace export nests tool calls and inferences under turns, with separate tracks for subagents.

### Fixed
- An interrupted turn ends at its last line, not at the next prompt.

## [0.3.0] - 2026-09-04

### Added
- `agent-top trace` exports a session's tool calls as a Chrome trace for Perfetto or `chrome://tracing`.
- `agent-top-core`: `SpanLog::unbounded`, `SpanRetention`, `SessionTracker::refresh_all`, `harness::detect` and `harness::open_transcript`.

### Fixed
- Claude Code subagent usage (2.1.233 and later) is counted in the parent row.

## [0.2.1] - 2026-09-04

### Fixed
- No change to the binary; the release workflow runs the full test suite before publishing.

## [0.2.0] - 2026-09-03

### Added
- Drift detection: a session that shows model responses but no tokens reads `?` and names the harness version, instead of `$0.00`.
- Shell completions with `--completions <shell>`.

### Changed
- One row per Codex conversation instead of one per process.

## [0.1.6] - 2026-09-03

### Changed
- No change to the binary; README corrections on pricing and Codex.

## [0.1.5] - 2026-09-03

### Added
- Prices live in `prices.toml`. Override or add models in `~/.config/agent-top/prices.toml` or `$AGENT_TOP_PRICES`.
- `--prices` prints the effective price table.

### Fixed
- A price file that fails to parse is reported on stderr.

## [0.1.4] - 2026-09-03

### Changed
- `agent-top-core`: `SpanLog::iter` is double-ended.

## [0.1.3] - 2026-09-03

### Changed
- No change to the binary; `agent-top-core` has its own crates.io README, and the install section lists every route.

## [0.1.2] - 2026-09-03

### Added
- Published on crates.io: `cargo install agent-top` and `cargo binstall agent-top`.

### Fixed
- Both crates ship with a README.

## [0.1.1] - 2026-09-03

### Added
- `--replay <file>` opens a `--json` snapshot in the interactive UI.

### Changed
- Rounded, lighter panel borders.
- Header totals are aligned in columns, and the sort direction moved to the footer.

## [0.1.0] - 2026-09-03

### Added
- Interactive TUI with host CPU and memory, a tokens-per-second sparkline, a sortable agent table and a detail pane.
- Discovers Claude Code, Codex, Gemini CLI, OpenCode, Aider, Copilot CLI and cursor-agent processes, with nested agents shown as subagents.
- Claude Code and Codex: tokens, cost, turns and tool calls from transcripts.
- Tool trace: a waterfall of recent tool calls, toggled with `Tab`.
- Orphaned MCP process detection.
- `--once` and `--json` output.
- Prebuilt binaries for macOS and Linux, and a Homebrew tap.
