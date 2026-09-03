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

## Golden fixtures

`crates/agent-top-core/tests/fixtures` holds real transcripts, reduced to the
fields the parser reads, with the exact numbers each should produce. They cover
what the inline unit tests cannot: a whole real session, at scale, including
cost. A price-table edit that the unit tests wave through will fail here.

When a golden test fails, decide which it is. If you meant to change the parse,
re-record with `UPDATE_GOLDEN=1 cargo test -p agent-top-core --test golden` and
review the diff, because that diff is the change to every user's numbers. If you
did not, the parse drifted.

Adding a fixture means sanitising it first: drop every field the parser does not
read rather than masking it, then audit the result for real paths, prompts and
tool output before committing. See the README in that directory.

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
