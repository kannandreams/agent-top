---
name: write-changelog
description: Write or edit agent-top's CHANGELOG.md in a short, public-facing release-notes style, with a Highlights paragraph for major features and a PR link on every entry. Use whenever a user-visible change lands (add a line under Unreleased), when cutting a release (turn Unreleased into a version), or when asked to "update the changelog", "write release notes" or "what changed in vX". Do not copy PR descriptions or commit messages into it.
---

# Writing the agent-top changelog

`CHANGELOG.md` is read by people deciding whether to upgrade. An engineer should
take in a whole release in under a minute, see which change matters most, and
click through to the PR when a line is not enough. PR descriptions, DEC records
and `docs/` hold the reasoning; the changelog says what changed for the user
and links to where the detail lives.

## Format

The file follows [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/).
Three things read it, so the heading shapes are fixed:

- `.github/workflows/release.yml` publishes the lines under `## [x.y.z]` as the
  GitHub release notes, and fails the release if that section is empty. Links
  must be inline and absolute; reference-style definitions at the bottom of the
  file would not travel with the section.
- `agent-top --whats-new` prints the first four `## [` sections, `[Unreleased]`
  included, from the copy compiled in by `crates/agent-top/build.rs`.
- `docs/hooks/changelog_links.py` puts the file on the docs site, drops the
  `# Changelog` H1 and strips the `docs/` prefix from links.

```markdown
## [0.19.0] - 2026-09-20

### Highlights
`agent-top --status` prints one line per live agent, sized for a shell prompt or a tmux status bar. It reads the same data as the table, so the numbers match the live view.

### Added
- `agent-top --status` prints one line per live agent. ([#41](https://github.com/kannandreams/agent-top/pull/41))

### Fixed
- Codex sessions resumed with `--resume` keep their cost after a restart. ([#42](https://github.com/kannandreams/agent-top/pull/42))
```

Sections, in this order, and only the ones that have entries: `Highlights`,
`Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`. One blank line
between sections and before each `## [`.

## Highlights

- Only for a release with a feature a one-liner undersells: a new panel, a new
  command, a new harness, a new kind of number. Patch releases and releases of
  small changes have none.
- One short paragraph per highlighted feature, two or three sentences: what it
  is, and the question it answers or the problem it catches. At most two
  paragraphs per release.
- No links, no bullets, no implementation detail. The feature still gets its
  own entries under `Added` or `Changed`, with the links.

## One entry

- One line, one change, about 25 words before the link. If it needs a second
  sentence, the detail belongs in the Highlights paragraph or the docs.
- End with the link in parentheses: `([#36](https://github.com/kannandreams/agent-top/pull/36))`.
  Several PRs: `([#36](…), [#37](…))`. A change committed straight to `main`
  links the commit: `([4afbce5](https://github.com/kannandreams/agent-top/commit/4afbce5))`.
  Find them with `git log --format='%h %s' vPREV..HEAD`; squash-merged PRs end
  their subject with `(#N)`.
- Start with what the user can now do or see: the key, flag, command, panel or
  harness. `` `m` opens the MCP servers panel. ``, not "Added a new Panel variant".
- Put keys, flags, commands, JSON fields and model names in backticks.
- Name the harness when the change applies to only some of them.
- Mention a new `--json` field when one is added. Scripts depend on it.
- No bold, no nested bullets.
- Present tense, plain statements. Follow [[user-prefers-plain-prose]]: no
  reassurance, no justification for the design.

## Leave out

- How it works: parsing rules, file formats, heuristics, crate internals.
- How it was verified: live checks, fixture names, "found when".
- Internal references in the text: RFC and DEC numbers. PR and commit numbers
  appear only as the trailing link.
- Changes with no user effect: refactors, tests, CI, fixture updates, price
  table date bumps, docs-site styling. If a release has nothing else, write one
  entry: `- No change to the binary; <what did change, e.g. README updates>.`
- Public `agent-top-core` API changes are the exception. List them under
  `Changed`, since library users see them.

## Examples

Too long (a PR description pasted in):

> - **MCP call counts for OpenCode.** OpenCode rows now get per-server MCP rows
>   with calls, errors and the last call... OpenCode writes an MCP call as an
>   ordinary tool part named `<server>_<tool>`, with no prefix... Verified
>   against a live OpenCode 1.18.15 session.

Right length:

> - OpenCode: MCP call counts per server, matched against the server names in OpenCode's config. ([#36](https://github.com/kannandreams/agent-top/pull/36))

Too internal:

> - The scanner now uses `refresh_processes_specifics` with `with_cmd`.

User-visible:

> - MCP servers were labelled `tool` and `--resume` sessions went unmatched, because process command lines were never read.

## Workflow

1. **With each change.** In the same PR as the change, add its entry under
   `## [Unreleased]`, in the right section. Write it from the diff and the
   user-facing result, not from the PR body. The PR number is known once the PR
   is open; add the link in a follow-up commit on the branch.
2. **At release** (only after the user confirms the release, see
   [[release-only-on-confirmation]]):
   - Reread the Unreleased entries together. Merge entries about the same
     feature (keeping all their links), drop any that no longer apply, and put
     the most important first.
   - Decide whether the release has a feature worth a Highlights paragraph, and
     write it.
   - Rename `## [Unreleased]` to `## [x.y.z] - YYYY-MM-DD` and add a fresh,
     empty `## [Unreleased]` above it.
   - Check the section with the same logic the release workflow uses:
     `awk -v v=x.y.z '$0 ~ "^## \\[" v "\\]" {s=1; next} s && /^## / {exit} s' CHANGELOG.md`
3. **Fixing a published entry.** Edit `CHANGELOG.md`, then update the release
   page with `gh release edit vX.Y.Z --notes-file <the section>` so both say the
   same thing, and republish the docs site with `mise run docs:publish`. Both
   are outward-facing; ask before running them.
