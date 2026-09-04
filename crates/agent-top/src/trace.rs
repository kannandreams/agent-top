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
//! `ui.perfetto.dev` and `chrome://tracing` both open it directly.

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
            .with_context(|| format!("{what}: not a transcript agent-top knows how to read (Claude Code or Codex JSONL)"))?;
        return Ok(Source { path: as_path.to_path_buf(), harness });
    }
    if what.is_empty() {
        bail!("a session id or transcript path is required");
    }
    let candidates = candidates(what, &harness::claude::recent_transcripts(UNIX_EPOCH), &harness::codex::recent_rollouts(UNIX_EPOCH));
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

fn candidates(prefix: &str, claude: &[PathBuf], codex: &[PathBuf]) -> Vec<Source> {
    let mut out = Vec::new();
    for p in claude {
        if stem(p).starts_with(prefix) {
            out.push(Source { path: p.clone(), harness: Harness::Claude });
        }
    }
    for p in codex {
        if codex_id(&stem(p)).starts_with(prefix) {
            out.push(Source { path: p.clone(), harness: Harness::Codex });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The id in `rollout-2026-05-14T21-37-50-<id>`: what follows the fixed-width
/// timestamp. A file named some other way is matched on its whole stem.
fn codex_id(stem: &str) -> &str {
    const TS_LEN: usize = "2026-05-14T21-37-50-".len();
    stem.strip_prefix("rollout-").and_then(|s| s.get(TS_LEN..)).unwrap_or(stem)
}

/// Read the whole transcript with every span kept.
pub fn read(src: &Source) -> Result<SessionSummary> {
    let mut tracker = harness::open_transcript(&src.path, src.harness, SpanRetention::All);
    tracker.refresh_all().with_context(|| format!("reading {}", src.path.display()))?;
    Ok(tracker.summary().clone())
}

/// Render a session in the requested format.
pub fn render(src: &Source, summary: &SessionSummary, format: Format) -> Value {
    match format {
        Format::Chrome => chrome(src, summary),
    }
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
    fn matches_ids_by_prefix_in_both_layouts() {
        let claude = vec![PathBuf::from("/c/p/00000000-1111-2222-3333-444444444444.jsonl"), PathBuf::from("/c/p/agent-0000aaaa.jsonl")];
        let codex = vec![
            PathBuf::from("/x/2026/05/14/rollout-2026-05-14T21-37-50-01000000-0000-7000-0000-000000000000.jsonl"),
            PathBuf::from("/x/2026/05/15/rollout-2026-05-15T09-00-00-0f000000-0000-7000-0000-000000000000.jsonl"),
        ];
        let one = candidates("0100", &claude, &codex);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].harness, Harness::Codex);
        let one = candidates("00000000-1111", &claude, &codex);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].harness, Harness::Claude);
        // "0" is a prefix of three of them: one Claude, two Codex.
        assert_eq!(candidates("0", &claude, &codex).len(), 3);
        // The timestamp part of a rollout name is not an id.
        assert!(candidates("2026-05", &claude, &codex).is_empty());
        assert!(candidates("zzz", &claude, &codex).is_empty());
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
