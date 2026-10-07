//! Web Push 服务端（agora-thc.6）：RFC 8291 向量、VAPID、订阅生命周期与投递。
//!
//! 假端点在本地起一条真的 HTTPS（rcgen 自签 + 客户端注根证书），走的加密与校验路径和线上
//! 一致——不是拿明文 socket 冒充满足验收。

mod common;

use std::sync::Arc;

use agora::push::crypto::{b64decode, b64encode, decrypt, encrypt_with};
use agora::push::vapid::Vapid;
use agora::session::Db;

const APPENDIX_A_PLAINTEXT: &[u8] = b"When I grow up, I want to be a watermelon";
const APPENDIX_A_UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const APPENDIX_A_UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const APPENDIX_A_AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const APPENDIX_A_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const APPENDIX_A_SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const APPENDIX_A_BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

fn fixed<const N: usize>(b64: &str) -> [u8; N] {
    b64decode(b64).unwrap().try_into().unwrap()
}

/// RFC 8291 附录 A 的向量：加密逐字节等于 RFC 的 body，解密 RFC 的 body 得到原句。
/// 这是"我们实现的是规范里的那个算法"唯一不靠自说自话的证据。
#[test]
fn rfc8291_appendix_a_vector() {
    let as_private = fixed::<32>(APPENDIX_A_AS_PRIVATE);
    let salt = fixed::<16>(APPENDIX_A_SALT);
    let ua_public = b64decode(APPENDIX_A_UA_PUBLIC).unwrap();
    let auth = b64decode(APPENDIX_A_AUTH).unwrap();

    let body = encrypt_with(&as_private, &salt, &ua_public, &auth, APPENDIX_A_PLAINTEXT).unwrap();
    assert_eq!(
        b64encode(&body),
        APPENDIX_A_BODY,
        "加密结果必须逐字节等于 RFC 8291 §5 的 body"
    );

    let body = b64decode(APPENDIX_A_BODY).unwrap();
    let ua_private = fixed::<32>(APPENDIX_A_UA_PRIVATE);
    assert_eq!(
        decrypt(&body, &ua_private, &auth).unwrap(),
        APPENDIX_A_PLAINTEXT,
        "解密 RFC 的 body 必须得到 watermelon 那句"
    );
}

/// VAPID：自己签的 JWT 能被自己的公钥验过（ES256），公钥形态是可交给浏览器订阅的 65 字节
/// 未压缩点。
#[test]
fn vapid_jwt_verifies_against_its_own_public_key() {
    let v = Vapid::generate();
    assert_eq!(
        b64decode(v.public_key()).unwrap().len(),
        65,
        "VAPID 公钥是未压缩 P-256 点"
    );
    let jwt = v.jwt(
        "https://web.push.apple.com",
        "mailto:agora@localhost",
        1_700_000_000,
    );
    assert!(v.verify(&jwt));
    // 负载与签名任一被改动都要验不过。
    let parts: Vec<&str> = jwt.split('.').collect();
    let mut claims: serde_json::Value =
        serde_json::from_slice(&b64decode(parts[1]).unwrap()).unwrap();
    claims["aud"] = serde_json::json!("https://evil.example");
    let tampered = format!(
        "{}.{}.{}",
        parts[0],
        b64encode(&serde_json::to_vec(&claims).unwrap()),
        parts[2]
    );
    assert!(!v.verify(&tampered));
}

/// 订阅按设备增删；吊销设备即停发（发送侧联查）。
#[test]
fn subscriptions_are_per_device_and_revocation_stops_delivery() {
    use agora::auth::{Auth, AuthConfig, PairedVia};
    use agora::push::PushStore;
    use rusqlite::params;

    let db = Arc::new(Db::open_in_memory().unwrap());
    let auth = Auth::new(db.clone(), AuthConfig::default());
    let (device, _) = auth
        .redeem(
            &auth.mint_pair_token(PairedVia::Socket).unwrap(),
            Some("iPhone"),
            None,
        )
        .unwrap();
    let store = PushStore::new(db.clone());
    assert!(!store
        .upsert(&device.id, "https://web.push.apple.com/A", "k", "a")
        .unwrap());
    assert_eq!(store.list_active().unwrap().len(), 1);
    assert!(store.remove("https://web.push.apple.com/A").unwrap());
    assert!(store.list_active().unwrap().is_empty());

    store
        .upsert(&device.id, "https://web.push.apple.com/B", "k", "a")
        .unwrap();
    db.conn()
        .execute(
            "UPDATE devices SET revoked_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1",
            params![device.id],
        )
        .unwrap();
    assert!(
        store.list_active().unwrap().is_empty(),
        "吊销设备后发送侧看不到它的订阅"
    );
}

// ---------- 假端点：真的 HTTPS + 客户端注根证书 ----------

mod fake {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicU16, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use axum::http::Request;
    use http_body_util::{BodyExt, Full};
    use hyper::body::{Bytes, Incoming};
    use hyper_util::rt::TokioIo;

    pub struct Hit {
        pub content_encoding: String,
        pub authorization: String,
        pub body: Vec<u8>,
    }

    pub struct FakeEndpoint {
        pub url: String,
        pub hits: Arc<Mutex<Vec<Hit>>>,
        pub status: Arc<AtomicU16>,
        pub ca: rustls::pki_types::CertificateDer<'static>,
    }

    impl FakeEndpoint {
        /// 起一条本地 HTTPS：rcgen 自签（SAN 127.0.0.1）+ tokio-rustls + hyper http1。
        pub async fn start() -> Self {
            let key = rcgen::KeyPair::generate().unwrap();
            let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
            let cert = params.self_signed(&key).unwrap();
            let ca = cert.der().clone();
            let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![ca.clone()], key_der)
                .unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let hits = Arc::new(Mutex::new(Vec::new()));
            let status = Arc::new(AtomicU16::new(201));
            {
                let hits = hits.clone();
                let status = status.clone();
                tokio::spawn(async move {
                    while let Ok((stream, _)) = listener.accept().await {
                        let acceptor = acceptor.clone();
                        let hits = hits.clone();
                        let status = status.clone();
                        tokio::spawn(async move {
                            let Ok(tls) = acceptor.accept(stream).await else {
                                return;
                            };
                            let service =
                                hyper::service::service_fn(move |req: Request<Incoming>| {
                                    let hits = hits.clone();
                                    let status = status.clone();
                                    async move {
                                        let (parts, body) = req.into_parts();
                                        let body =
                                            body.collect().await.unwrap().to_bytes().to_vec();
                                        let header = |name: &str| {
                                            parts
                                                .headers
                                                .get(name)
                                                .and_then(|v| v.to_str().ok())
                                                .unwrap_or("")
                                                .to_owned()
                                        };
                                        hits.lock().unwrap().push(Hit {
                                            content_encoding: header("content-encoding"),
                                            authorization: header("authorization"),
                                            body,
                                        });
                                        let code = status.load(Ordering::SeqCst);
                                        Ok::<_, Infallible>(
                                            axum::response::Response::builder()
                                                .status(code)
                                                .body(Full::new(Bytes::new()))
                                                .unwrap(),
                                        )
                                    }
                                });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(tls), service)
                                .await;
                        });
                    }
                });
            }
            FakeEndpoint {
                url: format!("https://127.0.0.1:{port}/push/token-abc"),
                hits,
                status,
                ca,
            }
        }

        pub async fn wait_for_hits(&self, n: usize) -> Vec<Hit> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if self.hits.lock().unwrap().len() >= n {
                    return self.hits.lock().unwrap().drain(..).collect();
                }
                assert!(Instant::now() < deadline, "5 秒内没等到 {n} 条投递");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    /// 客户端连接器：只信这张测试 CA（正是线上"只信 webpki-roots"的同一段代码路径）。
    pub fn connector(
        ca: &rustls::pki_types::CertificateDer<'static>,
    ) -> hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.clone()).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http1()
            .build()
    }
}

use std::time::Duration;

use agora::events::{notification_for, Transition};
use agora::push::{PushHealth, PushSender, PushStore, SenderConfig};
use agora::status::Status;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::SecretKey;

/// 浏览器侧的一对订阅密钥：P-256 的 p256dh 与 16 字节 auth。
fn browser_keys() -> (String, [u8; 32], String, [u8; 16]) {
    let sk = loop {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).unwrap();
        if let Ok(sk) = SecretKey::from_slice(&buf) {
            break sk;
        }
    };
    let public = sk.public_key().to_encoded_point(false);
    let mut private = [0u8; 32];
    private.copy_from_slice(&sk.to_bytes());
    let mut auth = [0u8; 16];
    getrandom::fill(&mut auth).unwrap();
    (
        b64encode(public.as_bytes()),
        private,
        b64encode(&auth),
        auth,
    )
}

fn human_device(db: &Arc<Db>) -> (String, agora::auth::Auth) {
    use agora::auth::{Auth, AuthConfig, PairedVia};
    let auth = Auth::new(db.clone(), AuthConfig::default());
    let (device, _) = auth
        .redeem(
            &auth.mint_pair_token(PairedVia::Socket).unwrap(),
            Some("iPhone"),
            None,
        )
        .unwrap();
    (device.id, auth)
}

fn test_sender(
    db: &Arc<Db>,
    health: PushHealth,
    connector: hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
) -> (PushSender, Vapid) {
    let vapid = Vapid::generate();
    let sender = PushSender::with_client(
        PushStore::new(db.clone()),
        health,
        vapid.clone(),
        SenderConfig {
            request_timeout: Duration::from_secs(5),
            subject: "mailto:test@example.invalid".into(),
            ..Default::default()
        },
        connector,
    );
    (sender, vapid)
}

/// WAITING 转换（带命令原文的 detail）经事件总线到假端点：密文能解出标题与会话 id、
/// 解不出命令原文（payload 只有两字段），VAPID 头能被自己公钥验签，health 报可达。
#[tokio::test]
async fn waiting_transition_reaches_the_subscriber_encrypted_without_the_detail() {
    let fake = fake::FakeEndpoint::start().await;
    let db = Arc::new(Db::open_in_memory().unwrap());
    let (device, _auth) = human_device(&db);
    let (p256dh, ua_private, auth_b64, auth) = browser_keys();
    PushStore::new(db.clone())
        .upsert(&device, &fake.url, &p256dh, &auth_b64)
        .unwrap();

    let bus = agora::events::EventBus::default();
    let health = PushHealth::new();
    let (sender, vapid) = test_sender(&db, health.clone(), fake::connector(&fake.ca));
    let sender = Arc::new(sender);
    let rx = bus.subscribe();
    tokio::spawn(sender.clone().run(rx));

    // 真的走状态转换那条路：RUNNING → WAITING，detail 是待放行的命令原文。
    let event = notification_for(Transition {
        id: "zuan:abc",
        agent_type: "claude",
        name: "修 CI",
        node: "zuan",
        origin: agora::session::Origin::Agora,
        prev: Status::Running,
        next: Status::Waiting,
        end_cause: None,
        detail: Some("Bash: git push origin main"),
    })
    .expect("RUNNING → WAITING 该发通知");
    bus.publish(event);

    let hits = fake.wait_for_hits(1).await;
    let hit = &hits[0];
    assert_eq!(hit.content_encoding, "aes128gcm", "RFC 8188 的内容编码");
    let authz = hit
        .authorization
        .strip_prefix("vapid t=")
        .expect("Authorization 是 vapid 方案");
    let (jwt, key) = authz.split_once(", k=").expect("vapid 头带 k=");
    assert_eq!(key, vapid.public_key(), "k 就是 /api/system 报的公钥");
    assert!(vapid.verify(jwt), "JWT 能被自己公钥验签");

    let payload = decrypt(&hit.body, &ua_private, &auth).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(v["session"], "zuan:abc", "载荷带全局会话 id");
    let title = v["title"].as_str().unwrap();
    assert!(title.contains("needs input"), "{title}");
    assert!(
        !String::from_utf8_lossy(&payload).contains("git push"),
        "权限命令的正文不进推送载荷（锁屏不显示）：{payload:?}"
    );
    assert_eq!(health.snapshot()["apple"], true, "拿到 2xx 就是可达");
}

/// 410 Gone：端点已经作废，订阅删掉、不再重试。
#[tokio::test]
async fn a_gone_endpoint_is_deleted() {
    let fake = fake::FakeEndpoint::start().await;
    fake.status.store(410, std::sync::atomic::Ordering::SeqCst);
    let db = Arc::new(Db::open_in_memory().unwrap());
    let (device, _auth) = human_device(&db);
    let (p256dh, _ua, auth_b64, _auth) = browser_keys();
    let store = PushStore::new(db.clone());
    store
        .upsert(&device, &fake.url, &p256dh, &auth_b64)
        .unwrap();

    let (sender, _vapid) = test_sender(&db, PushHealth::new(), fake::connector(&fake.ca));
    sender
        .notify("Claude / x @ zuan needs input", "zuan:abc")
        .await;
    fake.wait_for_hits(1).await;
    assert!(
        store.list_active().unwrap().is_empty(),
        "410 之后订阅必须被删（浏览器卸载 PWA 就是这个形状）"
    );
}

/// 连不上推送服务：health.push.apple = false 且带原因；订阅保留（可能只是暂时断网）。
#[tokio::test]
async fn an_unreachable_service_reports_health_false_with_a_reason() {
    // 拿到端口就关掉：连过去必然拒绝。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let db = Arc::new(Db::open_in_memory().unwrap());
    let (device, _auth) = human_device(&db);
    let (p256dh, _ua, auth_b64, _auth) = browser_keys();
    let store = PushStore::new(db.clone());
    // 端点形状不合法也没关系：这里测的是投递失败，API 那道校验在下面另一个测试里。
    let endpoint = format!("https://127.0.0.1:{port}/push/x");
    store
        .upsert(&device, &endpoint, &p256dh, &auth_b64)
        .unwrap();

    let fake = fake::FakeEndpoint::start().await; // 只为拿一张 CA 建连接器
    let health = PushHealth::new();
    let (sender, _vapid) = test_sender(&db, health.clone(), fake::connector(&fake.ca));
    sender
        .notify("Claude / x @ zuan needs input", "zuan:abc")
        .await;
    let snap = health.snapshot();
    assert_eq!(snap["apple"], false, "连不上必须报 false");
    assert!(
        snap["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "{snap}"
    );
    assert_eq!(store.list_active().unwrap().len(), 1, "暂时断网不删订阅");
}

/// peer 会话的通知：peer 视图把事件转发进本机总线，承载节点的 PushSender 直接发——不经 peer 中转。
#[tokio::test]
async fn peer_notifications_are_pushed_by_the_host_without_peer_relay() {
    let fake = fake::FakeEndpoint::start().await;
    let db = Arc::new(Db::open_in_memory().unwrap());
    let (device, _auth) = human_device(&db);
    let (p256dh, ua_private, auth_b64, auth) = browser_keys();
    PushStore::new(db.clone())
        .upsert(&device, &fake.url, &p256dh, &auth_b64)
        .unwrap();

    let bus = agora::events::EventBus::default();
    let (sender, _vapid) = test_sender(&db, PushHealth::new(), fake::connector(&fake.ca));
    let sender = Arc::new(sender);
    let rx = bus.subscribe();
    tokio::spawn(sender.clone().run(rx));

    // 与 peer 客户端同一条路：视图认出这是 mac 的会话，转发成本机 Event::Notification。
    let views = agora::peer::view::PeerViews::new();
    views.replace(
        "mac",
        vec![serde_json::json!({ "id": "mac:1", "node": "mac", "display_name": "跑测试" })],
        None,
    );
    let applied = views.apply(
        "mac",
        &serde_json::json!({
            "type": "notification",
            "id": "mac:1",
            "title": "Claude / 跑测试 @ mac needs input",
            "body": "Bash: rm -rf /tmp/x",
            "status": "waiting"
        }),
    );
    let agora::peer::view::Applied::Publish(event) = applied else {
        panic!("peer 的 notification 该转发，得到 {applied:?}");
    };
    bus.publish(*event);

    let hits = fake.wait_for_hits(1).await;
    let payload = decrypt(&hits[0].body, &ua_private, &auth).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(v["session"], "mac:1", "承载节点直接为 peer 会话发推送");
    assert!(
        !String::from_utf8_lossy(&payload).contains("rm -rf"),
        "{payload:?}"
    );
}

// ---------- HTTP 面：/api/push/subscriptions 与 /api/system、/api/health ----------

mod api_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const HOST: &str = "127.0.0.1:7680";

    fn json_request(
        method: &str,
        path: &str,
        cookie: &str,
        body: serde_json::Value,
    ) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 登记 / 更新 / 删除订阅；端点与密钥的形状校验都在 API 一侧。
    #[tokio::test]
    async fn api_registers_updates_and_deletes_subscriptions_for_the_device() {
        let fx = common::Fx::new();
        let cookie = fx.cookie();
        let device = fx.auth.list_devices().unwrap().remove(0);
        let (p256dh, _ua, auth_b64, _auth) = browser_keys();
        let body = serde_json::json!({
            "endpoint": "https://web.push.apple.com/QF-test-1",
            "keys": { "p256dh": p256dh, "auth": auth_b64 }
        });

        let resp = fx
            .app()
            .oneshot(json_request(
                "POST",
                "/api/push/subscriptions",
                &cookie,
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp = fx
            .app()
            .oneshot(json_request(
                "POST",
                "/api/push/subscriptions",
                &cookie,
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "同一端点重订是更新");

        let rows = fx.state.push_store.list_active().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].device_id, device.id, "订阅属于设备");

        // 端点不是 Apple 的：拒绝，不让浏览器拿节点当 SSRF 跳板。
        let evil = serde_json::json!({
            "endpoint": "https://evil.example/push/1",
            "keys": { "p256dh": p256dh, "auth": auth_b64 }
        });
        let resp = fx
            .app()
            .oneshot(json_request(
                "POST",
                "/api/push/subscriptions",
                &cookie,
                evil,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // 密钥长度不对：400，而不是收下之后每条通知都重试一遍坏订阅。
        let bad_key = serde_json::json!({
            "endpoint": "https://web.push.apple.com/QF-test-2",
            "keys": { "p256dh": "AAAA", "auth": auth_b64 }
        });
        let resp = fx
            .app()
            .oneshot(json_request(
                "POST",
                "/api/push/subscriptions",
                &cookie,
                bad_key,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let del = serde_json::json!({ "endpoint": "https://web.push.apple.com/QF-test-1" });
        for _ in 0..2 {
            let resp = fx
                .app()
                .oneshot(json_request(
                    "DELETE",
                    "/api/push/subscriptions",
                    &cookie,
                    del.clone(),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NO_CONTENT, "删除幂等");
        }
        assert!(fx.state.push_store.list_active().unwrap().is_empty());
    }

    /// `/api/system` 暴露 VAPID 公钥（没启用就是 null）；`/api/health` 的 push 段是实时三态。
    #[tokio::test]
    async fn system_exposes_the_vapid_key_and_health_reports_push_state() {
        fn get(path: &'static str, cookie: &str) -> Request<Body> {
            Request::get(path)
                .header(header::HOST, HOST)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap()
        }
        async fn call(fx: &common::Fx, path: &'static str, cookie: &str) -> serde_json::Value {
            let resp = fx.app().oneshot(get(path, cookie)).await.unwrap();
            body_json(resp).await
        }

        let mut fx = common::Fx::new();
        let cookie = fx.cookie();

        let system = call(&fx, "/api/system", &cookie).await;
        assert_eq!(
            system["push"]["vapid_public_key"],
            serde_json::Value::Null,
            "还没启用推送的节点要报 null，不是编一把密钥"
        );
        let health = call(&fx, "/api/health", &cookie).await;
        assert_eq!(health["push"]["apple"], serde_json::Value::Null);
        assert_eq!(health["push"]["fcm"], serde_json::Value::Null);

        let vapid = Vapid::generate();
        fx.state.vapid = Some(Arc::new(vapid.clone()));
        fx.state
            .push_health
            .set_unreachable("连不上 web.push.apple.com");
        let system = call(&fx, "/api/system", &cookie).await;
        assert_eq!(system["push"]["vapid_public_key"], vapid.public_key());
        let health = call(&fx, "/api/health", &cookie).await;
        assert_eq!(health["push"]["apple"], false);
        assert_eq!(health["push"]["reason"], "连不上 web.push.apple.com");
    }
}
