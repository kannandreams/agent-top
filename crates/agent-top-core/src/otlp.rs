//! Spans an agent sends over OTLP, reduced to what agent-top keeps (RFC-107
//! D3, DEC-026).
//!
//! The decoders in the binary turn a request into `ReceivedSpan`s, passing
//! every attribute key through `keep`. Keys not on the allow-list are dropped
//! there, so prompt, completion, system-instruction and tool argument or
//! result attributes never reach the store, and a content attribute added
//! upstream later is dropped by default.
//!
//! Attribute names follow the OpenTelemetry GenAI semantic conventions,
//! verified against `semantic-conventions-genai` at schema
//! `gen-ai-dev/1.42.0-dev` (commit cb10b70, 2026-10-05). In that version
//! `gen_ai.usage.input_tokens` includes the cache read and cache write
//! tokens, so the fresh input is what is left after subtracting them.

use crate::model::{SpanKind, TokenUsage};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Every attribute key a received span or resource may keep.
const ALLOWED: &[&str] = &[
    "service.name",
    "gen_ai.operation.name",
    "gen_ai.provider.name",
    "gen_ai.request.model",
    "gen_ai.response.model",
    "gen_ai.conversation.id",
    "gen_ai.agent.name",
    "gen_ai.tool.name",
    "gen_ai.usage.input_tokens",
    "gen_ai.usage.output_tokens",
    "gen_ai.usage.cache_read.input_tokens",
    "gen_ai.usage.cache_write.input_tokens",
    // Names older instrumentations still send.
    "gen_ai.usage.prompt_tokens",
    "gen_ai.usage.completion_tokens",
    "gen_ai.system",
    // What `agent-top trace --format otlp` writes.
    "agent_top.harness",
    "agent_top.session_id",
    "agent_top.model",
    "agent_top.kind",
    "agent_top.sidechain",
    "agent_top.mcp.server",
    "agent_top.open",
];

/// Whether an attribute with this key is kept.
pub fn keep(key: &str) -> bool {
    ALLOWED.contains(&key)
}

/// An attribute value. Arrays, maps and bytes are never on the allow-list.
#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Attr {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Attr::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A count: an integer, or a string or float holding one.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Attr::Int(i) => u64::try_from(*i).ok(),
            Attr::Float(f) if *f >= 0.0 && f.fract() == 0.0 => Some(*f as u64),
            Attr::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Attr::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// One received span, with only allow-listed attributes. Resource attributes
/// are merged in under the span's own, which win.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedSpan {
    /// Lower-case hex.
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_unix_nano: u64,
    pub end_unix_nano: u64,
    pub error: bool,
    pub attrs: BTreeMap<String, Attr>,
}

/// Where a span's tokens count: every inference span, and an agent span only
/// in a session that sent no inference span with usage, so an agent span
/// that repeats its children's totals is not counted twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRole {
    Inference,
    Agent,
    None,
}

impl ReceivedSpan {
    fn str(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).and_then(Attr::as_str)
    }

    fn count(&self, key: &str) -> Option<u64> {
        self.attrs.get(key).and_then(Attr::as_u64)
    }

    pub fn operation(&self) -> Option<&str> {
        self.str("gen_ai.operation.name")
    }

    /// The span's kind in agent-top's model, from `gen_ai.operation.name`,
    /// else the `agent_top.kind` the trace export writes. Other operations
    /// (embeddings, retrieval, memory) have none.
    pub fn kind(&self) -> Option<SpanKind> {
        match self.operation() {
            Some("chat" | "generate_content" | "text_completion") => Some(SpanKind::Inference),
            Some("execute_tool") => Some(SpanKind::Tool),
            Some("invoke_agent" | "invoke_workflow") => Some(SpanKind::Turn),
            Some(_) => None,
            None => match self.str("agent_top.kind") {
                Some("inference") => Some(SpanKind::Inference),
                Some("tool") => Some(SpanKind::Tool),
                Some("turn") => Some(SpanKind::Turn),
                _ => None,
            },
        }
    }

    pub fn usage_role(&self) -> UsageRole {
        match self.operation() {
            Some("chat" | "generate_content" | "text_completion" | "embeddings") => UsageRole::Inference,
            Some("invoke_agent" | "invoke_workflow") => UsageRole::Agent,
            _ => UsageRole::None,
        }
    }

    /// The name a span is shown under: the tool for a tool call, else the
    /// span's own name.
    pub fn display_name(&self) -> String {
        match (self.kind(), self.str("gen_ai.tool.name")) {
            (Some(SpanKind::Tool), Some(t)) => t.to_string(),
            _ => self.name.clone(),
        }
    }

    /// The response model, else the requested one, else the one the trace
    /// export names.
    pub fn model(&self) -> Option<&str> {
        self.str("gen_ai.response.model").or_else(|| self.str("gen_ai.request.model")).or_else(|| self.str("agent_top.model"))
    }

    pub fn provider(&self) -> Option<&str> {
        self.str("gen_ai.provider.name").or_else(|| self.str("gen_ai.system"))
    }

    pub fn service(&self) -> &str {
        self.str("service.name").unwrap_or("unknown_service")
    }

    /// `agent_top.harness`, else `otel`.
    pub fn harness(&self) -> &str {
        self.str("agent_top.harness").unwrap_or("otel")
    }

    /// The conversation this span belongs to: `gen_ai.conversation.id`, else
    /// the trace export's session id, else the trace id.
    pub fn conversation(&self) -> &str {
        self.str("gen_ai.conversation.id").or_else(|| self.str("agent_top.session_id")).unwrap_or(&self.trace_id)
    }

    /// The store's session id for this span: `<service.name>/<conversation>`,
    /// so a received session never collides with a transcript's id.
    pub fn session_id(&self) -> String {
        format!("{}/{}", self.service(), self.conversation())
    }

    pub fn sidechain(&self) -> bool {
        self.attrs.get("agent_top.sidechain").and_then(Attr::as_bool).unwrap_or(false)
    }

    /// Tokens, split the way the price table charges them.
    pub fn usage(&self) -> TokenUsage {
        let cache_read = self.count("gen_ai.usage.cache_read.input_tokens").unwrap_or(0);
        let cache_write = self.count("gen_ai.usage.cache_write.input_tokens").unwrap_or(0);
        let input = self.count("gen_ai.usage.input_tokens").or_else(|| self.count("gen_ai.usage.prompt_tokens")).unwrap_or(0);
        let output = self.count("gen_ai.usage.output_tokens").or_else(|| self.count("gen_ai.usage.completion_tokens")).unwrap_or(0);
        TokenUsage {
            input: input.saturating_sub(cache_read + cache_write),
            // The semconv has one cache-write count with no TTL; the 5-minute
            // rate is the default write every provider in the table uses.
            cache_write_5m: cache_write,
            cache_read,
            output,
            ..Default::default()
        }
    }

    pub fn started_at(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.start_unix_nano)
    }

    pub fn duration_ms(&self) -> u64 {
        self.end_unix_nano.saturating_sub(self.start_unix_nano) / 1_000_000
    }

    /// The trace export marks a span that had not finished.
    pub fn is_open(&self) -> bool {
        self.attrs.get("agent_top.open").and_then(Attr::as_bool).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(attrs: &[(&str, Attr)]) -> ReceivedSpan {
        ReceivedSpan {
            trace_id: "t".into(),
            span_id: "s".into(),
            parent_span_id: None,
            name: "chat claude".into(),
            start_unix_nano: 1_000_000_000,
            end_unix_nano: 1_250_000_000,
            error: false,
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }

    #[test]
    fn content_attributes_are_not_on_the_allow_list() {
        for k in [
            "gen_ai.input.messages",
            "gen_ai.output.messages",
            "gen_ai.system_instructions",
            "gen_ai.tool.call.arguments",
            "gen_ai.tool.call.result",
            "gen_ai.prompt.0.content",
        ] {
            assert!(!keep(k), "{k}");
        }
        assert!(keep("gen_ai.usage.input_tokens"));
    }

    #[test]
    fn input_tokens_include_the_cache_and_are_split_out() {
        let s = span(&[
            ("gen_ai.operation.name", Attr::Str("chat".into())),
            ("gen_ai.usage.input_tokens", Attr::Int(165)),
            ("gen_ai.usage.cache_read.input_tokens", Attr::Int(100)),
            ("gen_ai.usage.cache_write.input_tokens", Attr::Int(25)),
            ("gen_ai.usage.output_tokens", Attr::Str("12".into())),
        ]);
        let u = s.usage();
        assert_eq!((u.input, u.cache_read, u.cache_write_5m, u.output), (40, 100, 25, 12));
        assert_eq!(u.total(), 177);
        assert_eq!(s.kind(), Some(SpanKind::Inference));
        assert_eq!(s.usage_role(), UsageRole::Inference);
        assert_eq!(s.duration_ms(), 250);
    }

    #[test]
    fn older_names_still_count() {
        let s = span(&[("gen_ai.usage.prompt_tokens", Attr::Int(10)), ("gen_ai.usage.completion_tokens", Attr::Int(3))]);
        assert_eq!((s.usage().input, s.usage().output), (10, 3));
    }

    #[test]
    fn kinds_come_from_the_operation_or_the_trace_export() {
        let tool = span(&[("gen_ai.operation.name", Attr::Str("execute_tool".into())), ("gen_ai.tool.name", Attr::Str("search".into()))]);
        assert_eq!((tool.kind(), tool.display_name().as_str()), (Some(SpanKind::Tool), "search"));
        assert_eq!(span(&[("gen_ai.operation.name", Attr::Str("invoke_agent".into()))]).kind(), Some(SpanKind::Turn));
        assert_eq!(span(&[("gen_ai.operation.name", Attr::Str("embeddings".into()))]).kind(), None);
        assert_eq!(span(&[("agent_top.kind", Attr::Str("tool".into()))]).kind(), Some(SpanKind::Tool));
        assert_eq!(span(&[]).kind(), None);
    }

    #[test]
    fn a_session_is_keyed_by_service_and_conversation() {
        let s = span(&[("service.name", Attr::Str("triage-bot".into())), ("gen_ai.conversation.id", Attr::Str("c1".into()))]);
        assert_eq!(s.session_id(), "triage-bot/c1");
        assert_eq!(span(&[]).session_id(), "unknown_service/t");
        assert_eq!(s.harness(), "otel");
    }
}
