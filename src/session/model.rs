//! 会话 metadata（docs/spec/config.md 的 `sessions` 表；MISSION §4.2）。

use serde::Serialize;

/// agora 创建的 / 运行时里采纳的 / 只有 hook 看得见的（MISSION §5.5）。
///
/// `Headless` 是 `External` 的一个分档而不是第三种句柄形态：它同样没有运行时会话，只是
/// 「这一行不是一条人的会话」（裁决 agora-5gg.7 选 B）。判据只看载荷结构，见
/// [`crate::adapter::AgentHooks::is_headless`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Agora,
    Adopted,
    External,
    /// 宿主自己起的一次性会话（哪一轮算无头由 Adapter 的 `is_headless` 回答）与宿主内部的子代理。
    /// 照常登记、参与真值表，
    /// 但默认折叠、不通知、满 24 h 不论状态即删（agora-5gg.20）。
    Headless,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Agora => "agora",
            Origin::Adopted => "adopted",
            Origin::External => "external",
            Origin::Headless => "headless",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agora" => Some(Origin::Agora),
            "adopted" => Some(Origin::Adopted),
            "external" => Some(Origin::External),
            "headless" => Some(Origin::Headless),
            _ => None,
        }
    }

    /// 没有运行时句柄、只有 hook 看得见的那两种来源。活性探活、supersede、归档重建、清理走的
    /// 是同一条路，凡原来判 `== Origin::External` 的地方都该问这个函数：漏一处，headless 行就会
    /// 在那条逻辑上掉出 external 行的语义（终端、挂起、`ended_at` 各一处）。
    pub fn is_handleless(self) -> bool {
        matches!(self, Origin::External | Origin::Headless)
    }
}

/// 一行 `sessions`。时间字段是 SQLite 生成的 `YYYY-MM-DDTHH:MM:SSZ` 文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRecord {
    pub id: String,
    /// [`Origin::is_handleless`] 的两档（external / headless）为 None。
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
    /// 本代进程（`epoch`）的起始时刻；create / respawn 时写。v1 库里的旧行为 None。
    pub spawned_at: Option<String>,
    /// 进程退出时刻（MISSION §4.2；A42）。来源是运行时报的退出时刻（`RuntimeSession::exited_at`），
    /// daemon 停机期间退出、重启后 reconcile 补的也是它；Restart 清空。
    pub ended_at: Option<String>,
    /// `ended_at` 是 daemon 的时钟补的近似值（运行时会话已经不在、或运行时还没报退出时刻），
    /// 不是运行时报的退出时刻；`ended_at` 为 None 时无意义（agora-h1k.4）。
    pub ended_at_approximate: bool,
    /// 用户执行过 Kill 的时刻；Restart 清空。事件而非活性（不变量 7 允许）。
    pub killed_at: Option<String>,
    pub updated_at: String,
    pub origin: Origin,
}
