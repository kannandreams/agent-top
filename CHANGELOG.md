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
