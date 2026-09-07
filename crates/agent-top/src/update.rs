//! The one outbound call agent-top makes on its own: an update check.
//!
//! It asks crates.io for the latest published version of `agent-top` and
//! nothing else. It sends no data about you, your agents, your sessions or your
//! machine, only a generic User-Agent, so it does not break the promise that
//! matters: your data never leaves. The result is cached so it runs at most
//! once a day, it happens on a background thread so the UI never waits, it is
//! silent when there is no network, and `AGENT_TOP_NO_UPDATE_CHECK=1` turns it
//! off entirely.
//!
//! When a newer version is known, the TUI asks once whether to upgrade. Saying
//! yes runs the installer that put agent-top here (Homebrew or cargo) in the
//! terminal, in the open, and restarts agent-top on the new binary. That is
//! the one command agent-top runs that changes the machine, it changes only
//! agent-top itself, and it runs only on that keypress. Saying no is
//! remembered for that version, so the question is not asked again until the
//! next release; the footer badge stays.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const URL: &str = "https://crates.io/api/v1/crates/agent-top";
const USER_AGENT: &str = "agent-top-update-check";
/// Check the network at most this often; otherwise the cached answer stands.
const MAX_AGE_SECS: u64 = 24 * 60 * 60;
/// This build's version.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A handle the UI reads each frame: `Some(latest)` when a newer version than
/// this build is known, `None` otherwise.
pub type Latest = Arc<Mutex<Option<String>>>;

/// Whether the user has turned the check off.
pub fn disabled() -> bool {
    std::env::var_os("AGENT_TOP_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty() && v != "0")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cache_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(dir.join("agent-top").join("update-check.json"))
}

/// What the cache file holds: when the network was last asked, what it said,
/// and which version the user has declined to be asked about again.
#[derive(Debug, Clone, Default, PartialEq)]
struct Cache {
    checked_at: u64,
    latest: Option<String>,
    dismissed: Option<String>,
}

fn read_cache_at(path: &std::path::Path) -> Option<Cache> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Cache {
        checked_at: v.get("checked_at").and_then(|x| x.as_u64()).unwrap_or(0),
        latest: v.get("latest").and_then(|x| x.as_str()).map(str::to_string),
        dismissed: v.get("dismissed").and_then(|x| x.as_str()).map(str::to_string),
    })
}

fn write_cache_at(path: &std::path::Path, c: &Cache) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let doc = serde_json::json!({ "checked_at": c.checked_at, "latest": c.latest, "dismissed": c.dismissed });
    let _ = std::fs::write(path, doc.to_string());
}

fn read_cache() -> Option<Cache> {
    read_cache_at(&cache_path()?)
}

/// Record a fresh answer from the network, keeping any dismissal.
fn record_latest(latest: &str) {
    let Some(path) = cache_path() else { return };
    let mut c = read_cache_at(&path).unwrap_or_default();
    c.checked_at = now_secs();
    c.latest = Some(latest.to_string());
    write_cache_at(&path, &c);
}

/// The version the user last said "not now" to, if any.
pub fn dismissed() -> Option<String> {
    read_cache()?.dismissed
}

/// Remember that the user does not want to be asked about `version` again.
/// The footer badge keeps showing it; only the question goes quiet.
pub fn dismiss(version: &str) {
    let Some(path) = cache_path() else { return };
    let mut c = read_cache_at(&path).unwrap_or_default();
    c.dismissed = Some(version.to_string());
    write_cache_at(&path, &c);
}

/// How this binary got here, judged from where it is. Decides which command
/// an upgrade runs; anything unrecognised is left to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installer {
    Homebrew,
    /// Under `~/.cargo/bin` with `cargo-binstall` on PATH: the prebuilt route.
    CargoBinstall,
    /// Under `~/.cargo/bin` without it: rebuilt from crates.io.
    CargoInstall,
    Unknown,
}

impl Installer {
    pub fn detect() -> Installer {
        let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
        let cargo_home =
            std::env::var_os("CARGO_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
        Installer::from_path(exe.as_deref(), cargo_home.as_deref(), on_path("cargo-binstall"))
    }

    fn from_path(exe: Option<&std::path::Path>, cargo_home: Option<&std::path::Path>, binstall: bool) -> Installer {
        let Some(exe) = exe else { return Installer::Unknown };
        let text = exe.to_string_lossy();
        if text.contains("/Cellar/agent-top/") || text.contains("/homebrew/") || text.contains("/linuxbrew/") {
            return Installer::Homebrew;
        }
        if cargo_home.is_some_and(|c| exe.starts_with(c.join("bin"))) {
            return if binstall { Installer::CargoBinstall } else { Installer::CargoInstall };
        }
        Installer::Unknown
    }

    /// The commands an upgrade runs, in order, or `None` when the installer
    /// is not one agent-top knows how to drive.
    pub fn steps(self) -> Option<Vec<(&'static str, Vec<&'static str>)>> {
        match self {
            // `brew update` first: the tap formula is fetched by it, and an
            // upgrade without it reports "already up to date" against the
            // stale formula.
            Installer::Homebrew => Some(vec![("brew", vec!["update"]), ("brew", vec!["upgrade", "agent-top"])]),
            Installer::CargoBinstall => Some(vec![("cargo", vec!["binstall", "-y", "agent-top"])]),
            Installer::CargoInstall => Some(vec![("cargo", vec!["install", "--locked", "agent-top"])]),
            Installer::Unknown => None,
        }
    }

    /// The steps as one line, for the popup and the terminal.
    pub fn command_line(self) -> Option<String> {
        let steps = self.steps()?;
        Some(steps.iter().map(|(p, a)| format!("{p} {}", a.join(" "))).collect::<Vec<_>>().join(" && "))
    }
}

/// Whether a program of that name is on PATH.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file())).unwrap_or(false)
}

/// Run the upgrade in the terminal (after the TUI has been restored), then
/// start agent-top again on the new binary with the same arguments. Every
/// command is printed before it runs. Fails, with the command to run by
/// hand, when the installer is unknown or a step exits non-zero.
pub fn upgrade(latest: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    let installer = Installer::detect();
    let Some(steps) = installer.steps() else {
        anyhow::bail!(
            "agent-top v{latest} is available but this binary was not installed by Homebrew or cargo, so agent-top will not \
             guess how to replace it. Upgrade with one of:\n  brew update && brew upgrade agent-top\n  cargo install agent-top\n  \
             https://github.com/kannandreams/agent-top/releases/latest"
        );
    };
    eprintln!("agent-top: upgrading v{CURRENT} → v{latest}");
    for (prog, args) in steps {
        eprintln!("$ {prog} {}", args.join(" "));
        let status = std::process::Command::new(prog).args(&args).status().with_context(|| format!("running {prog}"))?;
        if !status.success() {
            anyhow::bail!("{prog} exited with {status}; agent-top is still v{CURRENT}");
        }
    }
    eprintln!("agent-top: upgraded; starting v{latest}");
    restart()
}

/// Replace this process with a fresh `agent-top` run the way the user typed
/// it. `argv[0]` rather than the executable path: after a Homebrew upgrade
/// the old Cellar path is gone, while the name on PATH now resolves to the
/// new one.
fn restart() -> anyhow::Result<()> {
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_else(|| "agent-top".into());
    let mut cmd = std::process::Command::new(argv0);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        Err(anyhow::anyhow!("could not restart agent-top ({err}); run it again by hand"))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    }
}

/// `major.minor.patch`, ignoring any pre-release or build suffix.
fn parse(v: &str) -> (u64, u64, u64) {
    let core = v.trim().split(['-', '+']).next().unwrap_or(v);
    let mut it = core.split('.').map(|n| n.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// `latest` when it is strictly newer than the running version, else `None`.
fn newer(latest: &str) -> Option<String> {
    (parse(latest) > parse(CURRENT)).then(|| latest.to_string())
}

/// Ask crates.io for the latest version. One GET, a generic User-Agent, no
/// body sent. Returns the version string, or `None` on any failure.
fn fetch() -> Option<String> {
    let mut resp = ureq::get(URL).header("User-Agent", USER_AGENT).call().ok()?;
    let body = resp.body_mut().read_to_string().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("crate")?.get("max_version")?.as_str().map(str::to_string)
}

/// Start the update check and return the handle the footer reads. Uses the
/// cached answer when it is fresh; when it is stale (or absent), shows the last
/// known answer immediately and refreshes on a background thread. Does nothing
/// when disabled.
pub fn start() -> Latest {
    let shared: Latest = Arc::new(Mutex::new(None));
    if disabled() {
        return shared;
    }
    let cached = read_cache();
    // Show whatever the cache knows right away.
    if let Some(latest) = cached.as_ref().and_then(|c| c.latest.as_deref())
        && let Some(n) = newer(latest)
    {
        *shared.lock().unwrap() = Some(n);
    }
    let fresh = cached.as_ref().map(|c| c.latest.is_some() && now_secs().saturating_sub(c.checked_at) < MAX_AGE_SECS).unwrap_or(false);
    if fresh {
        return shared;
    }
    // Stale or missing: refresh without blocking the UI.
    let handle = shared.clone();
    std::thread::spawn(move || {
        if let Some(latest) = fetch() {
            record_latest(&latest);
            *handle.lock().unwrap() = newer(&latest);
        }
    });
    shared
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_ignores_suffixes_and_widths() {
        assert!(parse("0.13.0") > parse("0.12.2"));
        assert!(parse("1.0.0") > parse("0.99.99"));
        assert_eq!(parse("0.12.1"), (0, 12, 1));
        assert_eq!(parse("0.12.1-rc.1"), (0, 12, 1));
        assert_eq!(parse("0.12"), (0, 12, 0));
    }

    #[test]
    fn the_installer_is_judged_from_where_the_binary_is() {
        use std::path::Path;
        let cargo = Path::new("/Users/me/.cargo");
        let at = |p: &str| Installer::from_path(Some(Path::new(p)), Some(cargo), false);
        assert_eq!(at("/opt/homebrew/Cellar/agent-top/0.14.0/bin/agent-top"), Installer::Homebrew);
        assert_eq!(at("/home/linuxbrew/.linuxbrew/Cellar/agent-top/0.14.0/bin/agent-top"), Installer::Homebrew);
        assert_eq!(at("/Users/me/.cargo/bin/agent-top"), Installer::CargoInstall);
        assert_eq!(Installer::from_path(Some(Path::new("/Users/me/.cargo/bin/agent-top")), Some(cargo), true), Installer::CargoBinstall);
        assert_eq!(at("/usr/local/bin/agent-top"), Installer::Unknown);
        assert_eq!(Installer::from_path(None, Some(cargo), true), Installer::Unknown);
        assert_eq!(Installer::Homebrew.command_line().as_deref(), Some("brew update && brew upgrade agent-top"));
        assert_eq!(Installer::CargoBinstall.command_line().as_deref(), Some("cargo binstall -y agent-top"));
        assert_eq!(Installer::CargoInstall.command_line().as_deref(), Some("cargo install --locked agent-top"));
        assert_eq!(Installer::Unknown.command_line(), None);
    }

    /// A dismissal survives the next network answer, and the answer survives
    /// a dismissal: the two live in one file and neither overwrites the other.
    #[test]
    fn dismissal_and_latest_share_the_cache_without_clobbering() {
        let dir = std::env::temp_dir().join(format!("agent-top-update-test-{}", std::process::id()));
        let path = dir.join("update-check.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read_cache_at(&path), None);
        write_cache_at(&path, &Cache { checked_at: 5, latest: Some("0.15.0".into()), dismissed: None });
        let mut c = read_cache_at(&path).unwrap();
        assert_eq!(c.latest.as_deref(), Some("0.15.0"));
        c.dismissed = Some("0.15.0".into());
        write_cache_at(&path, &c);
        let mut c = read_cache_at(&path).unwrap();
        c.checked_at = 9;
        c.latest = Some("0.16.0".into());
        write_cache_at(&path, &c);
        let c = read_cache_at(&path).unwrap();
        assert_eq!((c.checked_at, c.latest.as_deref(), c.dismissed.as_deref()), (9, Some("0.16.0"), Some("0.15.0")));
        // A file from before dismissals existed still reads.
        std::fs::write(&path, r#"{"checked_at": 1, "latest": "0.13.0"}"#).unwrap();
        assert_eq!(read_cache_at(&path).unwrap().dismissed, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_only_when_actually_ahead() {
        // A version below or equal to the current build is not an update.
        assert!(newer("0.0.1").is_none());
        assert_eq!(newer(CURRENT), None);
        assert_eq!(newer("999.0.0").as_deref(), Some("999.0.0"));
    }
}
