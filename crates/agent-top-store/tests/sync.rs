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
