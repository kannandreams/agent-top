//! Optional Kodelet subagent enrichment, independent of conversation accounting.
//!
//! Source-verified on 2026-09-19 against kodelet-subagent 0.6.5 (`fb85177`),
//! Kodelet v0.6.17-beta (`6dcef0a4`), and skills/code-search (`be66168`).
//!
//! `<base>/extensions/data/*/subagents.sqlite` supplies names and conversation
//! IDs, not accounting. Query SQLite read-only: upstream AgentStore read methods
//! can migrate/reconcile state.

use crate::model::SubagentInfo;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

pub(super) struct SubagentIndex {
    base: PathBuf,
    last_scan: Option<Instant>,
    children: HashMap<String, SubagentInfo>,
}

impl SubagentIndex {
    pub fn new(base: PathBuf) -> Self {
        Self { base, last_scan: None, children: HashMap::new() }
    }

    /// Returns whether a rescan was attempted, not whether records changed.
    pub fn refresh(&mut self) -> bool {
        self.refresh_at(Instant::now())
    }

    /// Keyed by conversation ID, not the extension's agent/run ID.
    /// If central metadata names a different parent, do not apply this record.
    pub fn get(&self, child_conversation_id: &str) -> Option<&SubagentInfo> {
        self.children.get(child_conversation_id)
    }

    fn refresh_at(&mut self, now: Instant) -> bool {
        if self.last_scan.is_some_and(|last| now.saturating_duration_since(last) < RESCAN_INTERVAL) {
            return false;
        }
        self.last_scan = Some(now);
        self.children.clear();
        let Ok(dirs) = std::fs::read_dir(self.base.join("extensions/data")) else { return true };
        for dir in dirs.flatten() {
            let db = dir.path().join("subagents.sqlite");
            if db.is_file()
                && let Ok(records) = read_records(&db)
            {
                self.children.extend(records);
            }
        }
        true
    }
}

fn read_records(db: &Path) -> rusqlite::Result<Vec<(String, SubagentInfo)>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // A busy optional sidecar must not stall the collector's refresh.
    conn.busy_timeout(Duration::ZERO)?;
    let revision: String = conn.query_row("SELECT version_num FROM alembic_version", [], |r| r.get(0))?;
    if !matches!(revision.as_str(), "0001_initial" | "0002_canceling_state") {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT child_conversation_id, owner_conversation_id, name
         FROM agents WHERE child_conversation_id IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            SubagentInfo { parent_session_id: r.get(1)?, nickname: Some(r.get(2)?), role: Some("subagent".into()) },
        ))
    })?;
    rows.collect()
}

/// Role labels only: never infer hierarchy from a profile or fork provenance.
pub(super) fn role(metadata: &Value) -> Option<&'static str> {
    metadata.get("parent_conversation_id")?.as_str().filter(|id| !id.trim().is_empty())?;
    if metadata.get("profile").and_then(Value::as_str).is_some_and(|profile| profile.trim() == "code-search") {
        return Some("code-search");
    }
    let initiator = metadata.pointer("/conversation_fork/initiator")?;
    let extension = initiator.get("extension_id")?.as_str()?;
    (initiator.get("type").and_then(Value::as_str) == Some("extension_tool")
        && extension.rsplit('/').next() == Some("subagent")
        && matches!(initiator.get("tool_name").and_then(Value::as_str), Some("spawn_agent" | "followup_agent")))
    .then_some("subagent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("agent-top-kodelet-subagent-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }

        fn database(&self, extension: &str, revision: &str) -> (PathBuf, Connection) {
            let dir = self.0.join("extensions/data").join(extension);
            std::fs::create_dir_all(&dir).unwrap();
            let db = dir.join("subagents.sqlite");
            let conn = Connection::open(&db).unwrap();
            // The writer stays open to exercise reads from a live WAL. No
            // run/status/task/result/usage columns: enrichment must not need them.
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE alembic_version (version_num TEXT PRIMARY KEY);
                 CREATE TABLE agents (
                    id TEXT PRIMARY KEY, name TEXT, owner_conversation_id TEXT,
                    child_conversation_id TEXT UNIQUE);
                 INSERT INTO agents VALUES ('agt_one', 'reader', 'parent', 'child');
                 INSERT INTO agents VALUES ('agt_starting', 'starting', 'parent', NULL);",
            )
            .unwrap();
            conn.execute("INSERT INTO alembic_version VALUES (?1)", [revision]).unwrap();
            (db, conn)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_only_attached_relationships_without_changing_database() {
        let fixture = Fixture::new("wal");
        let (db, _writer) = fixture.database("jingkaihe@kodelet-subagent_subagent", "0002_canceling_state");
        let wal = db.with_file_name("subagents.sqlite-wal");
        let before = (std::fs::read(&db).unwrap(), std::fs::read(&wal).unwrap());
        let records = read_records(&db).unwrap();
        assert_eq!(records.len(), 1);
        let (child, info) = &records[0];
        assert_eq!(child, "child");
        assert_eq!(info.parent_session_id, "parent");
        assert_eq!(info.nickname.as_deref(), Some("reader"));
        assert_eq!(info.role.as_deref(), Some("subagent"));
        assert_eq!(before, (std::fs::read(&db).unwrap(), std::fs::read(&wal).unwrap()));
    }

    #[test]
    fn rescans_at_most_once_every_five_seconds_and_removes_deleted_records() {
        let fixture = Fixture::new("cache");
        let (_db, writer) = fixture.database("subagent", "0001_initial");
        let mut index = SubagentIndex::new(fixture.0.clone());
        let now = Instant::now();
        assert!(index.refresh_at(now));
        assert_eq!(index.get("child").unwrap().nickname.as_deref(), Some("reader"));
        writer.execute("UPDATE agents SET name = 'renamed' WHERE id = 'agt_one'", []).unwrap();
        assert!(!index.refresh_at(now + Duration::from_millis(4_999)));
        assert_eq!(index.get("child").unwrap().nickname.as_deref(), Some("reader"));
        assert!(index.refresh_at(now + RESCAN_INTERVAL));
        assert_eq!(index.get("child").unwrap().nickname.as_deref(), Some("renamed"));
        writer.execute("DELETE FROM agents", []).unwrap();
        assert!(index.refresh_at(now + RESCAN_INTERVAL * 2));
        assert!(index.get("child").is_none());
    }

    #[test]
    fn missing_or_unsupported_sidecars_do_not_create_or_migrate_anything() {
        let fixture = Fixture::new("missing");
        let mut index = SubagentIndex::new(fixture.0.clone());
        assert!(index.refresh());
        assert!(!fixture.0.exists());
        assert!(index.get("child").is_none());
        let (db, writer) = fixture.database("unknown", "future_revision");
        assert!(read_records(&db).unwrap().is_empty());
        let version: String = writer.query_row("SELECT version_num FROM alembic_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, "future_revision");
        writer.execute("DROP TABLE alembic_version", []).unwrap();
        assert!(read_records(&db).is_err());
        let now = Instant::now() + RESCAN_INTERVAL;
        assert!(index.refresh_at(now));
        assert!(index.get("child").is_none());
    }

    #[test]
    fn labels_code_search_and_subagents_only_with_explicit_parentage() {
        assert_eq!(role(&json!({"parent_conversation_id": "parent", "profile": "code-search"})), Some("code-search"));
        assert_eq!(role(&json!({"profile": "code-search"})), None);
        assert_eq!(role(&json!({"parent_conversation_id": " ", "profile": "code-search"})), None);
        let mut metadata = json!({
            "conversation_fork": {
                "source_conversation_id": "parent",
                "initiator": {"type": "extension_tool", "extension_id": "jingkaihe@kodelet-subagent/subagent", "tool_name": "spawn_agent"}
            }
        });
        assert_eq!(role(&metadata), None, "fork provenance alone is not hierarchy");
        metadata["parent_conversation_id"] = json!("parent");
        assert_eq!(role(&metadata), Some("subagent"));
        metadata["conversation_fork"]["initiator"]["tool_name"] = json!("another_tool");
        assert_eq!(role(&metadata), None);
    }
}
