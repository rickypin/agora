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
//! - 生产实现（TLS + SPKI 指纹钉住 + Bearer）由 agora-7ku.10 按本 trait 填。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use axum::body::Body;
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

pub use tokio_tungstenite::tungstenite::Message as WsMessage;

/// 每次尝试（HTTP 到响应头、WS 到握手完成）的缺省上限（agora-7ku notes：5 s）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// WS 底下的字节流：fake 是 duplex 的一半，生产是 TLS 流；类型擦除后调用方对两者一视同仁。
pub trait PeerIo: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + ?Sized> PeerIo for T {}

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
    /// 连不上：TCP / TLS 建连失败。
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
        }
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

fn ws_error(e: WsError) -> TransportError {
    match e {
        WsError::Http(resp) => TransportError::WsRejected(resp.status()),
        WsError::Io(e) => TransportError::Unreachable(e),
        other => TransportError::Protocol(other.to_string()),
    }
}
