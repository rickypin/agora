//! 不变量 7：SQLite 只存 metadata，活性字段每次从运行时现算。

use agora::session::db::SCHEMA_VERSION;
use agora::session::Db;

fn columns(db: &Db, table: &str) -> Vec<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn no_liveness_columns() {
    let db = Db::open_in_memory().unwrap();
    let cols = columns(&db, "sessions");
    for forbidden in ["status", "alive", "exit_code", "exit", "last_activity_at"] {
        assert!(
            !cols.contains(&forbidden.to_string()),
            "sessions 长出了活性列 {forbidden}"
        );
    }
    for required in [
        "runtime_ref",
        "name_locked",
        "epoch",
        "origin",
        "ended_at",
        "transcript_path",
        "spawned_at",
        "killed_at",
    ] {
        assert!(cols.contains(&required.to_string()), "缺列 {required}");
    }
}

#[test]
fn migration_sets_user_version_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.db");
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), SCHEMA_VERSION);
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), SCHEMA_VERSION);
    assert!(columns(&db, "projects").contains(&"path".to_string()));
    assert!(columns(&db, "preferences").contains(&"key".to_string()));
}

#[test]
fn v1_database_upgrades_in_place_and_old_rows_read_back() {
    // 旧行 spawned_at / killed_at 为 NULL：不算 STARTING、不算用户杀的；不能因为加列丢数据。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.db");
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, runtime_ref TEXT UNIQUE, display_name TEXT NOT NULL,
                name_locked BOOLEAN NOT NULL DEFAULT FALSE, agent_type TEXT NOT NULL,
                working_directory TEXT, worktree TEXT, task_ref TEXT, command TEXT,
                agent_session_id TEXT, epoch INTEGER NOT NULL DEFAULT 1, transcript_path TEXT,
                created_at DATETIME NOT NULL, ended_at DATETIME, updated_at DATETIME NOT NULL,
                origin TEXT NOT NULL DEFAULT 'agora');
             CREATE TABLE projects (path TEXT PRIMARY KEY, name TEXT NOT NULL, last_used_at DATETIME);
             CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO sessions (id, runtime_ref, display_name, agent_type, created_at, updated_at)
                VALUES ('old001', 'tmux:agora:ag-old001', 'old', 'shell',
                        '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z');
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), SCHEMA_VERSION);
    let (spawned, killed): (Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT spawned_at, killed_at FROM sessions WHERE id = 'old001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((spawned, killed), (None, None));
}

#[test]
fn newer_database_is_refused_not_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.db");
    Db::open(&path).unwrap();
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", SCHEMA_VERSION + 5)
            .unwrap();
    }
    let err = Db::open(&path).unwrap_err();
    assert!(
        matches!(err, agora::session::DbError::TooNew { .. }),
        "{err}"
    );
}
