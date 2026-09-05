//! `agent-top trace`: one session's tool calls as a trace file.
//!
//! The live tracker keeps the newest `MAX_SPANS` calls, which is right for a
//! waterfall pane and wrong for an export, so this reads the transcript again
//! from the start with an unbounded span log. It works on a session that
//! finished last week, and on a harness with no telemetry of its own, because
//! the spans are reconstructed from the transcript the harness already wrote.
//! Nothing here talks to the network: the output is a file the user opens in
//! Perfetto or feeds to their own tooling.
//!
//! Chrome trace event format (the JSON object form, `{"traceEvents": [...]}`)
//! is the first target because it needs no dependency and no setup:
//! `ui.perfetto.dev` and `chrome://tracing` both open it directly. OTLP JSON
//! (an `ExportTraceServiceRequest`) is the second, for Jaeger and anything
//! else that speaks OpenTelemetry; the user posts the file to a collector
//! themselves, agent-top never does.

use agent_top_core::Harness;
use agent_top_core::harness::{self, SessionSummary, SpanRetention};
use agent_top_core::model::{SpanKind, ToolSpan};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Chrome trace event format, for Perfetto and chrome://tracing.
    Chrome,
    /// OTLP/JSON, the OpenTelemetry trace request body, for Jaeger and any
    /// OpenTelemetry collector.
    Otlp,
}

/// A transcript on disk and the harness that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub harness: Harness,
}

/// Find the transcript behind `what`: a path to a transcript file, or a
/// session id, or a unique prefix of one. Ids are matched against the file
/// names in every harness's transcript directory, so a session that has long
/// since fallen out of the live window still resolves.
pub fn resolve(what: &str) -> Result<Source> {
    let as_path = Path::new(what);
    if as_path.is_file() {
        let harness = harness::detect(as_path)
            .with_context(|| format!("{what}: not a transcript agent-top knows how to read (Claude Code, Codex or Gemini CLI JSONL)"))?;
        return Ok(Source { path: as_path.to_path_buf(), harness });
    }
    if what.is_empty() {
        bail!("a session id or transcript path is required");
    }
    let known: Vec<(Harness, String, PathBuf)> =
        harness::adapters().iter().flat_map(|a| a.transcripts().into_iter().map(move |(id, p)| (a.harness(), id, p))).collect();
    let candidates = candidates(what, &known);
    match candidates.len() {
        1 => Ok(candidates.into_iter().next().unwrap()),
        0 => bail!("no session id starts with {what:?}, and it is not a file"),
        _ => {
            let mut msg = format!("{what:?} matches {} sessions; give more of the id:", candidates.len());
            for c in &candidates {
                msg.push_str(&format!("\n  {:<7} {}", c.harness.label(), c.path.display()));
            }
            bail!(msg)
        }
    }
}

/// Every transcript whose id starts with `prefix`, in path order. Each
/// adapter says what the id of a file is: Claude Code names the file after it,
/// Codex puts it after a timestamp, Gemini keeps only its first eight
/// characters in the name and the whole id in the header.
fn candidates(prefix: &str, known: &[(Harness, String, PathBuf)]) -> Vec<Source> {
    let mut out: Vec<Source> =
        known.iter().filter(|(_, id, _)| id.starts_with(prefix)).map(|(h, _, p)| Source { path: p.clone(), harness: *h }).collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// POST an OTLP/JSON document to a collector's traces URL and return the
/// status. The one place in agent-top that opens a network connection, and
/// only ever with an address the user typed on this command line: no
/// default, no config key, no environment variable.
pub fn post(url: &str, doc: &str) -> Result<u16> {
    match ureq::post(url).header("content-type", "application/json").send(doc) {
        Ok(resp) => Ok(resp.status().as_u16()),
        Err(ureq::Error::StatusCode(code)) => bail!("{url} rejected the trace with HTTP {code}"),
        Err(e) => bail!("posting to {url}: {e}"),
    }
}

/// Read the whole transcript with every span kept.
pub fn read(src: &Source) -> Result<SessionSummary> {
    let mut tracker = harness::open_transcript(&src.path, src.harness, SpanRetention::All)
        .with_context(|| format!("{} has no transcript adapter", src.harness.label()))?;
    tracker.refresh_all().with_context(|| format!("reading {}", src.path.display()))?;
    Ok(tracker.summary().clone())
}

/// Render a session in the requested format.
pub fn render(src: &Source, summary: &SessionSummary, format: Format) -> Value {
    match format {
        Format::Chrome => chrome(src, summary),
        Format::Otlp => otlp(src, summary),
    }
}

/// OTLP/JSON: one resource (the session), one scope (agent-top), one span
/// per span. Ids are derived, not random: the trace id from the session id
/// and each span id from the session id and the span's own id, so exporting
/// twice produces the same trace instead of a duplicate, and two exports of
/// the same session can be diffed. A tool call or inference is parented to
/// the turn that was open when it started, so a backend that draws trees
/// draws the right one. OTLP requires an end time, so a span still open when
/// the transcript ended gets an end equal to its start and an
/// `agent_top.open` attribute, rather than an invented duration.
fn otlp(src: &Source, s: &SessionSummary) -> Value {
    let session = s.session_id.clone().unwrap_or_else(|| stem(&src.path));
    let trace_id = hex(&fnv1a(session.as_bytes(), 0xcbf2_9ce4_8422_2325), &fnv1a(session.as_bytes(), 0x84222325_cbf29ce4));
    let span_id = |sp: &ToolSpan| hex_one(&fnv1a(format!("{session}:{}:{}", sp.kind.label(), sp.id).as_bytes(), 0xcbf2_9ce4_8422_2325));
    let spans: Vec<&ToolSpan> = s.spans.iter().collect();
    let out: Vec<Value> = spans
        .iter()
        .enumerate()
        .map(|(i, sp)| {
            let parent = parent_turn(&spans, i).map(span_id);
            let start = nanos(sp.started_at);
            let end = sp.duration_ms.map(|ms| start + ms * 1_000_000).unwrap_or(start);
            let mut attrs = vec![
                attr("agent_top.kind", json!({"stringValue": sp.kind.label()})),
                attr("agent_top.call_id", json!({"stringValue": sp.id})),
                attr("agent_top.sidechain", json!({"boolValue": sp.sidechain})),
            ];
            if sp.is_open() {
                attrs.push(attr("agent_top.open", json!({"boolValue": true})));
            }
            let mut span = json!({
                "traceId": trace_id,
                "spanId": span_id(sp),
                "name": sp.name,
                "kind": 1,
                "startTimeUnixNano": start.to_string(),
                "endTimeUnixNano": end.to_string(),
                "attributes": attrs,
                "status": if sp.error { json!({"code": 2, "message": "the harness reported an error"}) } else { json!({"code": 0}) },
            });
            if let Some(p) = parent {
                span["parentSpanId"] = json!(p);
            }
            span
        })
        .collect();
    let mut resource = vec![
        attr("service.name", json!({"stringValue": format!("{}-code-session", src.harness.label())})),
        attr("agent_top.harness", json!({"stringValue": src.harness.label()})),
        attr("agent_top.session_id", json!({"stringValue": session})),
    ];
    if let Some(v) = &s.harness_version {
        resource.push(attr("agent_top.harness_version", json!({"stringValue": v})));
    }
    if let Some(m) = &s.model {
        resource.push(attr("agent_top.model", json!({"stringValue": m})));
    }
    if let Some(cwd) = &s.cwd {
        resource.push(attr("agent_top.cwd", json!({"stringValue": cwd.to_string_lossy()})));
    }
    json!({
        "resourceSpans": [{
            "resource": {"attributes": resource},
            "scopeSpans": [{
                "scope": {"name": "agent-top"},
                "spans": out,
            }],
        }],
    })
}

/// The turn a span belongs to: the newest turn that started at or before it
/// and had not ended when it started. Subagent spans prefer the subagent's
/// own turn, which their transcript carries, and fall back to the main
/// agent's. A turn has no parent.
fn parent_turn<'a>(spans: &[&'a ToolSpan], i: usize) -> Option<&'a ToolSpan> {
    let sp = spans[i];
    if sp.kind == SpanKind::Turn {
        return None;
    }
    let contains = |t: &ToolSpan| {
        t.kind == SpanKind::Turn
            && t.started_at <= sp.started_at
            && t.duration_ms.map(|ms| t.started_at + std::time::Duration::from_millis(ms) >= sp.started_at).unwrap_or(true)
    };
    let own = spans[..i].iter().rev().find(|t| t.sidechain == sp.sidechain && contains(t));
    own.or_else(|| spans[..i].iter().rev().find(|t| !t.sidechain && contains(t))).copied()
}

fn attr(key: &str, value: Value) -> Value {
    json!({"key": key, "value": value})
}

fn nanos(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

/// FNV-1a over `bytes` from a chosen seed, so two independent 64-bit values
/// can be drawn from one input.
fn fnv1a(bytes: &[u8], seed: u64) -> [u8; 8] {
    let mut h = seed;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h.to_be_bytes()
}

fn hex_one(b: &[u8; 8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex(a: &[u8; 8], b: &[u8; 8]) -> String {
    hex_one(a) + &hex_one(b)
}

/// Chrome trace event format. Each tool call is a complete event (`ph: "X"`)
/// with its wall-clock start and duration in microseconds; a call that never
/// came back is a begin event with no end (`ph: "B"`), which Perfetto shows as
/// a slice with no known end rather than inventing one. The main agent's calls
/// and its subagents' calls sit on separate tracks. The process id is derived
/// from the session id, so exporting the same session twice gives the same
/// trace, and two sessions concatenated into one file stay apart.
fn chrome(src: &Source, s: &SessionSummary) -> Value {
    let pid = pid_for(s.session_id.as_deref().unwrap_or(&stem(&src.path)));
    let label = match &s.cwd {
        Some(cwd) => format!("{} {}", src.harness.label(), cwd.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
        None => src.harness.label().to_string(),
    };
    // One track per kind and side, so nothing on a track overlaps partially:
    // a tool call starts inside the inference that issued it and ends after
    // it, which Perfetto would render as a mis-nested slice on one track.
    let mut events = vec![meta("process_name", pid, 0, &label)];
    let mut used: Vec<u64> = s.spans.iter().map(tid_for).collect();
    used.sort_unstable();
    used.dedup();
    for tid in used {
        events.push(meta("thread_name", pid, tid, track_name(tid)));
    }
    events.extend(s.spans.iter().map(|sp| span_event(sp, pid)));
    let open = s.spans.iter().filter(|sp| sp.is_open()).count();
    let count = |k: SpanKind| s.spans.iter().filter(|sp| sp.kind == k).count();
    json!({
        "traceEvents": events,
        "displayTimeUnit": "ms",
        "otherData": {
            "generator": "agent-top",
            "harness": src.harness.label(),
            "harness_version": s.harness_version,
            "session_id": s.session_id,
            "model": s.model,
            "cwd": s.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "transcript": src.path.to_string_lossy(),
            "tool_calls": s.tool_calls,
            "web_searches": s.web_searches,
            "spans": s.spans.len(),
            "tool_spans": count(SpanKind::Tool),
            "inference_spans": count(SpanKind::Inference),
            "turn_spans": count(SpanKind::Turn),
            "open_spans": open,
        },
    })
}

/// Tracks 1 to 3 are the main agent's turns, tool calls and model time;
/// 4 to 6 the same for its subagents. Perfetto sorts tracks by tid, so turns
/// sit on top and the two sides stay together.
fn tid_for(sp: &ToolSpan) -> u64 {
    let kind = match sp.kind {
        SpanKind::Turn => 1,
        SpanKind::Tool => 2,
        SpanKind::Inference => 3,
    };
    if sp.sidechain { kind + 3 } else { kind }
}

fn track_name(tid: u64) -> &'static str {
    match tid {
        1 => "turns",
        2 => "tools",
        3 => "model",
        4 => "subagent turns",
        5 => "subagent tools",
        _ => "subagent model",
    }
}

fn meta(name: &str, pid: u64, tid: u64, value: &str) -> Value {
    json!({"name": name, "ph": "M", "pid": pid, "tid": tid, "args": {"name": value}})
}

fn span_event(sp: &ToolSpan, pid: u64) -> Value {
    let tid = tid_for(sp);
    let cat = sp.kind.label();
    let args = json!({"call_id": sp.id, "error": sp.error, "sidechain": sp.sidechain});
    match sp.duration_ms {
        Some(ms) => json!({
            "name": sp.name, "cat": cat, "ph": "X",
            "ts": micros(sp.started_at), "dur": ms * 1000,
            "pid": pid, "tid": tid, "args": args,
        }),
        None => json!({
            "name": sp.name, "cat": cat, "ph": "B",
            "ts": micros(sp.started_at),
            "pid": pid, "tid": tid, "args": args,
        }),
    }
}

fn micros(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

/// A stable, positive process id for a session: FNV-1a of the id, folded to
/// 31 bits so every consumer that stores pids as a signed 32-bit integer is
/// happy. Never zero, which the format reserves for metadata.
fn pid_for(session_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    ((h ^ (h >> 32)) & 0x7fff_ffff).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn pids_are_deterministic_positive_and_distinct() {
        assert_eq!(pid_for("abc"), pid_for("abc"));
        assert_ne!(pid_for("abc"), pid_for("abd"));
        assert!(pid_for("") >= 1);
        assert!(pid_for("00000000-1111-2222-3333-444444444444") <= 0x7fff_ffff);
    }

    #[test]
    fn matches_ids_by_prefix_across_harnesses() {
        let known = vec![
            (
                Harness::Claude,
                "00000000-1111-2222-3333-444444444444".to_string(),
                PathBuf::from("/c/p/00000000-1111-2222-3333-444444444444.jsonl"),
            ),
            (
                Harness::Codex,
                "01000000-0000-7000-0000-000000000000".to_string(),
                PathBuf::from("/x/2026/05/14/rollout-2026-05-14T21-37-50-01000000-0000-7000-0000-000000000000.jsonl"),
            ),
            (
                Harness::Codex,
                "0f000000-0000-7000-0000-000000000000".to_string(),
                PathBuf::from("/x/2026/05/15/rollout-2026-05-15T09-00-00-0f000000-0000-7000-0000-000000000000.jsonl"),
            ),
            (
                Harness::Gemini,
                "0a1b2c3d-0000-4000-8000-000000000001".to_string(),
                PathBuf::from("/g/tmp/p/chats/session-2026-09-05T09-00-0a1b2c3d.jsonl"),
            ),
        ];
        let one = candidates("0100", &known);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].harness, Harness::Codex);
        let one = candidates("00000000-1111", &known);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].harness, Harness::Claude);
        // The Gemini file name holds eight characters of the id; the header holds the rest.
        let one = candidates("0a1b2c3d-0000", &known);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].harness, Harness::Gemini);
        // "0" is a prefix of all four.
        assert_eq!(candidates("0", &known).len(), 4);
        // The timestamp part of a file name is not an id.
        assert!(candidates("2026-05", &known).is_empty());
        assert!(candidates("zzz", &known).is_empty());
    }

    #[test]
    fn otlp_ids_are_deterministic_and_parents_are_turns() {
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(1_700_000_000 + s);
        let mk = |id: &str, kind: SpanKind, start: u64, dur: Option<u64>, side: bool| ToolSpan {
            id: id.into(),
            name: kind.label().into(),
            started_at: at(start),
            duration_ms: dur,
            sidechain: side,
            error: false,
            kind,
        };
        // Turn 1 (0..10 s) holds an inference and a tool call; a subagent
        // call at 5 s has no subagent turn and falls back to the main one.
        // Turn 2 is still open and holds the call at 20 s. The call at 12 s
        // sits between turns and has no parent.
        let spans = vec![
            mk("turn:1", SpanKind::Turn, 0, Some(10_000), false),
            mk("inference:1", SpanKind::Inference, 0, Some(2_000), false),
            mk("t1", SpanKind::Tool, 2, Some(1_000), false),
            mk("sub", SpanKind::Tool, 5, Some(1_000), true),
            mk("t2", SpanKind::Tool, 12, Some(1_000), false),
            mk("turn:2", SpanKind::Turn, 15, None, false),
            mk("t3", SpanKind::Tool, 20, None, false),
        ];
        let refs: Vec<&ToolSpan> = spans.iter().collect();
        assert!(parent_turn(&refs, 0).is_none());
        assert_eq!(parent_turn(&refs, 1).unwrap().id, "turn:1");
        assert_eq!(parent_turn(&refs, 2).unwrap().id, "turn:1");
        assert_eq!(parent_turn(&refs, 3).unwrap().id, "turn:1");
        assert!(parent_turn(&refs, 4).is_none());
        assert_eq!(parent_turn(&refs, 6).unwrap().id, "turn:2");

        let mut summary = SessionSummary { session_id: Some("abc".into()), ..Default::default() };
        let mut log = agent_top_core::harness::SpanLog::unbounded();
        for sp in &spans {
            log.open_kind(sp.id.clone(), sp.name.clone(), sp.started_at, sp.sidechain, sp.kind);
            if let Some(ms) = sp.duration_ms {
                log.end_at(&sp.id, sp.started_at + Duration::from_millis(ms));
            }
        }
        summary.spans = log;
        let src = Source { path: PathBuf::from("/x/abc.jsonl"), harness: Harness::Claude };
        let a = otlp(&src, &summary);
        let b = otlp(&src, &summary);
        assert_eq!(a, b, "same input, same document");
        let out = a["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
        assert_eq!(out.len(), 7);
        assert!(out.iter().all(|s| s["traceId"].as_str().unwrap().len() == 32));
        assert!(out.iter().all(|s| s["spanId"].as_str().unwrap().len() == 16));
        let ids: std::collections::HashSet<&str> = out.iter().map(|s| s["spanId"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), 7, "span ids are distinct");
        assert_eq!(out[2]["parentSpanId"], out[0]["spanId"]);
        assert!(out[4].get("parentSpanId").is_none());
        assert_eq!(out[6]["parentSpanId"], out[5]["spanId"]);
        // Open spans end where they start and say so.
        assert_eq!(out[6]["startTimeUnixNano"], out[6]["endTimeUnixNano"]);
        assert!(out[6]["attributes"].as_array().unwrap().iter().any(|a| a["key"] == "agent_top.open"));
        assert_ne!(
            trace_id_of(&a),
            trace_id_of(&otlp(
                &Source { path: PathBuf::from("/x/abd.jsonl"), harness: Harness::Claude },
                &SessionSummary { session_id: Some("abd".into()), ..Default::default() }
            ))
        );
    }

    fn trace_id_of(doc: &Value) -> String {
        doc["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .and_then(|v| v.first())
            .map(|s| s["traceId"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    }

    #[test]
    fn open_spans_become_begin_events_on_their_own_track() {
        let at = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let span = |id: &str, name: &str, dur: Option<u64>, sidechain: bool, error: bool, kind: SpanKind| ToolSpan {
            id: id.into(),
            name: name.into(),
            started_at: at,
            duration_ms: dur,
            sidechain,
            error,
            kind,
        };
        let closed = span("a", "Bash", Some(2_500), false, true, SpanKind::Tool);
        let open = span("b", "Grep", None, true, false, SpanKind::Tool);
        let thinking = span("inference:1", "inference", Some(900), false, false, SpanKind::Inference);
        let turn = span("turn:1", "turn", None, true, false, SpanKind::Turn);
        let x = span_event(&closed, 7);
        assert_eq!(x["ph"], "X");
        assert_eq!(x["ts"], 1_700_000_000_123_000u64);
        assert_eq!(x["dur"], 2_500_000u64);
        assert_eq!(x["tid"], 2);
        assert_eq!(x["cat"], "tool");
        assert_eq!(x["args"]["error"], true);
        let b = span_event(&open, 7);
        assert_eq!(b["ph"], "B");
        assert!(b.get("dur").is_none());
        assert_eq!(b["tid"], 5);
        assert_eq!(span_event(&thinking, 7)["tid"], 3);
        assert_eq!(span_event(&thinking, 7)["cat"], "inference");
        assert_eq!(span_event(&turn, 7)["tid"], 4);
        assert_eq!(track_name(4), "subagent turns");
    }
}
