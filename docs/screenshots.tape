# Screenshots for the docs and blog. Regenerate with:
#
#   cargo build --release && vhs docs/screenshots.tape
#
# Every frame is the synthetic docs/demo-snapshot.json replayed through
# `--replay`, never a live machine, so nothing private is in the images.

Set Shell "bash"
Set FontSize 13
Set Width 1280
Set Height 960
Set Padding 16
Set Framerate 30
Set Theme "Catppuccin Mocha"
Set TypingSpeed 0ms

Hide
Type "export COLORTERM=truecolor PS1='$ '"
Enter
Type "alias agent-top='./target/release/agent-top --replay docs/demo-snapshot.json'"
Enter
Type "clear"
Enter
Type "agent-top"
Enter
Sleep 2s
Show

# The table and the detail pane, process tree side, for the busiest agent.
Screenshot docs/screenshots/overview.png
Sleep 1500ms
Sleep 500ms

# Tab: the tool trace for the same agent.
Tab
Sleep 1s
Screenshot docs/screenshots/tool-trace.png
Sleep 1500ms
Tab
Sleep 500ms

# The Codex row: a guessed process match, a rate limit, a growing server.
Down
Down
Down
Sleep 1s
Screenshot docs/screenshots/codex-detail.png
Sleep 1500ms

# The stopped row: a transcript with no process, and the orphan list.
Type "G"
Sleep 1s
Screenshot docs/screenshots/stopped-and-orphans.png
Sleep 1500ms
Type "g"
Sleep 500ms

# The popups.
Type "a"
Sleep 1s
Screenshot docs/screenshots/advice.png
Sleep 1500ms
Type "a"
Type "l"
Sleep 1s
Screenshot docs/screenshots/slowest-tools.png
Sleep 1500ms
Type "l"
Type "f"
Sleep 1s
Screenshot docs/screenshots/failed-tools.png
Sleep 1500ms
Type "f"
Type "?"
Sleep 1s
Screenshot docs/screenshots/help.png
Sleep 1500ms
Type "?"
Sleep 300ms
Type "q"
Sleep 500ms
