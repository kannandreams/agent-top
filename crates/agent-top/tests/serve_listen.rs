//! `agent-top serve --listen` end to end, against the real binary: a session
//! exported with `agent-top trace --format otlp` comes back as a stored
//! session with the same spans, and bad requests get the status they should.
//! HOME and XDG_DATA_HOME point at an empty directory, so the sync loop reads
//! no real transcripts.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Serve {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-top")
}

fn start(tag: &str) -> Serve {
    let dir = std::env::temp_dir().join(format!("agent-top-serve-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = Command::new(bin())
        .args(["serve", "--interval", "3600", "--listen", &format!("127.0.0.1:{port}"), "--db"])
        .arg(dir.join("agent-top.db"))
        .env("HOME", dir.join("home"))
        .env("XDG_DATA_HOME", dir.join("home"))
        .env("AGENT_TOP_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "serve did not start listening");
        std::thread::sleep(Duration::from_millis(50));
    }
    Serve { child, port, dir }
}

/// POST and return the status code.
fn post(port: u16, path: &str, headers: &[(&str, &str)], body: &[u8]) -> u16 {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut req = format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let _ = s.write_all(body);
    let mut resp = String::new();
    let _ = s.read_to_string(&mut resp);
    resp.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

const JSON: (&str, &str) = ("Content-Type", "application/json");

fn query(db: &Path, sql: &str) -> String {
    let out = Command::new(bin()).args(["sql", "--csv", "--db"]).arg(db).arg(sql).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().nth(1).unwrap_or_default().to_string()
}

#[test]
fn an_exported_session_comes_back_with_the_same_spans() {
    let serve = start("roundtrip");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-top-core/tests/fixtures/claude-2.1.278.jsonl");
    let doc = serve.dir.join("trace.json");
    let out = Command::new(bin()).args(["trace", "--format", "otlp", "--session"]).arg(&fixture).arg("-o").arg(&doc).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body = std::fs::read(&doc).unwrap();
    let exported: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spans = exported["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
    let kinds =
        |k: &str| spans.iter().filter(|s| s["attributes"].as_array().unwrap().iter().any(|a| a["value"]["stringValue"] == k)).count();

    assert_eq!(post(serve.port, "/v1/traces", &[JSON], &body), 200);
    // Sent twice, as an exporter retrying would: nothing changes.
    assert_eq!(post(serve.port, "/v1/traces", &[JSON], &body), 200);

    let db = serve.dir.join("agent-top.db");
    let row = query(&db, "SELECT harness, attribution, (SELECT count(*) FROM spans), turns, tool_calls FROM sessions");
    assert_eq!(row, format!("claude,telemetry,{},{},{}", spans.len(), kinds("turn"), kinds("tool")));
    let durations = query(&db, "SELECT sum(duration_ms) FROM spans WHERE duration_ms IS NOT NULL");
    let expected: u64 = spans
        .iter()
        .filter(|s| !s["attributes"].as_array().unwrap().iter().any(|a| a["key"] == "agent_top.open"))
        .map(|s| {
            (s["endTimeUnixNano"].as_str().unwrap().parse::<u64>().unwrap()
                - s["startTimeUnixNano"].as_str().unwrap().parse::<u64>().unwrap())
                / 1_000_000
        })
        .sum();
    assert_eq!(durations, expected.to_string());
}

#[test]
fn prompt_text_sent_in_spans_is_not_stored() {
    let serve = start("content");
    let body = serde_json::json!({"resourceSpans": [{"scopeSpans": [{"spans": [{
        "traceId": "0af7651916cd43dd8448eb211c80319c", "spanId": "b7ad6b7169203331", "name": "chat",
        "startTimeUnixNano": "1000000000", "endTimeUnixNano": "2000000000",
        "attributes": [
            {"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}},
            {"key": "gen_ai.input.messages", "value": {"stringValue": "SECRET-PROMPT-51c2"}},
            {"key": "gen_ai.usage.input_tokens", "value": {"intValue": 10}}
        ],
        "events": [{"name": "gen_ai.content.prompt", "attributes": [{"key": "gen_ai.prompt", "value": {"stringValue": "SECRET-PROMPT-51c2"}}]}]
    }]}]}]});
    assert_eq!(post(serve.port, "/v1/traces", &[JSON], body.to_string().as_bytes()), 200);
    assert_eq!(query(&serve.dir.join("agent-top.db"), "SELECT tokens FROM counted_sessions"), "10");
    for f in std::fs::read_dir(&serve.dir).unwrap().flatten() {
        if f.file_name().to_string_lossy().starts_with("agent-top.db") {
            let bytes = std::fs::read(f.path()).unwrap();
            assert!(!bytes.windows(18).any(|w| w == b"SECRET-PROMPT-51c2"), "{:?} holds prompt text", f.file_name());
        }
    }
}

#[test]
fn bad_requests_get_the_status_that_says_why() {
    let serve = start("refused");
    let p = serve.port;
    assert_eq!(post(p, "/v1/traces", &[("Content-Type", "text/plain")], b"x"), 415);
    assert_eq!(post(p, "/v1/traces", &[JSON, ("Content-Encoding", "gzip")], b"x"), 415);
    assert_eq!(post(p, "/v1/traces", &[JSON], b"not json"), 400);
    assert_eq!(post(p, "/v1/spans", &[JSON], b"{}"), 404);
    assert_eq!(post(p, "/v1/metrics", &[JSON], b"{}"), 200);
    assert_eq!(post(p, "/v1/traces", &[JSON], &vec![b' '; 4 * 1024 * 1024 + 10]), 413);
    assert_eq!(post(p, "/v1/traces", &[JSON], b"{}"), 200);
}
