//! `agent-top trace` end to end: run the binary on the core crate's golden
//! transcripts and pin the exact document it writes.
//!
//! Re-record deliberately, and read the diff: it is the change to every
//! trace file users will export.
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test -p agent-top --test trace
//! ```

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn core_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-top-core/tests/fixtures")
}

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agent-top-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Export `fixture` and return the parsed document, with the one field that
/// depends on where the checkout lives removed.
fn export(fixture: &str, extra: &[&str]) -> (Value, String) {
    let transcript = core_fixtures().join(format!("{fixture}.jsonl"));
    let out = Command::new(env!("CARGO_BIN_EXE_agent-top"))
        .arg("trace")
        .arg("--session")
        .arg(&transcript)
        .args(extra)
        .output()
        .expect("binary runs");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = if extra.contains(&"-o") {
        let path = extra[extra.iter().position(|a| *a == "-o").unwrap() + 1];
        std::fs::read_to_string(path).unwrap()
    } else {
        String::from_utf8(out.stdout).unwrap()
    };
    let mut doc: Value = serde_json::from_str(&text).expect("output is JSON");
    let transcript_field = doc["otherData"].as_object_mut().unwrap().remove("transcript").expect("transcript is recorded");
    assert!(transcript_field.as_str().unwrap().ends_with(&format!("{fixture}.jsonl")));
    (doc, stderr)
}

fn check(fixture: &str) {
    let (got, _) = export(fixture, &["--format", "chrome"]);
    let golden = goldens().join(format!("{fixture}.chrome.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        return;
    }
    let want: Value = serde_json::from_str(
        &std::fs::read_to_string(&golden).unwrap_or_else(|e| panic!("{}: {e}. Record it with UPDATE_GOLDEN=1", golden.display())),
    )
    .expect("golden file is valid JSON");
    if got != want {
        let (g, w) = (got["traceEvents"].as_array().unwrap(), want["traceEvents"].as_array().unwrap());
        let first_diff = g.iter().zip(w).position(|(a, b)| a != b);
        panic!(
            "{fixture} exports differently than its golden.\n  otherData golden: {}\n  otherData now:    {}\n  \
             events: {} golden, {} now, first difference at index {:?}\n\n\
             If this change was intended, re-record and review the diff:\n  \
             UPDATE_GOLDEN=1 cargo test -p agent-top --test trace\n",
            want["otherData"],
            got["otherData"],
            w.len(),
            g.len(),
            first_diff
        );
    }
}

#[test]
fn claude_2_1_226_chrome() {
    check("claude-2.1.226");
}

#[test]
fn codex_0_130_chrome() {
    check("codex-0.130");
}

/// The document is what Perfetto expects: complete events on a shared pid,
/// every span accounted for, every span in a thread that has a name.
#[test]
fn chrome_document_is_well_formed() {
    let (doc, _) = export("claude-2.1.226", &[]);
    let events = doc["traceEvents"].as_array().unwrap();
    let pid = events[0]["pid"].as_u64().unwrap();
    assert!((1..=0x7fff_ffff).contains(&pid));
    assert!(events.iter().all(|e| e["pid"] == pid), "one process per session");
    let named: Vec<u64> = events.iter().filter(|e| e["name"] == "thread_name").map(|e| e["tid"].as_u64().unwrap()).collect();
    let spans: Vec<&Value> = events.iter().filter(|e| e["ph"] != "M").collect();
    assert_eq!(spans.len() as u64, doc["otherData"]["spans"].as_u64().unwrap());
    let tools: Vec<&&Value> = spans.iter().filter(|s| s["cat"] == "tool").collect();
    assert_eq!(tools.len() as u64, doc["otherData"]["tool_calls"].as_u64().unwrap(), "this fixture has no unpaired calls");
    assert_eq!(tools.len() as u64, doc["otherData"]["tool_spans"].as_u64().unwrap());
    assert!(doc["otherData"]["inference_spans"].as_u64().unwrap() > 0);
    assert!(doc["otherData"]["turn_spans"].as_u64().unwrap() > 0);
    for s in &tools {
        assert_eq!(s["ph"], "X", "every call in this fixture returned");
        assert_eq!(s["tid"], 2, "main-agent tool calls share one track");
    }
    for s in &spans {
        assert!(matches!(s["cat"].as_str(), Some("tool" | "inference" | "turn")));
        assert!(named.contains(&s["tid"].as_u64().unwrap()), "span on an unnamed track");
        assert!(s["args"]["call_id"].as_str().map(|id| !id.is_empty()).unwrap_or(false));
        if s["ph"] == "X" {
            assert!(s["dur"].as_u64().is_some());
        }
    }
    // Starts are in transcript order, which is chronological.
    let ts: Vec<u64> = spans.iter().map(|s| s["ts"].as_u64().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]));
    // On any one track, complete events nest properly or not at all: a span
    // that starts inside another on the same track must end inside it too.
    for tid in &named {
        let on: Vec<(u64, u64)> = spans
            .iter()
            .filter(|s| s["tid"].as_u64() == Some(*tid) && s["ph"] == "X")
            .map(|s| (s["ts"].as_u64().unwrap(), s["ts"].as_u64().unwrap() + s["dur"].as_u64().unwrap()))
            .collect();
        for (i, a) in on.iter().enumerate() {
            for b in &on[i + 1..] {
                let inside = b.0 >= a.0 && b.0 < a.1;
                assert!(!inside || b.1 <= a.1, "track {tid}: {b:?} starts inside {a:?} but ends after it");
            }
        }
    }
}

#[test]
fn writes_to_a_file_and_reports_on_stderr() {
    let out = scratch("codex.json");
    let (doc, stderr) = export("codex-0.130", &["-o", out.to_str().unwrap()]);
    assert_eq!(doc["otherData"]["harness"], "codex");
    assert!(
        stderr.contains("wrote 16 tool calls, 11 inferences, 3 turns from codex 01000000-0000-7000-0000-000000000000"),
        "stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(out.parent().unwrap());
}

#[test]
fn refuses_a_file_that_is_not_a_transcript() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-top")).args(["trace", "--session"]).arg(&manifest).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a transcript"));
}
