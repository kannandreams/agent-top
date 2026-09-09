# Getting started

## Install

| Route | Command | Notes |
|---|---|---|
| Homebrew | `brew install kannandreams/tap/agent-top` | macOS and Linux, prebuilt; installs shell completions |
| Cargo, prebuilt | `cargo binstall agent-top` | downloads the release binary, no compiler needed |
| Cargo, from source | `cargo install --locked agent-top` | builds from crates.io; needs Rust 1.85 or newer |
| By hand | [the releases page](https://github.com/kannandreams/agent-top/releases) | tarballs and `sha256` for macOS and Linux, x86_64 and arm64 |

Every route ends at the same single binary: no Python, no Node, no daemon. Without Homebrew, `agent-top --completions zsh` (or `bash`, `fish`) prints a completion script to source from your shell's startup file.

## First run

```sh
agent-top
```

That is the whole setup. Start it in any terminal while your agents run. It finds Claude Code, Codex, Gemini CLI and OpenCode sessions on its own, from the process table and the transcripts each harness writes.

If the table is empty, no supported agent is running or has written a transcript in the last 30 minutes. `agent-top --json` prints everything it found, which is the quickest way to see why a session is missing.

## Keys

| Key | What it does |
|---|---|
| `j` `k` or arrows | move the selection; the detail pane follows |
| `Tab` or `v` | switch the detail pane between the process tree and the tool trace |
| `t` or `Enter` | show or hide the detail pane |
| `s` `r` | cycle the sort column, reverse it |
| `x` | hide stopped sessions (shown only when there are some) |
| `p` or `Space` | pause refresh |
| `l` | slowest tools, across every agent |
| `f` | failed tool calls |
| `a` | advice: oversized results, idle and growing MCP servers |
| `?` | help, the version and how to upgrade |
| `q` | quit |

## Other ways to run it

```sh
agent-top --once                  # print the table once and exit
agent-top --json                  # one snapshot as JSON, for scripts and bug reports
agent-top --replay snap.json      # render a saved snapshot, keys and all
agent-top report --since 7d       # what every harness cost this week
agent-top trace --session 9f2c -o trace.json   # one session as a Perfetto trace
agent-top --interval-ms 500       # faster refresh
agent-top --stopped-window-min 120   # keep stopped sessions visible for two hours
agent-top --prices                # the price table in use, with your overrides marked
agent-top --whats-new             # this build's changelog, no network call
```

## Upgrading

agent-top checks crates.io once a day for a newer version. Set `AGENT_TOP_NO_UPDATE_CHECK=1` to turn the check off. When one is out, the footer badge turns amber and, once per run, a popup asks:

- `u` upgrades now, with the installer that put agent-top here: `brew update && brew upgrade agent-top`, `cargo binstall -y agent-top` or `cargo install --locked agent-top`, judged from where the binary is. The exact command is shown before you press anything. When it finishes, agent-top starts again on the new version with the same arguments.
- `n` or `Esc` is "not now": that version is not asked about again, the badge stays amber, and the next release asks afresh.

A binary installed by hand is not guessed at: the popup lists the routes and leaves the command to you. This upgrade is the one command agent-top ever runs that changes the machine. It changes only agent-top, and only on that keypress.
