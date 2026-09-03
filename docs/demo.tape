# Demo recording for the README. Regenerate with:
#
#   cargo build --release && vhs docs/demo.tape
#
# The data is docs/demo-snapshot.json, a synthetic snapshot replayed through
# `--replay`, not a recording of a real machine: a GIF of live sessions would
# publish real project names, working directories and session ids.

Output docs/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1240
Set Height 720
Set Padding 18
Set Framerate 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms

# Setup, off camera: true colour so the meter ramps render, and an alias so the
# visible command is the one a user would actually type.
Hide
Type "export COLORTERM=truecolor PS1='$ '"
Enter
Type "alias agent-top='./target/release/agent-top --replay docs/demo-snapshot.json'"
Enter
Type "clear"
Enter
Show

Type "agent-top"
Sleep 600ms
Enter

# The table: every agent on the machine, what it is spending, and two MCP
# servers whose agent is gone.
Sleep 3s

# Move down the table; the detail pane follows the selection.
Down
Sleep 1400ms
Down
Sleep 1400ms
Up
Up
Sleep 1200ms

# Tab swaps the process tree for the tool trace.
Tab
Sleep 4s

# Another agent's trace.
Down
Sleep 2500ms
Up
Sleep 1500ms

# Back to the process tree, where the orphaned servers are listed.
Tab
Sleep 2500ms

Type "q"
Sleep 800ms
