//! 结构化日志（MISSION §10.2）。
//!
//! 字段约定：`component` / `session_id` / `event`。**禁止记录 terminal content**——
//! 终端里可能有 API key、源码、prompt；需要调试终端流时记长度与哈希，不记正文。

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// 初始化全局 subscriber。`AGORA_LOG` 控制过滤（默认 `info`），
/// `AGORA_LOG_FORMAT=json` 输出 JSON 行，便于 launchd / systemd 采集。
pub fn init() {
    let filter = EnvFilter::try_from_env("AGORA_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("AGORA_LOG_FORMAT").is_ok_and(|v| v == "json");

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry.with(fmt::layer().json()).init();
    } else {
        registry.with(fmt::layer()).init();
    }
}
