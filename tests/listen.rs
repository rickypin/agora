//! ADR-003 D5 的守卫（A34）：明文监听器只在 loopback；TLS 监听器永不降级明文；开了 TLS 监听器
//! 而没有任何凭据只是警告、不是拒绝启动（agora-7ku.10）。

mod common;

use std::sync::Arc;
use std::time::Duration;

use agora::api::{self, plaintext_listen, AppState, ListenError};
use agora::auth::{self, Auth, AuthConfig};
use agora::config::Config;
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use agora::tls::{self, Fingerprint};
use axum::routing::get;
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const NOW: i64 = 1_788_652_800; // 2026-09-06T00:00:00Z

#[test]
fn plaintext_listener_refuses_non_loopback() {
    for bad in [
        "0.0.0.0:7680",
        "192.168.1.2:7680",
        "[::]:7680",
        "100.64.0.1:7680",
    ] {
        assert!(
            matches!(plaintext_listen(bad), Err(ListenError::NotLoopback(_))),
            "{bad} 应被拒绝"
        );
    }
    assert!(matches!(
        plaintext_listen("not an address"),
        Err(ListenError::Parse(_))
    ));
    for ok in ["127.0.0.1:7680", "127.0.0.2:1", "[::1]:7680"] {
        assert!(plaintext_listen(ok).is_ok(), "{ok} 是 loopback");
    }
}

/// 起一个 TLS 监听器（自签证书在临时目录），返回 `(端口, 指纹)`。
async fn tls_server(app: Router) -> (u16, Fingerprint) {
    let dir = tempfile::tempdir().unwrap();
    let identity = tls::load_or_generate_self_signed(dir.path(), NOW).unwrap();
    let acceptor = tls::server::Acceptor::new(&identity).unwrap();
    let listener = api::bind_tls("127.0.0.1:0".parse().unwrap(), acceptor)
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(api::serve_tls_router(listener, app));
    (port, identity.fingerprint())
}

/// 一次 HTTP/1.1 请求走 TLS（钉住 `pin`），返回原始应答文本。
async fn https_get(port: u16, pin: Fingerprint, path: &str, extra_headers: &str) -> String {
    let connector = tls::client::connector(pin).unwrap();
    let mut io = tls::client::connect(&connector, "127.0.0.1", port)
        .await
        .unwrap();
    io.write_all(
        format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{extra_headers}\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), io.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn tls_listener_never_serves_plaintext() {
    // A34：TLS 监听器上敲明文 HTTP，拿不到任何 HTTP 应答——连接被握手拒掉（EOF 或 TLS alert）。
    // 关掉这条守卫的做法是给 accept 循环加"握手失败按明文继续"的分支；这里对着同一端口两种客户端
    // 各打一次，明文那次绝不能看到 "HTTP/"。
    let app = Router::new().route("/api/health", get(|| async { "served" }));
    let (port, pin) = tls_server(app).await;

    let mut plain = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    plain
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), plain.read_to_end(&mut buf)).await;
    assert!(
        !buf.starts_with(b"HTTP/"),
        "TLS 监听器回了明文 HTTP: {:?}",
        String::from_utf8_lossy(&buf)
    );
    assert!(
        buf.is_empty() || buf[0] == 0x15,
        "明文客户端只该看到 EOF 或 TLS alert 记录，得到 {:02x?}",
        &buf[..buf.len().min(8)]
    );

    // 同一端口，正确的 TLS + 正确的指纹：正常服务。
    let resp = https_get(port, pin, "/api/health", "").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.ends_with("served"), "{resp}");
}

#[tokio::test]
async fn tls_listen_without_credentials_warns_not_refuses() {
    // ① 配置层：开 tls_listen 而节点上没有任何凭据，不是配置错误。
    let settings = Config::parse("server:\n  tls_listen: \"0.0.0.0:7695\"\n")
        .unwrap()
        .validate("rt")
        .unwrap();
    assert_eq!(settings.tls_listen.unwrap().to_string(), "0.0.0.0:7695");

    // ② 零凭据是一个**警告**值；返回类型里没有"拒绝"这一档（ADR-003 D5 把 v0.8 的拒绝启动降为警告）。
    let warning = tls::zero_credentials_warning(0, 0).expect("零凭据要警告");
    assert!(warning.to_string().contains("agora pair"), "{warning}");
    assert!(tls::zero_credentials_warning(1, 0).is_none());
    assert!(tls::zero_credentials_warning(0, 1).is_none());

    // ③ 监听器照样起来、照样服务：没有任何已配对设备与机器 token 的节点，TLS 端口上是 401 而不是连不上
    //    ——D1 没有直通路由，所以"监听在网络上但没配凭据"只是没用，不是漏洞（M2a 剧本第 2 步的
    //    "未配对设备直连 TLS 端口 → 401"）。
    let db = Arc::new(Db::open_in_memory().unwrap());
    let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
    assert!(auth.list_devices().unwrap().is_empty());
    let rt = Arc::new(common::FakeRuntime::default());
    let sessions = Arc::new(SessionManager::new(db, rt as Arc<dyn Runtime>));
    let state = AppState::new(auth, sessions, "n");
    let (port, pin) = tls_server(api::router(state)).await;

    let resp = https_get(port, pin, "/api/sessions", "").await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(resp.contains("\"unauthenticated\""), "{resp}");
    // 公开子集照常，只有 status 一个键（ADR-003 D1）。
    let resp = https_get(port, pin, "/api/health", "").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.ends_with(r#"{"status":"ok"}"#), "{resp}");
    // 带 Bearer 的请求在这个零 token 的节点上也是 401（A31：未签发 token 拒绝一切 Bearer），不是 5xx、不是放行。
    let resp = https_get(
        port,
        pin,
        "/api/sessions",
        "Authorization: Bearer apt_nobody_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
}

#[tokio::test]
async fn tls_listen_with_only_a_machine_token_does_not_warn() {
    // agora-y9v：只签了机器 token、还没配对过设备的节点（被 peer 访问的典型初始状态）开 tls_listen
    // 不该被说成"没有人能连进来"。main.rs 把未吊销的 token 数喂给 zero_credentials_warning——
    // 以前那里写死 0；这条守卫钉住计数的来源（peer_token::list）与过滤规则（revoked_at 为空才算）：
    // 吊销掉唯一的 token 之后又该警告。
    let db = Db::open_in_memory().unwrap();
    assert!(auth::peer_token::list(&db).unwrap().is_empty());
    let active = |db: &Db| {
        auth::peer_token::list(db)
            .unwrap()
            .iter()
            .filter(|t| t.revoked_at.is_none())
            .count()
    };
    assert!(tls::zero_credentials_warning(0, active(&db)).is_some());

    auth::peer_token::create(&db, "mac", false).unwrap();
    assert_eq!(active(&db), 1);
    assert!(
        tls::zero_credentials_warning(0, active(&db)).is_none(),
        "只有 token、没有设备也不该警告"
    );

    auth::peer_token::revoke(&db, "mac").unwrap();
    assert_eq!(active(&db), 0, "已吊销的不算");
    assert!(tls::zero_credentials_warning(0, active(&db)).is_some());
}
