//! 会话运行时抽象（ADR-001 D2）。
//!
//! 这里是 session / status / api 能看到的全部：一个 [`Runtime`] trait、一个不透明的
//! [`RuntimeRef`]、一组数据结构和 [`RuntimeError`]。具体运行时的标识符只许出现在它
//! 自己的子目录（`tests/arch_boundary.rs`）。trait 方法是**同步阻塞**的：调用方在
//! `tokio::task::spawn_blocking` 里跑，保证只有 tokio 一种并发模型（D8）。

use std::collections::BTreeMap;

/// hook 进程要把"我在哪个 pane 里"带回信封（ADR-002 D3/D4），而哪些环境变量能定位 pane
/// 是运行时的知识：放这里而不是 hook/，第二运行时来了只改这一处。
pub const PANE_ENV_VARS: &[&str] = &["TMUX", "TMUX_PANE"];

pub mod env_probe;
pub mod exec;
pub mod proctree;
pub mod tmux;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// 不透明字符串，形态 `<kind>:<socket>:<session>`；原样落库，只在运行时内部解析。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RuntimeRef(pub String);

impl std::fmt::Display for RuntimeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl RuntimeRef {
    /// 形态里的第二段（socket）。这是上面注释定的公共形态，不是某个运行时的私有知识；
    /// 段数不对（存进来一个不认识的东西）返回 None——调用方对"说不了 socket 的 ref"
    /// 一律按"扫过了"处理，见 [`RuntimeScan::scanned`]。
    pub fn socket(&self) -> Option<&str> {
        let mut parts = self.0.splitn(3, ':');
        parts.next()?;
        parts.next()
    }
}

/// 一次 [`Runtime::list`] 的产出：扫到的会话 **+ 哪几个 socket 这一次没扫成**。
///
/// 为什么第二半必须交出来（agora-dkv3）：`list()` 对采纳来的用户 socket 的失败是吞掉的
/// ——用户自己的 tmux 坏了不该拖垮 agora 的会话列表（ADR-001 D7）——吞掉之后"这个 ref 不在
/// 列表里"就有两种读法：①运行时答话说没有这个会话（FINISHED `runtime_gone`，agora-u5p）、
/// ②那一次根本没看那个 socket（什么都不知道）。只交出 `Vec<RuntimeSession>` 就只剩①这一种
/// 读法，于是②被读成①：adopted 行报成 FINISHED `runtime session gone (session gone)` 并写
/// `ended_at`（用户在 Dashboard 上看到的是一句"会话没了"，实际只是没扫到）。带 per-socket 的
/// 结论之后，两条 missing 分支（`view` 每 tick、`reconcile` 启动时）都能把②挡在 `ended_at` 之前。
#[derive(Debug, Clone, Default)]
pub struct RuntimeScan {
    /// 扫到的会话，含非 agora 创建的；没扫成的 socket 上那些会话不在里面（不是"没有"）。
    pub sessions: Vec<RuntimeSession>,
    /// 这一次扫描失败的 socket，按运行时给的名字（与 ref 的第二段同一个东西）。空 = 全都扫成了。
    pub unreadable: Vec<SocketScanFailure>,
}

/// 一个 socket 这一次的扫描失败。`reason` 是给调用方拼进 `reason` 那句话的人话。
#[derive(Debug, Clone)]
pub struct SocketScanFailure {
    pub socket: String,
    pub reason: String,
}

impl RuntimeScan {
    /// 全部 socket 都扫成了：只交出会话。
    pub fn complete(sessions: Vec<RuntimeSession>) -> Self {
        RuntimeScan {
            sessions,
            unreadable: Vec::new(),
        }
    }

    /// 这个 ref 所在的 socket 这一次**有没有被真扫过**。
    ///
    /// 没扫过（`true` 为假）意味着"列表里没有它"说不出任何事：既不能说还在，也不能说没了
    /// （agora-dkv3）。ref 解析不出 socket 时按"扫过"答——解析不了就不是本运行时的 socket，
    /// 说不了它这次扫没扫，不能拿这个去把已有的结束结论改成钉死在 UNKNOWN（与
    /// [`Runtime::server_present`] 对解析不了的 ref 按"在"答是同一条保守方向）。
    pub fn scanned(&self, r#ref: &RuntimeRef) -> bool {
        self.failure_for(r#ref).is_none()
    }

    /// 这个 ref 所在的 socket 这一次为什么没扫成；扫成了就 None。
    pub fn failure_for(&self, r#ref: &RuntimeRef) -> Option<&SocketScanFailure> {
        // 解析不出 socket 的 ref 不属于本运行时的任何一个 socket，按"扫过"答（见 [`Self::scanned`]）。
        let socket = r#ref.socket()?;
        self.unreadable.iter().find(|f| f.socket == socket)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Default for Size {
    fn default() -> Self {
        Size {
            cols: 160,
            rows: 48,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub name: String,
    /// 可移植的命令行（存 agent 的裸命令名），由运行时的 shell 解释；不改写成绝对路径（D7）。
    pub command: String,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub size: Size,
}

/// `exit` 是数据不是判断：FINISHED / FAILED 的映射归 ADR-002。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Exit {
    Code(i32),
    Signal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSession {
    pub r#ref: RuntimeRef,
    pub name: String,
    pub pid: Option<u32>,
    pub alive: bool,
    pub exit: Option<Exit>,
    pub exited_at: Option<SystemTime>,
    pub title: String,
    pub cwd: PathBuf,
    pub attached: bool,
    pub size: Size,
    /// 是否在 agora 自己的 socket 上；`false` 的会话只读。
    pub managed: bool,
    /// pane 最近一次产生输出的时刻（unix 秒）；运行时不提供就是 None。活动启发式（IDLE，
    /// ADR-002 D1 第 4 层）只用它，不抓屏——每 tick 抓一次屏对 tmux 是一个子进程，太贵。
    pub output_at: Option<i64>,
}

/// attach 要起的长进程：Terminal Gateway（agora-xqa.10）把它放进自己的 PTY。
/// 运行时只交出 argv 与环境，PTY 归 gateway，这样 PTY 读写的 blocking 线程只有一处。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("会话不存在: {0}")]
    NotFound(RuntimeRef),
    #[error("会话进程仍在运行: {0}")]
    StillAlive(RuntimeRef),
    #[error("运行时 server 不可用: {reason}")]
    ServerUnavailable { reason: String },
    #[error("运行时版本不满足: {reason}")]
    VersionMismatch { reason: String },
    #[error("运行时调用超时: {0}")]
    Timeout(String),
    #[error("运行时调用失败: {stderr_tail}")]
    Failed { stderr_tail: String },
    /// 非 agora socket 上的会话只读（ADR-001 D3）。
    #[error("会话不由 agora 管理，拒绝写操作: {0}")]
    ReadOnly(RuntimeRef),
}

impl RuntimeError {
    /// 这个错误说的是"整个运行时此刻不可信"，而不是某个会话的语义结论。
    /// `NotFound` / `StillAlive` / `ReadOnly` 是关于单个会话的**回答**，不算降级。
    pub fn degrades_runtime(&self) -> bool {
        matches!(
            self,
            RuntimeError::ServerUnavailable { .. }
                | RuntimeError::VersionMismatch { .. }
                | RuntimeError::Timeout(_)
                | RuntimeError::Failed { .. }
        )
    }
}

/// 运行时可用性的**实时**结论（ADR-001 D7）。daemon 启动时探一次版本，之后每一次 list /
/// inspect 的成败都更新它：失败 → degraded 并带原因，成功 → 自动转回 ok。典型场景是运行时
/// 升级之后新 client 连不上旧 server（协议版本不匹配），agora 对那个 socket 失明——此时
/// **绝不销毁 server、绝不退出 daemon**：失明期间已有会话报 UNKNOWN 而不是"已经死了"，
/// 等 server 换代后自己转回 ok。
#[derive(Debug, Default)]
pub struct RuntimeStatus {
    reason: Mutex<Option<String>>,
}

impl RuntimeStatus {
    /// `None` = ok。
    pub fn reason(&self) -> Option<String> {
        self.lock().clone()
    }

    pub fn is_degraded(&self) -> bool {
        self.lock().is_some()
    }

    /// 把一次运行时调用的结果记进来：成功即恢复，失败按 [`RuntimeError::degrades_runtime`] 判。
    /// 关于单个会话的错误（NotFound 等）既不降级也不恢复——它没有回答"运行时好不好"。
    pub fn observe<T>(&self, r: &Result<T, RuntimeError>) {
        match r {
            Ok(_) => *self.lock() = None,
            Err(e) if e.degrades_runtime() => *self.lock() = Some(e.to_string()),
            Err(_) => {}
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.reason.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl From<exec::ExecError> for RuntimeError {
    fn from(err: exec::ExecError) -> Self {
        match err {
            exec::ExecError::Timeout { program, .. } => RuntimeError::Timeout(program),
            e if e.is_not_found() => RuntimeError::ServerUnavailable {
                reason: e.to_string(),
            },
            e => RuntimeError::Failed {
                stderr_tail: e.to_string(),
            },
        }
    }
}

/// `signal` 能发的两种信号：Kill 的 TERM → 宽限 → KILL 拆成两步时用（agora-284）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateSignal {
    Term,
    Kill,
}

pub trait Runtime: Send + Sync {
    fn kind(&self) -> &'static str;
    /// 一次调用完成创建 + 会话级选项。
    fn create(&self, spec: &LaunchSpec) -> Result<RuntimeRef, RuntimeError>;
    /// 全部 socket 的全部会话，含非 agora 创建的。每 tick 每 socket一次子进程（D6）。
    ///
    /// 交出的是 [`RuntimeScan`] 而不是一个 Vec：对采纳 socket 的失败只 warn、不拖垮整张列表
    /// （ADR-001 D7），但那个 socket 上要留一个"这次没扫过"的记录，否则它上面的行会被误判成
    /// "会话没了"（agora-dkv3）。整体扫不成（agora 自己的 socket 坏了）仍然返回 Err。
    fn list(&self) -> Result<RuntimeScan, RuntimeError>;
    fn inspect(&self, r#ref: &RuntimeRef) -> Result<RuntimeSession, RuntimeError>;
    /// `list()` 说"列表里没有这个 ref"之后追问一句：它所在的那个 server 还在不在。
    ///
    /// 为什么要单独问：`list()` 对"server 没了"返回的是**空列表而不是错误**（ADR-001 D4：
    /// 没有会话不是故障），于是"有句柄却不在列表里"有两种说法——整个 server 没了，还是只有
    /// 这一个会话没了。状态层要分开写 reason（agora-u5p）。
    /// 实现只许用 connect 探活这一类廉价手段：读路径每 tick 会为每一个"不在列表里"的行问一次，
    /// 起子进程就是每 tick 每行一个 tmux client（ADR-001 D6 的账）。探不到就保守答 `true`。
    /// 默认 `true`：没有"server"概念的运行时的行只有"会话没了"一种说法。
    fn server_present(&self, r#ref: &RuntimeRef) -> bool {
        let _ = r#ref;
        true
    }
    fn attach(&self, r#ref: &RuntimeRef, size: Size) -> Result<AttachSpec, RuntimeError>;
    /// 只供观测与预览；返回 ≤ 64 KB 的尾部。
    fn capture_tail(&self, r#ref: &RuntimeRef, lines: u32) -> Result<Vec<u8>, RuntimeError>;
    /// TERM 进程组 → grace → KILL；不销毁会话。
    fn terminate(&self, r#ref: &RuntimeRef, grace: Duration) -> Result<(), RuntimeError>;
    /// 只给进程组发一个信号、不等它退：Kill 把宽限期放到后台时用（agora-284）。已经死了是 Ok。
    fn signal(&self, r#ref: &RuntimeRef, sig: TerminateSignal) -> Result<(), RuntimeError>;
    /// 同一会话内重建（Restart）；保留 scrollback。
    fn respawn(&self, r#ref: &RuntimeRef, spec: &LaunchSpec) -> Result<(), RuntimeError>;
    /// 销毁会话；进程仍活着时拒绝（StillAlive）。
    fn remove(&self, r#ref: &RuntimeRef) -> Result<(), RuntimeError>;

    /// 不经终端把字节写进会话的 PTY（MISSION §7.3 respond 的 `text`）：自由问答、下一条指令。
    /// 尾部的换行按"回车键"发，其余按字面写入。
    fn send_input(&self, r#ref: &RuntimeRef, data: &str) -> Result<(), RuntimeError>;

    /// 用 hook 进程带来的运行时环境（[`PANE_ENV_VARS`]）反查它跑在哪个会话里（MISSION §5.4）：
    /// 只认 agora 自己的 socket 与 `adopt_sockets`，其余一律 None（不是这个运行时 / 不可采纳）。
    fn locate(&self, env: &BTreeMap<String, String>) -> Result<Option<RuntimeRef>, RuntimeError> {
        let _ = env;
        Ok(None)
    }
}

/// capture_tail 的整体上限（D6：每次整体替换，不累积）。
pub const TAIL_BUFFER_MAX: usize = 64 * 1024;
