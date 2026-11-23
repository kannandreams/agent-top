# Golden fixtures

Each `.jsonl` here is a real transcript from the harness and version in its
filename, reduced to the fields `agent-top` actually reads. Everything else was
dropped rather than masked, which is the only reliable way not to leak a prompt:
no message text, no tool inputs, no tool output, no real paths or session ids.
The one field that cannot simply be dropped is the output text of a tool nested
in a Codex code-mode call, because the parser measures its byte length. In
`codex-code-mode-0.154.jsonl` each such string is replaced by `x` repeated to
the original byte length, so the lengths are real and the text is gone.
The scripts that produced them are not checked in because they are not meant to
be re-run against someone else's machine; regenerate by hand if a new harness
version needs covering, then audit the result before committing it.

The Gemini fixture is the exception to "real": no Gemini CLI session had ever
run on the machine that added the adapter, so `gemini-0.58.jsonl` and the
subagent file under `0a1b2c3d-0000-4000-8000-000000000001/` were written by
Gemini CLI 0.58.0's own recorder (`ChatRecordingService` in
`@google/gemini-cli-core`), driven by `gemini-0.58.drive.mjs` with a scripted
conversation and a fake clock. The layout is the harness's; the token counts
are chosen. Placeholder prompts, empty tool arguments and a made-up project
path, so there is nothing to sanitise. Rerun the script against a newer
`@google/gemini-cli-core` to see whether the layout moved; replace the fixture
with a reduced real transcript when one exists.

`claude-fork-2.1.304.jsonl` is a window of a real session, not the whole of
it: the 52 lines of the turn that forked a subagent, with the fork's complete
transcript and its meta file under `claude-fork-2.1.304/subagents/`. The meta
file is reduced to `agentType` and `toolUseId`; its `description` is prompt
text and was dropped. The agent id is made up, and so is the file name that
carries it. Its totals are the window's, so they say nothing about what the
session cost.

`claude-compact-2.1.308.jsonl` is also a window: 274 lines of a real session
around an automatic compaction, from the prompt of the turn that ran into it
to the prompt after it. Claude Code upgraded from 2.1.308 to 2.1.310 midway, so
both versions appear. The `compact_boundary` line keeps `trigger`, `preTokens`
and `postTokens`; the compaction summary that follows it is prose and is
reduced to an empty `content` like every other prompt. Message and tool-use ids
are renumbered, and the session id and cwd are made up.

Each `.expected.json` next to a fixture is the parse the current code produces
from it. `tests/golden.rs` asserts the two match.

## When a golden test fails

Ask which of these it is.

**You changed the parser or the pricing table on purpose.** Re-record the
goldens and read the diff as part of the review, because that diff is the change
in what every user's numbers will say:

```sh
UPDATE_GOLDEN=1 cargo test -p agent-top-core --test golden
git diff crates/agent-top-core/tests/fixtures
```

**You did not mean to change anything.** Then the parse drifted, and the diff
says exactly which number moved.

## What these do not cover

A fixture is frozen at the version in its name, so it keeps passing after the
harness moves on. These catch regressions in our own parsing. They cannot catch
an upstream field rename, which is a separate problem: every field falls back to
zero when missing, so a rename shows a user 0 tokens and no error at all. See
the drift-detection entry in `docs/roadmap.md`.
