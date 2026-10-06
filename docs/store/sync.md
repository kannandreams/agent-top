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

A session is kept only if a sync runs before the harness deletes its transcript. Claude Code deletes after 30 days by default, so a sync once a day is enough. The setups below run `agent-top sync` daily; each run takes about a second and nothing stays running in between.

### macOS: launchd

Save this as `~/Library/LaunchAgents/dev.agenttop.sync.plist`. The path in `ProgramArguments` is where Homebrew installs on Apple silicon; use the one `which agent-top` prints.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.agenttop.sync</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/homebrew/bin/agent-top</string>
    <string>sync</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>12</integer>
    <key>Minute</key>
    <integer>0</integer>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/tmp/agent-top-sync.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/agent-top-sync.log</string>
</dict>
</plist>
```

Then load it:

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.agenttop.sync.plist
```

It syncs once when loaded and at log-in (`RunAtLoad`), then daily at 12:00. If the Mac is asleep at 12:00, launchd runs the sync when it wakes. The output of the last run is in `/tmp/agent-top-sync.log`. To stop it:

```sh
launchctl bootout gui/$(id -u)/dev.agenttop.sync
```

### Linux: a systemd user timer

`~/.config/systemd/user/agent-top-sync.service`:

```ini
[Unit]
Description=agent-top sync

[Service]
Type=oneshot
ExecStart=%h/.cargo/bin/agent-top sync
```

`~/.config/systemd/user/agent-top-sync.timer`:

```ini
[Unit]
Description=Daily agent-top sync

[Timer]
OnCalendar=daily
Persistent=true

[Install]
WantedBy=timers.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now agent-top-sync.timer
```

`Persistent=true` runs a sync missed while the machine was off. Use the path `which agent-top` prints in `ExecStart`.

### cron

```text
0 12 * * *  /opt/homebrew/bin/agent-top sync > /dev/null
```

cron skips a run the machine was asleep or off for.
