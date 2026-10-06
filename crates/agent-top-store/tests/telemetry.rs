//! Received spans: kept once, folded into a session priced from the table,
//! and the tokens an agent span repeats are not counted twice.

mod common;

use agent_top_core::otlp::{Attr, ReceivedSpan};
use agent_top_core::pricing;
use agent_top_store::{Received, Store};
use common::TempDir;

fn span(id: &str, parent: Option<&str>, start_ms: u64, dur_ms: u64, attrs: &[(&str, Attr)]) -> ReceivedSpan {
    let mut all: Vec<(&str, Attr)> =
        vec![("service.name", Attr::Str("triage-bot".into())), ("gen_ai.conversation.id", Attr::Str("c1".into()))];
    all.extend(attrs.iter().cloned());
    ReceivedSpan {
        trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
        span_id: id.into(),
        parent_span_id: parent.map(str::to_string),
        name: id.into(),
        start_unix_nano: start_ms * 1_000_000,
        end_unix_nano: (start_ms + dur_ms) * 1_000_000,
        error: false,
        attrs: all.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn chat(id: &str, parent: Option<&str>, start_ms: u64, input: i64, output: i64) -> ReceivedSpan {
    span(
        id,
        parent,
        start_ms,
        800,
        &[
            ("gen_ai.operation.name", Attr::Str("chat".into())),
            ("gen_ai.response.model", Attr::Str("claude-sonnet-5".into())),
            ("gen_ai.usage.input_tokens", Attr::Int(input)),
            ("gen_ai.usage.output_tokens", Attr::Int(output)),
        ],
    )
}

fn tool(id: &str, parent: Option<&str>, start_ms: u64, name: &str) -> ReceivedSpan {
    span(
        id,
        parent,
        start_ms,
        300,
        &[("gen_ai.operation.name", Attr::Str("execute_tool".into())), ("gen_ai.tool.name", Attr::Str(name.into()))],
    )
}

fn agent(id: &str, start_ms: u64, input: i64, output: i64) -> ReceivedSpan {
    span(
        id,
        None,
        start_ms,
        5_000,
        &[
            ("gen_ai.operation.name", Attr::Str("invoke_agent".into())),
            ("gen_ai.request.model", Attr::Str("claude-sonnet-5".into())),
            ("gen_ai.usage.input_tokens", Attr::Int(input)),
            ("gen_ai.usage.output_tokens", Attr::Int(output)),
        ],
    )
}

/// (tokens, cost, turns, tool_calls, spans, attribution, project, harness)
fn session(store: &Store) -> (i64, f64, i64, i64, i64, String, String, String) {
    store
        .connection()
        .query_row(
            "SELECT tokens, cost_usd, turns, tool_calls,
                    (SELECT count(*) FROM spans p WHERE p.session_id = s.session_id), attribution, project, harness
             FROM counted_sessions s WHERE session_id = 'triage-bot/c1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .unwrap()
}

fn sonnet(input: u64, output: u64) -> f64 {
    let u = agent_top_core::model::TokenUsage { input, output, ..Default::default() };
    pricing::table().lookup("claude-sonnet-5").unwrap().cost(&u)
}

#[test]
fn a_batch_becomes_a_priced_session_and_a_retry_changes_nothing() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let batch = vec![
        agent("a", 0, 1_500, 70),
        chat("c1", Some("a"), 100, 1_000, 50),
        tool("t1", Some("a"), 1_000, "search"),
        chat("c2", Some("a"), 1_500, 500, 20),
    ];
    assert_eq!(store.receive(&batch).unwrap(), Received { spans: 4, new_spans: 4, sessions: 1 });
    let (tokens, cost, turns, tools, spans, attribution, project, harness) = session(&store);
    // The agent span repeats its children's totals; only the children count.
    assert_eq!(tokens, 1_570);
    assert!((cost - sonnet(1_500, 70)).abs() < 1e-12, "{cost}");
    assert_eq!((turns, tools, spans), (1, 1, 4));
    assert_eq!((attribution.as_str(), project.as_str(), harness.as_str()), ("telemetry", "triage-bot", "otel"));

    assert_eq!(store.receive(&batch).unwrap(), Received { spans: 4, new_spans: 0, sessions: 0 });
    assert_eq!(session(&store).0, 1_570);

    store.receive(&[chat("c3", Some("a"), 2_500, 200, 10)]).unwrap();
    assert_eq!(session(&store).0, 1_780);
}

#[test]
fn with_no_inference_spans_the_outermost_agent_span_counts() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let mut inner = agent("inner", 100, 300, 30);
    inner.parent_span_id = Some("outer".into());
    store.receive(&[agent("outer", 0, 1_000, 100), inner]).unwrap();
    assert_eq!(session(&store).0, 1_100);
}

#[test]
fn an_unknown_model_is_unpriced_and_a_tool_error_is_kept() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let mut c = chat("c1", None, 0, 100, 10);
    c.attrs.insert("gen_ai.response.model".into(), Attr::Str("no-such-model-9".into()));
    let mut t = tool("t1", None, 900, "mcp__docs__search");
    t.error = true;
    store.receive(&[c, t]).unwrap();
    let (cost, unpriced): (f64, i64) =
        store.connection().query_row("SELECT cost_usd, unpriced_tokens FROM sessions", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((cost, unpriced), (0.0, 110));
    let (server, calls, errors): (String, i64, i64) =
        store.connection().query_row("SELECT server, calls, errors FROM mcp_calls", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!((server.as_str(), calls, errors), ("docs", 1, 1));
}
