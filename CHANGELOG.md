# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/).

## [0.3.5] - 2026-09-04

### Added
- **The cost is broken down per kind of token in the detail pane**, with the price each was charged at and what it came to, and the total names the table it was priced from ("list price, built-in table" or "your price file"). A figure that differs from the harness's own display can now be traced to the one line that differs, instead of looking like a bug. `--json` carries the same lines as `cost_breakdown` and the table as `price_source`; both are additive.
- A README section, "If the cost does not match your harness", with a real worked example (Claude Code pricing Fable 5.1 cache reads at $0.50 per million where the published page says $0.25) and the one-model override that makes the figures agree.

### Changed
- The README's Prices section says plainly that a user file overrides the built-in table one model at a time.

## [0.3.4] - 2026-09-04

### Fixed
- **Codex rollouts were invisible after the first day of a month.** The scan of `~/.codex/sessions/YYYY/MM/DD/` pruned directories by their mtime, and a directory's mtime moves only when an entry is created directly inside it, so the year directory was last touched when the month directory was created. Every rollout written after that was skipped, which left every Codex process with `attribution: none`. The tree is now walked in full; it is a few hundred directories at most.
- **Two Codex app-servers no longer trade threads.** With the VS Code extension's app-server and a second one running, whichever was asked first took every live thread. A Codex process is now matched to the rollouts it holds open, which is exact: Codex opens a thread's rollout when the thread starts and closes it when the thread ends. The detail pane labels these rows "open transcript file (exact)" and `--json` says `"attribution": "open-file"`. A process that holds no rollout gets no thread, and a rollout nobody holds is a finished conversation in the stopped list. On a platform where open files cannot be read, the old heuristics still apply. Adds `libproc` on macOS; Linux reads `/proc/<pid>/fd`.
- **The header sparkline measures output tokens per second.** It was labelled tokens/s but plotted the change in total tokens per refresh, cache reads included, so one turn on a large context showed as a spike. It now plots output tokens per second over a ten second window and is labelled `out tok/s`.

### Changed
- CI and release workflows use `actions/checkout@v5`.

## [0.3.3] - 2026-09-04

### Added
- **`trace --endpoint URL`** posts the OTLP document to a collector and prints the response status. It is the one network call agent-top can make; it happens only with an address typed on the command line, and there is no default, config key or environment variable. Without the flag nothing leaves the machine. Requires `--format otlp`.
- **`examples/jaeger/compose.yaml`**, a local Jaeger with the OTLP port open, and an "OTLP to Jaeger" section in the README.
- The detail pane shows the exact `agent-top trace` command for the selected session.

### Changed
- The README explains how to open a Chrome trace file in Perfetto rather than saying it "opens directly", and the roadmap no longer carries target dates.

## [0.3.2] - 2026-09-04

### Added
- **`--format otlp` for the trace export.** Writes the OpenTelemetry trace request as JSON, which Jaeger, Tempo and any OTLP collector accept with one `curl`. Each tool call and inference is parented to the turn it happened in, so backends that draw trees draw the right one. The trace id is derived from the session id and each span id from the session and call ids, so exporting the same session twice produces the same trace rather than a duplicate. A span still open when the transcript ended gets an end equal to its start and an `agent_top.open` attribute rather than an invented duration. agent-top still never contacts a collector; posting the file is the user's command.
- A "How a session becomes a trace" diagram in the README.

## [0.3.1] - 2026-09-04

### Added
- **Web searches are priced.** Anthropic bills server-side web search per search ($10 per 1,000, on top of the tokens it produces); web fetch is free. The count is read from each message's usage record, deduplicated like the rest of usage, and added to the row's cost. The rate lives in `prices.toml` under `[server_tools]` and a user table can override it. Codex `web_search_call` items are counted and shown, not priced, because OpenAI's rate is not in the table. The count is in `--json` as `web_searches` and in the detail pane.
- **Turn and inference spans.** The waterfall used to show tool calls with blank gaps between them. Each gap is now labelled: a `model` row runs from a prompt or tool result being submitted to the last block of the reply, and each human turn is a span from the prompt to the model ending its reply. The summary line reads `in tools 58%  model 31%  turn 3m20s…`. Every span carries a `kind` (`tool`, `inference`, `turn`) in `--json`; snapshots from earlier versions read back with `tool`. The live span log grows from 128 to 256 to hold the extra rows.
- **The trace export nests.** Chrome trace files now carry six tracks: turns, tools and model time for the main agent and again for its subagents. Perfetto shows a turn as a bar with its tool calls and inferences beneath. Open spans stay begin-only.

### Fixed
- An inference that never got a reply (a message queued mid-turn, an interrupted request) is dropped rather than left running forever, and a turn the user interrupted ends at the last line written rather than at the next prompt, which could be days later. A `<synthetic>` message written by the harness on resume ends nothing.

## [0.3.0] - 2026-09-04

### Added
- **`agent-top trace`.** Exports one session's tool calls as a Chrome trace event file (`--format chrome`, the only format so far) that Perfetto and `chrome://tracing` open with no setup. The live tracker keeps the newest 128 calls, which is right for a waterfall pane and wrong for an export, so the subcommand reads the transcript again from the start with no cap. `--session` accepts a session id, a unique prefix of one, or a path to a transcript; an ambiguous prefix lists the candidates. The process id in the file is derived from the session id, so exporting the same session twice gives the same trace. A call that never returned is written as a begin event with no end. Output goes to standard output or to `-o FILE`; nothing is sent anywhere.
- `agent-top-core`: `SpanLog::unbounded`, `SpanRetention`, `SessionTracker::refresh_all`, `harness::detect` and `harness::open_transcript`, which is what the export is built from. Existing types and defaults are unchanged.

### Fixed
- **Subagent usage was not counted for Claude Code.** Since 2.1.233 each Agent-tool call writes its own transcript under `<session>/subagents/agent-<id>.jsonl`, and the parent transcript carries no sidechain lines at all. agent-top read only the parent, so a session that used subagents showed fewer tokens and a lower cost than Claude Code's own display, and the waterfall never showed a subagent call. The tracker now discovers those files, tails each one incrementally, and folds their tokens, cost, turns, tool calls and spans into the parent row. Subagents on a different model from the parent are priced by the model each line names. The row's model, state and start time remain the parent's.

## [0.2.1] - 2026-09-04

No change to the binary's behaviour. This release exists so that a published
version corresponds to a green build.

### Fixed
- A test asserted that three rollout files written microseconds apart would carry distinct modification times. Linux gave all three the same mtime, the stable sort preserved insertion order, and the test failed there while passing on macOS. The files now carry explicit modification times, so the expected ordering does not depend on filesystem timestamp resolution.
- The release workflow ran no tests. It built the binaries and smoke-ran one, which is how 0.2.0 reached crates.io, the Homebrew tap and the releases page while a test was failing on `main`. The build matrix now depends on a job that runs the full checks on macOS and Linux, so a tag cannot publish what CI would have rejected.

## [0.2.0] - 2026-09-03

### Added
- **Drift detection.** Every transcript field falls back to zero when it is missing, so a harness renaming one showed 0 tokens and `$0.00` with no error: numbers wrong in the direction that looks like good news, and therefore never reported. A session that produced model responses while accounting for no tokens is now reported as a parser that has fallen behind the format, named by harness and version, and the row prints `?` instead of a believable zero. Both shapes are caught: the usage record renamed or moved, and the fields inside an intact record renamed. A partial rename, where some fields still read and the total is merely too low, is not detected, and there is a test asserting that gap rather than leaving it to be assumed.
- **Shell completions** for bash, zsh, fish, elvish and powershell via `--completions <shell>`, generated from the CLI definition so they cannot drift from the flags they describe. Homebrew builds them at install time.

### Changed
- **One row per Codex conversation, not per process.** A VS Code app-server hosts many conversations over its life, and attributing a single rollout to it collapsed them into one row carrying whichever was newest. Each live conversation now gets its own row with its own working directory, model and tokens. CPU, memory and the process tree stay on the row that owns the process, so a machine's totals are not multiplied by the number of conversations; the other rows show `·` rather than `0.0%`, which would read as an idle agent. A conversation already attributed to another process is skipped, and one that has not been written to within the activity window is treated as finished rather than as a live thread.

## [0.1.6] - 2026-09-03

Documentation only; no change to the binary's behaviour.

### Fixed
- Two README claims had gone stale with 0.1.5 and were telling users something untrue: that prices came from a static table, and that Codex output was unpriced until a user price table existed. It exists.

### Changed
- The Codex bug reports cited as evidence for orphaned-MCP detection are a table with links and state rather than four bare issue numbers. Checked against the GitHub API: three are still open, and the pull request that closed one path merged in April 2026.
- "How it works" is now "Where the numbers come from", and answers what a reader of the README actually needs to decide: which numbers are counted rather than estimated, which attribution is exact and which is a heuristic, that only metadata is read, and that nothing is written or sent anywhere. The mechanism it described duplicated `docs/architecture.md`, which it now links to.

## [0.1.5] - 2026-09-03

### Added
- Prices are data rather than code. The built-in table is `prices.toml`, compiled into the binary, and `~/.config/agent-top/prices.toml` (or `$XDG_CONFIG_HOME/agent-top/prices.toml`, or `$AGENT_TOP_PRICES`) is merged over it at startup. An entry whose prefix matches a built-in one replaces it, so a stale price can be corrected without waiting for a release; a new prefix is added, which is how a model this project ships no price for gets costed at all. Cache writes default to Anthropic's multipliers of the input price and can be set explicitly per model.
- `--prices` prints the effective table and whether each row came from the built-in file or yours, which is the quickest way to find out why a model shows `n/a`.

### Fixed
- A price file that cannot be parsed is reported on stderr and ignored, rather than silently leaving the built-in prices in place. A wrong cost is worse than a missing one.
- The golden tests priced with whatever `prices.toml` the developer happened to have in their home directory, so a contributor with one of their own would have seen the cost assertions fail for no visible reason. Both trackers now take a price table explicitly and the golden tests pass the built-in one.

## [0.1.4] - 2026-09-03

### Added
- Golden fixtures: two real transcripts, one per harness, reduced to the fields the parser reads and checked in with the exact numbers they should produce. The inline unit tests only prove the parser agrees with its author's description of the format; these pin the parse of a whole real session, cost included. A one-digit price typo that all twelve unit tests wave through fails here.

### Changed
- `agent-top-core` has a plainer README, and `docs/roadmap.md` no longer claims golden fixtures catch an upstream format change. They cannot: a fixture recorded at one harness version keeps passing after the harness moves on. Detecting a renamed field is a separate job, now listed separately, and it matters because every field falls back to zero when missing, so a rename shows a user 0 tokens rather than an error.
- `SpanLog::iter` is double-ended, so callers can take the newest spans without collecting the log.

## [0.1.3] - 2026-09-03

Documentation only; no change to the binary's behaviour.

### Changed
- `agent-top-core` has its own README on crates.io. Both crates inherited the workspace one, so the library's page rendered the tool's page and the two listings read as duplicates of each other. It now says what the library is, that anyone wanting the tool should install `agent-top` instead, and what its pre-1.0 stability amounts to.
- The README's install section lists every route — Homebrew, `cargo binstall`, `cargo install`, a clone, and the release tarballs — recommends `--locked`, and says how to upgrade.

## [0.1.2] - 2026-09-03

First release published to crates.io. No functional change from 0.1.1: the
version exists because a registry release needs one and 0.1.1 was already
tagged.

### Added
- Published on crates.io, so `cargo install agent-top` and `cargo binstall agent-top` work without a clone.

### Fixed
- Both crates were packaged without a README. The file lives at the workspace root, outside either package directory, so crates.io would have shown an empty page; it is now inherited through `[workspace.package]` and verified present in each package.
- The crates.io publish step skips a version already on the registry instead of failing on it, so a release job retried after a partial publish completes rather than dying on the half that succeeded.

## [0.1.1] - 2026-09-03

### Added
- `--replay <file>` renders a snapshot saved by `--json` in the full interactive UI, every key working, without reading anything on the local machine. The `--json` output was already the thing to attach to a bug report; this is what opens one. It also records the README demo, from a synthetic `docs/demo-snapshot.json` rather than from real sessions, which would otherwise publish real project names, working directories and session ids.
- A demo GIF in the README, regenerated by `vhs docs/demo.tape`.

### Changed
- Panel borders are a light slate and rounded, instead of a dark grey that read as noise beside the meters rather than as structure.
- The header totals are laid out as a table — fixed label column, numbers right-aligned in a column of their own — instead of four ragged lines whose values landed wherever the text ended. The sum over all agents is now labelled `total cost`; the per-agent detail pane still says `cost`.
- The sort direction moved from the header to the footer, next to the `s` key that changes it, freeing a header row for what the agents are costing the machine (`agent use  109.3%  cpu · 1.9G resident`).
- A manual run of the release workflow is now a credentials preflight: it verifies `HOMEBREW_TAP_TOKEN` can write to the tap and publishes nothing, so a bad token is found before a tag has put binaries in front of users. The tap bump is idempotent, so re-running a publish for an already-bumped tag succeeds instead of failing on an empty commit.

## [0.1.0] - 2026-09-03

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
