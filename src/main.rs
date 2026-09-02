//! agora daemon 入口。
//!
//! 现阶段（agora-xqa.6）只做三件事：起 tracing、在 loopback 起 axum、回答
//! `GET /api/health` 的公开子集。配置文件与 listen 校验归 agora-xqa.9，
//! 这里先把地址写死为 127.0.0.1:7680（docs/spec/config.md 的默认值）。

use std::net::SocketAddr;

use agora::api;

/// 配置文件落地前的默认监听地址（docs/spec/config.md：只允许 loopback）。
const DEFAULT_LISTEN: &str = "127.0.0.1:7680";

#[tokio::main]
async fn main() {
    agora::telemetry::init();

    let addr: SocketAddr = match DEFAULT_LISTEN.parse() {
        Ok(addr) => addr,
        Err(err) => {
            tracing::error!(component = "main", listen = DEFAULT_LISTEN, %err, "listen 地址无法解析");
            std::process::exit(2);
        }
    };

    if let Err(err) = api::serve(addr).await {
        tracing::error!(component = "main", %err, "daemon 退出");
        std::process::exit(1);
    }
}
