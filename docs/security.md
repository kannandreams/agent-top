---
description: "The security model behind agent-top: what it reads, what it never does, and the two network calls it can make, enumerated exactly."
---

# Security

agent-top's security model is a consequence of its design, not a policy bolted on afterward: it is read-only and local-only, so it never needs more trust than any other unprivileged process reading files you could already read yourself.

## What it reads

The process table (through `sysinfo` and, on macOS, `libproc` — not by shelling out to `ps`) and the transcript files each harness already writes. From a transcript it takes metadata: usage records (token counts), tool and MCP call names, ids, timestamps, session id, model, working directory. It does not read prompt text or tool output. [Accounting](accounting.md) is the arithmetic that follows from that boundary; [Context by source](accounting.md#context-by-source) is the clearest case of it — what a tool result added to the prompt is priced from the token counts alone, never from the result itself.

## What it never does

- **Never writes to a transcript.** The files a harness owns are never touched.
- **Never signals or kills a process.** An orphaned MCP server is reported — pid, memory, age, the agent it came from — and the `kill` is yours. See [what agent-top is not](vision.md#what-agent-top-is-not).
- **No shell-outs for discovery.** Process and file information comes from library calls (`sysinfo`, `libproc`), never from parsing the output of `ps`, `lsof`, or any other command.
- **No credentials, no provider.** agent-top does not hold an API key or talk to a model. It has nothing to leak on that front because it was never given anything to hold.
- **No `unsafe`.** Both crates are safe Rust; nothing in agent-top reaches past what the compiler checks.

## The network, enumerated exactly

Two calls, both narrow enough to name in full:

1. **A daily version check.** `GET https://crates.io/api/v1/crates/agent-top` with a fixed `User-Agent` and nothing else — no session data, no machine identifier, no query string. Cached to run at most once a day, in `~/.cache/agent-top/update-check.json` (or `$XDG_CACHE_HOME/agent-top/update-check.json`), which holds only when it last checked, what it found, and which version you dismissed. `AGENT_TOP_NO_UPDATE_CHECK=1` turns it off.
2. **`trace --endpoint <url>`.** Posts the OTLP document to the exact address typed on the command line, once, only when typed. No default endpoint, no config key, no environment variable that turns it on silently.

Nothing else opens a connection. `--json` and `--replay` never touch the network — replay reads only the file you give it.

## The one command that changes the machine

Accepting the upgrade prompt (`u`) runs a fixed command line chosen by detecting how the running binary was installed — `brew update && brew upgrade agent-top`, `cargo binstall -y agent-top`, or `cargo install --locked agent-top` — never a string built from anything you typed, never through a shell. It is printed before it runs, it happens only on that keypress, and it is the only thing agent-top ever changes outside of itself.

## Local files it touches

**Reads:** the transcripts, the process table, and — if it exists — `~/.config/agent-top/prices.toml`. A malformed price file is reported on stderr and ignored; the built-in prices still apply. See [Prices](prices.md).

**Writes:** the one cache file above, and nothing else on its own. `--replay` and `--json` are read/print only; neither touches disk beyond the file you point at.

## Reporting an issue

agent-top is MIT-licensed and the source is the whole story: [github.com/kannandreams/agent-top](https://github.com/kannandreams/agent-top). Open an issue, or, for anything that should not be public first, a private report through GitHub's Security tab.
