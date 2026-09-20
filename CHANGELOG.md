# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Highlights
Kodelet sessions now appear in the table alongside Claude Code, Codex, Gemini CLI and OpenCode. agent-top reads Kodelet's SQLite store read-only and uses the costs Kodelet recorded itself, and child sessions nest under the parent that started them.

### Added
- Kodelet: sessions in the table, the report and the trace, with tokens and cost from Kodelet's own records and child sessions nested under their parent. ([#47](https://github.com/kannandreams/agent-top/pull/47))
- `TOOLS` shows `≥N` where a harness has compacted its history and the count is a floor; `--json` gains `tool_calls_lower_bound`. ([#47](https://github.com/kannandreams/agent-top/pull/47))
- `--json` gains `cache_write_unsplit` on `usage` and `cost_breakdown`, for cache writes a harness records without a TTL. ([#47](https://github.com/kannandreams/agent-top/pull/47))

### Fixed
- OpenCode and Kodelet rows no longer name agent-top's price table as the source of a cost the harness recorded itself. ([#47](https://github.com/kannandreams/agent-top/pull/47))
- `trace -o` no longer prints the session id in its stderr summary. ([#46](https://github.com/kannandreams/agent-top/pull/46))

## [0.19.1] - 2026-09-19

### Added
- `--theme auto|dark|light` picks Catppuccin Mocha or Latte without asking the terminal; `AGENT_TOP_THEME` sets it for every run. ([#45](https://github.com/kannandreams/agent-top/pull/45))

## [0.19.0] - 2026-09-18

### Highlights
Codex subagents now appear as what they are: sessions inside their parent's process. Each one nests under its parent in the table with its nickname and role, keeps its own tokens and cost, and shares one process tree, so memory is counted once.

Codex code mode is opened up. A session that used to show 28 rows of `exec` now shows the commands and patches that ran inside them, in the trace and in context by source.

### Added
- Codex: subagent sessions nest under their parent in the table and in `--once`, named by nickname and role. `--json` gains `subagent` (`parent_session_id`, `nickname`, `role`) per agent. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- Codex: tools run inside a code-mode `exec` call, such as `exec_command` and `apply_patch`, appear in the trace, the tool-call count and context by source. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- Catppuccin Mocha and Latte themes, chosen from the terminal's background colour. ([#43](https://github.com/kannandreams/agent-top/pull/43))

### Changed
- Codex: a code-mode call's context share is divided between its nested tools by the length of each tool's output. Only the length is taken; no output text is kept. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- A nested agent process is labelled `agent` in the process tree and in `--json`; the `subagent` process kind is gone. Older snapshots still load. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- `agent-top-core`: `ProcKind::Subagent` is removed, `HarnessAdapter::prepare` takes the process map, and `Agent` and `SessionSummary` gain `subagent`. ([#44](https://github.com/kannandreams/agent-top/pull/44))

### Fixed
- Linux: worker threads were counted as child processes, which multiplied an agent's memory and process count and showed threads as subagents. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- Codex: on recent versions, context by source sized each tool result from the wrong response and charged typed messages to tools. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- Codex installed through npm is matched to its transcript by the open rollout file instead of by working directory. ([#44](https://github.com/kannandreams/agent-top/pull/44))
- Codex helper processes such as `codex mcp` and the sandbox are no longer listed as agents. ([#44](https://github.com/kannandreams/agent-top/pull/44))

## [0.18.1] - 2026-09-16

### Security
- Bump rustls to 0.23.45 for [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285). ([#42](https://github.com/kannandreams/agent-top/pull/42))

## [0.18.0] - 2026-09-13

### Added
- OpenCode: MCP call counts per server, matched against the server names in OpenCode's config. ([#36](https://github.com/kannandreams/agent-top/pull/36))
- OpenCode: context by source in the detail pane, `--once` and `--json`. ([#37](https://github.com/kannandreams/agent-top/pull/37))
- Prices for `deepseek-v4-pro` and `deepseek-flash` (also `deepseek-v4-flash`), at DeepSeek's peak list rate. ([#38](https://github.com/kannandreams/agent-top/pull/38))

## [0.17.0] - 2026-09-13

### Highlights
Every panel now comes in four sizes: a popup over the table, the full terminal with `Enter`, its own command such as `agent-top mcp`, or a split pane beside the table with `o` under tmux, zellij, WezTerm or kitty. A panel you want to keep watching no longer has to share the screen with the table.

The new `m` panel lists every MCP server on the machine in one place, with the orphaned MCP processes and the agent each one came from.

### Added
- `Enter` expands a popup to fill the terminal, with scrolling; `Esc` returns to the table. ([#34](https://github.com/kannandreams/agent-top/pull/34))
- `agent-top slow`, `fails`, `advice` and `mcp` start directly on one panel. ([#34](https://github.com/kannandreams/agent-top/pull/34))
- `o` opens the current panel in a split pane under tmux, zellij, WezTerm or kitty. ([#34](https://github.com/kannandreams/agent-top/pull/34))
- `m` opens the MCP servers panel: every server under every agent, plus orphaned MCP processes and where they came from. ([#34](https://github.com/kannandreams/agent-top/pull/34))

### Changed
- The help popup lists the panels, and the footer shows `m mcp`. ([#34](https://github.com/kannandreams/agent-top/pull/34))

## [0.16.0] - 2026-09-11

### Added
- A documentation site with a guide to every panel, the cost report, trace export, replay and prices, plus a blog. ([#8](https://github.com/kannandreams/agent-top/pull/8))

### Fixed
- `q` on the update popup quits agent-top; before, it declined the update. ([32fd1a6](https://github.com/kannandreams/agent-top/commit/32fd1a6))
- The detail pane's `web search` label no longer runs into its value. ([#8](https://github.com/kannandreams/agent-top/pull/8))

## [0.15.2] - 2026-09-07

### Changed
- Shorter wording on the update popup. ([#7](https://github.com/kannandreams/agent-top/pull/7))

## [0.15.1] - 2026-09-07

### Fixed
- Timestamps with a numeric zone offset (`+01:00`, `+0100`) now parse. ([#6](https://github.com/kannandreams/agent-top/pull/6))

## [0.15.0] - 2026-09-07

### Highlights
Advice reads the numbers for you. Press `a` to see what looks wasteful right now: a tool whose large results every later response pays to re-read, an MCP server attached but unused for ten minutes, or one whose memory keeps growing with no calls. Each line gives the numbers behind it; agent-top points and changes nothing.

agent-top can now upgrade itself. When a newer version exists, a popup shows the exact command and runs it on `u`, using the installer that installed agent-top.

### Added
- `a` opens the advice panel: expensive context sources, idle MCP servers and MCP servers whose memory keeps growing. Also in `--once` and in `--json` as `advice`. ([#4](https://github.com/kannandreams/agent-top/pull/4))
- When a newer version exists, a popup offers `u` to upgrade and restart. `n` or `Esc` skips that version. ([#5](https://github.com/kannandreams/agent-top/pull/5))

## [0.14.0] - 2026-09-07

### Highlights
Context by source shows which tools and MCP servers are filling the prompt, and what re-reading them has cost. Every response re-reads the whole conversation, so one large file read or a chatty MCP server is paid for again on every later response until compaction. No harness shows this.

### Added
- Context by source in the detail pane, for Claude Code, Codex and Gemini CLI. ([#3](https://github.com/kannandreams/agent-top/pull/3))
- `CONTEXT BY SOURCE` in `--once`, and `context` in `--json`. ([#3](https://github.com/kannandreams/agent-top/pull/3))

## [0.13.0] - 2026-09-05

### Added
- Burn rate: the header shows spend per hour across all agents, averaged over the last minute. ([e9e74e5](https://github.com/kannandreams/agent-top/commit/e9e74e5))
- A daily update check against crates.io. The footer version badge turns amber when a newer version exists. `AGENT_TOP_NO_UPDATE_CHECK=1` turns the check off. ([eafab41](https://github.com/kannandreams/agent-top/commit/eafab41))

### Changed
- The version badge moved from the header to the footer. ([a2035e5](https://github.com/kannandreams/agent-top/commit/a2035e5))
- The help popup is sized to its content. ([eafab41](https://github.com/kannandreams/agent-top/commit/eafab41))

## [0.12.1] - 2026-09-05

### Changed
- Footer keys are grouped, with `q quit` at the right. `x` appears only when there are stopped sessions to hide. ([f4688f6](https://github.com/kannandreams/agent-top/commit/f4688f6))

## [0.12.0] - 2026-09-05

### Added
- `agent-top --whats-new` prints the changelog shipped with the binary. ([d11ab19](https://github.com/kannandreams/agent-top/commit/d11ab19))
- The header, help popup and `--help` show the running version and the upgrade command. ([d11ab19](https://github.com/kannandreams/agent-top/commit/d11ab19))

## [0.11.0] - 2026-09-05

### Added
- `l` opens the slowest tools panel and `f` opens the failed tool calls panel, both across all agents. ([bd2c199](https://github.com/kannandreams/agent-top/commit/bd2c199))

## [0.10.2] - 2026-09-05

### Changed
- The detail pane lists cost, cache, tokens, turns and tool calls before the cost breakdown. ([0ae8767](https://github.com/kannandreams/agent-top/commit/0ae8767))
- With the detail pane open, the agents table takes only the height its rows need. ([67b886e](https://github.com/kannandreams/agent-top/commit/67b886e))

## [0.10.1] - 2026-09-05

### Changed
- No change to the binary; the README gains a key features section. ([19c3730](https://github.com/kannandreams/agent-top/commit/19c3730))

## [0.10.0] - 2026-09-05

### Added
- Cache efficiency: the detail pane shows the share of the prompt served from cache, and `agent-top report` has a `CACHE` column. ([16a08ef](https://github.com/kannandreams/agent-top/commit/16a08ef))

## [0.9.2] - 2026-09-05

### Changed
- No change to the binary; the README introduction is rewritten. ([e910a45](https://github.com/kannandreams/agent-top/commit/e910a45))

## [0.9.1] - 2026-09-05

### Added
- OpenAI prices for the GPT-5 family, so Codex sessions show a cost. ([3430445](https://github.com/kannandreams/agent-top/commit/3430445))

### Changed
- The README is reorganised, with background moved to [docs/why-this-exists.md](docs/why-this-exists.md) and [docs/accounting.md](docs/accounting.md). ([3430445](https://github.com/kannandreams/agent-top/commit/3430445))

## [0.9.0] - 2026-09-05

### Highlights
`agent-top report` answers "what have my agents cost me" across Claude Code, Codex, Gemini CLI and OpenCode together, priced the same way. It reads the transcripts already on disk, so it covers history from before agent-top was installed, and tokens on a model without a price are shown separately rather than counted as free.

### Added
- `agent-top report` totals cost and tokens from transcripts on disk, grouped with `--by harness|model|project|day` over a `--since` window. Supports `--json`. ([5e8cc24](https://github.com/kannandreams/agent-top/commit/5e8cc24))

## [0.8.0] - 2026-09-05

### Added
- Codex rate limits (5-hour and weekly windows) in the detail pane and in `--json` as `rate_limit`. ([370d1ea](https://github.com/kannandreams/agent-top/commit/370d1ea))
- `--once` lists agents at 75% or more of a rate limit under `RATE LIMITS`. ([370d1ea](https://github.com/kannandreams/agent-top/commit/370d1ea))

## [0.7.1] - 2026-09-05

### Added
- OpenCode: turn and inference spans in the tool trace. ([394bfcd](https://github.com/kannandreams/agent-top/commit/394bfcd))

## [0.7.0] - 2026-09-05

### Highlights
OpenCode is the fourth supported harness. Its sessions get a row with tokens, cost, tool calls and a tool trace, read from OpenCode's own database without writing to it, and the cost is the figure OpenCode computed itself.

### Added
- OpenCode support: tokens, cost, model, tool calls, subagents and tool trace, read from OpenCode's database without writing to it. ([2e2f92c](https://github.com/kannandreams/agent-top/commit/2e2f92c))

## [0.6.0] - 2026-09-05

### Added
- Codex and Gemini CLI: MCP call counts per server. ([5a5facd](https://github.com/kannandreams/agent-top/commit/5a5facd))

## [0.5.0] - 2026-09-05

### Highlights
The detail pane shows each MCP server an agent uses as its own row: its process, how often the agent called it, how many calls failed, and its CPU and memory. When an MCP process outlives its agent, the orphan list now says which agent it was left behind by.

### Added
- Claude Code: the detail pane has one row per MCP server with pid, calls, errors, last call, CPU and memory. Also `MCP SERVERS` in `--once` and `mcp_servers` in `--json`. ([9fa6c4d](https://github.com/kannandreams/agent-top/commit/9fa6c4d))
- Orphaned MCP processes name the agent they were orphaned from, and how long ago. In `--json` as `orphan_origins`. ([9fa6c4d](https://github.com/kannandreams/agent-top/commit/9fa6c4d))
- An `npx` or `uvx` wrapper and the server it starts count as one MCP server. ([9fa6c4d](https://github.com/kannandreams/agent-top/commit/9fa6c4d))

### Fixed
- MCP servers were labelled `tool`, and `--resume` and working-directory attribution never matched, because process command lines were never read. ([9fa6c4d](https://github.com/kannandreams/agent-top/commit/9fa6c4d))
- `--json` snapshots from before 0.2.0 replay again. ([9fa6c4d](https://github.com/kannandreams/agent-top/commit/9fa6c4d))

## [0.4.0] - 2026-09-05

### Highlights
Gemini CLI is the third supported harness, with the same row as Claude Code and Codex: tokens, cost, turns, tool calls and the tool trace. Every harness now plugs in through one adapter interface, so adding the next one does not touch the collector.

### Added
- Gemini CLI support: tokens, cost, turns, tool calls, web searches, tool trace and trace export. ([d47ddc2](https://github.com/kannandreams/agent-top/commit/d47ddc2))
- Prices for Gemini 2.5, 3, 3.1, 3.5, 3.6, 3.7 and 3.8 models. ([d47ddc2](https://github.com/kannandreams/agent-top/commit/d47ddc2))

### Changed
- `agent-top-core`: each harness is a `HarnessAdapter`, and `harness::open_transcript` returns `None` for a harness without one. ([d47ddc2](https://github.com/kannandreams/agent-top/commit/d47ddc2))

## [0.3.5] - 2026-09-04

### Added
- The detail pane breaks cost down by token type with the rate applied, and names the price table used. In `--json` as `cost_breakdown` and `price_source`. ([1fb2578](https://github.com/kannandreams/agent-top/commit/1fb2578))

## [0.3.4] - 2026-09-04

### Fixed
- Codex sessions started after the first day of a month were not found. ([e70bc83](https://github.com/kannandreams/agent-top/commit/e70bc83))
- Two Codex app-servers running at once no longer swap conversations. ([e70bc83](https://github.com/kannandreams/agent-top/commit/e70bc83))
- The header sparkline plots output tokens per second, labelled `out tok/s`. ([e70bc83](https://github.com/kannandreams/agent-top/commit/e70bc83))

## [0.3.3] - 2026-09-04

### Added
- `agent-top trace --endpoint URL` posts an OTLP trace to a collector. ([1c8d4a8](https://github.com/kannandreams/agent-top/commit/1c8d4a8))
- A local Jaeger setup in `examples/jaeger/`. ([1c8d4a8](https://github.com/kannandreams/agent-top/commit/1c8d4a8))
- The detail pane shows the `agent-top trace` command for the selected session. ([1c8d4a8](https://github.com/kannandreams/agent-top/commit/1c8d4a8))

## [0.3.2] - 2026-09-04

### Added
- `agent-top trace --format otlp` writes OpenTelemetry JSON for Jaeger, Tempo or any OTLP collector. ([cc4c1df](https://github.com/kannandreams/agent-top/commit/cc4c1df))

## [0.3.1] - 2026-09-04

### Added
- Claude Code web searches are counted and priced; Codex web searches are counted. ([0ad32d5](https://github.com/kannandreams/agent-top/commit/0ad32d5))
- Turn and inference spans in the waterfall, and `kind` on each span in `--json`. ([0ad32d5](https://github.com/kannandreams/agent-top/commit/0ad32d5))
- Chrome trace export nests tool calls and inferences under turns, with separate tracks for subagents. ([0ad32d5](https://github.com/kannandreams/agent-top/commit/0ad32d5))

### Fixed
- An interrupted turn ends at its last line, not at the next prompt. ([0ad32d5](https://github.com/kannandreams/agent-top/commit/0ad32d5))

## [0.3.0] - 2026-09-04

### Highlights
`agent-top trace` turns a session into a timeline you can open in Perfetto: every tool call as a bar with its real duration, so a slow turn shows exactly which call it was waiting on. It reads the whole transcript, not only the recent calls the live view keeps.

### Added
- `agent-top trace` exports a session's tool calls as a Chrome trace for Perfetto or `chrome://tracing`. ([075d2a5](https://github.com/kannandreams/agent-top/commit/075d2a5))
- `agent-top-core`: `SpanLog::unbounded`, `SpanRetention`, `SessionTracker::refresh_all`, `harness::detect` and `harness::open_transcript`. ([075d2a5](https://github.com/kannandreams/agent-top/commit/075d2a5))

### Fixed
- Claude Code subagent usage (2.1.233 and later) is counted in the parent row. ([dcfd7ae](https://github.com/kannandreams/agent-top/commit/dcfd7ae))

## [0.2.1] - 2026-09-04

### Fixed
- No change to the binary; the release workflow runs the full test suite before publishing. ([d0ac169](https://github.com/kannandreams/agent-top/commit/d0ac169))

## [0.2.0] - 2026-09-03

### Highlights
Drift detection catches a harness update that breaks parsing. A renamed transcript field used to show as 0 tokens and `$0.00`, which looks like good news; a session with model responses but no tokens now shows `?` and names the harness version that changed.

### Added
- Drift detection: a session that shows model responses but no tokens reads `?` and names the harness version, instead of `$0.00`. ([4afbce5](https://github.com/kannandreams/agent-top/commit/4afbce5))
- Shell completions with `--completions <shell>`. ([4afbce5](https://github.com/kannandreams/agent-top/commit/4afbce5))

### Changed
- One row per Codex conversation instead of one per process. ([4afbce5](https://github.com/kannandreams/agent-top/commit/4afbce5))

## [0.1.6] - 2026-09-03

### Changed
- No change to the binary; README corrections on pricing and Codex. ([8e6b6bc](https://github.com/kannandreams/agent-top/commit/8e6b6bc))

## [0.1.5] - 2026-09-03

### Added
- Prices live in `prices.toml`. Override or add models in `~/.config/agent-top/prices.toml` or `$AGENT_TOP_PRICES`. ([17165be](https://github.com/kannandreams/agent-top/commit/17165be))
- `--prices` prints the effective price table. ([17165be](https://github.com/kannandreams/agent-top/commit/17165be))

### Fixed
- A price file that fails to parse is reported on stderr. ([17165be](https://github.com/kannandreams/agent-top/commit/17165be))

## [0.1.4] - 2026-09-03

### Changed
- `agent-top-core`: `SpanLog::iter` is double-ended. ([4fb523c](https://github.com/kannandreams/agent-top/commit/4fb523c))

## [0.1.3] - 2026-09-03

### Changed
- No change to the binary; `agent-top-core` has its own crates.io README, and the install section lists every route. ([1be1657](https://github.com/kannandreams/agent-top/commit/1be1657), [fb68ea3](https://github.com/kannandreams/agent-top/commit/fb68ea3))

## [0.1.2] - 2026-09-03

### Added
- Published on crates.io: `cargo install agent-top` and `cargo binstall agent-top`. ([67392ac](https://github.com/kannandreams/agent-top/commit/67392ac))

### Fixed
- Both crates ship with a README. ([67392ac](https://github.com/kannandreams/agent-top/commit/67392ac))

## [0.1.1] - 2026-09-03

### Added
- `--replay <file>` opens a `--json` snapshot in the interactive UI. ([a355d77](https://github.com/kannandreams/agent-top/commit/a355d77))

### Changed
- Rounded, lighter panel borders. ([4435080](https://github.com/kannandreams/agent-top/commit/4435080))
- Header totals are aligned in columns, and the sort direction moved to the footer. ([4435080](https://github.com/kannandreams/agent-top/commit/4435080))

## [0.1.0] - 2026-09-03

### Highlights
The first release: a `top` for coding agents. One table shows every Claude Code and Codex session on the machine with its CPU, memory, tokens and cost, and a detail pane shows its process tree and a waterfall of its recent tool calls. MCP processes left running with no agent above them are flagged as orphans.

### Added
- Interactive TUI with host CPU and memory, a tokens-per-second sparkline, a sortable agent table and a detail pane.
- Discovers Claude Code, Codex, Gemini CLI, OpenCode, Aider, Copilot CLI and cursor-agent processes, with nested agents shown as subagents.
- Claude Code and Codex: tokens, cost, turns and tool calls from transcripts.
- Tool trace: a waterfall of recent tool calls, toggled with `Tab`.
- Orphaned MCP process detection.
- `--once` and `--json` output.
- Prebuilt binaries for macOS and Linux, and a Homebrew tap.
