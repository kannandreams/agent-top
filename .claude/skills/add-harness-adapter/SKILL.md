---
name: add-harness-adapter
description: Add or update an agent-top transcript adapter for a coding-agent harness (Aider, Copilot CLI, cursor-agent, Amp, a new Claude/Codex/Gemini/OpenCode format version). Use when asked to "support harness X", "read X's transcripts", "count X's tokens/cost/MCP calls", when a golden or drift test fails after an upstream release, or when a harness shows up in the table with no tokens, no cost, or the wrong attribution.
---

# Adding a harness to agent-top

The collector never names a harness. Everything harness-specific lives behind
`HarnessAdapter` in `crates/agent-top-core/src/harness/<name>.rs`, so a new
harness is one module plus a short, fixed list of registrations. Work in that
order; the touchpoint checklist below is the part that gets forgotten.

## 1. Learn the format before writing any Rust

Do not guess the layout from another harness. Find the harness's own writer —
the recorder module in its npm package, its SQLite schema, its session
directory — and read it. Then write the findings into the module doc comment of
the new adapter *first*, with the version and the date verified, exactly as
`harness/gemini.rs` and `harness/opencode.rs` do. That comment is the contract
the parser is reviewed against, and AGENTS.md requires it to be updated
whenever the on-disk layout moves.

Answer these before coding:

- Where do transcripts live, and how does a path map back to a working directory?
- What is the record for one model response, and where is its token usage? Which
  numbers are cumulative and which are per-response (double counting lives here).
- Does the harness price the session itself? If so prefer its figure over our
  table (`opencode.rs` explains why), and set `unpriced_tokens` to 0.
- How are tool calls started and finished, and what pairs them (a call id)?
- Subagents: separate file, separate row in a table, or a flag on the message?
- Does the harness publish a registry of its processes (pid → session), or must
  a process be matched by cwd and start time?
- Does it log rate limits, MCP calls, or a web-search tool?

## 2. Implement the adapter

- New module `crates/agent-top-core/src/harness/<name>.rs` with two types: an
  `XAdapter` implementing `HarnessAdapter` and an `XTranscript` implementing
  `SessionTracker`.
- Follow the constructor shape of the JSONL adapters:
  `XTranscript::new(path).with_prices(table).with_spans(retention)`.
- Tail append-only logs with `jsonl::TailReader::read_new_lines(budget)` and
  respect `REFRESH_BUDGET_BYTES`: `refresh` returns `true` when data is left, so
  a 100 MB transcript streams over several ticks instead of freezing a frame.
- Fill `SessionSummary` and nothing else. Spans go through `SpanLog::open` /
  `close` / `open_kind` (`SpanKind::Turn`, `Inference`, `Tool`) — never push
  spans directly, and pair by call id, because calls interleave.
- Count `ParseHealth` as you parse: `billable_messages` for every model
  response, `usage_records` for every usage record found, `empty_usage_records`
  for the ones that read as all-zero. This is what turns a silent upstream
  rename into a visible drift warning instead of a believable `$0.00`.
- MCP calls go into `summary.mcp` (a `BTreeMap<String, McpUsage>` keyed by
  server name), counted once per call. Do not also fold an MCP end-event into
  `tool_calls` if the call is already counted from its start record.
- Attribution: exact only when the harness gives real evidence (a registry
  entry, an open file handle). Otherwise return the matching
  `Attribution::CwdHeuristic` variant — AGENTS.md forbids presenting a guess as
  exact.
- Read metadata fields only. No prompt text, no tool inputs, no tool output. And
  nothing that writes, signals or connects: open the store read-only, the way
  `opencode.rs::open_ro` opens SQLite with `SQLITE_OPEN_READ_ONLY` so agent-top
  can never write to or lock the file the harness is using.
- No file per session (a database, a server)? Mint a virtual path that stands
  for one session and hand that to the rest of the system, as `opencode.rs`
  does; `SessionTracker::path` is an identifier, not necessarily a real file.

## 3. Register it — the checklist

1. `harness/mod.rs`: `pub mod <name>;` and a `Box::new(<Name>Adapter::default())`
   in `adapters()`. Order matters for `detect` only — a harness whose first
   lines look like another's must be asked first (Gemini before Claude).
2. `model.rs`: a `Harness` variant and its `label()` string, if it is not
   already there (`Aider`, `Copilot`, `Cursor` exist with no adapter yet).
3. `process.rs::classify_agent`: recognise the process. Node-hosted CLIs appear
   as `node <path>/cli.js`, so match the script path too.
4. `prices.toml` (crate root): every model the harness can run, with the date
   the price was checked. A missing model is `unpriced_tokens`, never a guess.
5. `collector.rs` detect test (the fixture list near the bottom of the file).
6. Docs: `README.md` (the harness list in the intro, the bullets, and the
   per-harness support table near the end), `crates/agent-top-core/README.md`,
   `docs/architecture.md`, `docs/accounting.md` if pricing differs,
   and `CHANGELOG.md`.

## 4. Golden fixture

Unit tests only prove the parser agrees with its author. Add a fixture:

- Capture a real session, then **reduce it to the fields the parser reads** —
  drop everything else rather than masking it. Audit the result for prompts,
  tool output, real paths and session ids before committing.
- Name it `<harness>-<version>.jsonl` in
  `crates/agent-top-core/tests/fixtures/`, add a `#[test]` in `tests/golden.rs`
  next to `gemini_0_58`, and record the expectation with
  `UPDATE_GOLDEN=1 cargo test -p agent-top-core --test golden`.
- No real session available? Drive the harness's own recorder with a scripted
  conversation and a fake clock, and check the driver in — see
  `gemini-0.58.drive.mjs` and the fixtures README.
- Add a case to `tests/drift.rs` that renames the usage field the way an
  upstream release eventually will, and assert `fields_unrecognised()`.

When a golden test fails later, decide which it is: an intended parser change
(re-record and review the diff, because that diff is every user's numbers) or a
drift you did not intend.

## 5. Checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --once     # with a live session of the new harness, if possible
```

Do not tag or release. Changes accumulate on `main` until the user explicitly
asks for a release; the release runbook is in the internal handbook.
