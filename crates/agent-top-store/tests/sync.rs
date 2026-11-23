//! Sync against the golden fixtures: the store holds what the parser
//! computes, a second sync reads nothing, and history outlives the transcript.

use agent_top_core::Harness;
use agent_top_core::harness::{self, SpanRetention};
use agent_top_store::{Store, SyncStats, WRITER_VERSION};
mod common;

use common::{TempDir, copy, source};

fn stats(stored: u64, unchanged: u64) -> SyncStats {
    SyncStats { seen: stored + unchanged, stored, unchanged, ..Default::default() }
}

#[test]
fn stored_numbers_are_the_parsers_numbers() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("db/agent-top.db")).unwrap();
    for (h, name) in [(Harness::Claude, "claude-2.1.278.jsonl"), (Harness::Codex, "codex-mcp-0.152.jsonl")] {
        let path = copy(&dir, name);
        assert_eq!(store.sync_sources(&[source(h, &path)], None).unwrap(), stats(1, 0));

        let mut t = harness::open_transcript(&path, h, SpanRetention::All).unwrap();
        t.refresh_all().unwrap();
        let s = t.summary();
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        let (tokens, cost, turns, tool_calls, spans, mcp): (i64, f64, i64, i64, i64, i64) = store
            .connection()
            .query_row(
                "SELECT input + cache_write_5m + cache_write_1h + cache_write_unsplit + cache_read + output,
                        cost_usd, turns, tool_calls,
                        (SELECT count(*) FROM spans p WHERE p.harness = s.harness AND p.session_id = s.session_id),
                        (SELECT coalesce(sum(calls), 0) FROM mcp_calls m WHERE m.harness = s.harness AND m.session_id = s.session_id)
                 FROM sessions s WHERE harness = ?1 AND session_id = ?2",
                [h.label(), id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert!(s.usage.total() > 0, "{name} should have usage");
        assert_eq!(tokens as u64, s.usage.total(), "{name}");
        assert_eq!(cost, s.cost_usd, "{name}");
        assert_eq!(turns as u64, s.turns, "{name}");
        assert_eq!(tool_calls as u64, s.tool_calls, "{name}");
        assert_eq!(spans as usize, s.spans.len(), "{name}");
        assert_eq!(mcp as u64, s.mcp.values().map(|m| m.calls).sum::<u64>(), "{name}");
    }
    assert_eq!(store.session_count().unwrap(), 2);
}

/// The window is stored only when the harness wrote one: Codex does, Claude
/// Code does not.
#[test]
fn the_context_window_is_the_harness_s_own() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    for (h, name) in [(Harness::Codex, "codex-0.130.jsonl"), (Harness::Claude, "claude-2.1.278.jsonl")] {
        store.sync_sources(&[source(h, &copy(&dir, name))], None).unwrap();
    }
    let windows: Vec<(String, Option<i64>)> = store
        .connection()
        .prepare("SELECT harness, context_window FROM sessions ORDER BY harness")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(windows, [("claude".to_string(), None), ("codex".to_string(), Some(258_400))]);
}

#[test]
fn every_tool_span_inside_a_turn_points_at_it() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = copy(&dir, "claude-2.1.278.jsonl");
    store.sync_sources(&[source(Harness::Claude, &path)], None).unwrap();
    let (tools, parented): (i64, i64) = store
        .connection()
        .query_row(
            "SELECT count(*), count(p.seq) FROM spans c
             LEFT JOIN spans p ON p.harness = c.harness AND p.session_id = c.session_id AND p.seq = c.parent_seq AND p.kind = 'turn'
             WHERE c.kind = 'tool'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(tools > 0);
    assert_eq!(tools, parented);
}

#[test]
fn a_second_sync_reads_nothing_until_the_transcript_grows() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = copy(&dir, "claude-2.1.278.jsonl");
    let src = [source(Harness::Claude, &path)];
    assert_eq!(store.sync_sources(&src, None).unwrap(), stats(1, 0));
    assert_eq!(store.sync_sources(&src, None).unwrap(), stats(0, 1));

    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(
        "{\"type\":\"system\",\"subtype\":\"informational\",\"timestamp\":\"2026-09-21T20:30:00.000Z\",\"version\":\"2.1.278\"}\n",
    );
    std::fs::write(&path, text).unwrap();
    assert_eq!(store.sync_sources(&src, None).unwrap(), stats(1, 0));
}

#[test]
fn an_older_writer_version_is_read_again() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = copy(&dir, "claude-2.1.278.jsonl");
    let src = [source(Harness::Claude, &path)];
    store.sync_sources(&src, None).unwrap();
    store.connection().execute("UPDATE sources SET synced_by_version = '0.0.1'", []).unwrap();
    assert_eq!(store.sync_sources(&src, None).unwrap(), stats(1, 0));
    let v: String = store.connection().query_row("SELECT synced_by_version FROM sessions", [], |r| r.get(0)).unwrap();
    assert_eq!(v, WRITER_VERSION);
}

#[test]
fn history_outlives_the_transcript() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = copy(&dir, "claude-2.1.278.jsonl");
    let src = [source(Harness::Claude, &path)];
    store.sync_sources(&src, None).unwrap();
    let cost = |s: &Store| s.connection().query_row("SELECT cost_usd FROM sessions", [], |r| r.get::<_, f64>(0)).unwrap();
    let before = cost(&store);

    // The harness deletes it: it is no longer listed, and nothing changes.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(store.sync_sources(&[], None).unwrap(), SyncStats::default());
    // Or it is still listed and cannot be read: reported, the row untouched.
    let gone = store.sync_sources(&src, None).unwrap();
    assert_eq!((gone.seen, gone.stored, gone.failed.len()), (1, 0, 1));
    // Or it is still there but truncated: it reads as empty, and the row is kept.
    std::fs::write(&path, "").unwrap();
    let after = store.sync_sources(&src, None).unwrap();
    assert_eq!(after, SyncStats { seen: 1, kept: 1, ..Default::default() });
    assert_eq!(store.session_count().unwrap(), 1);
    assert_eq!(cost(&store), before);
}

#[test]
fn since_skips_files_last_written_before_it() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = copy(&dir, "claude-2.1.278.jsonl");
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    let s = store.sync_sources(&[source(Harness::Claude, &path)], Some(future)).unwrap();
    assert_eq!(s, SyncStats { seen: 1, outside_window: 1, ..Default::default() });
    assert_eq!(store.session_count().unwrap(), 0);
}

#[test]
fn prompt_text_never_reaches_the_file() {
    const SECRET: &str = "SECRET-PROMPT-7f3a91";
    let dir = TempDir::new();
    let db = dir.0.join("agent-top.db");
    let path = copy(&dir, "claude-2.1.278.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-09-21T20:30:00.000Z\",\"isSidechain\":false,\"version\":\"2.1.278\",\"cwd\":\"/Users/dev/code/example\",\"sessionId\":\"00000000-1111-2222-3333-555555555555\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{SECRET}\"}}]}}}}\n"
    ));
    std::fs::write(&path, text).unwrap();
    {
        let mut store = Store::open(&db).unwrap();
        assert_eq!(store.sync_sources(&[source(Harness::Claude, &path)], None).unwrap().stored, 1);
    }
    for f in std::fs::read_dir(&dir.0).unwrap().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.starts_with("agent-top.db") {
            let bytes = std::fs::read(f.path()).unwrap();
            assert!(!bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()), "{name} holds prompt text");
        }
    }
}

#[test]
fn a_subagent_run_is_stored_with_the_call_that_spawned_it() {
    const DESCRIPTION: &str = "SECRET-TASK-4c1d07";
    let dir = TempDir::new();
    let db = dir.0.join("agent-top.db");
    let path = dir.0.join("abc.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"user","timestamp":"2026-09-03T07:00:00.000Z","sessionId":"abc","message":{"role":"user","content":[{"type":"text","text":"go"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-03T07:00:01.000Z","sessionId":"abc","message":{"id":"m1","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_agent","name":"Agent"}],"usage":{"input_tokens":100,"output_tokens":10}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let subs = dir.0.join("abc/subagents");
    std::fs::create_dir_all(&subs).unwrap();
    std::fs::write(
        subs.join("agent-a1.meta.json"),
        format!(r#"{{"agentType":"Explore","toolUseId":"toolu_agent","description":"{DESCRIPTION}"}}"#),
    )
    .unwrap();
    std::fs::write(
        subs.join("agent-a1.jsonl"),
        concat!(
            r#"{"type":"assistant","timestamp":"2026-09-03T07:00:02.000Z","sessionId":"abc","isSidechain":true,"agentId":"a1","message":{"id":"s1","model":"claude-opus-5","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_sub","name":"Grep"}],"usage":{"input_tokens":1000,"output_tokens":100}}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-09-03T07:00:04.000Z","sessionId":"abc","isSidechain":true,"agentId":"a1","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_sub"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    {
        let mut store = Store::open(&db).unwrap();
        assert_eq!(store.sync_sources(&[source(Harness::Claude, &path)], None).unwrap().stored, 1);
        let c = store.connection();
        let run: (String, String, String, i64, i64) = c
            .query_row("SELECT agent_id, agent_type, spawned_by, tokens, tool_calls FROM subagents WHERE session_id = 'abc'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap();
        assert_eq!(run, ("a1".into(), "Explore".into(), "toolu_agent".into(), 1100, 1));
        // The run joins to its spawning call by id, and its spans carry its id.
        let spawn: String = c
            .query_row(
                "SELECT p.name FROM subagents a JOIN spans p ON p.harness = a.harness AND p.session_id = a.session_id AND p.span_id = a.spawned_by",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(spawn, "Agent");
        let tagged: Vec<(String, Option<String>)> = c
            .prepare("SELECT name, agent FROM spans WHERE kind = 'tool' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(tagged, vec![("Agent".into(), None), ("Grep".into(), Some("a1".into()))]);
    }
    for f in std::fs::read_dir(&dir.0).unwrap().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.starts_with("agent-top.db") {
            let bytes = std::fs::read(f.path()).unwrap();
            assert!(!bytes.windows(DESCRIPTION.len()).any(|w| w == DESCRIPTION.as_bytes()), "{name} holds the task description");
        }
    }
}

#[test]
fn the_context_timeline_is_stored_without_the_compaction_summary() {
    const SUMMARY: &str = "SECRET-SUMMARY-9e2b44";
    let dir = TempDir::new();
    let db = dir.0.join("agent-top.db");
    let path = dir.0.join("ctx.jsonl");
    let reply = |id: &str, ts: &str, prompt: u64| {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"ctx","message":{{"id":"{id}","model":"claude-sonnet-5","stop_reason":"tool_use","content":[{{"type":"tool_use","id":"t{id}","name":"Read"}}],"usage":{{"input_tokens":{prompt},"output_tokens":10}}}}}}"#
        )
    };
    let result = |id: &str, ts: &str| {
        format!(
            r#"{{"type":"user","timestamp":"{ts}","sessionId":"ctx","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{id}"}}]}}}}"#
        )
    };
    let lines = [
        r#"{"type":"user","timestamp":"2026-09-03T07:00:00.000Z","sessionId":"ctx","message":{"role":"user","content":"go"}}"#.to_string(),
        reply("m1", "2026-09-03T07:00:01.000Z", 10_000),
        result("m1", "2026-09-03T07:00:02.000Z"),
        reply("m2", "2026-09-03T07:00:03.000Z", 50_000),
        r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-09-03T07:01:00.000Z","compactMetadata":{"trigger":"auto","preTokens":52000,"postTokens":3000}}"#.to_string(),
        format!(r#"{{"type":"user","timestamp":"2026-09-03T07:01:01.000Z","sessionId":"ctx","isCompactSummary":true,"message":{{"role":"user","content":"{SUMMARY}"}}}}"#),
        reply("m3", "2026-09-03T07:01:05.000Z", 12_000),
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    // A subagent with two responses of its own.
    let sub = dir.0.join("ctx").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    let sub_reply = |id: &str, ts: &str, prompt: u64| {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"ctx","isSidechain":true,"agentId":"a1","message":{{"id":"{id}","model":"claude-sonnet-5","stop_reason":"end_turn","content":[],"usage":{{"input_tokens":{prompt},"output_tokens":5}}}}}}"#
        )
    };
    let sub_lines = [sub_reply("s1", "2026-09-03T07:00:04.000Z", 4_000), sub_reply("s2", "2026-09-03T07:00:05.000Z", 6_000)];
    std::fs::write(sub.join("agent-a1.jsonl"), sub_lines.join("\n") + "\n").unwrap();
    {
        let mut store = Store::open(&db).unwrap();
        assert_eq!(store.sync_sources(&[source(Harness::Claude, &path)], None).unwrap().stored, 1);
        let c = store.connection();
        let prompts: Vec<(i64, i64, i64)> = c
            .prepare(
                "SELECT seq, prompt, tool_tokens + mcp_tokens + other_tokens FROM context_points
                 WHERE session_id = 'ctx' AND agent = '' ORDER BY seq",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(prompts, vec![(0, 10_000, 10_000), (1, 50_000, 50_000), (2, 12_000, 12_000)], "the split sums to the prompt");
        let sub_prompts: Vec<i64> = c
            .prepare("SELECT prompt FROM context_points WHERE session_id = 'ctx' AND agent = 'a1' ORDER BY seq")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(sub_prompts, vec![4_000, 6_000], "the subagent's timeline is its own");
        let comp: (i64, String, String, i64, i64, i64) = c
            .query_row(
                "SELECT seq, triggered_by, detected_by, before_tokens, summary_tokens, after_tokens FROM compactions WHERE session_id = 'ctx' AND agent = ''",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(comp, (2, "auto".into(), "harness".into(), 52_000, 3_000, 12_000));
        let read: i64 = c
            .query_row("SELECT tokens FROM compaction_sources WHERE session_id = 'ctx' AND origin = 'tool' AND name = 'Read'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(read > 0, "what filled it is kept");
        let per_day: i64 = c.query_row("SELECT compactions FROM compactions_by_day", [], |r| r.get(0)).unwrap();
        assert_eq!(per_day, 1);
    }
    for f in std::fs::read_dir(&dir.0).unwrap().flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.starts_with("agent-top.db") {
            let bytes = std::fs::read(f.path()).unwrap();
            assert!(!bytes.windows(SUMMARY.len()).any(|w| w == SUMMARY.as_bytes()), "{name} holds the compaction summary");
        }
    }
}

#[cfg(unix)]
#[test]
fn the_file_is_private_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new();
    let db = dir.0.join("agent-top.db");
    Store::open(&db).unwrap();
    assert_eq!(std::fs::metadata(&db).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn a_newer_schema_is_refused() {
    let dir = TempDir::new();
    let db = dir.0.join("agent-top.db");
    Store::open(&db).unwrap().connection().pragma_update(None, "user_version", 99).unwrap();
    let err = Store::open(&db).err().expect("a newer schema must not open").to_string();
    assert!(err.contains("upgrade agent-top"), "{err}");
}

#[test]
fn an_empty_session_read_empty_again_is_not_kept() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = dir.0.join("empty.jsonl");
    std::fs::write(&path, "").unwrap();
    let src = [source(Harness::Claude, &path)];
    assert_eq!(store.sync_sources(&src, None).unwrap(), stats(1, 0));
    store.connection().execute("UPDATE sources SET synced_by_version = '0.0.1'", []).unwrap();
    assert_eq!(store.sync_sources(&src, None).unwrap(), stats(1, 0));
}

/// Claude Code's adapter with no file stamp, the way a harness that keeps its
/// sessions in a database looks to the store.
struct Unstamped(agent_top_core::harness::claude::ClaudeAdapter);

impl agent_top_core::harness::HarnessAdapter for Unstamped {
    fn harness(&self) -> Harness {
        Harness::Claude
    }
    fn rescan(&mut self, _: std::time::SystemTime) {}
    fn attribute(
        &self,
        _: &agent_top_core::model::ProcNode,
        _: Option<&agent_top_core::process::RawProc>,
        _: &agent_top_core::harness::AttributeContext,
    ) -> (Vec<std::path::PathBuf>, agent_top_core::model::Attribution) {
        (Vec::new(), agent_top_core::model::Attribution::CwdHeuristic)
    }
    fn unowned(&self, _: &std::collections::HashSet<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
    fn open(
        &self,
        path: &std::path::Path,
        spans: agent_top_core::harness::SpanRetention,
    ) -> Box<dyn agent_top_core::harness::SessionTracker> {
        self.0.open(path, spans)
    }
    fn detect(&self, _: &std::path::Path) -> bool {
        true
    }
    fn transcripts(&self) -> Vec<(String, std::path::PathBuf)> {
        Vec::new()
    }
    fn stamp(&self, _: &std::path::Path) -> Option<agent_top_core::harness::SourceStamp> {
        None
    }
}

#[test]
fn a_session_with_no_stamp_is_written_only_when_it_reads_differently() {
    let dir = TempDir::new();
    let mut store = Store::open(&dir.0.join("agent-top.db")).unwrap();
    let path = copy(&dir, "claude-2.1.278.jsonl");
    let src = [source(Harness::Claude, &path)];
    let adapter = Unstamped(Default::default());
    assert_eq!(store.sync_with(&adapter, &src, None).unwrap(), stats(1, 0));
    assert_eq!(store.sync_with(&adapter, &src, None).unwrap(), stats(0, 1));

    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(
        "{\"type\":\"system\",\"subtype\":\"informational\",\"timestamp\":\"2026-09-21T20:30:00.000Z\",\"version\":\"2.1.278\"}\n",
    );
    std::fs::write(&path, text).unwrap();
    assert_eq!(store.sync_with(&adapter, &src, None).unwrap(), stats(1, 0));
}
