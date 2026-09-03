//! HTTP + WebSocket 层（docs/spec/api.md）。
//!
//! 未认证白名单只有：SPA 静态资源、`GET /api/health` 的公开子集、`POST /api/auth/pair`
//! （ADR-003 D1，没有 loopback 例外）。其余每个 handler 都以 `Principal` 提取器开头；
//! `ROUTES` / `PUBLIC_ROUTES` 两张表是 tests/auth.rs 遍历的对象——加路由必须同时登记，
//! 否则守卫 `every_route_requires_principal_except_allowlist` 不会去敲它。

mod auth;
mod health;
mod spa;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::connect_info::ConnectInfo;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::{Auth, AuthError};

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

#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("listen 地址无法解析: {0}")]
    Parse(String),
    /// 明文监听器只允许 loopback（ADR-003 D5）；非 loopback 请用 TLS 监听器。
    #[error(
        "明文监听器 {0} 不是 loopback 地址：凭据会明文上网；非 loopback 请配置 server.tls_listen"
    )]
    NotLoopback(SocketAddr),
}

/// 明文监听地址的配置校验：字面上只接受 loopback。
pub fn plaintext_listen(addr: &str) -> Result<SocketAddr, ListenError> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|_| ListenError::Parse(addr.to_owned()))?;
    let loopback = match addr.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    };
    if !loopback {
        return Err(ListenError::NotLoopback(addr));
    }
    Ok(addr)
}

#[derive(Clone)]
pub struct AppState {
    pub auth: Arc<Auth>,
}

impl FromRef<AppState> for Arc<Auth> {
    fn from_ref(s: &AppState) -> Self {
        s.auth.clone()
    }
}

/// 全部 API 路由（method, path）。
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/health"),
    ("POST", "/api/auth/pair"),
    ("POST", "/api/auth/pair/new"),
    ("POST", "/api/auth/logout"),
    ("GET", "/api/auth/devices"),
    ("DELETE", "/api/auth/devices/{id}"),
];

/// 未认证白名单（ADR-003 D1）。加一条就是加一个免认证端点，先改 ADR 再改这里。
pub const PUBLIC_ROUTES: &[(&str, &str)] = &[("GET", "/api/health"), ("POST", "/api/auth/pair")];

/// 组装路由；测试直接拿 Router 走 `tower::ServiceExt::oneshot`，不占端口。
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health::public))
        .route("/api/auth/pair", post(auth::pair))
        .route("/api/auth/pair/new", post(auth::pair_new))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/devices", get(auth::devices))
        .route("/api/auth/devices/{id}", delete(auth::revoke_device))
        .fallback(get(spa::serve))
        .with_state(state)
}

/// 在 `addr` 上跑到 SIGINT / SIGTERM 为止。
pub async fn serve(addr: SocketAddr, state: AppState) -> Result<(), ServeError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    tracing::info!(component = "api", %addr, "listening");

    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(ServeError::Serve)
}

/// 对端地址；只在真实监听器上有（oneshot 测试里没有 ConnectInfo）。
pub struct ClientAddr(pub Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for ClientAddr {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientAddr(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// 错误应答：`{ "error": "<type>", "message": "..." }`（docs/spec/api.md）。
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub kind: &'static str,
    pub message: String,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.kind,
                message: &self.message,
            }),
        )
            .into_response()
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let (status, kind) = match &e {
            AuthError::Unauthenticated => (StatusCode::UNAUTHORIZED, "unauthenticated"),
            AuthError::BearerRequiresTls => (StatusCode::UNAUTHORIZED, "bearer_requires_tls"),
            AuthError::CrossOrigin => (StatusCode::FORBIDDEN, "cross_origin"),
            AuthError::PairInvalid => (StatusCode::UNAUTHORIZED, "pair_invalid"),
            AuthError::PairPendingLimit(_) => (StatusCode::TOO_MANY_REQUESTS, "pair_pending_limit"),
            AuthError::DeviceNotFound(_) => (StatusCode::NOT_FOUND, "device_not_found"),
            AuthError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database"),
        };
        if status.is_server_error() {
            tracing::error!(component = "api", error = %e, "内部错误");
        }
        ApiError {
            status,
            kind,
            message: e.to_string(),
        }
    }
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
