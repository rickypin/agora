//! agora：多 Agent 管理工具的单 binary daemon（MISSION.md；ADR-001..004）。
//!
//! 模块边界由 `tests/arch_boundary.rs` 机械守卫：
//! - `runtime/`：会话运行时抽象；具体运行时的标识符只允许出现在它自己的子目录（ADR-001）。
//! - `session/`：会话生命周期与 metadata（ADR-001 D4）。
//! - `status/`：四层状态来源仲裁（ADR-002 D1）。
//! - `adapter/`：agent 特定代码唯一的落脚点（ADR-002 D2）。
//! - `hook/`：`agora hook` 投递箱与 unix socket（ADR-002 D3/D5）。
//! - `auth/`：设备配对、session、peer token（ADR-003）。
//! - `api/`：HTTP + WS，前端只经这里与节点通信（A36 不变量 9）。
//! - `local/`：`AGORA_HOME` 与本机 unix socket 通道（ADR-003 D6）。

pub mod adapter;
pub mod api;
pub mod auth;
pub mod clock;
pub mod hook;
pub mod local;
pub mod runtime;
pub mod session;
pub mod status;
pub mod telemetry;
