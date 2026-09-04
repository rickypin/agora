//! 状态与四层来源仲裁（MISSION §4.3 §4.4 §5.1 §5.3；ADR-002 D1；agora-dvh.4）。
//!
//! 本文件是数据形态与进程状态层（第 2 层）；裁决在 [`machine`]。核心只吃 [`AgoraEvent`]
//! 与 [`DetectionResult`]，不知道任何 agent 的 payload 键（规则 5，`tests/arch_boundary.rs`）。
//! 状态是"人要做什么"的定义，不落库（不变量 7）。

pub mod machine;

use serde::{Deserialize, Serialize};

use crate::runtime::{Exit, RuntimeSession};

pub use machine::{Liveness, Machine, MachineConfig, Observation};

/// agent 经 hook 自报的事件（MISSION §5.6；ADR-002 D2）。Adapter 把宿主 payload 映射成它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgoraEvent {
    SessionStarted,
    /// agent 自报的对话 id，每次命中覆盖 `agent_session_id`（D7）。
    SessionId(String),
    PromptSubmitted(String),
    /// RUNNING 行的"正在做什么"。
    Activity(String),
    /// 提问类工具：WAITING(question)，只能在终端答（D5）；同 `tool_use_id` 的解除才算答完。
    InputNeeded {
        tool_use_id: String,
        question: String,
    },
    /// 权限请求：WAITING(decision)。
    DecisionNeeded {
        tool_use_id: String,
        summary: String,
    },
    /// 挂起的决定 / 提问被终端或 Dashboard 解决：`Some(id)` 只解一个（并行工具各有各的），
    /// `None` 全解。全部解完才回 RUNNING。
    DecisionResolved(Option<String>),
    /// 一轮做完；带最后一条回复。
    TurnEnded(Option<String>),
    /// 一轮以错误 / 中断结束；reason 是错误类型。
    TurnFailed(String),
    /// agent 自报空闲：TURN_DONE 的确认 / 补漏。
    Idle,
    SessionEnded(Option<String>),
}

/// 文本层 / 活动层的判定（ADR-002 D6 的 `DetectionResult`）：Adapter 的兜底给出，核心裁决。
#[derive(Debug, Clone, PartialEq)]
pub struct DetectionResult {
    pub status: Status,
    pub confidence: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Running,
    Waiting,
    TurnDone,
    Idle,
    Finished,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Hook,
    Process,
    Text,
    Activity,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assessment {
    pub status: Status,
    pub source: Source,
    /// 0.0–1.0；API 返回、日志记录，UI 不显示（MISSION §5.3）。
    pub confidence: f32,
    pub reason: Option<String>,
}

impl Assessment {
    pub fn new(status: Status, source: Source, confidence: f32, reason: Option<&str>) -> Self {
        Assessment {
            status,
            source,
            confidence,
            reason: reason.map(str::to_owned),
        }
    }

    pub fn unknown(reason: &str) -> Self {
        Assessment::new(Status::Unknown, Source::None, 0.0, Some(reason))
    }
}

/// 本代进程起始后多少秒内、还没有任何活动信息时算 STARTING。
pub const STARTING_WINDOW_SECS: u64 = 2;

/// 进程状态层：只看运行时事实加两个落库的事件时刻。
/// `spawn_age_secs` 是本代进程起始（`sessions.spawned_at`）距今的秒数，None = 不知道起始时刻，
/// 不算 STARTING；`killed_by_user` 来自 `sessions.killed_at`，daemon 重启后仍在。
pub fn process_layer(
    runtime: Option<&RuntimeSession>,
    spawn_age_secs: Option<u64>,
    killed_by_user: bool,
) -> Assessment {
    let Some(rt) = runtime else {
        return Assessment::unknown("runtime session missing");
    };
    if rt.alive {
        let starting = spawn_age_secs.is_some_and(|a| a < STARTING_WINDOW_SECS);
        return Assessment::new(
            if starting {
                Status::Starting
            } else {
                Status::Running
            },
            Source::Process,
            1.0,
            None,
        );
    }
    match &rt.exit {
        Some(Exit::Code(0)) => Assessment::new(Status::Finished, Source::Process, 1.0, None),
        Some(Exit::Code(n)) => Assessment::new(
            Status::Failed,
            Source::Process,
            1.0,
            Some(&format!("exit code {n}")),
        ),
        Some(Exit::Signal(sig)) if killed_by_user => Assessment::new(
            Status::Finished,
            Source::Process,
            1.0,
            Some(&format!("killed by user (signal {sig})")),
        ),
        Some(Exit::Signal(sig)) => Assessment::new(
            Status::Failed,
            Source::Process,
            1.0,
            Some(&format!("signal {sig}")),
        ),
        None => Assessment::new(
            Status::Unknown,
            Source::Process,
            0.0,
            Some("process exited, exit status not yet collected"),
        ),
    }
}
