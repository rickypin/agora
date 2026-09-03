//! 状态机（MISSION §4.3 §4.4）。
//!
//! 本阶段只有进程状态层（ADR-002 D1 第 2 层）：STARTING / RUNNING / FINISHED / FAILED /
//! UNKNOWN。WAITING / TURN_DONE / IDLE 与四层来源仲裁在 M1b（agora-dvh.4）落地。
//! 状态是"人要做什么"的定义，不落库（不变量 7）。

use serde::Serialize;

use crate::runtime::{Exit, RuntimeSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Hook,
    Process,
    Text,
    Activity,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assessment {
    pub status: Status,
    pub source: Source,
    pub reason: Option<String>,
}

/// 进程创建后多少秒内、还没有任何活动信息时算 STARTING。
pub const STARTING_WINDOW_SECS: u64 = 2;

/// 进程状态层：只看运行时事实。`killed_by_user` 是 daemon 内存里的事实
/// （agora 自己 terminate 过它），重启后丢失——那时按信号退出就是 FAILED。
pub fn process_layer(
    runtime: Option<&RuntimeSession>,
    age_secs: Option<u64>,
    killed_by_user: bool,
) -> Assessment {
    let Some(rt) = runtime else {
        return Assessment {
            status: Status::Unknown,
            source: Source::None,
            reason: Some("runtime session missing".into()),
        };
    };
    if rt.alive {
        let starting = age_secs.is_some_and(|a| a < STARTING_WINDOW_SECS);
        return Assessment {
            status: if starting {
                Status::Starting
            } else {
                Status::Running
            },
            source: Source::Process,
            reason: None,
        };
    }
    match &rt.exit {
        Some(Exit::Code(0)) => Assessment {
            status: Status::Finished,
            source: Source::Process,
            reason: None,
        },
        Some(Exit::Code(n)) => Assessment {
            status: Status::Failed,
            source: Source::Process,
            reason: Some(format!("exit code {n}")),
        },
        Some(Exit::Signal(sig)) if killed_by_user => Assessment {
            status: Status::Finished,
            source: Source::Process,
            reason: Some(format!("killed by user (signal {sig})")),
        },
        Some(Exit::Signal(sig)) => Assessment {
            status: Status::Failed,
            source: Source::Process,
            reason: Some(format!("signal {sig}")),
        },
        None => Assessment {
            status: Status::Unknown,
            source: Source::Process,
            reason: Some("process exited, exit status not yet collected".into()),
        },
    }
}
