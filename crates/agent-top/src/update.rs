//! The one outbound call agent-top makes on its own: an update check.
//!
//! It asks crates.io for the latest published version of `agent-top` and
//! nothing else. It sends no data about you, your agents, your sessions or your
//! machine, only a generic User-Agent, so it does not break the promise that
//! matters: your data never leaves. The result is cached so it runs at most
//! once a day, it happens on a background thread so the UI never waits, it is
//! silent when there is no network, and `AGENT_TOP_NO_UPDATE_CHECK=1` turns it
//! off entirely.

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

/// The cached `(checked_at, latest_version)`, if the file is present and valid.
fn read_cache() -> Option<(u64, String)> {
    let text = std::fs::read_to_string(cache_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let at = v.get("checked_at")?.as_u64()?;
    let latest = v.get("latest")?.as_str()?.to_string();
    Some((at, latest))
}

fn write_cache(latest: &str) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let doc = serde_json::json!({ "checked_at": now_secs(), "latest": latest });
    let _ = std::fs::write(path, doc.to_string());
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
    if let Some((_, latest)) = &cached
        && let Some(n) = newer(latest)
    {
        *shared.lock().unwrap() = Some(n);
    }
    let fresh = cached.as_ref().map(|(at, _)| now_secs().saturating_sub(*at) < MAX_AGE_SECS).unwrap_or(false);
    if fresh {
        return shared;
    }
    // Stale or missing: refresh without blocking the UI.
    let handle = shared.clone();
    std::thread::spawn(move || {
        if let Some(latest) = fetch() {
            write_cache(&latest);
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
    fn newer_only_when_actually_ahead() {
        // A version below or equal to the current build is not an update.
        assert!(newer("0.0.1").is_none());
        assert_eq!(newer(CURRENT), None);
        assert_eq!(newer("999.0.0").as_deref(), Some("999.0.0"));
    }
}
