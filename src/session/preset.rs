//! 预设：在桌面 / 终端里用 `agora preset` 定义好的"一键启动"（agora-prdg.3；epic agora-hxva）。
//!
//! 一行 = agent 类型 + 工作目录 + 启动参数（可选）+ 首句（可选）。手机端只消费（只读列表 +
//! 按钮，S2 的 `GET /api/presets`），零打字；"能起什么"冻结在桌面侧的 CLI 里。
//!
//! 存储进 SQLite（`presets` 表，schema v8）而不是 config.yaml，跟着 `agora peer token` 的先例：
//! CLI 直接操作 `AGORA_HOME/agora.db`、不需要 daemon 在跑（ADR-003 D6），而 daemon 每次请求
//! 查库、不缓存，所以定义 / 修改即时生效——config.yaml 没有热重载（只有 TLS 证书被 watch），
//! 放配置就得重启才生效。
//!
//! `args` 是**原样字符串**：与人在终端里敲的完全一致（`--model opus`、`--continue`、`-c` …），
//! 起会话时经 shell 追加在裸命令名之后。会话行把它的一份快照复制进 `sessions.launch_args`
//! （[`crate::session::manager::SessionManager::create_with_prompt`]）：预设之后被改 / 删，
//! Restart 仍按当时的样子重放。

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::db::{Db, DbError};

/// 预设读写的失败；"未知 agent / 目录不存在"这类取值校验在 CLI 层（[`crate::cli::preset`]），
/// 这里只管库。
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("预设不存在: {0}")]
    NotFound(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for PresetError {
    fn from(e: rusqlite::Error) -> Self {
        PresetError::Db(DbError::Sql(e))
    }
}

/// `presets` 一行。`args` / `prompt` 没有就是 NULL；时间字段是 SQLite 生成的
/// `YYYY-MM-DDTHH:MM:SSZ` 文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preset {
    pub name: String,
    pub agent_type: String,
    pub working_directory: String,
    pub args: Option<String>,
    pub prompt: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const SELECT: &str = "SELECT name, agent_type, working_directory, args, prompt, created_at,
    updated_at FROM presets";

/// 同名 upsert（`agora preset add` 幂等）：覆盖 agent / 目录 / 参数 / 首句与 `updated_at`，
/// 保留 `created_at`。整行替换而不是部分更新——"同名覆盖"就该是"这一条现在长这样"，
/// 想留旧参数就把它写全。
pub fn upsert(
    db: &Db,
    name: &str,
    agent_type: &str,
    working_directory: &str,
    args: Option<&str>,
    prompt: Option<&str>,
) -> Result<Preset, PresetError> {
    db.conn().execute(
        "INSERT INTO presets (name, agent_type, working_directory, args, prompt, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5,
            strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now'))
         ON CONFLICT(name) DO UPDATE SET
            agent_type = excluded.agent_type,
            working_directory = excluded.working_directory,
            args = excluded.args,
            prompt = excluded.prompt,
            updated_at = excluded.updated_at",
        params![name, agent_type, working_directory, args, prompt],
    )?;
    get(db, name)
}

/// 全部预设，按名字排序（确定、好扫；展示排序是消费方的事）。
pub fn list(db: &Db) -> Result<Vec<Preset>, PresetError> {
    let conn = db.conn();
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY name"))?;
    let rows = stmt.query_map([], row_to_preset)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn get(db: &Db, name: &str) -> Result<Preset, PresetError> {
    db.conn()
        .query_row(&format!("{SELECT} WHERE name = ?1"), [name], row_to_preset)
        .optional()?
        .ok_or_else(|| PresetError::NotFound(name.to_owned()))
}

/// 删一条；没有这条就报 [`PresetError::NotFound`]（`agora preset rm` 的退出码 1 靠它）。
pub fn remove(db: &Db, name: &str) -> Result<(), PresetError> {
    let n = db
        .conn()
        .execute("DELETE FROM presets WHERE name = ?1", [name])?;
    if n == 0 {
        return Err(PresetError::NotFound(name.to_owned()));
    }
    Ok(())
}

fn row_to_preset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Preset> {
    Ok(Preset {
        name: row.get(0)?,
        agent_type: row.get(1)?,
        working_directory: row.get(2)?,
        args: row.get(3)?,
        prompt: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}
