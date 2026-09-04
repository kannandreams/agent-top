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
    // The Chrome document records the transcript path, which depends on
    // where the checkout lives; the OTLP one carries no path.
    if let Some(other) = doc.get_mut("otherData").and_then(Value::as_object_mut) {
        let transcript_field = other.remove("transcript").expect("transcript is recorded");
        assert!(transcript_field.as_str().unwrap().ends_with(&format!("{fixture}.jsonl")));
    }
    (doc, stderr)
}

fn check(fixture: &str) {
    check_format(fixture, "chrome");
    check_format(fixture, "otlp");
}

fn check_format(fixture: &str, format: &str) {
    let (got, _) = export(fixture, &["--format", format]);
    let golden = goldens().join(format!("{fixture}.{format}.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        return;
    }
    let want: Value = serde_json::from_str(
        &std::fs::read_to_string(&golden).unwrap_or_else(|e| panic!("{}: {e}. Record it with UPDATE_GOLDEN=1", golden.display())),
    )
    .expect("golden file is valid JSON");
    if got != want {
        let list = |d: &Value| match format {
            "chrome" => d["traceEvents"].as_array().cloned().unwrap_or_default(),
            _ => d["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().cloned().unwrap_or_default(),
        };
        let (g, w) = (list(&got), list(&want));
        let first_diff = g.iter().zip(&w).position(|(a, b)| a != b);
        panic!(
            "{fixture} ({format}) exports differently than its golden.\n  \
             entries: {} golden, {} now, first difference at index {:?}\n\n\
             If this change was intended, re-record and review the diff:\n  \
             UPDATE_GOLDEN=1 cargo test -p agent-top --test trace\n",
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

/// The OTLP document is what a collector expects: hex ids of the right
/// length, every parent present and a turn, every span inside its parent.
#[test]
fn otlp_document_is_well_formed() {
    let (doc, _) = export("claude-2.1.226", &["--format", "otlp"]);
    let rs = &doc["resourceSpans"][0];
    let names: Vec<&str> = rs["resource"]["attributes"].as_array().unwrap().iter().map(|a| a["key"].as_str().unwrap()).collect();
    assert!(names.contains(&"service.name") && names.contains(&"agent_top.session_id"));
    let spans = rs["scopeSpans"][0]["spans"].as_array().unwrap();
    assert_eq!(spans.len() as u64, doc_count(&export("claude-2.1.226", &["--format", "chrome"]).0));
    let by_id: std::collections::HashMap<&str, &Value> = spans.iter().map(|s| (s["spanId"].as_str().unwrap(), s)).collect();
    assert_eq!(by_id.len(), spans.len(), "span ids are distinct");
    let trace_id = spans[0]["traceId"].as_str().unwrap();
    assert_eq!(trace_id.len(), 32);
    let kind_of = |s: &Value| {
        s["attributes"].as_array().unwrap().iter().find(|a| a["key"] == "agent_top.kind").unwrap()["value"]["stringValue"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let mut parented = 0;
    for s in spans {
        assert_eq!(s["traceId"], trace_id);
        assert_eq!(s["spanId"].as_str().unwrap().len(), 16);
        let start: u64 = s["startTimeUnixNano"].as_str().unwrap().parse().unwrap();
        let end: u64 = s["endTimeUnixNano"].as_str().unwrap().parse().unwrap();
        assert!(end >= start);
        if let Some(p) = s.get("parentSpanId") {
            parented += 1;
            let parent = by_id[p.as_str().unwrap()];
            assert_eq!(kind_of(parent), "turn");
            let ps: u64 = parent["startTimeUnixNano"].as_str().unwrap().parse().unwrap();
            assert!(ps <= start, "a child starts after its parent");
        } else {
            assert_eq!(kind_of(s), "turn", "only turns are roots");
        }
    }
    assert!(parented > 0);
}

fn doc_count(chrome: &Value) -> u64 {
    chrome["otherData"]["spans"].as_u64().unwrap()
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

/// `--endpoint` posts the OTLP document to the given URL and nothing else:
/// a listener on localhost receives exactly one request with the same
/// document the file form writes.
#[test]
fn endpoint_posts_the_otlp_document_once() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 65536];
        let mut header_end = None;
        let mut content_length = 0usize;
        loop {
            let n = sock.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if header_end.is_none()
                && let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            {
                header_end = Some(i + 4);
                let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                content_length =
                    head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
            }
            if let Some(h) = header_end
                && buf.len() >= h + content_length
            {
                break;
            }
        }
        sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").unwrap();
        let h = header_end.unwrap();
        (String::from_utf8_lossy(&buf[..h]).into_owned(), buf[h..h + content_length].to_vec())
    });
    let transcript = core_fixtures().join("codex-0.130.jsonl");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-top"))
        .args(["trace", "--session"])
        .arg(&transcript)
        .args(["--format", "otlp", "--endpoint", &format!("http://127.0.0.1:{port}/v1/traces")])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "posted, so nothing is printed");
    assert!(String::from_utf8_lossy(&out.stderr).contains("accepted the trace (200)"));
    let (head, body) = server.join().unwrap();
    assert!(head.starts_with("POST /v1/traces HTTP/1.1"), "{head}");
    assert!(head.to_ascii_lowercase().contains("content-type: application/json"));
    let posted: Value = serde_json::from_slice(&body).unwrap();
    let (expected, _) = export("codex-0.130", &["--format", "otlp"]);
    assert_eq!(posted, expected);
}

#[test]
fn endpoint_requires_otlp() {
    let transcript = core_fixtures().join("codex-0.130.jsonl");
    let out = Command::new(env!("CARGO_BIN_EXE_agent-top"))
        .args(["trace", "--session"])
        .arg(&transcript)
        .args(["--endpoint", "http://127.0.0.1:9/v1/traces"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--format otlp"));
}
