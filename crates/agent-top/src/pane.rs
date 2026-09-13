//! Opening a panel in its own pane of the terminal multiplexer agent-top is
//! running inside. The pane runs agent-top itself, on the dedicated
//! subcommand for that panel (`agent-top mcp` and the like), with the same
//! arguments as this run. That is the whole of what is started: no shell
//! script, no other program, nothing that touches an agent.

use std::path::Path;

/// A terminal multiplexer agent-top knows how to ask for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    Tmux,
    Zellij,
    WezTerm,
    Kitty,
}

impl Multiplexer {
    /// Which multiplexer this process is running inside, from the variables
    /// each of them sets, or `None` when it is a plain terminal.
    pub fn detect() -> Option<Multiplexer> {
        Multiplexer::from_env(|k| std::env::var_os(k).is_some())
    }

    /// tmux is checked first: run inside WezTerm or kitty, it is the innermost
    /// thing that owns panes, and the outer terminal's variable is still set.
    fn from_env(set: impl Fn(&str) -> bool) -> Option<Multiplexer> {
        if set("TMUX") {
            Some(Multiplexer::Tmux)
        } else if set("ZELLIJ") {
            Some(Multiplexer::Zellij)
        } else if set("WEZTERM_PANE") {
            Some(Multiplexer::WezTerm)
        } else if set("KITTY_WINDOW_ID") {
            Some(Multiplexer::Kitty)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Multiplexer::Tmux => "tmux",
            Multiplexer::Zellij => "zellij",
            Multiplexer::WezTerm => "wezterm",
            Multiplexer::Kitty => "kitty",
        }
    }

    /// The command that opens a pane to the right of this one running
    /// `program args`, started in `cwd`. Focus stays on this pane where the
    /// multiplexer offers the choice (tmux `-d`, kitty `--keep-focus`);
    /// zellij and WezTerm move to the new pane.
    pub fn split(self, program: &str, args: &[String], cwd: &Path) -> (&'static str, Vec<String>) {
        let cwd = cwd.to_string_lossy().into_owned();
        let argv = || std::iter::once(program.to_string()).chain(args.iter().cloned());
        match self {
            // tmux takes the command as one shell string, so it is quoted here
            // rather than passed as separate arguments.
            Multiplexer::Tmux => {
                let shell = argv().map(|a| sh_quote(&a)).collect::<Vec<_>>().join(" ");
                ("tmux", vec!["split-window".into(), "-h".into(), "-d".into(), "-c".into(), cwd, shell])
            }
            Multiplexer::Zellij => {
                let mut v: Vec<String> =
                    ["action", "new-pane", "--direction", "right", "--close-on-exit", "--cwd"].iter().map(|s| s.to_string()).collect();
                v.push(cwd);
                v.push("--".into());
                v.extend(argv());
                ("zellij", v)
            }
            Multiplexer::WezTerm => {
                let mut v: Vec<String> = ["cli", "split-pane", "--right", "--cwd"].iter().map(|s| s.to_string()).collect();
                v.push(cwd);
                v.push("--".into());
                v.extend(argv());
                ("wezterm", v)
            }
            Multiplexer::Kitty => {
                let mut v: Vec<String> =
                    ["@", "launch", "--type=window", "--location=vsplit", "--keep-focus"].iter().map(|s| s.to_string()).collect();
                v.push(format!("--cwd={cwd}"));
                v.extend(argv());
                ("kitten", v)
            }
        }
    }

    /// The command as the popup shows it before `o` is pressed: the same
    /// builder with the working directory and forwarded flags left out, so
    /// it fits on one line and still names every program involved.
    pub fn describe(self, subcommand: &str) -> String {
        let (prog, args) = self.split("agent-top", &[subcommand.to_string()], Path::new("."));
        let mut shown: Vec<String> = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            // The working directory is the flag and the value after it, or one
            // `--cwd=` word; neither belongs on the line the user reads.
            if a == "-c" || a == "--cwd" {
                it.next();
                continue;
            }
            if a.starts_with("--cwd=") {
                continue;
            }
            shown.push(sh_quote(a));
        }
        format!("{prog} {}", shown.join(" "))
    }
}

/// Quote one word for `sh`, leaving plain words alone.
fn sh_quote(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-./=:+,@%".contains(c));
    if plain { s.to_string() } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

/// Open `subcommand` in a new pane: this binary, the flags of this run, then
/// the subcommand. Returns the one-line notice for the footer either way. The
/// multiplexer's own output is discarded so it cannot land on the TUI.
pub fn open(mux: Multiplexer, forwarded: &[String], subcommand: &str) -> Result<String, String> {
    let program = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agent-top".to_string());
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut args = forwarded.to_vec();
    args.push(subcommand.to_string());
    let (prog, argv) = mux.split(&program, &args, &cwd);
    let status = std::process::Command::new(prog)
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(format!("opened agent-top {subcommand} in a {} pane", mux.label())),
        Ok(s) => Err(format!("{prog} exited with {s}; run agent-top {subcommand} in another terminal")),
        Err(e) => Err(format!("could not run {prog}: {e}; run agent-top {subcommand} in another terminal")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_wins_when_nested_and_a_plain_terminal_is_none() {
        let env = |vars: &'static [&'static str]| move |k: &str| vars.contains(&k);
        assert_eq!(Multiplexer::from_env(env(&["TMUX", "WEZTERM_PANE"])), Some(Multiplexer::Tmux));
        assert_eq!(Multiplexer::from_env(env(&["ZELLIJ", "KITTY_WINDOW_ID"])), Some(Multiplexer::Zellij));
        assert_eq!(Multiplexer::from_env(env(&["WEZTERM_PANE"])), Some(Multiplexer::WezTerm));
        assert_eq!(Multiplexer::from_env(env(&["KITTY_WINDOW_ID"])), Some(Multiplexer::Kitty));
        assert_eq!(Multiplexer::from_env(env(&["TERM"])), None);
    }

    /// Each multiplexer gets exactly the command line the docs table shows:
    /// a split to the right, started in the working directory, running this
    /// binary with the forwarded flags and the subcommand last.
    #[test]
    fn every_multiplexer_builds_the_documented_split_command() {
        let args = vec!["--replay".to_string(), "/tmp/a snapshot.json".to_string(), "mcp".to_string()];
        let cwd = Path::new("/work/repo");
        let (prog, v) = Multiplexer::Tmux.split("/usr/local/bin/agent-top", &args, cwd);
        assert_eq!(prog, "tmux");
        assert_eq!(v, ["split-window", "-h", "-d", "-c", "/work/repo", "/usr/local/bin/agent-top --replay '/tmp/a snapshot.json' mcp"]);

        let (prog, v) = Multiplexer::Zellij.split("agent-top", &args, cwd);
        assert_eq!(prog, "zellij");
        assert_eq!(
            v,
            [
                "action",
                "new-pane",
                "--direction",
                "right",
                "--close-on-exit",
                "--cwd",
                "/work/repo",
                "--",
                "agent-top",
                "--replay",
                "/tmp/a snapshot.json",
                "mcp"
            ]
        );

        let (prog, v) = Multiplexer::WezTerm.split("agent-top", &args, cwd);
        assert_eq!(prog, "wezterm");
        assert_eq!(
            v,
            ["cli", "split-pane", "--right", "--cwd", "/work/repo", "--", "agent-top", "--replay", "/tmp/a snapshot.json", "mcp"]
        );

        let (prog, v) = Multiplexer::Kitty.split("agent-top", &args, cwd);
        assert_eq!(prog, "kitten");
        assert_eq!(
            v,
            [
                "@",
                "launch",
                "--type=window",
                "--location=vsplit",
                "--keep-focus",
                "--cwd=/work/repo",
                "agent-top",
                "--replay",
                "/tmp/a snapshot.json",
                "mcp"
            ]
        );
    }

    #[test]
    fn the_popup_shows_the_command_without_the_directory() {
        assert_eq!(Multiplexer::Tmux.describe("mcp"), "tmux split-window -h -d 'agent-top mcp'");
        assert_eq!(Multiplexer::Zellij.describe("advice"), "zellij action new-pane --direction right --close-on-exit -- agent-top advice");
        assert_eq!(Multiplexer::WezTerm.describe("slow"), "wezterm cli split-pane --right -- agent-top slow");
        assert_eq!(Multiplexer::Kitty.describe("fails"), "kitten @ launch --type=window --location=vsplit --keep-focus agent-top fails");
    }

    #[test]
    fn shell_quoting_leaves_plain_words_alone() {
        assert_eq!(sh_quote("agent-top"), "agent-top");
        assert_eq!(sh_quote("/tmp/x.json"), "/tmp/x.json");
        assert_eq!(sh_quote("it's here"), "'it'\\''s here'");
        assert_eq!(sh_quote(""), "''");
    }
}
