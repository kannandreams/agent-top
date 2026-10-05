---
description: "agent-top sync reads the transcripts on disk into the local store, skipping the ones that have not changed, and keeps a session after its transcript is deleted."
---

# Sync

`agent-top sync` reads the transcripts on disk into the [local store](index.md).

```sh
agent-top sync                  # every transcript that changed since the last sync
agent-top sync --since 7d       # only transcripts written in the last week
agent-top sync --db ./mine.db   # a different file
agent-top sync --json           # what the sync did, structured
```

```text
240 transcripts: 49 stored, 191 unchanged
/Users/you/.local/share/agent-top/agent-top.db holds 240 sessions
```

## What it reads

The first sync reads every transcript on disk. Later ones read only transcripts whose size or modification time changed, so a sync with nothing new takes a fraction of a second. OpenCode and Kodelet sessions live in a database rather than a file, so they are read every time; each one is a few small queries.

A changed transcript is read in full, and its session's rows are replaced in one transaction.

## What it keeps

- A session stays in the store after its transcript is deleted.
- A transcript that reads as empty where the store already has a session does not replace it.
- A transcript that cannot be read is reported on stderr and skipped. Its stored rows are untouched.

After you upgrade agent-top, the next sync re-reads every transcript still on disk, so a price or parser fix reaches it.

## Running it on a schedule

Sync as often as you like. A transcript has to be in the store before the harness deletes it, so once a day is enough for Claude Code's 30-day default. With cron, using the path `which agent-top` prints:

```text
0 12 * * *  /opt/homebrew/bin/agent-top sync > /dev/null
```

A long-running `agent-top serve` that syncs on an interval is planned.
