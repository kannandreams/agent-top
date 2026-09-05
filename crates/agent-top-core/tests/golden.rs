//! Golden tests: parse a real (sanitised) transcript and assert every number
//! it produces, field by field.
//!
//! The unit tests next to each adapter build their input inline, so they only
//! prove the parser agrees with what its author believed the format was. These
//! run against transcripts a harness actually wrote, and they pin the output,
//! so a refactor that quietly changes a token count, a cost, or a span
//! duration fails here instead of shipping.
//!
//! Re-record deliberately, and read the diff:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test -p agent-top-core --test golden
//! ```

use agent_top_core::SpanKind;
use agent_top_core::harness::claude::ClaudeTranscript;
use agent_top_core::harness::codex::CodexTranscript;
use agent_top_core::harness::gemini::GeminiTranscript;
use agent_top_core::harness::{SessionSummary, SessionTracker};
use agent_top_core::pricing;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn millis(t: Option<SystemTime>) -> Value {
    match t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => json!(d.as_millis() as u64),
        None => Value::Null,
    }
}

/// Everything the collector goes on to use. If a field is not in here, a change
/// to it is not covered, so add rather than trim.
fn describe(s: &SessionSummary) -> Value {
    let u = &s.usage;
    json!({
        "session_id": s.session_id,
        "cwd": s.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        "model": s.model,
        "harness_version": s.harness_version,
        "usage": {
            "input": u.input,
            "cache_write_5m": u.cache_write_5m,
            "cache_write_1h": u.cache_write_1h,
            "cache_read": u.cache_read,
            "output": u.output,
            "total": u.total(),
        },
        "cost_usd_micros": (s.cost_usd * 1_000_000.0).round() as i64,
        "cost_breakdown_micros": {
            "input": (s.cost_breakdown.input * 1_000_000.0).round() as i64,
            "cache_write_5m": (s.cost_breakdown.cache_write_5m * 1_000_000.0).round() as i64,
            "cache_write_1h": (s.cost_breakdown.cache_write_1h * 1_000_000.0).round() as i64,
            "cache_read": (s.cost_breakdown.cache_read * 1_000_000.0).round() as i64,
            "output": (s.cost_breakdown.output * 1_000_000.0).round() as i64,
            "web_search": (s.cost_breakdown.web_search * 1_000_000.0).round() as i64,
        },
        "unpriced_tokens": s.unpriced_tokens,
        "turns": s.turns,
        "subagent_turns": s.subagent_turns,
        "tool_calls": s.tool_calls,
        "web_searches": s.web_searches,
        "mcp": s.mcp.iter().map(|(k, u)| json!({"server": k, "calls": u.calls, "errors": u.errors})).collect::<Vec<_>>(),
        "activity": format!("{:?}", s.activity),
        "started_at_ms": millis(s.started_at),
        "last_activity_ms": millis(s.last_activity),
        "spans": {
            "count": s.spans.len(),
            "by_kind": SpanKind::ALL.iter().map(|k| json!({
                "kind": k.label(),
                "count": s.spans.iter().filter(|sp| sp.kind == *k).count(),
                "open": s.spans.iter().filter(|sp| sp.kind == *k && sp.is_open()).count(),
                "total_duration_ms": s.spans.iter().filter(|sp| sp.kind == *k).filter_map(|sp| sp.duration_ms).sum::<u64>(),
            })).collect::<Vec<_>>(),
            "open": s.spans.iter().filter(|sp| sp.is_open()).count(),
            "errors": s.spans.iter().filter(|sp| sp.error).count(),
            "sidechain": s.spans.iter().filter(|sp| sp.sidechain).count(),
            "total_duration_ms": s.spans.iter().filter_map(|sp| sp.duration_ms).sum::<u64>(),
            // The first and last few in full, so a pairing bug that keeps the
            // count right cannot slip through.
            "first": s.spans.iter().take(3).map(span_json).collect::<Vec<_>>(),
            "last": s.spans.iter().rev().take(3).map(span_json).collect::<Vec<_>>(),
        },
    })
}

fn span_json(s: &agent_top_core::ToolSpan) -> Value {
    json!({"kind": s.kind.label(), "name": s.name, "duration_ms": s.duration_ms, "error": s.error, "sidechain": s.sidechain})
}

fn check(fixture: &str, mut tracker: Box<dyn SessionTracker>) {
    tracker.refresh().expect("fixture is readable");
    let got = describe(tracker.summary());
    let golden = fixtures().join(format!("{fixture}.expected.json"));

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
        return;
    }

    let want: Value = serde_json::from_str(
        &std::fs::read_to_string(&golden).unwrap_or_else(|e| panic!("{}: {e}. Record it with UPDATE_GOLDEN=1", golden.display())),
    )
    .expect("golden file is valid JSON");

    if got != want {
        // A whole-document diff is unreadable; name the fields that moved.
        let (g, w) = (got.as_object().unwrap(), want.as_object().unwrap());
        let mut moved = String::new();
        for (k, wv) in w {
            let gv = g.get(k).unwrap_or(&Value::Null);
            if gv != wv {
                moved.push_str(&format!("\n  {k}:\n    golden: {wv}\n    now:    {gv}"));
            }
        }
        panic!(
            "{fixture} parses differently than its golden.{moved}\n\n\
             If this change was intended, re-record and review the diff:\n  \
             UPDATE_GOLDEN=1 cargo test -p agent-top-core --test golden\n"
        );
    }
}

// Both price with the built-in table only. Reading the developer's own
// ~/.config/agent-top/prices.toml here would make the golden costs depend on
// whose machine the suite runs on.
#[test]
fn claude_2_1_226() {
    let p = fixtures().join("claude-2.1.226.jsonl");
    check("claude-2.1.226", Box::new(ClaudeTranscript::new(p).with_prices(pricing::builtin_table())));
}

#[test]
fn codex_0_130() {
    let p = fixtures().join("codex-0.130.jsonl");
    check("codex-0.130", Box::new(CodexTranscript::new(p).with_prices(pricing::builtin_table())));
}

/// Written by Gemini CLI 0.58.0's own recorder, driven with a scripted
/// conversation (see the fixture README), so the layout is the harness's and
/// the numbers are known: three turns on `gemini-2.5-pro`, one subagent turn
/// on `gemini-2.5-flash` folded in, a failed tool call, a web search, and a
/// rewind that must not change what was spent.
#[test]
fn gemini_0_58() {
    let p = fixtures().join("gemini-0.58.jsonl");
    check("gemini-0.58", Box::new(GeminiTranscript::new(p).with_prices(pricing::builtin_table())));
}

/// A redacted real Codex session (0.152) whose only point is the MCP calls:
/// two to `codex_apps` (one failing) and one to `node_repl`, each recorded as
/// a `response_item` function call plus an `mcp_tool_call_end` that names the
/// server. The end lines feed the per-server map; the tool count and spans
/// come from the function-call pairs, so the MCP lines must not double them.
#[test]
fn codex_mcp_0_152() {
    let p = fixtures().join("codex-mcp-0.152.jsonl");
    check("codex-mcp-0.152", Box::new(CodexTranscript::new(p).with_prices(pricing::builtin_table())));
}
