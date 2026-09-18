# The `--once` screenshot for docs/live-view.md. Regenerate with:
#
#   cargo build --release && vhs docs/once.tape
#
# The same synthetic docs/demo-snapshot.json as the other screenshots. A tape of
# its own because the plain table needs a taller frame than the TUI.

Output docs/once-scratch.gif

Set Shell "bash"
Set FontSize 13
Set Width 1280
Set Height 1290
Set Padding 16
Set Theme "Catppuccin Mocha"
Set TypingSpeed 0ms

Hide
Type "export PS1='$ '"
Enter
Type "alias agent-top='./target/release/agent-top --replay docs/demo-snapshot.json'"
Enter
Type "clear"
Enter
Sleep 800ms
Show

Type "agent-top --once"
Enter
Sleep 1500ms
Screenshot docs/screenshots/once.png
Sleep 300ms
