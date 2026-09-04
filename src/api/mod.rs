//! HTTP + WebSocket 层（docs/spec/api.md）。
//!
//! 未认证白名单只有：SPA 静态资源、`GET /api/health` 的公开子集、`POST /api/auth/pair`
//! （ADR-003 D1，没有 loopback 例外）。其余每个 handler 都以 `Principal` 提取器开头；
//! `ROUTES` / `PUBLIC_ROUTES` 两张表是 tests/auth.rs 遍历的对象——加路由必须同时登记，
//! 否则守卫 `every_route_requires_principal_except_allowlist` 不会去敲它。

mod agents;
mod auth;
mod events;
mod health;
mod projects;
mod sessions;
mod spa;
mod terminal;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::connect_info::ConnectInfo;
use axum::extract::{FromRef, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use tracing::Instrument;

use crate::auth::{Auth, AuthError};
use crate::config::AgentOverride;
use crate::events::EventBus;
use crate::project::Projects;
use crate::session::SessionManager;

pub use health::{RuntimeHealth, API_VERSION};

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
    pub sessions: Arc<SessionManager>,
    pub events: EventBus,
    /// 节点 id：对外会话 id `<node>:<id>` 的前缀（MISSION §3.5）。
    pub node: Arc<str>,
    /// `agents.<name>.command` 覆盖；创建会话时缺省命令从这里取。
    pub agents: Arc<BTreeMap<String, AgentOverride>>,
    /// New Agent 对话框的项目与 worktree 数据源；`project_roots` 由 `main.rs` 注入。
    pub projects: Arc<Projects>,
    /// 启动时探到的 PATH 来源（"shell" / "daemon"）；运行时好不好是实时的，见 `RuntimeStatus`。
    pub runtime_path_source: &'static str,
    /// hook 挂起表：respond 的 `decision` 走它；没接（测试）时一律 `no_pending_decision`。
    pub hooks: Option<Arc<crate::hook::Receiver>>,
    /// `<program> --version` 的探测缓存（ADR-002 D7 三值），键是程序名。Available 记住到
    /// daemon 重启；不可用的记 `PROBE_RETRY` 这么久再重探，装好了很快就好。
    /// 测试预先塞值免得真跑本机的 agent。
    pub probes: Arc<std::sync::Mutex<BTreeMap<String, (crate::adapter::VersionProbe, Instant)>>>,
}

/// 不可用的探测结果保留多久再重探。
pub const PROBE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

impl AppState {
    pub fn new(auth: Arc<Auth>, sessions: Arc<SessionManager>, node: &str) -> Self {
        AppState {
            auth,
            sessions: sessions.clone(),
            events: EventBus::default(),
            node: Arc::from(node),
            agents: Arc::new(BTreeMap::new()),
            projects: Arc::new(Projects::new(sessions.db_handle(), Vec::new())),
            runtime_path_source: "daemon",
            hooks: None,
            probes: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }

    /// 探 agent 可用性（带缓存）。跑子进程，调用方在 blocking 线程。
    pub fn probe_version(
        &self,
        adapter: &dyn crate::adapter::Adapter,
        program: &str,
    ) -> crate::adapter::VersionProbe {
        use crate::adapter::VersionProbe;
        if let Ok(cache) = self.probes.lock() {
            if let Some((p, at)) = cache.get(program) {
                if matches!(p, VersionProbe::Available(_)) || at.elapsed() < PROBE_RETRY {
                    return p.clone();
                }
            }
        }
        let probed = crate::adapter::probe(adapter, program, std::time::Duration::from_secs(10));
        if let Ok(mut cache) = self.probes.lock() {
            cache.insert(program.to_owned(), (probed.clone(), Instant::now()));
        }
        probed
    }
}

impl FromRef<AppState> for Arc<Auth> {
    fn from_ref(s: &AppState) -> Self {
        s.auth.clone()
    }
}

/// 全部 API 路由（method, path）。
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/health"),
    ("GET", "/api/system"),
    ("GET", "/api/sessions"),
    ("POST", "/api/sessions"),
    ("POST", "/api/sessions/adopt"),
    ("GET", "/api/sessions/{id}"),
    ("PATCH", "/api/sessions/{id}"),
    ("DELETE", "/api/sessions/{id}"),
    ("POST", "/api/sessions/{id}/kill"),
    ("POST", "/api/sessions/{id}/restart"),
    ("POST", "/api/sessions/{id}/cleanup"),
    ("POST", "/api/sessions/{id}/input"),
    ("GET", "/api/sessions/{id}/terminal"),
    ("GET", "/api/projects"),
    ("GET", "/api/projects/worktrees"),
    ("GET", "/api/agents"),
    ("GET", "/api/events"),
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
        .route("/api/health", get(health::health))
        .route("/api/system", get(health::system))
        .route("/api/sessions", get(sessions::list).post(sessions::create))
        // 静态段先于 `{id}`：axum 的匹配按具体度排，`adopt` 不会被当成 id。
        .route("/api/sessions/adopt", post(sessions::adopt))
        .route(
            "/api/sessions/{id}",
            get(sessions::get)
                .patch(sessions::patch)
                .delete(sessions::delete),
        )
        .route("/api/sessions/{id}/kill", post(sessions::kill))
        .route("/api/sessions/{id}/restart", post(sessions::restart))
        .route("/api/sessions/{id}/cleanup", post(sessions::cleanup))
        .route("/api/sessions/{id}/input", post(sessions::input))
        .route("/api/sessions/{id}/terminal", get(terminal::upgrade))
        // 静态段先于 `{id}`：`worktrees` 不会被当成项目路径。
        .route("/api/projects/worktrees", get(projects::worktrees))
        .route("/api/projects", get(projects::list))
        .route("/api/agents", get(agents::list))
        .route("/api/events", get(events::upgrade))
        .route("/api/auth/pair", post(auth::pair))
        .route("/api/auth/pair/new", post(auth::pair_new))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/devices", get(auth::devices))
        .route("/api/auth/devices/{id}", delete(auth::revoke_device))
        .fallback(get(spa::serve))
        .layer(middleware::from_fn(log_request))
        .layer(middleware::from_fn(renew_session_cookie))
        .with_state(state)
}

/// 滑动续期：`Principal` 提取器认证成功且刚把 `last_seen_at` 往后推时，会把要重发的
/// cookie 放进请求扩展里的 `RenewSlot`，这里在响应上补 `Set-Cookie`（ADR-003 D2）。
///
/// 服务端的 30 天与浏览器 cookie 的 30 天必须是同一个滑动窗口。只在配对时发一次
/// `Max-Age` 的话，天天在用的人第 31 天被浏览器丢掉 cookie，服务端却还认——用户看到的
/// 是"明明一直在用，却要求重新配对"（agora-z8b）。频率跟着 `last_seen_at` 的每小时至多
/// 一次写走，所以正常使用下每小时至多多出一个响应头。
async fn renew_session_cookie(mut req: Request, next: Next) -> Response {
    let slot = auth::RenewSlot::default();
    req.extensions_mut().insert(slot.clone());
    let mut resp = next.run(req).await;
    if let Some(cookie) = slot.take() {
        resp.headers_mut().append(header::SET_COOKIE, cookie);
    }
    resp
}

/// 每条 API 请求一行日志：方法、路径、状态、耗时、principal（MISSION §10.2）。
/// principal 由 `Principal` 提取器在 span 上补记；未认证的请求这一栏是空。
/// 只记 `/api/`：SPA 静态资源的日志没有价值。
async fn log_request(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    if !path.starts_with("/api/") {
        return next.run(req).await;
    }
    let span = tracing::info_span!(
        "request",
        method = %req.method(),
        path = %path,
        principal = tracing::field::Empty,
    );
    let started = Instant::now();
    let resp = next.run(req).instrument(span.clone()).await;
    let _enter = span.enter();
    tracing::info!(
        component = "api",
        status = resp.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "request"
    );
    resp
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
