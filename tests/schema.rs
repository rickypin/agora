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
        // v5 / v6：ended_at 是不是 daemon 猜的、猜的那一档能不能被下一轮推翻（agora-psj0）。
        "ended_at_approximate",
        "ended_at_from_missing",
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
    // v3：devices 只有哈希列，没有明文 token 列（ADR-003 D2）。
    let devices = columns(&db, "devices");
    assert!(devices.contains(&"session_sha256".to_string()));
    assert!(!devices.iter().any(|c| c == "session" || c == "token"));
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
fn v7_database_upgrades_in_place_to_presets_and_launch_args() {
    // v8（agora-prdg.3）：presets 表 + sessions.launch_args。造一个 v7 形状（拿掉 v8 加的两样），
    // 里面躺着升级前就有的行；Db::open 升到 v8 后旧行一条不少、launch_args 默认 NULL、
    // presets 可读写。SQLite 3.35+ 的 DROP COLUMN：bundled 的 rusqlite 0.40 带 SQLite 3.50+。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.db");
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute_batch(
                "DROP TABLE presets;
                 ALTER TABLE sessions DROP COLUMN launch_args;",
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO sessions (id, runtime_ref, display_name, agent_type, command,
                    created_at, updated_at, origin)
                 VALUES ('oldv7', 'tmux:agora:ag-oldv7', 'v7 行', 'shell', 'sleep 300',
                    '2026-10-01T00:00:00Z', '2026-10-01T00:00:00Z', 'agora')",
                [],
            )
            .unwrap();
        db.conn().pragma_update(None, "user_version", 7).unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), SCHEMA_VERSION);
    let (command, launch_args): (Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT command, launch_args FROM sessions WHERE id = 'oldv7'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(command.as_deref(), Some("sleep 300"), "旧行不受影响");
    assert_eq!(launch_args, None, "旧行的 launch_args 默认 NULL");
    // 同趟迁移建的 presets 表可读写。
    let p = agora::session::preset::upsert(
        &db,
        "p1",
        "claude",
        "/tmp",
        Some("--model opus"),
        Some("hi"),
    )
    .unwrap();
    assert_eq!(p.name, "p1");
    assert_eq!(agora::session::preset::list(&db).unwrap().len(), 1);
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
