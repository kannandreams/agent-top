//! `agent-top serve --listen`: an OTLP/HTTP receiver that writes what it
//! receives into the local store (RFC-107 D1, D3, D8; DEC-026).
//!
//! It exists only when the user passes an address, and accepts `POST
//! /v1/traces` in both OTLP encodings, `application/x-protobuf` and
//! `application/json`. `/v1/metrics` and `/v1/logs` are answered and dropped,
//! so an exporter pointed at the base address does not log errors for them.
//! Every attribute passes `otlp::keep` during decoding; span events are not
//! decoded at all.

use agent_top_core::otlp_decode::{decode_json, decode_protobuf};
use anyhow::{Result, anyhow};
use std::io::Read;
use std::path::PathBuf;

/// A request body over this size is refused with 413, and the exporter
/// retries in smaller batches.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// What the listener did with one request, for the log.
enum Handled {
    Received(agent_top_store::Received),
    /// Metrics and logs: answered, not stored.
    Dropped,
}

/// Bind `addr` and serve on a thread, writing into the store at `db`.
/// Binding happens before this returns, so a taken port is an error here.
pub fn start(addr: &str, db: PathBuf) -> Result<std::thread::JoinHandle<()>> {
    let server = tiny_http::Server::http(addr).map_err(|e| anyhow!("cannot listen on {addr}: {e}"))?;
    let mut store = agent_top_store::Store::open(&db)?;
    Ok(std::thread::spawn(move || {
        let now = || crate::report::timestamp_utc(std::time::SystemTime::now());
        for mut req in server.incoming_requests() {
            let (status, body, kind) = match handle(&mut req, &mut store) {
                Ok((Handled::Received(r), kind)) => {
                    if r.new_spans > 0 {
                        let s = if r.sessions == 1 { "" } else { "s" };
                        println!("{} received {} spans ({} new) for {} session{s}", now(), r.spans, r.new_spans, r.sessions);
                    }
                    (200, empty_reply(kind), kind)
                }
                Ok((Handled::Dropped, kind)) => (200, empty_reply(kind), kind),
                Err((status, why)) => {
                    eprintln!("{} refused a request ({status}): {why}", now());
                    (status, why.into_bytes(), "text/plain")
                }
            };
            let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], kind.as_bytes()).expect("a valid header");
            let _ = req.respond(tiny_http::Response::from_data(body).with_status_code(status).with_header(header));
        }
    }))
}

/// An empty `ExportTraceServiceResponse`: no bytes in protobuf, `{}` in JSON.
fn empty_reply(kind: &str) -> Vec<u8> {
    if kind == "application/json" { b"{}".to_vec() } else { Vec::new() }
}

fn handle(req: &mut tiny_http::Request, store: &mut agent_top_store::Store) -> std::result::Result<(Handled, &'static str), (u16, String)> {
    let header = |name: &str| {
        req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_ascii_lowercase())
    };
    let kind = match header("content-type").as_deref().map(|c| c.split(';').next().unwrap_or("").trim().to_string()).as_deref() {
        Some("application/x-protobuf") => "application/x-protobuf",
        Some("application/json") => "application/json",
        other => return Err((415, format!("content type {other:?} is not application/x-protobuf or application/json"))),
    };
    if *req.method() != tiny_http::Method::Post {
        return Err((405, "OTLP takes POST".into()));
    }
    if header("content-encoding").is_some_and(|e| e != "identity") {
        return Err((415, "compressed bodies are not accepted; set the exporter's compression to none".into()));
    }
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if path == "/v1/metrics" || path == "/v1/logs" {
        return Ok((Handled::Dropped, kind));
    }
    if path != "/v1/traces" {
        return Err((404, format!("{path} is not an OTLP path; spans go to /v1/traces")));
    }
    let mut body = Vec::new();
    req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body).map_err(|e| (400, format!("reading the body: {e}")))?;
    if body.len() > MAX_BODY {
        return Err((413, format!("bodies over {MAX_BODY} bytes are refused; send smaller batches")));
    }
    let spans =
        if kind == "application/json" { decode_json(&body) } else { decode_protobuf(&body) }.map_err(|e| (400, format!("{e:#}")))?;
    let received = store.receive(&spans).map_err(|e| (500, format!("writing the store: {e:#}")))?;
    Ok((Handled::Received(received), kind))
}
