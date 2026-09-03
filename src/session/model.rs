//! 会话 metadata（docs/spec/config.md 的 `sessions` 表；MISSION §4.2）。

use serde::Serialize;

/// agora 创建的 / 运行时里采纳的 / 只有 hook 看得见的（MISSION §5.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Agora,
    Adopted,
    External,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Agora => "agora",
            Origin::Adopted => "adopted",
            Origin::External => "external",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agora" => Some(Origin::Agora),
            "adopted" => Some(Origin::Adopted),
            "external" => Some(Origin::External),
            _ => None,
        }
    }
}

/// 一行 `sessions`。时间字段是 SQLite 生成的 `YYYY-MM-DDTHH:MM:SSZ` 文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRecord {
    pub id: String,
    /// `origin == External` 时为 None。
    pub runtime_ref: Option<String>,
    pub display_name: String,
    pub name_locked: bool,
    /// 自由字符串：核心层不知道任何具体 agent（ADR-002 D2）。
    pub agent_type: String,
    pub working_directory: Option<String>,
    pub worktree: Option<String>,
    pub task_ref: Option<String>,
    pub command: Option<String>,
    pub agent_session_id: Option<String>,
    pub epoch: i64,
    pub transcript_path: Option<String>,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub updated_at: String,
    pub origin: Origin,
}
