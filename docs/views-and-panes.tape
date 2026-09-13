# The tmux recording for the Views and panes page. Regenerate with:
#
#   cargo build --release && vhs docs/views-and-panes.tape
#
# A real tmux runs inside the recording, on its own server (-L vhs) with a
# two-line config, so nothing from the machine's tmux setup is in the frames.
# The data is docs/demo-snapshot.json replayed through `--replay`; the `o`
# key splits a real pane and the pane runs the real binary on the same file.

Output docs/views-and-panes.gif

Set Shell "bash"
Set FontSize 12
Set Width 1400
Set Height 760
Set Padding 16
Set Framerate 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 55ms

Hide
# docs/tmux-demo.sh puts `agent-top` on PATH as a wrapper around the release
# build and the replay file, so the typed command is the one a user types,
# then starts tmux. The pane opened by `o` runs the binary itself.
Type "clear && . docs/tmux-demo.sh"
Enter
Sleep 1s
Type "export PS1='$ ' && clear"
Enter
Sleep 300ms
Show

Type "agent-top"
Sleep 500ms
Enter
Sleep 2500ms

# m: the MCP servers popup, with its Enter and o lines.
Type "m"
Sleep 3s

# o: the popup moves into a tmux pane on the right; focus stays here.
Type "o"
Sleep 4s

# Enter on a popup fills this terminal instead; Esc brings the table back.
Type "l"
Sleep 1500ms
Enter
Sleep 2500ms
Escape
Sleep 2500ms

Hide
Type "q"
Sleep 300ms
Type "tmux -L vhs kill-server"
Enter
Sleep 300ms
