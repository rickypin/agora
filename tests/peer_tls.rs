//! peer 链路 TLS 半边的守卫（agora-7ku.10；ADR-003 D4 / D5）：指纹钉住、无 TOFU、自签一次生成、
//! external 热加载、CLI 指纹与证书一致。真 TLS、真 socket（`127.0.0.1:0`），对面是本进程里的
//! 另一个节点实例或一个小 Router；客户端是生产的 `HttpsTransport`。

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agora::api::{self, AppState, OverTls};
use agora::auth::{Auth, AuthConfig};
use agora::config::PeerSection;
use agora::peer::transport::{
    HttpsTransport, PeerConfigError, PeerTransport, TransportError, WsMessage, DEFAULT_TIMEOUT,
};
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use agora::tls::{self, Fingerprint, Identity, TlsFiles};
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};

const NOW: i64 = 1_788_652_800; // 2026-09-06T00:00:00Z

/// 0600 的 token 文件（D3 的持有方形态）。
fn write_token(dir: &Path, token: &str) -> PathBuf {
    let p = dir.join("zuan.token");
    std::fs::write(&p, format!("{token}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    p
}

fn section(port: u16, pin: &str, token_file: &Path) -> PeerSection {
    PeerSection {
        name: "zuan".into(),
        url: format!("https://127.0.0.1:{port}"),
        token_file: token_file.to_path_buf(),
        cert_fingerprint: pin.into(),
    }
}

/// 在 `home` 生成（或复用）自签证书，起 TLS 监听器服务 `app`，返回端口、指纹与 acceptor。
async fn tls_server(home: &Path, app: Router) -> (u16, Fingerprint, tls::server::Acceptor) {
    let identity = tls::load_or_generate_self_signed(home, NOW).unwrap();
    let acceptor = tls::server::Acceptor::new(&identity).unwrap();
    let listener = api::bind_tls("127.0.0.1:0".parse().unwrap(), acceptor.clone())
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(api::serve_tls_router(listener, app));
    (port, identity.fingerprint(), acceptor)
}

/// 一个真节点实例（内存库、假运行时、零凭据）的 Router。
fn node_router(name: &str) -> Router {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
    let rt = Arc::new(common::FakeRuntime::default());
    let sessions = Arc::new(SessionManager::new(db, rt as Arc<dyn Runtime>));
    api::router(AppState::new(auth, sessions, name))
}

/// 回显请求头与 TLS 标记的小 Router：验证传输层补了什么、监听器标了什么。
fn echo_router() -> Router {
    async fn echo(headers: HeaderMap, tls: Option<Extension<OverTls>>) -> Json<Value> {
        let h = |n: header::HeaderName| {
            headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        Json(json!({
            "authorization": h(header::AUTHORIZATION),
            "host": h(header::HOST),
            "over_tls": tls.is_some(),
        }))
    }
    async fn ws_echo(ws: WebSocketUpgrade, headers: HeaderMap) -> Response {
        if !headers.contains_key(header::AUTHORIZATION) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        ws.on_upgrade(|mut s| async move {
            while let Some(Ok(m)) = s.recv().await {
                if s.send(m).await.is_err() {
                    break;
                }
            }
        })
    }
    Router::new()
        .route("/echo", get(echo))
        .route("/ws-echo", get(ws_echo))
}

async fn body_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn fingerprint_mismatch_is_refused_not_stale() {
    // ADR-003 D4：指纹不匹配是独立状态，绝不并进"离线 / stale"，也绝不 TOFU 接受。
    // 对端证书合法、握手签名也对，只是公钥不是配置里那把——中间人拿着任何证书都长这样。
    let home = tempfile::tempdir().unwrap();
    let (port, real, _acceptor) = tls_server(home.path(), node_router("zuan")).await;
    let other = tls::load_or_generate_self_signed(tempfile::tempdir().unwrap().path(), NOW)
        .unwrap()
        .fingerprint();
    assert_ne!(other, real);
    let token = write_token(home.path(), "apt_zuan_abc");

    let wrong = HttpsTransport::new(section(port, &other.to_string(), &token), DEFAULT_TIMEOUT);
    let err = wrong
        .request(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap_err();
    match &err {
        TransportError::FingerprintMismatch { expected, actual } => {
            assert_eq!(expected, &other.to_string());
            assert_eq!(
                actual,
                &real.to_string(),
                "错误里带对端真实指纹，人能据此核对"
            );
        }
        other => panic!("应是 FingerprintMismatch，而不是 {other:?}"),
    }
    assert!(
        !matches!(
            err,
            TransportError::Unreachable(_) | TransportError::Timeout(_)
        ),
        "不匹配不能被当成离线"
    );
    // WS 同样在握手阶段拒绝。
    let err = wrong.connect_ws("/api/events").await.unwrap_err();
    assert!(
        matches!(err, TransportError::FingerprintMismatch { .. }),
        "{err:?}"
    );

    // 同一对端、正确的指纹：请求到达对方节点（零 token 的节点对 Bearer 回 401，A31——
    // 这是"到达"而不是"传输失败"，非 2xx 原样交回调用方）。
    let right = HttpsTransport::new(section(port, &real.to_string(), &token), DEFAULT_TIMEOUT);
    let resp = right
        .request(Request::get("/api/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn no_pin_no_connect() {
    // 没有指纹就没有信任锚（无 TOFU）：peers[].cert_fingerprint 为空或不合法时连 TCP 都不拨——
    // 对面是一个只 bind 不 accept 的监听器，任何连接尝试都会留在 backlog 里被 accept() 立刻取到。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let token = write_token(dir.path(), "apt_zuan_abc");

    for pin in ["", "sha256:", "sha256:abc", "md5:00"] {
        let t = HttpsTransport::new(section(port, pin, &token), DEFAULT_TIMEOUT);
        let err = t
            .request(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(err, TransportError::Config(PeerConfigError::Pin(_))),
            "{pin:?}: {err:?}"
        );
        let err = t.connect_ws("/api/events").await.unwrap_err();
        assert!(
            matches!(err, TransportError::Config(PeerConfigError::Pin(_))),
            "{pin:?}: {err:?}"
        );
    }
    // 明文 URL 同理：peer 链路永不走明文（D5）。
    let mut plain = section(port, &format!("sha256:{}", "0".repeat(64)), &token);
    plain.url = format!("http://127.0.0.1:{port}");
    let err = HttpsTransport::new(plain, DEFAULT_TIMEOUT)
        .request(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap_err();
    assert!(
        matches!(err, TransportError::Config(PeerConfigError::Url(_))),
        "{err:?}"
    );

    let arrived = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
    assert!(arrived.is_err(), "没有指纹却有连接到达: {arrived:?}");

    // 对照：指纹格式合法（哪怕值不对）才会去拨号——这一次 backlog 里就有连接了。
    let t = HttpsTransport::new(
        section(port, &format!("sha256:{}", "0".repeat(64)), &token),
        Duration::from_millis(500),
    );
    let _ = t
        .request(Request::get("/api/health").body(Body::empty()).unwrap())
        .await;
    let arrived = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
    assert!(arrived.is_ok(), "合法指纹应当拨号");
}

#[tokio::test]
async fn self_signed_generated_once_and_reused() {
    // ADR-003 D4 self-signed：首次开 TLS 监听器时生成密钥 + 10 年证书到 <AGORA_HOME>/tls/，之后复用。
    // 指纹稳定是 peer 配置成立的前提——每次启动换一把钥匙，peer 的 cert_fingerprint 就永远对不上。
    let home = tempfile::tempdir().unwrap();
    let files = TlsFiles::self_signed(home.path());
    assert!(!files.cert_file.exists() && !files.key_file.exists());

    let first = tls::load_or_generate_self_signed(home.path(), NOW).unwrap();
    assert_eq!(files.cert_file, home.path().join("tls").join("cert.pem"));
    let cert_bytes = std::fs::read(&files.cert_file).unwrap();
    let key_bytes = std::fs::read(&files.key_file).unwrap();
    for p in [&files.cert_file, &files.key_file] {
        assert_eq!(
            std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
            0o600,
            "{}",
            p.display()
        );
    }
    let (nb, na) = first.validity().unwrap();
    assert_eq!(na - nb, 3650 * 86_400, "10 年");

    // 第二次（daemon 重启、agora tls fingerprint）：同一对文件、同一指纹，文件一个字节都没动。
    let second = tls::load_or_generate_self_signed(home.path(), NOW + 86_400 * 30).unwrap();
    assert_eq!(second.fingerprint(), first.fingerprint());
    assert_eq!(std::fs::read(&files.cert_file).unwrap(), cert_bytes);
    assert_eq!(std::fs::read(&files.key_file).unwrap(), key_bytes);
    assert_eq!(
        Identity::from_files(&files).unwrap().fingerprint(),
        first.fingerprint()
    );

    // 用它起监听器、用生产传输按这个指纹连上：证明磁盘上的那对文件就是对外出示的那张证书。
    let (port, served, _acceptor) = tls_server(home.path(), echo_router()).await;
    assert_eq!(served, first.fingerprint());
    let token = write_token(home.path(), "apt_zuan_abc");
    let t = HttpsTransport::new(section(port, &served.to_string(), &token), DEFAULT_TIMEOUT);
    let resp = t
        .request(Request::get("/echo").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 只剩半对（有人手动删了私钥）：当作没有，重新生成一对——指纹会变，日志里的新指纹要发给 peer。
    std::fs::remove_file(&files.key_file).unwrap();
    let third = tls::load_or_generate_self_signed(home.path(), NOW).unwrap();
    assert_ne!(third.fingerprint(), first.fingerprint());
    assert!(files.key_file.exists());
    assert_ne!(std::fs::read(&files.cert_file).unwrap(), cert_bytes);
}

#[tokio::test]
async fn pinned_transport_carries_bearer_and_serves_ws_over_tls() {
    // 生产传输补的东西：Host、Authorization: Bearer <token_file 内容>；监听器标的东西：OverTls
    // （7ku.2 的 Bearer 校验据此分支）。WS 走同一条 TLS 建连路径，Bearer 也在握手请求上。
    let home = tempfile::tempdir().unwrap();
    let (port, pin, _acceptor) = tls_server(home.path(), echo_router()).await;
    let token = write_token(home.path(), "apt_zuan_s3cr3t");
    let t = HttpsTransport::new(section(port, &pin.to_string(), &token), DEFAULT_TIMEOUT);
    assert_eq!(t.name(), "zuan");
    assert_eq!(t.timeout(), DEFAULT_TIMEOUT);

    let resp = t
        .request(Request::get("/echo?x=1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["authorization"], "Bearer apt_zuan_s3cr3t");
    assert_eq!(body["host"], format!("127.0.0.1:{port}"));
    assert_eq!(body["over_tls"], true, "TLS 监听器上的请求带 OverTls 标记");

    let mut ws = t.connect_ws("/ws-echo").await.unwrap();
    ws.send(WsMessage::Text("ping".into())).await.unwrap();
    match ws.next().await.unwrap().unwrap() {
        WsMessage::Text(t) => assert_eq!(t.as_str(), "ping"),
        other => panic!("{other:?}"),
    }
    ws.close(None).await.unwrap();

    // 明文监听器上没有 OverTls：同一个 Router 经 oneshot（等价于明文路径）看不到标记。
    use tower::ServiceExt;
    let resp = echo_router()
        .oneshot(Request::get("/echo").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(body_json(resp).await["over_tls"], false);
}
