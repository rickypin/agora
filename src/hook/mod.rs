//! `agora hook` 命令、投递箱、unix socket 唤醒与挂起（ADR-002 D3/D5；agora-dvh.3）。
//!
//! 链路是三段：
//! - [`cmd`]：agent 进程里跑的 `agora hook --host <h> --home <dir>`——读 stdin 落盘、连 socket
//!   唤醒、该挂起的挂起等决定；daemon 不在、socket 断、超时一律 exit 0 不输出（fail-open）。
//! - [`inbox`]：投递箱文件的形态与权限：`<home>/hooks/inbox/<host>/<agent_session_id>/<ts>-<seq>.json`，
//!   先 `.part` 再 rename；应用后移 `done/`，保留 24 h。
//! - [`Receiver`]：daemon 侧——启动时按文件名顺序重放、运行中被唤醒即读；epoch 小于当前的丢；
//!   挂起表以 `(session, tool_use_id)` 为键，每会话 8、节点 256，超时 55 min；同 tool_use_id 的
//!   PostToolUse 等、Stop / SessionEnd、进程退出都解除挂起（`decision.resolved`）。
//!
//! 本模块只知道"一个宿主、一坨 JSON"；宿主名、payload 键、哪些事件挂起与解除、决定写回的
//! 形态都问宿主的 `adapter::AgentHooks`（ADR-002 规则 5）；映射成的事件交给状态机（dvh.4）。

pub mod cmd;
pub mod inbox;
mod receiver;

pub use inbox::{Delivery, Envelope, Inbox};
pub use receiver::{
    Received, Receiver, RespondError, HOLD_TIMEOUT, MAX_HOLDS_PER_NODE, MAX_HOLDS_PER_SESSION,
};

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("投递箱 {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("投递箱 {path} 权限过宽（{mode:o}）：其他用户能伪造事件或读到 prompt；请执行 chmod -R go-rwx {path}")]
    TooOpen { path: String, mode: u32 },
    #[error("投递箱 {path} 不属于当前用户（uid {owner} ≠ {me}）")]
    WrongOwner { path: String, owner: u32, me: u32 },
    #[error("投递文件 {path} 不是合法 JSON: {source}")]
    Malformed {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("路径 {0} 不在投递箱里")]
    Outside(String),
}
