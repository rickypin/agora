//! SQLite 打开与 `user_version` 迁移框架（MISSION §2.3 规则 10）。
//!
//! 每个版本一段 SQL，只追加不改历史；打开时按版本顺序补齐。表形态是
//! docs/spec/config.md 的 SQLite Schema；`sessions` 表**没有**活性列（不变量 7）。

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OpenFlags};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("打开数据库 {path} 失败: {source}")]
    Open {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("迁移到 user_version {version} 失败: {source}")]
    Migrate {
        version: i64,
        #[source]
        source: rusqlite::Error,
    },
    #[error("数据库 user_version {found} 高于本程序认识的 {supported}，拒绝降级运行")]
    TooNew { found: i64, supported: i64 },
    #[error("SQL 失败: {0}")]
    Sql(#[from] rusqlite::Error),
}

/// 按顺序执行；索引 + 1 就是目标 `user_version`。
const MIGRATIONS: &[&str] = &[
    // v1：MVP 表。sessions 不含 status / alive / exit_code——那是运行时的实时结果。
    "CREATE TABLE sessions (
        id TEXT PRIMARY KEY,
        runtime_ref TEXT UNIQUE,
        display_name TEXT NOT NULL,
        name_locked BOOLEAN NOT NULL DEFAULT FALSE,
        agent_type TEXT NOT NULL,
        working_directory TEXT,
        worktree TEXT,
        task_ref TEXT,
        command TEXT,
        agent_session_id TEXT,
        epoch INTEGER NOT NULL DEFAULT 1,
        transcript_path TEXT,
        created_at DATETIME NOT NULL,
        ended_at DATETIME,
        updated_at DATETIME NOT NULL,
        origin TEXT NOT NULL DEFAULT 'agora'
    );
    CREATE TABLE projects (path TEXT PRIMARY KEY, name TEXT NOT NULL, last_used_at DATETIME);
    CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    // v2：两个"事件时刻"，不是活性（agora-xqa.15 / .16，2026-09-03）。
    // spawned_at：本代进程（epoch）起始时刻，STARTING 窗口只看它——updated_at 被 rename /
    // kill / cleanup 刷新，改名后两秒内会误报 STARTING。旧行留 NULL：没有起始时刻就不算 STARTING。
    // killed_at：用户执行过 Kill 的事实，与 ended_at 同类；daemon 重启后据它把按信号退出的
    // 会话报 FINISHED（killed by user）而不是 FAILED。Restart 时清空。
    "ALTER TABLE sessions ADD COLUMN spawned_at DATETIME;
    ALTER TABLE sessions ADD COLUMN killed_at DATETIME;",
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// 单连接 + Mutex：daemon 是单进程，写入量极小，不值得连接池。
pub struct Db {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db")
    }
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| DbError::Open {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self, DbError> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// 锁中毒时取回内层继续用（ADR-001 D8 施工约束 4）。
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn user_version(&self) -> Result<i64, DbError> {
        Ok(self
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }
}

fn migrate(conn: &Connection) -> Result<(), DbError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(DbError::TooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    for (idx, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = idx as i64 + 1;
        let apply = || -> rusqlite::Result<()> {
            conn.execute_batch("BEGIN")?;
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", version)?;
            conn.execute_batch("COMMIT")
        };
        if let Err(source) = apply() {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(DbError::Migrate { version, source });
        }
        tracing::info!(component = "session", version, "schema migrated");
    }
    Ok(())
}
