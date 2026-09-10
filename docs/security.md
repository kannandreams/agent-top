---
description: "The security model behind agent-top: what it reads, what it never does, and the network calls it can make."
---

# Security

agent-top is read-only and local-only. It reads the transcripts a harness already wrote and the process table the OS already keeps, and changes neither.

## What the scans say

<!--security-summary-->

`scripts/security_report.py` runs [cargo-audit](https://github.com/rustsec/rustsec) against the workspace's `Cargo.lock` and writes the result to `docs/data/security.json`. This page and the sidebar are rendered from that file at build time, and CI fails if the published number disagrees with a fresh scan.

Three checks run in [the Security workflow](https://github.com/kannandreams/agent-top/actions/workflows/security.yml), each answering a different question:

| Check | Question | When |
|---|---|---|
| [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/) | Is any dependency subject to a RustSec advisory, an unexpected licence, or a source that is not crates.io? | every push and pull request, and daily |
| [CodeQL](https://codeql.github.com) | Does the code itself contain a pattern recognised as a vulnerability? | every push and pull request, and daily |
| `security_report.py --check` | Does the count published above still match a fresh scan? | every push and pull request, and daily |

The daily run catches what a push cannot: an advisory published against an unchanged `Cargo.lock` is a new vulnerability in a release that has already shipped.

The supply-chain rules are in [`deny.toml`](https://github.com/kannandreams/agent-top/blob/main/deny.toml). Every crate must come from crates.io — no git dependencies, no alternative registries — and carry a licence from an explicit permissive allowlist. A wildcard version requirement fails the build.

To run the same checks yourself:

```sh
cargo deny check              # advisories, licences, sources, duplicate versions
cargo audit                   # RustSec advisories alone
mise run security             # both, the way CI does
```

## What it reads

The process table (through `sysinfo` and, on macOS, `libproc` — not by shelling out to `ps`) and the transcript files each harness already writes. From a transcript it takes metadata: usage records (token counts), tool and MCP call names, ids, timestamps, session id, model, working directory. It does not read prompt text or tool output. [Context by source](accounting.md#context-by-source) works within that boundary: what a tool result added to the prompt is priced from the token counts alone, not from the result. [Accounting](accounting.md) has the arithmetic.

## What it never does

- **Never writes to a transcript.** The files a harness owns are never touched.
- **Never signals or kills a process.** An orphaned MCP server is reported — pid, memory, age, the agent it came from — and the `kill` is yours. See [what agent-top is not](vision.md#what-agent-top-is-not).
- **No shell-outs for discovery.** Process and file information comes from library calls (`sysinfo`, `libproc`), not from parsing the output of `ps`, `lsof`, or any other command.
- **No credentials, no provider.** agent-top does not hold an API key or talk to a model.
- **No `unsafe`.** There is no `unsafe` block in either crate.

## Network calls

There are two.

1. **A daily version check.** `GET https://crates.io/api/v1/crates/agent-top` with a fixed `User-Agent` and nothing else — no session data, no machine identifier, no query string. Cached to run at most once a day, in `~/.cache/agent-top/update-check.json` (or `$XDG_CACHE_HOME/agent-top/update-check.json`), which holds when it last checked, what it found, and which version you dismissed. `AGENT_TOP_NO_UPDATE_CHECK=1` turns it off.
2. **`trace --endpoint <url>`.** Posts the OTLP document to the address typed on the command line, once. No default endpoint, no config key, no environment variable that turns it on.

Nothing else opens a connection. `--json` and `--replay` never touch the network; replay reads only the file you give it.

## The one command that changes the machine

Accepting the upgrade prompt (`u`) runs a fixed command line chosen by detecting how the running binary was installed — `brew update && brew upgrade agent-top`, `cargo binstall -y agent-top`, or `cargo install --locked agent-top` — never a string built from input, and never through a shell. The command is printed before it runs, and it runs only on that keypress.

## Local files it touches

**Reads:** the transcripts, the process table, and — if it exists — `~/.config/agent-top/prices.toml`. A malformed price file is reported on stderr and ignored; the built-in prices still apply. See [Prices](prices.md).

**Writes:** the one cache file above. `--replay` and `--json` read and print; neither touches disk beyond the file you point at.

## Reporting an issue

agent-top is MIT-licensed: [github.com/kannandreams/agent-top](https://github.com/kannandreams/agent-top). Open an issue, or, for anything that should not be public first, a private report through GitHub's Security tab.
