//! `agent-top sql`: the views answer what they say, and nothing a query does
//! can change the store or write a file.

mod common;

use agent_top_core::Harness;
use agent_top_store::{Reader, Store, Value};
use common::{TempDir, copy, source};
use std::path::PathBuf;

/// A store synced from a Claude and a Codex fixture, closed again.
fn synced(dir: &TempDir) -> PathBuf {
    let db = dir.0.join("agent-top.db");
    let mut store = Store::open(&db).unwrap();
    let claude = copy(dir, "claude-2.1.278.jsonl");
    let codex = copy(dir, "codex-mcp-0.152.jsonl");
    store.sync_sources(&[source(Harness::Claude, &claude), source(Harness::Codex, &codex)], None).unwrap();
    db
}

fn real(v: &Value) -> f64 {
    match v {
        Value::Real(f) => *f,
        Value::Integer(i) => *i as f64,
        other => panic!("not a number: {other:?}"),
    }
}

fn one(r: &Reader, sql: &str) -> Value {
    r.query(sql).unwrap().rows.remove(0).remove(0)
}

#[test]
fn every_cost_view_adds_up_to_the_counted_sessions() {
    let dir = TempDir::new();
    let r = Reader::open(&synced(&dir)).unwrap();
    let total = real(&one(&r, "SELECT sum(cost_usd) FROM counted_sessions"));
    assert!(total > 0.0);
    for view in ["cost_by_day", "cost_by_harness", "cost_by_model", "cost_by_project"] {
        let sum = real(&one(&r, &format!("SELECT sum(cost_usd) FROM {view}")));
        assert!((sum - total).abs() < 1e-9, "{view}: {sum} != {total}");
    }
    let harnesses = r.query("SELECT harness FROM cost_by_harness ORDER BY 1").unwrap();
    assert_eq!(harnesses.rows, vec![vec![Value::Text("claude".into())], vec![Value::Text("codex".into())]]);
}

#[test]
fn tool_latency_takes_nearest_rank_percentiles() {
    let dir = TempDir::new();
    let db = dir.0.join("agent-top.db");
    {
        let store = Store::open(&db).unwrap();
        let c = store.connection();
        for d in 1..=100 {
            c.execute(
                "INSERT INTO spans VALUES ('claude', 's', ?1, ?2, 'tool', 'Bash', 0, ?3, 0, ?4, NULL)",
                rusqlite::params![d, format!("t{d}"), d, (d % 10 == 0) as i64],
            )
            .unwrap();
        }
        // Still open: not a duration yet, so not counted.
        c.execute("INSERT INTO spans VALUES ('claude', 's', 101, 'open', 'tool', 'Bash', 0, NULL, 0, 0, NULL)", []).unwrap();
    }
    let r = Reader::open(&db).unwrap();
    let row = r.query("SELECT calls, errors, p50_ms, p95_ms, max_ms FROM tool_latency WHERE name = 'Bash'").unwrap();
    assert_eq!(row.rows, vec![[100, 10, 50, 95, 100].map(Value::Integer).to_vec()]);
}

#[test]
fn mcp_errors_counts_per_server() {
    let dir = TempDir::new();
    let r = Reader::open(&synced(&dir)).unwrap();
    let calls = real(&one(&r, "SELECT sum(calls) FROM mcp_errors"));
    let direct = real(&one(&r, "SELECT sum(calls) FROM mcp_calls"));
    assert!(calls > 0.0);
    assert_eq!(calls, direct);
}

#[test]
fn nothing_a_query_does_changes_the_store_or_writes_a_file() {
    let dir = TempDir::new();
    let db = synced(&dir);
    let before = std::fs::read(&db).unwrap();
    let r = Reader::open(&db).unwrap();
    let elsewhere = dir.0.join("copy.db");
    for sql in [
        "DELETE FROM sessions".to_string(),
        "UPDATE sessions SET cost_usd = 0".to_string(),
        "CREATE TABLE x (a)".to_string(),
        "DROP VIEW cost_by_day".to_string(),
        "PRAGMA user_version = 9".to_string(),
        format!("VACUUM INTO '{}'", elsewhere.display()),
        format!("ATTACH '{}' AS other", elsewhere.display()),
    ] {
        assert!(r.query(&sql).is_err(), "{sql} should be refused");
    }
    assert!(!elsewhere.exists(), "a query wrote a file");
    assert_eq!(real(&one(&r, "PRAGMA user_version")), agent_top_store::SCHEMA_VERSION as f64);
    drop(r);
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

#[test]
fn a_missing_store_is_not_created() {
    let dir = TempDir::new();
    let db = dir.0.join("none.db");
    let err = Reader::open(&db).err().unwrap().to_string();
    assert!(err.contains("agent-top sync"), "{err}");
    assert!(!db.exists());
}

#[test]
fn a_version_one_store_is_upgraded_by_sync_not_by_sql() {
    let dir = TempDir::new();
    let db = synced(&dir);
    {
        // Make it the file 0.23.0 wrote: schema 1, no views.
        let store = Store::open(&db).unwrap();
        let c = store.connection();
        for v in ["mcp_errors", "tool_latency", "cost_by_project", "cost_by_model", "cost_by_harness", "cost_by_day", "counted_sessions"] {
            c.execute(&format!("DROP VIEW {v}"), []).unwrap();
        }
        c.execute("DROP TABLE telemetry_spans", []).unwrap();
        c.execute("ALTER TABLE sessions DROP COLUMN tool_calls_lower_bound", []).unwrap();
        c.pragma_update(None, "user_version", 1).unwrap();
    }
    let err = Reader::open(&db).err().unwrap().to_string();
    assert!(err.contains("run `agent-top sync`"), "{err}");
    Store::open(&db).unwrap();
    let r = Reader::open(&db).unwrap();
    assert!(real(&one(&r, "SELECT count(*) FROM cost_by_day")) > 0.0);
}

#[test]
fn describe_lists_every_table_and_view_with_columns() {
    let dir = TempDir::new();
    let r = Reader::open(&synced(&dir)).unwrap();
    let d = r.describe().unwrap();
    assert_eq!(d.len(), 11);
    assert!(d.iter().all(|t| !t.columns.is_empty()));
    assert_eq!(d.iter().filter(|t| t.kind == "table").count(), 4);
}
