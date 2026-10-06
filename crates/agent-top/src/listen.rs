//! `agent-top serve --listen`: an OTLP/HTTP receiver that writes what it
//! receives into the local store (RFC-107 D1, D3, D8; DEC-026).
//!
//! It exists only when the user passes an address, and accepts `POST
//! /v1/traces` in both OTLP encodings, `application/x-protobuf` and
//! `application/json`. `/v1/metrics` and `/v1/logs` are answered and dropped,
//! so an exporter pointed at the base address does not log errors for them.
//! Every attribute passes `otlp::keep` during decoding; span events are not
//! decoded at all.

use agent_top_core::otlp::{self, Attr, ReceivedSpan};
use anyhow::{Context, Result, anyhow, bail};
use prost::Message;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

/// A request body over this size is refused with 413, and the exporter
/// retries in smaller batches.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// How deep a JSON body may nest before it is refused.
const MAX_DEPTH: usize = 32;

/// The OTLP trace messages, reduced to the fields agent-top reads. Field
/// numbers are those of `opentelemetry/proto/trace/v1/trace.proto`; fields
/// left out (events, links, scope, array and map values) are skipped by the
/// decoder without being read into memory as values.
mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ExportTraceServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_spans: Vec<ResourceSpans>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ResourceSpans {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_spans: Vec<ScopeSpans>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Resource {
        #[prost(message, repeated, tag = "1")]
        pub attributes: Vec<KeyValue>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ScopeSpans {
        #[prost(message, repeated, tag = "2")]
        pub spans: Vec<Span>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Span {
        #[prost(bytes = "vec", tag = "1")]
        pub trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        pub span_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "4")]
        pub parent_span_id: Vec<u8>,
        #[prost(string, tag = "5")]
        pub name: String,
        #[prost(fixed64, tag = "7")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "8")]
        pub end_time_unix_nano: u64,
        #[prost(message, repeated, tag = "9")]
        pub attributes: Vec<KeyValue>,
        #[prost(message, optional, tag = "15")]
        pub status: Option<Status>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Status {
        #[prost(int32, tag = "3")]
        pub code: i32,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct KeyValue {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(message, optional, tag = "2")]
        pub value: Option<AnyValue>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct AnyValue {
        #[prost(oneof = "Value", tags = "1, 2, 3, 4")]
        pub value: Option<Value>,
    }
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Value {
        #[prost(string, tag = "1")]
        Str(String),
        #[prost(bool, tag = "2")]
        Bool(bool),
        #[prost(int64, tag = "3")]
        Int(i64),
        #[prost(double, tag = "4")]
        Double(f64),
    }
}

/// OTLP's error status code.
const STATUS_ERROR: i64 = 2;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Allow-listed attributes, span's own over the resource's.
fn merge(resource: &BTreeMap<String, Attr>, own: BTreeMap<String, Attr>) -> BTreeMap<String, Attr> {
    let mut all = resource.clone();
    all.extend(own);
    all
}

fn proto_attrs(kvs: Vec<proto::KeyValue>) -> BTreeMap<String, Attr> {
    kvs.into_iter()
        .filter(|kv| otlp::keep(&kv.key))
        .filter_map(|kv| {
            let v = match kv.value?.value? {
                proto::Value::Str(s) => Attr::Str(s),
                proto::Value::Bool(b) => Attr::Bool(b),
                proto::Value::Int(i) => Attr::Int(i),
                proto::Value::Double(f) => Attr::Float(f),
            };
            Some((kv.key, v))
        })
        .collect()
}

/// An `ExportTraceServiceRequest` in protobuf.
pub fn decode_protobuf(body: &[u8]) -> Result<Vec<ReceivedSpan>> {
    let req = proto::ExportTraceServiceRequest::decode(body).context("not an OTLP protobuf trace request")?;
    let mut out = Vec::new();
    for rs in req.resource_spans {
        let resource = proto_attrs(rs.resource.map(|r| r.attributes).unwrap_or_default());
        for ss in rs.scope_spans {
            for sp in ss.spans {
                if sp.trace_id.is_empty() || sp.span_id.is_empty() {
                    continue;
                }
                out.push(ReceivedSpan {
                    trace_id: hex(&sp.trace_id),
                    span_id: hex(&sp.span_id),
                    parent_span_id: (!sp.parent_span_id.is_empty()).then(|| hex(&sp.parent_span_id)),
                    name: sp.name,
                    start_unix_nano: sp.start_time_unix_nano,
                    end_unix_nano: sp.end_time_unix_nano,
                    error: sp.status.is_some_and(|s| s.code as i64 == STATUS_ERROR),
                    attrs: merge(&resource, proto_attrs(sp.attributes)),
                });
            }
        }
    }
    Ok(out)
}

fn depth(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(depth).max().unwrap_or(0),
        Value::Object(o) => 1 + o.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// A number OTLP/JSON may write as a string (64-bit integers) or a number.
fn json_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn json_attrs(v: Option<&Value>) -> BTreeMap<String, Attr> {
    let Some(list) = v.and_then(Value::as_array) else { return BTreeMap::new() };
    list.iter()
        .filter_map(|kv| {
            let key = kv.get("key")?.as_str()?;
            if !otlp::keep(key) {
                return None;
            }
            let val = kv.get("value")?;
            let attr = if let Some(s) = val.get("stringValue").and_then(Value::as_str) {
                Attr::Str(s.to_string())
            } else if let Some(b) = val.get("boolValue").and_then(Value::as_bool) {
                Attr::Bool(b)
            } else if let Some(i) = val.get("intValue") {
                Attr::Int(i.as_i64().or_else(|| i.as_str().and_then(|s| s.parse().ok()))?)
            } else if let Some(f) = val.get("doubleValue").and_then(Value::as_f64) {
                Attr::Float(f)
            } else {
                return None;
            };
            Some((key.to_string(), attr))
        })
        .collect()
}

/// An `ExportTraceServiceRequest` in OTLP/JSON: camelCase keys, hex ids,
/// 64-bit integers as strings or numbers.
pub fn decode_json(body: &[u8]) -> Result<Vec<ReceivedSpan>> {
    let doc: Value = serde_json::from_slice(body).context("not JSON")?;
    if depth(&doc) > MAX_DEPTH {
        bail!("nested deeper than {MAX_DEPTH} levels");
    }
    let mut out = Vec::new();
    for rs in doc.get("resourceSpans").and_then(Value::as_array).into_iter().flatten() {
        let resource = json_attrs(rs.get("resource").and_then(|r| r.get("attributes")));
        for ss in rs.get("scopeSpans").and_then(Value::as_array).into_iter().flatten() {
            for sp in ss.get("spans").and_then(Value::as_array).into_iter().flatten() {
                let id = |k: &str| sp.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase);
                let (Some(trace_id), Some(span_id)) = (id("traceId"), id("spanId")) else { continue };
                let code = sp.get("status").and_then(|s| s.get("code"));
                let error = code.and_then(Value::as_i64) == Some(STATUS_ERROR) || code.and_then(Value::as_str) == Some("STATUS_CODE_ERROR");
                out.push(ReceivedSpan {
                    trace_id,
                    span_id,
                    parent_span_id: id("parentSpanId"),
                    name: sp.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    start_unix_nano: sp.get("startTimeUnixNano").and_then(json_u64).unwrap_or(0),
                    end_unix_nano: sp.get("endTimeUnixNano").and_then(json_u64).unwrap_or(0),
                    error,
                    attrs: merge(&resource, json_attrs(sp.get("attributes"))),
                });
            }
        }
    }
    Ok(out)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kv(key: &str, value: proto::Value) -> proto::KeyValue {
        proto::KeyValue { key: key.into(), value: Some(proto::AnyValue { value: Some(value) }) }
    }

    #[test]
    fn protobuf_keeps_allowed_attributes_and_drops_content() {
        let req = proto::ExportTraceServiceRequest {
            resource_spans: vec![proto::ResourceSpans {
                resource: Some(proto::Resource { attributes: vec![kv("service.name", proto::Value::Str("bot".into()))] }),
                scope_spans: vec![proto::ScopeSpans {
                    spans: vec![proto::Span {
                        trace_id: vec![0xab; 16],
                        span_id: vec![0xcd; 8],
                        parent_span_id: vec![],
                        name: "chat".into(),
                        start_time_unix_nano: 1,
                        end_time_unix_nano: 2,
                        attributes: vec![
                            kv("gen_ai.usage.input_tokens", proto::Value::Int(7)),
                            kv("gen_ai.input.messages", proto::Value::Str("SECRET PROMPT".into())),
                        ],
                        status: Some(proto::Status { code: 2 }),
                    }],
                }],
            }],
        };
        let spans = decode_protobuf(&req.encode_to_vec()).unwrap();
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!((s.trace_id.len(), s.span_id.as_str(), s.parent_span_id.as_deref(), s.error), (32, "cdcdcdcdcdcdcdcd", None, true));
        assert_eq!(s.attrs.get("service.name"), Some(&Attr::Str("bot".into())));
        assert_eq!(s.attrs.get("gen_ai.usage.input_tokens"), Some(&Attr::Int(7)));
        assert!(!s.attrs.contains_key("gen_ai.input.messages"));
    }

    #[test]
    fn json_reads_ids_strings_and_numbers() {
        let doc = json!({"resourceSpans": [{
            "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "bot"}}]},
            "scopeSpans": [{"spans": [{
                "traceId": "0AF7651916CD43DD8448EB211C80319C", "spanId": "B7AD6B7169203331", "parentSpanId": "",
                "name": "execute_tool search", "startTimeUnixNano": "1000000000", "endTimeUnixNano": 1300000000u64,
                "status": {"code": "STATUS_CODE_ERROR"},
                "attributes": [
                    {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "12"}},
                    {"key": "gen_ai.tool.call.arguments", "value": {"stringValue": "{\"q\": \"SECRET\"}"}}
                ]
            }]}]
        }]});
        let spans = decode_json(doc.to_string().as_bytes()).unwrap();
        let s = &spans[0];
        assert_eq!((s.trace_id.as_str(), s.parent_span_id.as_deref(), s.error), ("0af7651916cd43dd8448eb211c80319c", None, true));
        assert_eq!(s.duration_ms(), 300);
        assert_eq!(s.attrs.get("gen_ai.usage.output_tokens"), Some(&Attr::Int(12)));
        assert_eq!(s.attrs.len(), 2);
    }

    #[test]
    fn garbage_is_an_error_never_a_panic() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for len in 0..512 {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            let _ = decode_protobuf(&bytes);
            let _ = decode_json(&bytes);
        }
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert!(decode_json(deep.as_bytes()).is_err());
    }
}
