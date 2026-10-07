//! `agent-top ui` end to end, against the real binary and a store synced from
//! a golden fixture: the page and its API answer, the numbers match `sql`,
//! and requests that should not be served are refused.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Ui {
    child: Child,
    port: u16,
    dir: PathBuf,
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent-top")
}

/// A store holding one Claude fixture session, and `ui` serving it.
fn start(tag: &str) -> Ui {
    let dir = std::env::temp_dir().join(format!("agent-top-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let projects = home.join(".claude/projects/-Users-dev-code-example");
    std::fs::create_dir_all(&projects).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../agent-top-core/tests/fixtures/claude-2.1.278.jsonl");
    std::fs::copy(&fixture, projects.join("00000000-1111-2222-3333-555555555555.jsonl")).unwrap();
    let db = dir.join("agent-top.db");
    let run = |args: &[&str]| {
        let mut c = Command::new(bin());
        c.args(args)
            .arg("--db")
            .arg(&db)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &home)
            .env("AGENT_TOP_NO_UPDATE_CHECK", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    };
    assert!(run(&["sync"]).status().unwrap().success());
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = run(&["ui", "--addr", &format!("127.0.0.1:{port}")]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "ui did not start listening");
        std::thread::sleep(Duration::from_millis(50));
    }
    Ui { child, port, dir }
}

/// Status, headers and body of one request.
fn request(port: u16, method: &str, path: &str, host: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((&resp, ""));
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, head.to_ascii_lowercase(), body.to_string())
}

fn get_json(ui: &Ui, path: &str) -> serde_json::Value {
    let (status, _, body) = request(ui.port, "GET", path, &format!("127.0.0.1:{}", ui.port));
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

#[test]
fn the_page_and_its_api_answer_with_the_store_s_numbers() {
    let ui = start("api");
    let host = format!("127.0.0.1:{}", ui.port);
    let (status, head, body) = request(ui.port, "GET", "/", &host);
    assert_eq!(status, 200);
    assert!(body.contains("<script src=\"/app.js\""));
    assert!(head.contains("content-security-policy: default-src 'self'"), "{head}");
    assert!(head.contains("x-frame-options: deny"));
    for asset in ["/app.js", "/app.css"] {
        assert_eq!(request(ui.port, "GET", asset, &host).0, 200, "{asset}");
    }

    let ov = get_json(&ui, "/api/overview?since=0");
    let totals = &ov["totals"];
    assert_eq!(totals["sessions"], 1);
    assert!(totals["cost_usd"].as_f64().unwrap() > 0.0);
    let out = Command::new(bin())
        .args(["sql", "--json", "--db"])
        .arg(ui.dir.join("agent-top.db"))
        .arg("SELECT tokens, cost_usd FROM counted_sessions")
        .output()
        .unwrap();
    let direct: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(totals["tokens"], direct[0]["tokens"]);
    assert_eq!(totals["cost_usd"], direct[0]["cost_usd"]);
    assert_eq!(ov["by_day"].as_array().unwrap().len(), 1);

    let sessions = get_json(&ui, "/api/sessions?since=0&harness=claude");
    let id = sessions[0]["session_id"].as_str().unwrap().to_string();
    let detail = get_json(&ui, &format!("/api/session?harness=claude&id={id}"));
    assert_eq!(detail["session"]["session_id"], id.as_str());
    assert!(!detail["spans"].as_array().unwrap().is_empty());
    assert!(get_json(&ui, "/api/tools?since=0").as_array().is_some());
    assert_eq!(get_json(&ui, "/api/sessions?since=0&harness=codex").as_array().unwrap().len(), 0);
    assert_eq!(get_json(&ui, "/api/meta")["counts"]["sessions"], 1);
}

#[test]
fn requests_it_should_not_serve_are_refused() {
    let ui = start("refused");
    let host = format!("127.0.0.1:{}", ui.port);
    assert_eq!(request(ui.port, "GET", "/api/meta", "evil.example").0, 403);
    assert_eq!(request(ui.port, "GET", "/api/meta", &format!("attacker.test:{}", ui.port)).0, 403);
    assert_eq!(request(ui.port, "POST", "/api/meta", &host).0, 405);
    assert_eq!(request(ui.port, "GET", "/api/nope", &host).0, 404);
    assert_eq!(request(ui.port, "GET", "/api/session?harness=claude&id=missing", &host).0, 404);
    assert_eq!(request(ui.port, "GET", "/../Cargo.toml", &host).0, 404);
    assert_eq!(request(ui.port, "GET", "/", &format!("localhost:{}", ui.port)).0, 200);
}
