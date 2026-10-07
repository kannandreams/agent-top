//! `agent-top ui`: a read-only web view of the local store (DEC-027).
//!
//! One page and a small JSON API, both served from the address the command
//! binds, `127.0.0.1:4320` unless `--addr` says otherwise. The page, its
//! stylesheet and script are compiled into the binary; nothing is fetched
//! from anywhere else. Every API request opens the store with the read-only
//! `Reader`, so the view shows the latest sync and cannot change the file.

use agent_top_store::{QueryResult, Reader, Value};
use anyhow::{Result, anyhow};
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");

/// The default address: loopback, so only this machine can open it.
pub const DEFAULT_ADDR: &str = "127.0.0.1:4320";

/// Sessions that count, in the window, for one harness or all of them.
const FILTER: &str = "last_activity >= ?1 AND (?2 = '' OR harness = ?2)";

/// The answer to one request.
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Reply {
    fn json(v: Json) -> Reply {
        Reply { status: 200, content_type: "application/json", body: v.to_string().into_bytes() }
    }

    fn text(status: u16, msg: impl Into<String>) -> Reply {
        Reply { status, content_type: "text/plain; charset=utf-8", body: msg.into().into_bytes() }
    }

    fn asset(content_type: &'static str, body: &str) -> Reply {
        Reply { status: 200, content_type, body: body.as_bytes().to_vec() }
    }
}

/// Bind and serve until the process is stopped.
pub fn run(addr: &str, db: PathBuf) -> Result<()> {
    // Fail before binding when there is nothing to show.
    Reader::open(&db)?;
    let server = tiny_http::Server::http(addr).map_err(|e| anyhow!("cannot listen on {addr}: {e}"))?;
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    println!("agent-top ui at http://{addr}/ reading {}", db.display());
    for req in server.incoming_requests() {
        let host = req.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str().to_string());
        let get = *req.method() == tiny_http::Method::Get;
        let reply = handle(get, req.url(), host.as_deref(), addr, port, &db);
        let mut resp = tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
        for (k, v) in headers(reply.content_type) {
            resp.add_header(tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("a valid header"));
        }
        let _ = req.respond(resp);
    }
    Ok(())
}

fn headers(content_type: &str) -> [(&'static str, String); 6] {
    [
        ("Content-Type", content_type.to_string()),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; \
             base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
                .into(),
        ),
        ("X-Content-Type-Options", "nosniff".into()),
        ("Referrer-Policy", "no-referrer".into()),
        ("X-Frame-Options", "DENY".into()),
        ("Cache-Control", "no-store".into()),
    ]
}

/// Whether a request's `Host` names this server: the bound address, or a
/// loopback name on its port. Anything else is a page elsewhere trying to
/// reach the API under another name (DNS rebinding).
pub fn host_allowed(host: Option<&str>, addr: &str, port: u16) -> bool {
    let Some(host) = host else { return false };
    let host = host.to_ascii_lowercase();
    host == addr.to_ascii_lowercase() || ["127.0.0.1", "localhost", "[::1]"].iter().any(|h| host == format!("{h}:{port}"))
}

/// Route one request. Separate from the socket so tests can call it.
pub fn handle(get: bool, url: &str, host: Option<&str>, addr: &str, port: u16, db: &Path) -> Reply {
    if !host_allowed(host, addr, port) {
        return Reply::text(403, "unexpected Host header");
    }
    if !get {
        return Reply::text(405, "the view is read-only; only GET");
    }
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    match path {
        "/" | "/index.html" => Reply::asset("text/html; charset=utf-8", INDEX_HTML),
        "/app.js" => Reply::asset("text/javascript; charset=utf-8", APP_JS),
        "/app.css" => Reply::asset("text/css; charset=utf-8", APP_CSS),
        p if p.starts_with("/api/") => match api(&p[5..], &Params::parse(query), db) {
            Ok(Some(v)) => Reply::json(v),
            Ok(None) => Reply::text(404, "not found"),
            Err(e) => Reply::text(500, format!("{e:#}")),
        },
        _ => Reply::text(404, "not found"),
    }
}

/// The query string, decoded.
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn parse(q: &str) -> Params {
        Params(
            q.split('&')
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    (decode(k), decode(v))
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> &str {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    /// `since` in milliseconds since the epoch, and the harness ('' for all).
    fn filter(&self) -> [Value; 2] {
        [Value::Integer(self.get("since").parse().unwrap_or(0)), Value::Text(self.get("harness").to_string())]
    }
}

/// Percent-decoding for a query string, with `+` as a space.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn rows(r: QueryResult) -> Json {
    crate::sql::to_json(&r)
}

fn api(route: &str, p: &Params, db: &Path) -> Result<Option<Json>> {
    let r = Reader::open(db)?;
    let f = p.filter();
    let v = match route {
        "meta" => {
            let one = |sql: &str| -> Result<Json> { Ok(rows(r.query(sql)?).get(0).cloned().unwrap_or(Json::Null)) };
            json!({
                "store": db.to_string_lossy(),
                "counts": one("SELECT count(*) AS sessions FROM counted_sessions")?,
                "last_sync": one("SELECT max(synced_at) AS at FROM sources")?,
                "last_received": one("SELECT max(received_at) AS at FROM telemetry_spans")?,
                "harnesses": rows(r.query("SELECT DISTINCT harness FROM counted_sessions ORDER BY 1")?),
                "first_activity": one("SELECT min(last_activity) AS at FROM counted_sessions")?,
            })
        }
        "overview" => json!({
            "totals": rows(r.query_with(
                &format!(
                    "SELECT count(*) AS sessions, coalesce(sum(tokens), 0) AS tokens, coalesce(sum(cost_usd), 0) AS cost_usd,
                            coalesce(sum(unpriced_tokens), 0) AS unpriced_tokens, coalesce(sum(cache_read), 0) AS cache_read,
                            coalesce(sum(input + cache_write_5m + cache_write_1h + cache_write_unsplit + cache_read), 0) AS prompt,
                            coalesce(sum(turns), 0) AS turns, coalesce(sum(tool_calls), 0) AS tool_calls
                     FROM counted_sessions WHERE {FILTER}"
                ),
                &f,
            )?)
            .get(0)
            .cloned(),
            "by_day": rows(r.query_with(
                &format!(
                    "SELECT date(last_activity / 1000, 'unixepoch') AS day, harness, sum(cost_usd) AS cost_usd,
                            sum(tokens) AS tokens, count(*) AS sessions
                     FROM counted_sessions WHERE {FILTER} GROUP BY 1, 2 ORDER BY 1, 2"
                ),
                &f,
            )?),
            "by_project": rows(r.query_with(
                &format!(
                    "SELECT coalesce(project, 'unknown') AS name, sum(cost_usd) AS cost_usd, sum(tokens) AS tokens, count(*) AS sessions
                     FROM counted_sessions WHERE {FILTER} GROUP BY 1 ORDER BY 2 DESC, 3 DESC LIMIT 10"
                ),
                &f,
            )?),
            "by_model": rows(r.query_with(
                &format!(
                    "SELECT coalesce(model, 'unknown') AS name, sum(cost_usd) AS cost_usd, sum(tokens) AS tokens, count(*) AS sessions,
                            sum(unpriced_tokens) AS unpriced_tokens
                     FROM counted_sessions WHERE {FILTER} GROUP BY 1 ORDER BY 2 DESC, 3 DESC LIMIT 10"
                ),
                &f,
            )?),
        }),
        "tools" => rows(r.query_with(
            &format!(
                "WITH ranked AS (
                    SELECT p.name, p.duration_ms, p.error,
                           row_number() OVER (PARTITION BY p.name ORDER BY p.duration_ms) AS rank,
                           count(*) OVER (PARTITION BY p.name) AS n
                    FROM spans p JOIN counted_sessions s ON s.harness = p.harness AND s.session_id = p.session_id
                    WHERE p.kind = 'tool' AND p.duration_ms IS NOT NULL AND s.{}
                 )
                 SELECT name, max(n) AS calls, sum(error) AS errors,
                        min(CASE WHEN rank >= 0.5 * n THEN duration_ms END) AS p50_ms,
                        min(CASE WHEN rank >= 0.95 * n THEN duration_ms END) AS p95_ms,
                        max(duration_ms) AS max_ms
                 FROM ranked GROUP BY name ORDER BY calls DESC LIMIT 25",
                FILTER.replace("harness = ?2", "s.harness = ?2")
            ),
            &f,
        )?),
        "mcp" => rows(r.query_with(
            &format!(
                "SELECT m.server, count(*) AS sessions, sum(m.calls) AS calls, sum(m.errors) AS errors, max(m.last_call_at) AS last_call_at
                 FROM mcp_calls m JOIN counted_sessions s ON s.harness = m.harness AND s.session_id = m.session_id
                 WHERE s.{} GROUP BY 1 ORDER BY 3 DESC",
                FILTER.replace("harness = ?2", "s.harness = ?2")
            ),
            &f,
        )?),
        "sessions" => rows(r.query_with(
            &format!(
                "SELECT harness, session_id, project, model, started_at, last_activity, tokens, cost_usd, unpriced_tokens,
                        turns, tool_calls, attribution
                 FROM counted_sessions WHERE {FILTER} ORDER BY last_activity DESC LIMIT 200"
            ),
            &f,
        )?),
        "session" => {
            let key = [Value::Text(p.get("harness").to_string()), Value::Text(p.get("id").to_string())];
            let session = rows(r.query_with(
                "SELECT harness, session_id, project, cwd, model, harness_version, attribution, started_at, last_activity,
                        tokens, input, cache_write_5m, cache_write_1h, cache_write_unsplit, cache_read, output, cost_usd,
                        unpriced_tokens, price_source, harness_cost_usd, turns, subagent_turns, tool_calls, web_searches,
                        parent_session_id, synced_by_version
                 FROM counted_sessions WHERE harness = ?1 AND session_id = ?2",
                &key,
            )?);
            let Some(session) = session.get(0).cloned() else { return Ok(None) };
            json!({
                "session": session,
                "spans": rows(r.query_with(
                    "SELECT seq, kind, name, started_at, duration_ms, error, sidechain, parent_seq
                     FROM spans WHERE harness = ?1 AND session_id = ?2 ORDER BY seq LIMIT 2000",
                    &key,
                )?),
                "mcp": rows(r.query_with(
                    "SELECT server, calls, errors, last_call_at FROM mcp_calls WHERE harness = ?1 AND session_id = ?2 ORDER BY calls DESC",
                    &key,
                )?),
            })
        }
        _ => return Ok(None),
    };
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_server_s_names_are_accepted_as_host() {
        let a = "127.0.0.1:4320";
        assert!(host_allowed(Some("127.0.0.1:4320"), a, 4320));
        assert!(host_allowed(Some("localhost:4320"), a, 4320));
        assert!(host_allowed(Some("[::1]:4320"), a, 4320));
        assert!(!host_allowed(Some("evil.example:4320"), a, 4320));
        assert!(!host_allowed(Some("localhost:9999"), a, 4320));
        assert!(!host_allowed(None, a, 4320));
    }

    #[test]
    fn query_strings_are_decoded() {
        let p = Params::parse("harness=open%20code&id=a%2Fb&since=5&x=%zz+y");
        assert_eq!((p.get("harness"), p.get("id"), p.get("since"), p.get("x")), ("open code", "a/b", "5", "%zz y"));
    }
}
