# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Interactive TUI with host CPU/memory gauges, a tokens-per-second sparkline, a sortable agent table and a detail pane with the process tree and token breakdown.
- Discovery of Claude Code, Codex, Gemini CLI, OpenCode, Aider, Copilot CLI and cursor-agent processes; nested harness processes are shown as subagents.
- Exact Claude Code attribution via `~/.claude/sessions/<pid>.json`; heuristic Codex attribution by working directory.
- Incremental transcript tailing for Claude Code and Codex with token, cost, turn and tool-call accounting; Claude usage deduplicated per API message id.
- Static Anthropic price table with per-TTL cache-write pricing; unknown models are reported as unpriced tokens, never guessed.
- Orphaned MCP process detection (MCP-looking processes with no live agent ancestor).
- `--once` and `--json` non-interactive modes.
- **Tool trace.** Tool calls are reconstructed as spans by pairing each harness's call and result records (Claude `tool_use` / `tool_result` by `tool_use_id`, Codex `function_call` / `function_call_output` by `call_id`) and reading the timestamps that bracket them. `Tab` switches the detail pane between the process tree and a waterfall of the agent's recent calls, with the share of the window actually spent in tools, the slowest call, in-flight calls and failures. The spans are in `--json` too.
- **Prebuilt binaries and a Homebrew tap.** A tag-driven release workflow builds macOS and Linux binaries for x86_64 and arm64, publishes them with checksums, renders the Homebrew formula and (when the tokens are configured) pushes the tap bump and the crates.io release. `cargo-binstall` metadata points at the same assets.
- `schema_version` in the `--json` document, so scripts can tell when the shape changes.
- **btop-style meters.** Bars are drawn in the seven-eighths block, which most terminal fonts render with a one-pixel gap, and sit in a visible near-black track. Colour is a three-stop ramp: host CPU and memory ramp along the meter's own length, while a trace bar's colour is its call's duration on a log scale from 50 ms to a minute — width is the call's share of the window, so at a typical zoom, where nearly every bar is one cell wide, colour carries the magnitude that width cannot. Subagent, in-flight and failed calls each get their own hue family plus a marker (`↳`, `…`, `!`). True colour where the terminal advertises it, nearest xterm-256 entry otherwise.

### Fixed
- `guess_transcript` returned no transcript at all when any single file in a project directory could not be stat'd, silently dropping fallback attribution for every agent in that directory.
