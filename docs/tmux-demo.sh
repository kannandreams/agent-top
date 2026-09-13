#!/bin/sh
# Setup for docs/views-and-panes.tape, sourced off camera. Puts an `agent-top`
# on PATH that replays the demo snapshot through the release build, writes a
# minimal tmux config, and starts a tmux server of its own (-L vhs) so nothing
# from the machine's tmux setup is in the recording. The shell inside tmux is
# a bare bash: a login shell would put Homebrew back in front of the wrapper
# on PATH and the recording would show the live machine instead of the replay.
set -e
dir=/tmp/at-vhs
mkdir -p "$dir"
cat > "$dir/agent-top" <<WRAP
#!/bin/sh
exec "$PWD/target/release/agent-top" --replay "$PWD/docs/demo-snapshot.json" "\$@"
WRAP
chmod +x "$dir/agent-top"
cat > "$dir/tmux.conf" <<CONF
set -g default-terminal tmux-256color
set -ga terminal-overrides ,*:Tc
set -g status-style bg=#1f2229,fg=#9aa1b0
set -g status-right ""
set -g pane-border-style fg=#30343e
set -g pane-active-border-style fg=#6a90f6
set -g default-shell /bin/bash
set -g default-command "bash --noprofile --norc"
CONF
export PATH="$dir:$PATH" COLORTERM=truecolor
tmux -L vhs kill-server 2>/dev/null || true
exec tmux -L vhs -f "$dir/tmux.conf" new-session -s agents
