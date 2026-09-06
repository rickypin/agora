//! peer 链路的传输接缝（agora-7ku.11；MISSION §3.5；ADR-003 D3 / D4；ADR-004）。
//!
//! 节点是 peer 的 API 客户端：拉全量、收事件流、转发写操作与终端流，用的就是浏览器那套 API。
//! 本模块只回答"怎么把一个 HTTP 请求 / 一次 WS 建连送到某个 peer"；认证（Bearer 机器 token）
//! 与证书指纹校验封在实现里，调用方（agora-7ku.5 并入视图、agora-7ku.7 一跳转发）看不见也碰不到。
//!
//! 每次尝试都带超时，且超时是 transport 的属性而不是调用方的参数：devcenter 的
//! `TcpStream::connect` 没有超时，一次 plan 被拖满 45 s 还泄漏阻塞线程（agora-7ku notes 引
//! `docs/analysis/devcenter/appendix-b-multihost.md`）——忘带超时在这里于类型上就不可能。
//!
//! 实现：
//! - [`InProcessTransport`]：同一进程里另一个节点实例的 axum `Router`。HTTP 走 `oneshot`，
//!   WS 走 `tokio::io::duplex` 上的真握手；不开 socket、不碰 TLS，给 fake 多节点测试用
//!   （MISSION §2.3 规则 9 "单进程多节点的测试骨架先于真实集成"）。
//! - [`HttpsTransport`]：生产实现——`peers[]` 一项 → TCP + TLS（只比对端 SPKI 指纹，ADR-003 D4）
//!   加 `Authorization: Bearer <token_file 的内容>`（D3）。URL 不是 `https://`、指纹不合法、
//!   token_file 读不出来都是 [`PeerConfigError`]（显示为「配置错误」，不是离线），且一个字节
//!   都不往网上发。

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{self, HeaderValue};
use axum::http::{Request, Response, StatusCode};
use axum::{Extension, Router};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::WebSocketStream;
use tower::ServiceExt;

use crate::api::InProcessPeer;
use crate::auth::peer_token::{load_token_file, TokenFileError};
use crate::config::PeerSection;
use crate::tls::client::{self as tls_client, Mismatch};
use crate::tls::{Fingerprint, FingerprintError, TlsError};

pub use tokio_tungstenite::tungstenite::Message as WsMessage;

/// 每次尝试（HTTP 到响应头、WS 到握手完成）的缺省上限：就是退避策略里的
/// [`CONNECT_TIMEOUT`](super::backoff::CONNECT_TIMEOUT)（5 s），只此一处定义。
pub const DEFAULT_TIMEOUT: Duration = super::backoff::CONNECT_TIMEOUT;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// WS 底下的字节流：fake 是 duplex 的一半，生产是 TLS 流；类型擦除后调用方对两者一视同仁。
/// 带 `Debug` 是让 `PeerWs` 也 `Debug`——不然 `Result<PeerWs, _>` 连 `unwrap_err` 都调不了。
pub trait PeerIo: AsyncRead + AsyncWrite + Send + Unpin + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + std::fmt::Debug + ?Sized> PeerIo for T {}

/// 建好的 WS 连接；帧类型是 [`WsMessage`]。
pub type PeerWs = WebSocketStream<Box<dyn PeerIo>>;

/// 传输层的失败，按类型分（MISSION §2.3 规则 10）。peer 的状态模型（agora-7ku.12）从这里映射：
/// `Timeout` / `Unreachable` → 不可达，`FingerprintMismatch` 是独立状态、绝不并进"离线"
/// （ADR-003 D4），`WsRejected(401)` → 未授权。HTTP 请求的非 2xx 不是传输错误，原样交给调用方。
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// 一次尝试超过 transport 的超时：HTTP 到响应头、WS 到握手完成。
    #[error("{0:?} 内没有应答")]
    Timeout(Duration),
    /// 连不上：TCP / TLS 建连失败；fake 里是对端被 [`InProcessTransport::set_offline`]。
    #[error("peer 不可达: {0}")]
    Unreachable(#[source] std::io::Error),
    /// 对端证书 SPKI 的 SHA-256 与 `peers[].cert_fingerprint` 不符（ADR-003 D4：没有 TOFU）。
    #[error("证书指纹不匹配：配置 {expected}，对端 {actual}")]
    FingerprintMismatch { expected: String, actual: String },
    /// WS 握手被对端以非 101 拒绝。
    #[error("WS 升级被拒：HTTP {0}")]
    WsRejected(StatusCode),
    /// 线上的协议错误（HTTP 解析、WS 帧）。
    #[error("协议错误: {0}")]
    Protocol(String),
    /// `peers[]` 这一项字面上就用不了（见 [`PeerConfigError`]）：显示为「配置错误」，不是离线
    /// （ADR-003 D3）。发生在拨号之前，网上没有任何字节。
    #[error("peer 配置错误: {0}")]
    Config(#[from] PeerConfigError),
    /// 本机的 TLS 客户端配置建不起来；实际上只会是 rustls 版本不配这类编程错误。
    #[error("TLS 客户端: {0}")]
    Tls(#[from] TlsError),
}

/// `peers[]` 一项字面上就用不了：URL 不是 https、指纹格式不对、token_file 读不了 / 权限过宽 /
/// 内容不像 token。ADR-003 D3 说这类显示为「配置错误」而不是离线——状态模型（agora-7ku.5）从
/// `TransportError::Config` 这一个变体映射，不必认识每一种。
#[derive(Debug, thiserror::Error)]
pub enum PeerConfigError {
    /// peer 链路永不走明文（ADR-003 D5）：`http://` 在这里就拒绝，连都不连。IPv6 写 `[::1]:7681`。
    #[error("peers[].url 必须是 https://host[:port]，得到 {0:?}")]
    Url(String),
    /// 没有指纹就没有信任锚（无 TOFU）：空串、不是 `sha256:<64 hex>` 都在这里拒绝。
    #[error("peers[].cert_fingerprint: {0}")]
    Pin(#[from] FingerprintError),
    /// D3：token_file 读不到 / 不属于自己 / 权限过宽 / 内容不是 token——持有方那一半的判定只有一份，
    /// 在 [`crate::auth::peer_token::load_token_file`]（agora-7ku.2）；这里透传它的类型与文案
    /// （文案带 `chmod 600 <path>` 提示，peer 客户端转入「配置错误」时原样进 warn 日志）。
    /// 2026-09-06（agora-41e）之前本模块另有一份更松的检查（不看属主、内容只查 ASCII），两份判定
    /// 会让同一个文件在 CLI 与 daemon 眼里一好一坏——别再加回来。
    #[error(transparent)]
    TokenFile(#[from] TokenFileError),
}

/// 给 `peers[]` 一项的传输。dyn 兼容（注册表存 `Arc<dyn PeerTransport>`），所以 async 方法
/// 手写成 [`BoxFuture`]。
pub trait PeerTransport: Send + Sync + 'static {
    /// peer 名（`peers[].name`），也是注册表的键。
    fn name(&self) -> &str;

    /// 每次尝试的上限。
    fn timeout(&self) -> Duration;

    /// 发一个 API 请求。`req` 的 URI 只写路径与查询串（`/api/sessions?x=1`）：主机、TLS、Bearer
    /// 由实现补。超时覆盖到响应头到达；响应 body 是流，由调用方消费。非 2xx 是正常返回。
    fn request(&self, req: Request<Body>) -> BoxFuture<'_, Result<Response<Body>, TransportError>>;

    /// 建一条 WS（`/api/events`、`/api/sessions/{id}/terminal?cols=..&rows=..`）；超时覆盖到握手完成。
    fn connect_ws<'a>(
        &'a self,
        path_and_query: &'a str,
    ) -> BoxFuture<'a, Result<PeerWs, TransportError>>;
}

// ---------- 进程内 fake ----------

/// fake 的 WS 握手用的主机名：进程内没有网络，只是让 `Host` 头有个值。
const IN_PROCESS_HOST: &str = "in-process";
/// duplex 每个方向的缓冲；终端流一帧远小于此。
const DUPLEX_BUF: usize = 64 * 1024;

/// 同一进程里另一个节点实例的 Router 当 peer。对端看到的 principal 是 `Peer { name: caller }`：
/// 经 [`InProcessPeer`] 请求扩展注入，不走 Bearer，所以不开 socket、不碰 TLS（agora-7ku.11 ③）。
///
/// HTTP 直接 `oneshot`；WS 要真握手（axum 的 `WebSocketUpgrade` 只认 hyper 放进请求扩展的
/// `OnUpgrade`，`oneshot` 给不了），所以每次 `connect_ws` 起一对 `tokio::io::duplex`，服务端那半
/// 交给 hyper 服务一条连接，客户端那半跑 tungstenite 的握手。连接结束任务自然结束，没有监听器要收。
pub struct InProcessTransport {
    name: String,
    timeout: Duration,
    app: Router,
    /// 测试拨这个开关模拟 peer 断线：置上后每次尝试立即 `Unreachable`，已建好的 WS 不受影响
    /// （对端真断线时旧连接也是各自死各自的）。
    offline: AtomicBool,
    /// 测试开关（agora-7ku.6）：对端进程活着但坏了——每个 HTTP 请求 500、WS 握手被 502 拒。
    /// 与 `offline` 是不变量 8 要挡的两种坏 peer：连不上的，和连得上却答非所问的。
    broken: AtomicBool,
    /// 测试观测口（agora-7ku.6）：连接尝试计数，`request` 与 `connect_ws` 各算一次，被
    /// `offline` / `broken` 挡掉的也算——"退避封顶、永不放弃"的集成版就是数它涨了多少。
    attempts: AtomicUsize,
}

impl InProcessTransport {
    /// `name`：对端节点名；`caller`：本机 `node.id`，对端看到的就是 `Peer { name: caller }`；
    /// `target`：对端实例的 Router（`api::router(state)`）。
    pub fn new(name: &str, caller: &str, target: Router, timeout: Duration) -> Self {
        let app = target.layer(Extension(InProcessPeer {
            name: caller.to_owned(),
        }));
        InProcessTransport {
            name: name.to_owned(),
            timeout,
            app,
            offline: AtomicBool::new(false),
            broken: AtomicBool::new(false),
            attempts: AtomicUsize::new(0),
        }
    }

    /// 模拟 peer 断线 / 恢复（agora-7ku.5 的 stale 行、agora-7ku.6 的重连测试用）。
    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }

    /// 模拟 peer 活着但坏了：HTTP 一律 500（body 是文本不是 JSON）、WS 握手 502。
    pub fn set_broken(&self, broken: bool) {
        self.broken.store(broken, Ordering::SeqCst);
    }

    pub fn is_broken(&self) -> bool {
        self.broken.load(Ordering::SeqCst)
    }

    /// 到现在为止的连接尝试次数（含被挡掉的）。
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    fn broken_response(&self) -> Response<Body> {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(format!("fake peer {} 坏了", self.name)))
            .expect("静态响应")
    }

    fn refused(&self) -> TransportError {
        TransportError::Unreachable(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("fake peer {} 处于离线", self.name),
        ))
    }
}

impl PeerTransport for InProcessTransport {
    fn name(&self) -> &str {
        &self.name
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn request(&self, req: Request<Body>) -> BoxFuture<'_, Result<Response<Body>, TransportError>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.is_offline() {
                return Err(self.refused());
            }
            if self.is_broken() {
                return Ok(self.broken_response());
            }
            tokio::time::timeout(self.timeout, self.app.clone().oneshot(req))
                .await
                .map_err(|_| TransportError::Timeout(self.timeout))?
                .map_err(|never| match never {})
        })
    }

    fn connect_ws<'a>(
        &'a self,
        path_and_query: &'a str,
    ) -> BoxFuture<'a, Result<PeerWs, TransportError>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.is_offline() {
                return Err(self.refused());
            }
            if self.is_broken() {
                return Err(TransportError::WsRejected(StatusCode::BAD_GATEWAY));
            }
            if !path_and_query.starts_with('/') {
                return Err(TransportError::Protocol(format!(
                    "WS 路径必须以 / 开头: {path_and_query:?}"
                )));
            }
            let (client, server) = tokio::io::duplex(DUPLEX_BUF);
            let app = self.app.clone();
            tokio::spawn(async move {
                let conn = hyper::server::conn::http1::Builder::new()
                    // 超时由客户端那半（本 transport）负责；hyper 的缺省 header 超时需要显式
                    // timer，不关会每条连接 warn 一次 "no timer set"。
                    .header_read_timeout(None)
                    .serve_connection(TokioIo::new(server), TowerToHyperService::new(app))
                    .with_upgrades();
                if let Err(err) = conn.await {
                    tracing::debug!(component = "peer", %err, "进程内 fake 连接结束");
                }
            });
            let req = format!("ws://{IN_PROCESS_HOST}{path_and_query}")
                .into_client_request()
                .map_err(ws_error)?;
            let io: Box<dyn PeerIo> = Box::new(client);
            let (ws, _response) =
                tokio::time::timeout(self.timeout, tokio_tungstenite::client_async(req, io))
                    .await
                    .map_err(|_| TransportError::Timeout(self.timeout))?
                    .map_err(ws_error)?;
            Ok(ws)
        })
    }
}

// ---------- 生产实现 ----------

/// 生产传输：`peers[]` 一项 → TLS（SPKI 指纹钉住，ADR-003 D4）+ Bearer（`token_file`，D3）。
///
/// 每次 `request` / `connect_ws` 自己建一条 TCP + TLS 连接、用完即弃，没有连接池：peer 客户端
/// （agora-7ku.5）常驻的只有一条 `/api/events` WS 与偶发的转发请求，池子省下的一次握手不值它的
/// 复杂度；将来要池化只改这里。token 明文按 D3 只在发请求时从 `token_file` 读，不进这个结构体、
/// 不进日志、不缓存——吊销 / 轮换后换文件即生效。
pub struct HttpsTransport {
    peer: PeerSection,
    timeout: Duration,
}

/// `peers[].url` 拆出来的拨号目标。只认 `https://host[:port]`；路径忽略——peer 的 API 路径由调用方给。
struct Target {
    host: String,
    port: u16,
    /// `Host` 头 / WS URL 里的 authority 原样（含端口）。
    authority: String,
}

impl Target {
    fn parse(url: &str) -> Result<Target, PeerConfigError> {
        let bad = || PeerConfigError::Url(url.to_owned());
        let rest = url.trim().strip_prefix("https://").ok_or_else(bad)?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            return Err(bad());
        }
        let port_of = |p: &str| p.parse::<u16>().map_err(|_| bad());
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']').ok_or_else(bad)?;
            let port = match after.strip_prefix(':') {
                Some(p) => Some(port_of(p)?),
                None if after.is_empty() => None,
                None => return Err(bad()),
            };
            (host, port)
        } else if let Some((host, p)) = authority.rsplit_once(':') {
            (host, Some(port_of(p)?))
        } else {
            (authority, None)
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Target {
            host: host.to_owned(),
            port: port.unwrap_or(443),
            authority: authority.to_owned(),
        })
    }
}

impl HttpsTransport {
    /// `timeout` 用 `backoff::CONNECT_TIMEOUT`（5 s）：一次尝试从 TCP 到响应头 / WS 握手完成的总上限。
    pub fn new(peer: PeerSection, timeout: Duration) -> Self {
        HttpsTransport { peer, timeout }
    }

    pub fn peer(&self) -> &PeerSection {
        &self.peer
    }

    /// 配置检查 + 建 TLS 连接。三项检查都在拨号之前：URL 不是 https、指纹不合法、token 读不出来，
    /// 一个字节都不往网上发（守卫：`tests/peer_tls.rs::no_pin_no_connect`）。
    async fn dial(
        &self,
    ) -> Result<
        (
            Target,
            String,
            tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
        ),
        TransportError,
    > {
        let target = Target::parse(&self.peer.url)?;
        let pin = Fingerprint::parse(&self.peer.cert_fingerprint).map_err(PeerConfigError::Pin)?;
        let token = read_token(&self.peer.token_file)?;
        let connector = tls_client::connector(pin)?;
        let io = tls_client::connect(&connector, &target.host, target.port)
            .await
            .map_err(tls_error)?;
        Ok((target, token, io))
    }
}

impl PeerTransport for HttpsTransport {
    fn name(&self) -> &str {
        &self.peer.name
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn request(
        &self,
        mut req: Request<Body>,
    ) -> BoxFuture<'_, Result<Response<Body>, TransportError>> {
        Box::pin(async move {
            let attempt = async {
                let (target, token, io) = self.dial().await?;
                let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
                    .await
                    .map_err(hyper_error)?;
                // 连接由自己的任务驱动到 body 读完；响应 body 是流，交给调用方消费。
                let peer = self.peer.name.clone();
                tokio::spawn(async move {
                    if let Err(err) = conn.await {
                        tracing::debug!(component = "peer", %peer, %err, "到 peer 的 HTTP 连接结束");
                    }
                });
                let headers = req.headers_mut();
                headers.insert(header::HOST, header_value(&target.authority)?);
                headers.insert(
                    header::AUTHORIZATION,
                    header_value(&format!("Bearer {token}"))?,
                );
                let resp = send.send_request(req).await.map_err(hyper_error)?;
                Ok(resp.map(Body::new))
            };
            tokio::time::timeout(self.timeout, attempt)
                .await
                .map_err(|_| TransportError::Timeout(self.timeout))?
        })
    }

    fn connect_ws<'a>(
        &'a self,
        path_and_query: &'a str,
    ) -> BoxFuture<'a, Result<PeerWs, TransportError>> {
        Box::pin(async move {
            if !path_and_query.starts_with('/') {
                return Err(TransportError::Protocol(format!(
                    "WS 路径必须以 / 开头: {path_and_query:?}"
                )));
            }
            let attempt = async {
                let (target, token, io) = self.dial().await?;
                let mut req = format!("wss://{}{path_and_query}", target.authority)
                    .into_client_request()
                    .map_err(ws_error)?;
                req.headers_mut().insert(
                    header::AUTHORIZATION,
                    header_value(&format!("Bearer {token}"))?,
                );
                let io: Box<dyn PeerIo> = Box::new(io);
                let (ws, _response) = tokio_tungstenite::client_async(req, io)
                    .await
                    .map_err(ws_error)?;
                Ok(ws)
            };
            tokio::time::timeout(self.timeout, attempt)
                .await
                .map_err(|_| TransportError::Timeout(self.timeout))?
        })
    }
}

/// D3：token 明文在 `token_file`，0600、属于自己、形态是 `apt_<name>_<43 字符>`；每次发请求读一遍、
/// 不缓存（吊销 / 轮换后换文件即生效）。判定全在 `auth::peer_token::load_token_file`，这里只换类型。
fn read_token(path: &Path) -> Result<String, PeerConfigError> {
    Ok(load_token_file(path)?)
}

fn header_value(s: &str) -> Result<HeaderValue, TransportError> {
    HeaderValue::from_str(s).map_err(|e| TransportError::Protocol(e.to_string()))
}

/// TLS 建连的 `io::Error`：指纹不匹配是独立类型（ADR-003 D4，绝不并进"离线"），其余都是不可达。
fn tls_error(e: std::io::Error) -> TransportError {
    match Mismatch::from_io(&e) {
        Some(m) => TransportError::FingerprintMismatch {
            expected: m.expected.to_string(),
            actual: m.actual.to_string(),
        },
        None => TransportError::Unreachable(e),
    }
}

fn hyper_error(e: hyper::Error) -> TransportError {
    TransportError::Protocol(e.to_string())
}

fn ws_error(e: WsError) -> TransportError {
    match e {
        WsError::Http(resp) => TransportError::WsRejected(resp.status()),
        WsError::Io(e) => TransportError::Unreachable(e),
        other => TransportError::Protocol(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ws::WebSocketUpgrade;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use std::os::unix::fs::PermissionsExt;

    fn slow_router() -> Router {
        Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    "late"
                }),
            )
            .route(
                "/ws-slow",
                get(|ws: WebSocketUpgrade| async move {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    ws.on_upgrade(|_| async {})
                }),
            )
            .route(
                "/ws-401",
                get(|| async { StatusCode::UNAUTHORIZED.into_response() }),
            )
            .route("/ok", get(|| async { "ok" }))
    }

    fn get_req(path: &str) -> Request<Body> {
        Request::get(path).body(Body::empty()).unwrap()
    }

    /// 注入的超时要短到测试不真等、又长到 CI 慢机上 `/ok` 这种空 handler 不会误超。
    const SHORT: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn request_timeout_is_injectable_and_enforced() {
        let t = InProcessTransport::new("b", "a", slow_router(), SHORT);
        assert_eq!(t.timeout(), SHORT);
        let err = t.request(get_req("/slow")).await.unwrap_err();
        assert!(
            matches!(err, TransportError::Timeout(d) if d == SHORT),
            "{err:?}"
        );
        // 不慢的路径在同一个超时下正常返回：超时是上限，不是延迟。
        let resp = t.request(get_req("/ok")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn ws_handshake_timeout_is_enforced() {
        let t = InProcessTransport::new("b", "a", slow_router(), SHORT);
        let err = t.connect_ws("/ws-slow").await.unwrap_err();
        assert!(matches!(err, TransportError::Timeout(_)), "{err:?}");
    }

    #[tokio::test]
    async fn ws_rejection_carries_the_status_not_a_message() {
        // 对端以 401 拒绝升级：调用方按 WsRejected(401) 分支，不解析文本（MISSION §2.3 规则 10）。
        let t = InProcessTransport::new("b", "a", slow_router(), DEFAULT_TIMEOUT);
        let err = t.connect_ws("/ws-401").await.unwrap_err();
        assert!(
            matches!(err, TransportError::WsRejected(StatusCode::UNAUTHORIZED)),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn offline_fake_is_unreachable_immediately_and_recovers() {
        let t = InProcessTransport::new("b", "a", slow_router(), DEFAULT_TIMEOUT);
        assert_eq!(t.attempts(), 0);
        t.set_offline(true);
        assert!(t.is_offline());
        let err = t.request(get_req("/ok")).await.unwrap_err();
        assert!(matches!(err, TransportError::Unreachable(_)), "{err:?}");
        let err = t.connect_ws("/ws-401").await.unwrap_err();
        assert!(matches!(err, TransportError::Unreachable(_)), "{err:?}");
        assert_eq!(t.attempts(), 2, "被拒掉的尝试也算");
        t.set_offline(false);
        assert_eq!(
            t.request(get_req("/ok")).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(t.attempts(), 3);
    }

    #[tokio::test]
    async fn broken_fake_answers_500_and_refuses_ws_but_is_not_unreachable() {
        // 不变量 8 的第二种坏 peer（agora-7ku.6）：进程活着、连得上、答非所问。HTTP 是"正常返回
        // 非 2xx"（调用方按状态码判），WS 是 WsRejected(502)——两者都不是 Unreachable。
        let t = InProcessTransport::new("b", "a", slow_router(), DEFAULT_TIMEOUT);
        t.set_broken(true);
        assert!(t.is_broken() && !t.is_offline());
        let resp = t.request(get_req("/ok")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let err = t.connect_ws("/ws-401").await.unwrap_err();
        assert!(
            matches!(err, TransportError::WsRejected(StatusCode::BAD_GATEWAY)),
            "{err:?}"
        );
        t.set_broken(false);
        assert_eq!(
            t.request(get_req("/ok")).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(t.attempts(), 3);
    }

    #[test]
    fn peer_url_must_be_https_with_optional_port() {
        let t = Target::parse("https://zuan.tail6f613.ts.net:7681/").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("zuan.tail6f613.ts.net", 7681));
        assert_eq!(t.authority, "zuan.tail6f613.ts.net:7681");
        let t = Target::parse(" https://10.0.0.2 ").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("10.0.0.2", 443));
        let t = Target::parse("https://[fd00::1]:7681/api/x?y").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("fd00::1", 7681));
        assert_eq!(t.authority, "[fd00::1]:7681");
        for bad in [
            "http://zuan:7681",
            "zuan:7681",
            "https://",
            "https://user@zuan",
            "https://zuan:notaport",
            "https://[fd00::1",
            "https://[fd00::1]x",
        ] {
            assert!(
                matches!(Target::parse(bad), Err(PeerConfigError::Url(_))),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn https_transport_refuses_bad_config_before_dialing() {
        // 名字来自 peers[].name（注册表的键）；配置字面上就错的三种情形都是 Config 而不是
        // 不可达 / 超时——7ku.5 的状态模型据此显示「配置错误」（ADR-003 D3），且这里没有任何
        // 网络地址可连（url 指向一个不存在的主机名），能立刻返回就说明没去拨号。
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("zuan.token");
        // 形态要过 load_token_file 的 parse：apt_<name>_<43 字符>（agora-7ku.2）。
        let plain = format!("apt_zuan_{}", "x".repeat(43));
        std::fs::write(&token, format!("{plain}\n")).unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let section = |url: &str, pin: &str, token: &Path| PeerSection {
            name: "zuan".into(),
            url: url.into(),
            token_file: token.to_path_buf(),
            cert_fingerprint: pin.into(),
        };
        let good_pin = format!("sha256:{}", "0".repeat(64));

        let t = HttpsTransport::new(
            section("http://zuan.invalid:7681", &good_pin, &token),
            DEFAULT_TIMEOUT,
        );
        assert_eq!(t.name(), "zuan");
        assert_eq!(t.peer().url, "http://zuan.invalid:7681");
        let err = t.request(get_req("/api/system")).await.unwrap_err();
        assert!(
            matches!(err, TransportError::Config(PeerConfigError::Url(_))),
            "{err:?}"
        );

        let t = HttpsTransport::new(
            section("https://zuan.invalid:7681", "", &token),
            DEFAULT_TIMEOUT,
        );
        let err = t.connect_ws("/api/events").await.unwrap_err();
        assert!(
            matches!(err, TransportError::Config(PeerConfigError::Pin(_))),
            "{err:?}"
        );

        let t = HttpsTransport::new(
            section(
                "https://zuan.invalid:7681",
                &good_pin,
                &dir.path().join("missing"),
            ),
            DEFAULT_TIMEOUT,
        );
        let err = t.request(get_req("/api/system")).await.unwrap_err();
        assert!(
            matches!(
                err,
                TransportError::Config(PeerConfigError::TokenFile(TokenFileError::Read { .. }))
            ),
            "{err:?}"
        );

        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        let t = HttpsTransport::new(
            section("https://zuan.invalid:7681", &good_pin, &token),
            DEFAULT_TIMEOUT,
        );
        let err = t.request(get_req("/api/system")).await.unwrap_err();
        assert!(
            matches!(
                err,
                TransportError::Config(PeerConfigError::TokenFile(TokenFileError::TooOpen {
                    mode: 0o644,
                    ..
                }))
            ),
            "{err:?}"
        );
        // 文案透传自 TokenFileError：带 chmod 600 提示，peer 客户端的 warn 日志就是这一句。
        assert!(err.to_string().contains("chmod 600"), "{err}");

        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&token, "  \n").unwrap();
        assert!(matches!(
            read_token(&token),
            Err(PeerConfigError::TokenFile(TokenFileError::Malformed { .. }))
        ));
        std::fs::write(&token, format!("{plain}\n")).unwrap();
        assert_eq!(read_token(&token).unwrap(), plain);
    }

    #[tokio::test]
    async fn ws_path_must_be_absolute() {
        let t = InProcessTransport::new("b", "a", slow_router(), DEFAULT_TIMEOUT);
        let err = t.connect_ws("api/events").await.unwrap_err();
        assert!(matches!(err, TransportError::Protocol(_)), "{err:?}");
    }
}
