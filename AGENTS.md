# Agent and contributor guide

`agent-top` is a Rust workspace with two crates:

- `crates/agent-top-core` — discovery, transcript parsing, pricing, process trees. **No terminal dependency.** Everything here must be testable without a TTY and is what `--json` prints.
- `crates/agent-top` — the ratatui front end, CLI flags, formatting.

## Rules

- **Read-only.** agent-top observes; it never signals, writes to, or connects to an agent. A PR that adds `kill`, a socket client, or a hook that mutates agent state needs an RFC first (see the internal handbook).
- **Never guess a price.** If a model is not in `pricing.rs`, its tokens are `unpriced_tokens`. Update the table with the date it was checked.
- **Format notes live next to the parser.** When a harness changes its on-disk layout, update the module doc comment in `harness/<name>.rs` with the version you verified against.
- **Heuristics are labelled.** Anything attributed by cwd or by name matching sets `Attribution::CwdHeuristic` or is a `ProcKind` guess; the UI must not present it as exact.
- **Edition 2024, no `unsafe`, no async.** One refresh per tick is cheap enough; keep it simple until measurements say otherwise.
- Commit subjects follow Conventional Commits. No AI attribution trailers.

## The README demo

`docs/demo.gif` is recorded by `vhs docs/demo.tape` from `docs/demo-snapshot.json`,
a hand-authored snapshot replayed through `--replay`. Never re-record it against
a live machine: the frame would carry real project names, working directories
and session ids into a public README. Edit the JSON to change what the demo
shows.

## Checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --once
```

## Where the thinking is

Design documents (PRD, roadmap, RFCs, ADRs, decision log, debt register) are in the sibling repository `agent-top-internal-docs`, an mdBook. `docs/` in this repository holds only the public-facing vision, architecture and roadmap.
