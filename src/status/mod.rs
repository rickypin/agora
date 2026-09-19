//! 状态与四层来源仲裁（MISSION §4.3 §4.4 §5.1 §5.3；ADR-002 D1；agora-dvh.4）。
//!
//! 本文件是数据形态与进程状态层（第 2 层）；裁决在 [`machine`]。核心只吃 [`AgoraEvent`]
//! 与 [`DetectionResult`]，不知道任何 agent 的 payload 键（规则 5，`tests/arch_boundary.rs`）。
//! 状态是"人要做什么"的定义，不落库（不变量 7）。

pub mod machine;

use serde::{Deserialize, Serialize};

use crate::runtime::{Exit, RuntimeSession};

pub use machine::{
    AgentProcess, Liveness, Machine, MachineConfig, Observation, EXTERNAL_SILENT_REASON,
    HOOK_SNAPSHOT_VERSION, SUPERSEDED_REASON,
};

/// agent 经 hook 自报的事件（MISSION §5.6；ADR-002 D2）。Adapter 把宿主 payload 映射成它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgoraEvent {
    SessionStarted,
    /// agent 自报的对话 id，每次命中覆盖 `agent_session_id`（D7）。
    SessionId(String),
    PromptSubmitted(String),
    /// 宿主自己注入的 prompt（后台任务完成通知、system-reminder、斜杠命令回显……）：一轮照样
    /// 开始（RUNNING、挂起清空），但它不是人说的话——不进 `❯` 行，不当 task_ref 摘要，
    /// `↳` 也不动（agora-3s5）。
    PromptInjected,
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
    /// agora 自己合成的（不是 hook 发的）：同一个 agent 进程报来了**另一个**对话 id，这一行的对话到此
    /// 为止——一个 CLI agent 进程一次只跑一个对话，Grok 的 /clear、Codex TUI 的 /new 换 id 却不发
    /// SessionEnd，旧行会带着活着的进程号永远 TURN_DONE（2026-09-08 现场：一个 Grok 进程占了三行；
    /// agora-tql）。
    Superseded,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// 会话形态导出给调用方的进程三值（Q4 裁决 agora-5gg.4 选 A；`docs/spec/api.md`「会话形态」）。
///
/// 与 [`Liveness`] 的分工：`Liveness` 是状态机内部的输入——"最后看到的那个进程号此刻在不在"；
/// `ProcessState` 是给人看的事实，在此之上多一条裁决：**对话结束即不再谈进程**，FINISHED / FAILED
/// 行一律 `gone`，哪怕那个 pid 还在跑别的对话（2026-09-18 Mac 盘点：10 行 `finished` + `alive: true`，
/// 全是 superseded / SessionEnd 留下的旧行）。反过来的另一半是 `unknown`：Codex Desktop 这类
/// 无可信进程号的 external 行，agora 说不上进程在不在，布尔 `alive` 把它压成 `false` 就等于
/// 说"没了"，而它和真的探到没了（现场另有 7 行 `turn_done`）在 API 上长得一样（盘点 B2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Alive,
    Gone,
    Unknown,
}

impl ProcessState {
    /// 从状态机结论 + [`Liveness`] 导出 `process`。`runtime_unreadable` 是"这一代运行时会话
    /// 根本没读到"（协议不匹配、版本低于下限那一类运行时降级），不是"读到了、里面没有它"。
    ///
    /// 三条规则按此顺序：
    /// 1. FINISHED / FAILED 一律 `gone`——裁决 Q4，压过一切进程事实；
    /// 2. 运行时整体读不到 → `unknown`。ADR-001 D7「不许拿读不到当已经死了」：`view()` 上半段在
    ///    这种情况下把 liveness 压成 [`Liveness::Dead`] 只为了让状态机别把"读不到"当成"还活着"，
    ///    那是一处内部编码，导出时必须还原（不还原就会给出一条假的 `gone`，2026-09-19 定）；
    /// 3. 其余按三值直译：alive → `alive`、dead → `gone`、没有可信进程号 → `unknown`。
    pub fn derive(status: Status, liveness: Liveness, runtime_unreadable: bool) -> Self {
        if matches!(status, Status::Finished | Status::Failed) {
            return ProcessState::Gone;
        }
        if runtime_unreadable {
            return ProcessState::Unknown;
        }
        match liveness {
            Liveness::Alive => ProcessState::Alive,
            Liveness::Dead => ProcessState::Gone,
            Liveness::Unknown => ProcessState::Unknown,
        }
    }
}

/// 本代进程起始后多少秒内、还没有任何活动信息时算 STARTING。
pub const STARTING_WINDOW_SECS: u64 = 2;

/// 进程状态层：只看运行时事实加两个落库的事件时刻。
/// `spawn_age_secs` 是本代进程起始（`sessions.spawned_at`）距今的秒数，None = 不知道起始时刻，
/// 不算 STARTING；`killed_by_user` 来自 `sessions.killed_at`，daemon 重启后仍在。
/// 128+signo 的壳退出码里，只认 agora Kill 会发的那几个（HUP/INT/KILL/TERM），其余不算。
/// 137 是宽限满后的 SIGKILL 经壳包装的样子（agora-284 把宽限放到后台后，被 KILL 的会话
/// 也得显示成"用户杀的"而不是 FAILED）。
fn is_shell_signal_code(code: i32) -> bool {
    matches!(code, 129 | 130 | 137 | 143)
}

/// 进程层看到的"运行时会话不在列表里"的两种说法（agora-u5p，ADR-001 D4）。
/// 区别只在 reason：两者的结论都是 FINISHED，把握也都是 0.8（见 [`runtime_gone`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGone {
    /// server 还在应答，只有这一个会话没了（`kill-session`、窗口被关、采纳的会话自己退了）。
    Session,
    /// 整个运行时 server 连不上：它上面的每一个会话都不可能还存在。
    Server,
}

impl RuntimeGone {
    /// reason 里那半句人话。
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeGone::Session => "session gone",
            RuntimeGone::Server => "server gone",
        }
    }
}

/// 运行时会话没了：有 `runtime_ref`、运行时正常应答、列表里找不到它。
///
/// 这是**结束的事实**而不是"看不清"（MISSION §4.3；ADR-001 D4 于 2026-09-19 据此修订）：
/// 会话销毁时 pane 进程收 SIGHUP，agent 确定不在，与 external 行的 `external process gone`
/// 同性质。拿不到退出码（pane 连同会话一起没了），所以：
/// - conf 0.8 而非 1.0 —— "连不上 socket"有一个已知的假阳性：socket 文件被 tmpfiles 之类清掉
///   而运行时进程还活着（它会靠 SIGUSR1 重建 socket），那时说"server 没了"是错的（notes ③，
///   反例写进 ADR-001 D4）；
/// - 不盖 hook 先说的结束（同 0.8，`Machine::process_fact_is_no_better`）：行上留着宿主自己的
///   说法，只有 `alive` 变假（agora-rzh 同一条理由）。
///
/// 用户按过 Kill（`killed_at` 在）的写成 killed by user：那是他自己干的，不弹通知
/// （`events.rs` 的通知规则按 reason 前缀静音）。
///
/// 运行时整体降级（协议不匹配、超时）不走这里，那是 UNKNOWN `runtime unavailable: …`（D7）。
pub fn runtime_gone(gone: RuntimeGone, killed_by_user: bool) -> Assessment {
    let reason = if killed_by_user {
        format!("killed by user (runtime session gone; {})", gone.as_str())
    } else {
        format!("runtime session gone ({}; no exit status)", gone.as_str())
    };
    Assessment::new(Status::Finished, Source::Process, 0.8, Some(&reason))
}

/// 运行时对**这一个会话**答不上话来时的结论。与 [`runtime_gone`] 的分工：这条是"没有运行时事实
/// 可给"，那条才是"运行时说了：没有这个会话"。两种行走到这里：`runtime_ref` 为 NULL 的行
/// （external 行恒 NULL），以及有句柄但本代进程还在 STARTING 窗口里、这一 tick 运行时还没报到
/// 它的行（agora-u5p：那种"没看见"不能当成"没了"，否则每起一次会话都先给自己写一个 ended_at）。
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
        // agora-3ib（2026-09-04 实测 Claude 2.1.260）：agent 收到 agora 的 SIGTERM 后自己捕获并以
        // 128+signo 退出（143），或被 sh 包装成退出码，运行时报的是 Code 不是 Signal。用户自己按的
        // Kill 不能显示成 FAILED，所以 128+TERM/INT/HUP 在 killed_by_user 时也算 FINISHED。
        // 其它非零码（agent 真崩了）照旧 FAILED。
        Some(Exit::Code(n)) if killed_by_user && is_shell_signal_code(*n) => Assessment::new(
            Status::Finished,
            Source::Process,
            1.0,
            Some(&format!("killed by user (exit code {n})")),
        ),
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

#[cfg(test)]
mod tests {
    use super::{Liveness, ProcessState, Status};
    use serde_json::json;

    #[test]
    fn process_state_wire_names_are_the_locked_vocabulary() {
        // `process` 的取值是 API 形态（docs/spec/api.md「会话形态」；调用方按它分支、不做字符串
        // 匹配，MISSION §2.3 规则 10）。写死字面量：改 `rename_all` 或改变体名都会红这里。
        assert_eq!(
            serde_json::to_value(ProcessState::Alive).unwrap(),
            json!("alive")
        );
        assert_eq!(
            serde_json::to_value(ProcessState::Gone).unwrap(),
            json!("gone")
        );
        assert_eq!(
            serde_json::to_value(ProcessState::Unknown).unwrap(),
            json!("unknown")
        );
        assert_eq!(
            serde_json::from_value::<ProcessState>(json!("gone")).unwrap(),
            ProcessState::Gone
        );
    }

    #[test]
    fn finished_and_failed_always_report_gone_whatever_the_process_says() {
        // 裁决 Q4（agora-5gg.4）：对话结束即不再谈进程。现场（2026-09-18 Mac）10 行
        // `finished` + `alive: true` 都是 superseded / SessionEnd 的旧行——pid 还活着，因为它
        // 正在跑新对话。关掉 derive 的第一个分支 → 这些断言全红。
        for st in [Status::Finished, Status::Failed] {
            for lv in [Liveness::Alive, Liveness::Dead, Liveness::Unknown] {
                assert_eq!(
                    ProcessState::derive(st, lv, false),
                    ProcessState::Gone,
                    "{st:?} + {lv:?}"
                );
                // 运行时读不到也不能越过这条：状态机既然已经说"结束"，结束就是事实。
                assert_eq!(
                    ProcessState::derive(st, lv, true),
                    ProcessState::Gone,
                    "{st:?} + {lv:?} + unreadable"
                );
            }
        }
    }

    #[test]
    fn unreadable_runtime_is_unknown_not_gone() {
        // ADR-001 D7：不许拿"读不到"当"已经死了"。view() 上半段在降级时给出 Liveness::Dead
        // 只是内部编码（让状态机别把失明当成活着），导出时必须还原成 unknown。
        // 关掉 derive 的 runtime_unreadable 分支 → 第一组断言红（报成 gone）。
        for lv in [Liveness::Alive, Liveness::Dead, Liveness::Unknown] {
            assert_eq!(
                ProcessState::derive(Status::Unknown, lv, true),
                ProcessState::Unknown,
                "{lv:?}"
            );
        }
    }

    #[test]
    fn liveness_maps_straight_through_otherwise() {
        // 盘点 B2：没有可信进程号（Liveness::Unknown）不能压成"没了"。
        // 把 Unknown 那一支并到 Gone → 第二条断言红。
        assert_eq!(
            ProcessState::derive(Status::Running, Liveness::Alive, false),
            ProcessState::Alive
        );
        assert_eq!(
            ProcessState::derive(Status::TurnDone, Liveness::Unknown, false),
            ProcessState::Unknown
        );
        assert_eq!(
            ProcessState::derive(Status::Unknown, Liveness::Dead, false),
            ProcessState::Gone
        );
    }
}
