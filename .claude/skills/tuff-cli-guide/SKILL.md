---
name: tuff-cli-guide
description: Reference for using Tuff CLI to manage agent capabilities — create, add (including MCP servers), list, diff, check, update, and scope operations.
---

# Tuff CLI Guide

Tuff is a capability lifecycle manager. It installs, versions, diffs, and validates the skills, tools, hooks, workflows, and MCP servers a repository gives its coding agents. Use these commands when the user asks about managing any of those, or when they mention "drift", "baseline", or "tuff".

## Before You Start

Run `tuff --version`. If the command is missing, ask the user before installing anything. When they agree:

```
uv tool install tuffcli
```

If `uv` is not available, use `brew tap kannandreams/homebrew-tuff && brew install tuff` on macOS, or `cargo install tuffcli` where Rust is present. Avoid bare `pip install`: most systems refuse it as an externally-managed environment, and a virtual environment install is not on PATH afterwards.

Confirm with `tuff --version` before continuing. If the command is still not found, the install directory is not on PATH; `~/.local/bin` is the usual one.

If the project has no `tuff.lock`, run `tuff init` before anything else. Never hand-edit `tuff.lock` or the files Tuff emits under a harness directory; edit the source under `.agents/` and let Tuff re-emit.

## Available Commands

### Install
- `tuff add <path> -a <agent>` — install a local capability (auto-detect type)
- `tuff add skill <path> [name] -a <agent>` — install a skill
- `tuff add tool <path> [name] -a <agent>` — install a tool
- `tuff add hook <path> [name] -a <agent>` — install a hook
- `tuff add workflow <path> [name] -a <agent>` — install a workflow
- `tuff add .agents/skills/<id> -a open-agents` — track existing agent files in place
- `tuff add <git-url> skill <name> -a <agent>` — install skill from git
- `tuff add <git-url> tool <name> -a <agent>` — install tool from git
- `tuff add <git-url> hook <name> -a <agent>` — install hook from git
- `tuff add skill <git-url> <name>@<version|range> -a <agent>` — install a tagged release (`1.2.0`, `^1.2`, `>=1, <2`); the lockfile records the tag, the requirement, and the commit
- without `@`, a git install records the version the source declares (`version` in `tuff.toml`, or `version:`/`metadata.version:` in `SKILL.md` frontmatter), shown as `1.2.0 (declared)` since it is weaker than a release; a source declaring nothing records the commit SHA
- `tuff create <type> <id> -a <agent>` — create and track a capability
- `tuff add pack <artifact.tuffpack> -a <agent>` — atomically install a verified capability pack
- `tuff add mcp <catalog-id|path|git-url>... -a <agent>` — wire external MCP servers (catalog: filesystem, memory, github, fetch, git, time, sequentialthinking, everything, brave-search, notion, playwright, sentry, linear, context7) into each harness config; secrets stay as `${VAR}` references. Prompts once per required variable at a real terminal (add `--yes` to skip, or it's automatic in a non-interactive shell)

### Packs
- `tuff pack build --name <name>` — package accepted project capabilities into `tuff-dist/<name>-0.1.0.tuffpack`
- `tuff pack build --name <name> --capability <id> --version <version> -a <agent>` — build a selected project pack
- `tuff pack init <name> --from-project --capability <id>` — save a reusable ID-based definition under `tuff-packs/`
- `tuff pack check [path]` — validate a local source pack
- `tuff pack build [path] -o <artifact.tuffpack>` — build a reusable project definition or standalone source pack
- `tuff pack inspect <artifact.tuffpack>` — inspect verified metadata
- `tuff pack verify <artifact.tuffpack>` — verify metadata and every stored file
- `tuff pack push <artifact.tuffpack> <registry/repository:tag>` — publish exact pack bytes as an OCI artifact
- `tuff pack pull <registry/repository:tag-or-digest> -o <artifact.tuffpack>` — download and verify an OCI-distributed pack without overwriting an existing file
- `tuff pack extract <artifact.tuffpack> -a <agent> -o <dir>` — extract a runtime-native target

### Inspect
- `tuff list` — show all installed capabilities with drift status
- `tuff list --type skill` — filter by capability type
- `tuff list --scope global` — show global scope
- `tuff status` — detailed status with override warnings
- `tuff diff <id>` — show local changes against baseline
- `tuff diff <id> --upstream` — show upstream changes (git sources only); for a release-pinned entry, against the newest release its requirement allows
- `tuff diff <id>@<range> --upstream` — preview what a different requirement would change before `tuff update <id>@<range>`
- `tuff outdated` — check all capabilities for available updates; `repointed` or `tag missing` on a release-pinned entry means the tag moved or vanished upstream, and `tuff update <id> --force` replaces the install with what the tag names now
- `tuff mcp doctor` — spawn each installed MCP server for real and verify the handshake + tool list, not just that the config entry exists

### Update & Merge
- `tuff update <id> --check` — preview local baseline promotion or upstream changes
- `tuff update <id>` — accept local edits or reconcile with upstream
- `tuff update <id> -a <agent>` — update one recorded agent
- `tuff update <id> --force` — overwrite local changes with source output
- `tuff update <id>@<range>` — change a tag-pinned capability's requirement and move to the newest release it allows; a bare `update` never leaves the recorded range

### CI & Validation
- `tuff check` — validate all capabilities (exit 1 on any failure)
- `tuff check --json` — machine-readable output for CI
- `tuff list --json`, `tuff outdated --json`, `tuff diff <id> --json` — the same rows as JSON, with the same `type`/`target`/`status` keys as `check --json`; prefer these over parsing tables

### Manage
- `tuff remove <id>` — remove a capability
- `tuff agent list` — show available agent harnesses
- `tuff agent add <id>` — register an agent and initialize its project directory
- `tuff init --global` — initialize global scope

### Agents
- `open-agents` — Codex, Cursor, OpenCode, Copilot, Gemini CLI, Roo, Cline
- `claude` — Claude Code

### Scope
- Project (default): `tuff.lock` in repo root — committed with project
- Global state: XDG config/state/cache Tuff directories — available across all projects
- Project always wins when both exist

## Directory Model
- `.agents/skills/` — create and edit skills here (single source of truth)
- `.agents/tools/` — create and edit tools here
- `.agents/hooks/` — create and edit hooks here
- `.agents/workflows/` — create and edit workflows here
- `.agents/mcp-servers/` — generated `server.toml` records for external MCP servers (edit the manifest, not these)
- `tuff.lock` — committed capability identity and lifecycle metadata
- `tuff.config.json` — project preferences; `tuff.lock` remains the project source of truth
- No separate source directory — agent files are the source

## Status Values
- `clean` — installed content matches baseline
- `modified` — local changes detected (run `tuff diff <id>` to see)
- `missing` — installed file no longer exists

## Quick Cheat Sheet
```
tuff init                              # initialize repo and register open-agents
tuff add <path> -a open-agents         # install capability (auto-detect type)
tuff add skill <path> [name]           # install a skill explicitly
tuff add mcp github -a claude         # wire a catalog MCP server
tuff list                          # check drift
tuff diff <id>                     # see what changed
tuff check                         # CI validation
tuff outdated                      # check for updates
tuff update <id>                   # accept local edits or update from source
```
