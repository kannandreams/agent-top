---
name: write-changelog
description: Write or edit agent-top's CHANGELOG.md in a short, public-facing release-notes style. Use whenever a user-visible change lands (add a line under Unreleased), when cutting a release (turn Unreleased into a version), or when asked to "update the changelog", "write release notes" or "what changed in vX". Do not copy PR descriptions or commit messages into it.
---

# Writing the agent-top changelog

`CHANGELOG.md` is read by people deciding whether to upgrade. An engineer should
take in a whole release in under a minute. PR descriptions, commit messages,
DEC records and `docs/` hold the reasoning; the changelog only says what changed
for the user.

## Format

The file follows [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/).
Three things read it, so the heading shapes are fixed:

- `.github/workflows/release.yml` publishes the lines under `## [x.y.z]` as the
  GitHub release notes, and fails the release if that section is empty.
- `agent-top --whats-new` prints the first four `## [` sections, `[Unreleased]`
  included, from the copy compiled in by `crates/agent-top/build.rs`.
- `docs/hooks/changelog_links.py` puts the file on the docs site, drops the
  `# Changelog` H1 and strips the `docs/` prefix from links.

```markdown
# Changelog

<one-line preamble, unchanged>

## [Unreleased]

## [0.19.0] - 2026-09-20

### Added
- `agent-top --status` prints one line per live agent for shell prompts and status bars.

### Fixed
- Codex sessions resumed with `--resume` keep their cost after a restart.
```

Sections, in this order, and only the ones that have entries: `Added`,
`Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. One blank line between
sections and before each `## [`.

## One entry

- One line, one change, about 25 words at most. If it needs a second sentence,
  the detail goes in the docs, and the entry can link there.
- Start with what the user can now do or see: the key, flag, command, panel or
  harness. `` `m` opens the MCP servers panel. ``, not "Added a new Panel variant".
- Put keys, flags, commands, JSON fields and model names in backticks.
- Name the harness when the change applies to only some of them.
- Mention a new `--json` field when one is added. Scripts depend on it.
- No bold, no nested bullets, no paragraphs.
- Present tense, plain statements. Follow [[user-prefers-plain-prose]]: no
  "honest about", no reassurance, no justification for the design.

## Leave out

- How it works: parsing rules, file formats, heuristics, crate internals.
- How it was verified: live checks, fixture names, "found when".
- Internal references: RFC and DEC numbers, PR numbers, commit hashes.
- Changes with no user effect: refactors, tests, CI, fixture updates, price
  table date bumps. If a release has nothing else, write one entry:
  `- No change to the binary; <what did change, e.g. README updates>.`
- Public `agent-top-core` API changes are the exception. List them under
  `Changed`, since library users see them.

## Examples

Too long (a PR description pasted in):

> - **MCP call counts for OpenCode.** OpenCode rows now get per-server MCP rows
>   with calls, errors and the last call... OpenCode writes an MCP call as an
>   ordinary tool part named `<server>_<tool>`, with no prefix... Verified
>   against a live OpenCode 1.18.15 session.

Right length:

> - OpenCode: MCP call counts per server, matched against the server names in OpenCode's config.

Too internal:

> - The scanner now uses `refresh_processes_specifics` with `with_cmd`.

User-visible:

> - MCP servers were labelled `tool` and `--resume` sessions went unmatched, because process command lines were never read.

## Workflow

1. **With each change.** In the same PR as the change, add its entry under
   `## [Unreleased]`, in the right section. Write it from the diff and the
   user-facing result, not from the PR body.
2. **At release** (only after the user confirms the release, see
   [[release-only-on-confirmation]]):
   - Reread the Unreleased entries together. Merge entries about the same
     feature, drop any that no longer apply, and put the most important first.
   - Rename `## [Unreleased]` to `## [x.y.z] - YYYY-MM-DD` and add a fresh,
     empty `## [Unreleased]` above it.
   - Check the section with the same logic the release workflow uses:
     `awk -v v=x.y.z '$0 ~ "^## \\[" v "\\]" {s=1; next} s && /^## / {exit} s' CHANGELOG.md`
3. **Fixing a published entry.** Edit `CHANGELOG.md`, then update the release
   page with `gh release edit vX.Y.Z --notes "<the section>"` so both say the
   same thing. The GitHub release is outward-facing; ask before editing it.
