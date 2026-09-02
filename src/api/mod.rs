//! HTTP + WebSocket 层（docs/spec/api.md）。
//!
//! 未认证白名单只有：SPA 静态资源、`GET /api/health` 的公开子集、`POST /api/auth/pair`
//! （ADR-003 D1，没有 loopback 例外）。Principal 提取器与其余端点在 agora-xqa.9 落地；
//! 本文件现在只有 health 与静态资源。

mod health;
mod spa;

use std::net::SocketAddr;

use axum::{routing::get, Router};

/// 服务器层的错误枚举（ADR-001 D8 施工约束 4：模块边界用枚举，不用万能错误类型）。
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("绑定 {addr} 失败: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("HTTP 服务异常退出: {0}")]
    Serve(#[source] std::io::Error),
}

/// 组装路由；测试直接拿 Router 走 `tower::ServiceExt::oneshot`，不占端口。
pub fn router() -> Router {
    Router::new()
        .route("/api/health", get(health::public))
        .fallback(get(spa::serve))
}

/// 在 `addr` 上跑到 SIGINT / SIGTERM 为止。
pub async fn serve(addr: SocketAddr) -> Result<(), ServeError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    tracing::info!(component = "api", %addr, "listening");

    axum::serve(listener, router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServeError::Serve)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(component = "api", %err, "无法监听 SIGINT，退回到永不主动关闭");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::warn!(component = "api", %err, "无法监听 SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!(component = "api", "shutdown signal received");
}
