//! Spans received by `agent-top serve --listen` (RFC-107 D2, DEC-026).
//!
//! Each span is kept once in `telemetry_spans`, keyed by trace and span id,
//! so a batch an exporter retries changes nothing. After each batch, every
//! session it touched is rebuilt from all of its spans and written to the
//! same `sessions`, `spans` and `mcp_calls` tables a transcript fills, with
//! `attribution = 'telemetry'` and the service name as the project.

use crate::{Store, ms, replace_session};
use agent_top_core::harness::{McpUsage, SessionSummary, SpanLog, mcp_server_of};
use agent_top_core::model::{SpanKind, TokenUsage};
use agent_top_core::otlp::{Attr, ReceivedSpan, UsageRole};
use agent_top_core::pricing;
use anyhow::Result;
use rusqlite::{Transaction, params};
use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What one received batch did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Received {
    pub spans: u64,
    /// Spans not already in the store.
    pub new_spans: u64,
    /// Sessions rebuilt.
    pub sessions: u64,
}

impl Store {
    /// Keep a batch of received spans and rebuild the sessions they belong to.
    pub fn receive(&mut self, spans: &[ReceivedSpan]) -> Result<Received> {
        let now = ms(SystemTime::now());
        let tx = self.conn.transaction()?;
        let mut touched = BTreeSet::new();
        let mut new_spans = 0;
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO telemetry_spans (trace_id, span_id, parent_span_id, harness, session_id, service, kind,
                    usage_role, name, model, provider, mcp_server, started_at, duration_ms, open, error, sidechain,
                    input, cache_write_5m, cache_read, output, received_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            )?;
            for sp in spans {
                let u = sp.usage();
                let kind = sp.kind();
                let name = sp.display_name();
                let mcp = match kind {
                    Some(SpanKind::Tool) => sp
                        .attrs
                        .get("agent_top.mcp.server")
                        .and_then(Attr::as_str)
                        .map(str::to_string)
                        .or_else(|| mcp_server_of(&name).map(str::to_string)),
                    _ => None,
                };
                let role = match sp.usage_role() {
                    UsageRole::Inference => "inference",
                    UsageRole::Agent => "agent",
                    UsageRole::None => "none",
                };
                let (harness, session) = (sp.harness().to_string(), sp.session_id());
                let added = insert.execute(params![
                    sp.trace_id,
                    sp.span_id,
                    sp.parent_span_id,
                    harness,
                    session,
                    sp.service(),
                    kind.map(|k| k.label()),
                    role,
                    name,
                    sp.model(),
                    sp.provider(),
                    mcp,
                    ms(sp.started_at()),
                    sp.duration_ms() as i64,
                    sp.is_open(),
                    sp.error,
                    sp.sidechain(),
                    u.input as i64,
                    u.cache_write_5m as i64,
                    u.cache_read as i64,
                    u.output as i64,
                    now,
                ])?;
                if added > 0 {
                    new_spans += 1;
                    touched.insert((harness, session));
                }
            }
        }
        for (harness, session) in &touched {
            rebuild(&tx, harness, session, now)?;
        }
        tx.commit()?;
        Ok(Received { spans: spans.len() as u64, new_spans, sessions: touched.len() as u64 })
    }
}

struct Row {
    span_id: String,
    parent: Option<String>,
    service: String,
    kind: Option<SpanKind>,
    role: String,
    name: String,
    model: Option<String>,
    mcp: Option<String>,
    started_at: SystemTime,
    duration: Duration,
    open: bool,
    error: bool,
    sidechain: bool,
    usage: TokenUsage,
}

/// Write one received session from every span the store holds for it.
fn rebuild(tx: &Transaction, harness: &str, session: &str, now: i64) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT span_id, parent_span_id, service, kind, usage_role, name, model, mcp_server, started_at, duration_ms,
                open, error, sidechain, input, cache_write_5m, cache_read, output
         FROM telemetry_spans WHERE harness = ?1 AND session_id = ?2 ORDER BY started_at, span_id",
    )?;
    let n = |r: &rusqlite::Row, i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
    let rows: Vec<Row> = stmt
        .query_map(params![harness, session], |r| {
            let kind: Option<String> = r.get(3)?;
            Ok(Row {
                span_id: r.get(0)?,
                parent: r.get(1)?,
                service: r.get(2)?,
                kind: kind.and_then(|k| SpanKind::ALL.into_iter().find(|s| s.label() == k)),
                role: r.get(4)?,
                name: r.get(5)?,
                model: r.get(6)?,
                mcp: r.get(7)?,
                started_at: UNIX_EPOCH + Duration::from_millis(n(r, 8)?),
                duration: Duration::from_millis(n(r, 9)?),
                open: r.get(10)?,
                error: r.get(11)?,
                sidechain: r.get(12)?,
                usage: TokenUsage {
                    input: n(r, 13)?,
                    cache_write_5m: n(r, 14)?,
                    cache_read: n(r, 15)?,
                    output: n(r, 16)?,
                    ..Default::default()
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let Some(first) = rows.first() else { return Ok(()) };
    let service = first.service.clone();

    // Tokens count from inference spans. Only a session that sent none with
    // usage counts its outermost agent spans instead, since an agent span may
    // repeat its children's totals.
    let has = |r: &Row, role: &str| r.role == role && r.usage.total() > 0;
    let inference = rows.iter().any(|r| has(r, "inference"));
    let agents: HashSet<&str> = rows.iter().filter(|r| has(r, "agent")).map(|r| r.span_id.as_str()).collect();
    let counted = rows.iter().filter(|r| {
        if inference { has(r, "inference") } else { has(r, "agent") && !r.parent.as_deref().is_some_and(|p| agents.contains(p)) }
    });

    let table = pricing::table();
    let model = rows
        .iter()
        .rev()
        .find(|r| r.role == "inference" && r.model.is_some())
        .or_else(|| rows.iter().rev().find(|r| r.model.is_some()))
        .and_then(|r| r.model.clone());
    let mut s = SessionSummary { model: model.clone(), spans: SpanLog::unbounded(), ..Default::default() };
    for r in counted {
        s.usage.add(&r.usage);
        match r.model.as_deref().or(model.as_deref()).and_then(|m| table.lookup(m)) {
            Some(price) => s.cost_usd += price.cost(&r.usage),
            None => s.unpriced_tokens += r.usage.total(),
        }
    }
    s.price_source = model.as_deref().and_then(|m| table.source_for(m));
    s.started_at = rows.iter().map(|r| r.started_at).min();
    s.last_activity = rows.iter().map(|r| r.started_at + r.duration).max();
    for r in &rows {
        let Some(kind) = r.kind else { continue };
        match kind {
            SpanKind::Turn => s.turns += 1,
            SpanKind::Tool => s.tool_calls += 1,
            SpanKind::Inference => {}
        }
        s.spans.open_kind(r.span_id.clone(), r.name.clone(), r.started_at, r.sidechain, kind);
        if !r.open {
            s.spans.close(&r.span_id, r.started_at + r.duration, r.error);
        }
        if let Some(server) = &r.mcp {
            let end = r.started_at + r.duration;
            s.mcp.entry(server.clone()).or_default().add(&McpUsage { calls: 1, errors: r.error as u64, last_call: Some(end) });
        }
    }
    replace_session(tx, harness, session, &format!("otlp:{service}"), "telemetry", Some(service.clone()), &s, now)
}
