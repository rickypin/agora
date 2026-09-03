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
