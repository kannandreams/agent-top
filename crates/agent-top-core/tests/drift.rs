//! Drift detection: what happens when a harness changes its transcript format.
//!
//! Every field is read with a fallback to zero, so a rename does not raise an
//! error anywhere. It shows a user 0 tokens and $0.00, which reads as a cheap
//! session rather than as a broken tool, and is therefore never reported. These
//! tests take the real fixtures and rename fields the way an upstream release
//! eventually will.

use agent_top_core::harness::claude::ClaudeTranscript;
use agent_top_core::harness::{SessionTracker, codex::CodexTranscript};
use agent_top_core::pricing;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

fn write_temp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agent-top-drift-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("transcript.jsonl");
    std::fs::write(&p, body).unwrap();
    p
}

fn claude_health(name: &str, body: &str) -> (u64, bool) {
    let p = write_temp(name, body);
    let mut t = ClaudeTranscript::new(&p).with_prices(pricing::builtin_table());
    t.refresh().unwrap();
    let s = t.summary();
    (s.usage.total(), s.health.fields_unrecognised())
}

#[test]
fn the_fixture_itself_is_healthy() {
    let (tokens, broken) = claude_health("healthy", &fixture("claude-2.1.226.jsonl"));
    assert!(tokens > 1_000_000, "the fixture really does have tokens: {tokens}");
    assert!(!broken, "a transcript we parse correctly must never be flagged");
}

#[test]
fn a_renamed_usage_record_is_caught() {
    // The container moves or is renamed, so no usage record is found at all.
    // A count of empty records cannot see this: there is nothing to count.
    let (tokens, broken) = claude_health("renamed-record", &fixture("claude-2.1.226.jsonl").replace("\"usage\":", "\"token_usage\":"));
    assert_eq!(tokens, 0, "this is what the user would have been shown");
    assert!(broken, "and it must be reported rather than displayed as zero");
}

#[test]
fn renamed_fields_inside_an_intact_record_are_caught() {
    let mut body = fixture("claude-2.1.226.jsonl");
    for field in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "ephemeral_1h_input_tokens",
        "ephemeral_5m_input_tokens",
    ] {
        body = body.replace(&format!("\"{field}\":"), &format!("\"{field}_v2\":"));
    }
    let (tokens, broken) = claude_health("renamed-fields", &body);
    assert_eq!(tokens, 0);
    assert!(broken);
}

/// Known gap, asserted so nobody assumes coverage that is not there.
///
/// The check fires on a session that accounts for no tokens at all, which is the
/// catastrophic case: every number on screen wrong, and wrong in the direction
/// that looks like good news. It cannot see a partial rename, where some fields
/// still read and the total is merely too low. Catching that needs a notion of
/// which fields ought to be present, and every harness adds fields routinely
/// (this fixture alone carries `iterations`, `service_tier`, `speed` and
/// `inference_geo`, none of which agent-top reads), so a check on unknown keys
/// would cry wolf on healthy transcripts and train people to ignore it.
#[test]
fn a_partial_rename_is_not_detected() {
    let body = fixture("claude-2.1.226.jsonl")
        .replace("\"input_tokens\":", "\"input_tokens_v2\":")
        .replace("\"output_tokens\":", "\"output_tokens_v2\":");
    let (tokens, broken) = claude_health("partial", &body);
    assert!(tokens > 0, "cache fields still read, so the total is wrong rather than zero");
    assert!(!broken, "and nothing flags it; this is the limit of the current check");
}

#[test]
fn a_session_too_young_to_judge_is_left_alone() {
    // Two responses with no usage is not evidence of anything. Crying wolf on a
    // session that has barely started would train users to ignore the warning.
    let line = r#"{"type":"assistant","timestamp":"2026-09-03T07:00:0{N}.000Z","message":{"id":"m{N}","model":"claude-sonnet-5","stop_reason":"end_turn","content":[]}}"#;
    let body = format!("{}\n{}\n", line.replace("{N}", "1"), line.replace("{N}", "2"));
    let (_, broken) = claude_health("young", &body);
    assert!(!broken, "two messages is not enough to accuse the parser");
}

#[test]
fn codex_drift_is_caught_too() {
    let body = fixture("codex-0.130.jsonl").replace("\"total_token_usage\":", "\"total_usage\":");
    let p = write_temp("codex", &body);
    let mut t = CodexTranscript::new(&p).with_prices(pricing::builtin_table());
    t.refresh().unwrap();
    assert_eq!(t.summary().usage.total(), 0);
    assert!(t.summary().health.fields_unrecognised(), "the same rename in the other harness");
}
