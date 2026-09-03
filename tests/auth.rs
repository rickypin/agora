//! ADR-003 "什么会让它变危险"里点名的守卫：设备配对 + session（agora-xqa.5）。

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use agora::api::{self, AppState, PUBLIC_ROUTES, ROUTES};
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:7680";

struct Fx {
    auth: Arc<Auth>,
    db: Arc<Db>,
}

impl Fx {
    fn new() -> Self {
        Self::with(AuthConfig::default())
    }

    fn with(cfg: AuthConfig) -> Self {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let auth = Arc::new(Auth::new(db.clone(), cfg));
        Fx { auth, db }
    }

    fn app(&self) -> Router {
        let rt = Arc::new(common::FakeRuntime::default());
        let sessions = Arc::new(SessionManager::new(self.db.clone(), rt as Arc<dyn Runtime>));
        api::router(AppState::new(self.auth.clone(), sessions, common::NODE))
    }

    /// 经"socket"铸造再兑换，返回 cookie 值。
    async fn pair(&self) -> String {
        let token = self.auth.mint_pair_token(PairedVia::Socket).unwrap();
        let resp = self.pair_with(&token).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let set = resp.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(
            set.contains("HttpOnly") && set.contains("SameSite=Lax"),
            "{set}"
        );
        assert!(!set.contains("Secure"), "明文监听器不加 Secure");
        assert!(
            set.contains(&format!(
                "Max-Age={}",
                self.auth.config().session_idle.as_secs()
            )),
            "配对 cookie 必须带 Max-Age，否则浏览器一关就丢：{set}"
        );
        set.split(';').next().unwrap().to_owned()
    }

    async fn pair_with(&self, token: &str) -> axum::response::Response {
        self.app()
            .oneshot(
                Request::post("/api/auth/pair")
                    .header(header::HOST, HOST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::USER_AGENT,
                        "Mozilla/5.0 (Macintosh) Chrome/1 Safari/1",
                    )
                    .body(Body::from(format!("{{\"token\":\"{token}\"}}")))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn get_devices(&self, cookie: &str) -> StatusCode {
        self.get_devices_resp(cookie).await.status()
    }

    async fn get_devices_resp(&self, cookie: &str) -> axum::response::Response {
        self.app()
            .oneshot(
                Request::get("/api/auth/devices")
                    .header(header::HOST, HOST)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// 把 last_seen_at 往前拨，模拟"上次用是多久以前"。
    fn set_idle(&self, secs: u64) {
        self.db
            .conn()
            .execute(
                &format!(
                    "UPDATE devices SET last_seen_at =
                     strftime('%Y-%m-%dT%H:%M:%SZ','now','-{secs} seconds')"
                ),
                [],
            )
            .unwrap();
    }
}

async fn json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// `ROUTES` 是上面那条测试遍历的对象：router 里新加一条路由而忘了登记，它就照样绿，
/// 新路由的认证从此没人敲（agora-gwm）。这里从 `router()` 的源码机械派生出路由清单，
/// 与常量表双向比对——两边不一致就红，方向和原因都写在断言里。
///
/// 派生靠文本扫描而不是 axum 的内省：axum 0.8 不公开路由表。扫的是 `router()` 函数体里
/// 的 `.route("<path>", get(..).post(..))`，方法名从第二个参数里取。
#[test]
fn routes_table_is_derived_from_the_router_not_maintained_by_hand() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api/mod.rs"),
    )
    .unwrap();
    let derived = parse_routes(&router_body(&src));
    assert!(
        derived.len() >= 5,
        "解析器没认出路由，先修解析器再看这条断言: {derived:?}"
    );

    let mut declared: Vec<(String, String)> = ROUTES
        .iter()
        .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
        .collect();
    declared.sort();
    let mut derived: Vec<(String, String)> = derived.into_iter().collect();
    derived.sort();

    let missing: Vec<_> = derived.iter().filter(|r| !declared.contains(r)).collect();
    assert!(
        missing.is_empty(),
        "router() 里有而 ROUTES 没登记（这些路由的认证没人敲）: {missing:?}"
    );
    let stale: Vec<_> = declared.iter().filter(|r| !derived.contains(r)).collect();
    assert!(
        stale.is_empty(),
        "ROUTES 里有而 router() 没注册（表过期了）: {stale:?}"
    );
}

/// `pub fn router(` 的函数体（大括号配对）。
fn router_body(src: &str) -> String {
    let start = src
        .find("pub fn router(")
        .expect("router() 在 src/api/mod.rs 里");
    let open = src[start..].find('{').unwrap() + start;
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return src[open..open + i].to_owned();
                }
            }
            _ => {}
        }
    }
    panic!("router() 的大括号没配上");
}

/// 从函数体里抽出 `(METHOD, path)`。
fn parse_routes(body: &str) -> Vec<(String, String)> {
    const METHODS: &[&str] = &["get", "post", "patch", "delete", "put", "head", "options"];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(".route(") {
        let args = balanced(&rest[at + ".route(".len() - 1..]);
        rest = &rest[at + ".route(".len()..];
        let Some(path) = string_literal(&args) else {
            continue;
        };
        // 第一个字符串字面量之后才是 method router，避免把路径里的字符当方法名。
        let after = &args[args.find(&format!("\"{path}\"")).unwrap() + path.len() + 2..];
        for m in METHODS {
            let needle = format!("{m}(");
            let mut from = 0;
            while let Some(i) = after[from..].find(&needle) {
                let abs = from + i;
                let prev = after[..abs].chars().last().unwrap_or(' ');
                // `sessions::get(` 这种不是方法路由；方法名前面只会是 `(` 或 `.`。
                if !prev.is_alphanumeric() && prev != '_' && prev != ':' {
                    out.push((m.to_uppercase(), path.clone()));
                    break;
                }
                from = abs + needle.len();
            }
        }
    }
    out
}

/// 从 `(` 开始取到配对的 `)`（含两端）。
fn balanced(s: &str) -> String {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return s[..=i].to_owned();
                }
            }
            _ => {}
        }
    }
    s.to_owned()
}

fn string_literal(s: &str) -> Option<String> {
    let a = s.find('"')? + 1;
    let b = s[a..].find('"')? + a;
    Some(s[a..b].to_owned())
}

#[tokio::test]
async fn every_route_requires_principal_except_allowlist() {
    let fx = Fx::new();
    for (method, path) in ROUTES {
        let concrete = path.replace("{id}", "someid");
        let resp = fx
            .app()
            .oneshot(
                Request::builder()
                    .method(*method)
                    .uri(&concrete)
                    .header(header::HOST, HOST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let public = PUBLIC_ROUTES.contains(&(*method, *path));
        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "{method} {path} 没注册进 router"
        );
        assert_ne!(
            resp.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} 方法不对"
        );
        if public {
            assert_ne!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} 在白名单里"
            );
        } else {
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} 无凭据必须 401"
            );
            assert_eq!(json(resp).await["error"], "unauthenticated");
        }
    }
    // WS 升级端点：无凭据同样 401（Principal 先于 WebSocketUpgrade 提取）。
    assert!(ROUTES.contains(&("GET", "/api/events")));
    assert_eq!(
        PUBLIC_ROUTES.len(),
        2,
        "白名单只有 health 公开子集与 pair；扩大先改 ADR-003 D1"
    );
}

#[tokio::test]
async fn loopback_requires_session() {
    // 代码里没有 loopback 例外：来自 127.0.0.1 的无凭据请求照样 401。
    let fx = Fx::new();
    let addr: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    let mut req = Request::get("/api/auth/devices")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    let resp = fx.app().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pair_token_single_use_and_expires() {
    let fx = Fx::new();
    let token = fx.auth.mint_pair_token(PairedVia::Socket).unwrap();
    assert_eq!(fx.pair_with(&token).await.status(), StatusCode::OK);
    let again = fx.pair_with(&token).await;
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED, "单次使用");
    assert_eq!(json(again).await["error"], "pair_invalid");
    assert_eq!(
        fx.pair_with("not-a-token").await.status(),
        StatusCode::UNAUTHORIZED
    );

    let short = Fx::with(AuthConfig {
        pair_ttl: Duration::from_millis(20),
        ..Default::default()
    });
    let token = short.auth.mint_pair_token(PairedVia::Socket).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        short.pair_with(&token).await.status(),
        StatusCode::UNAUTHORIZED,
        "过期"
    );
    assert_eq!(short.auth.pending_count(), 0);
}

#[tokio::test]
async fn pair_token_minted_only_via_socket_or_session() {
    let fx = Fx::new();
    // 未认证：铸造端点 401，且 ROUTES 里没有第二个未认证的铸造路径。
    let resp = fx
        .app()
        .oneshot(
            Request::post("/api/auth/pair/new")
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(fx.auth.pending_count(), 0);

    // 已认证 session 可以铸造，链接指向同一 Host、token 在 fragment 里。
    let cookie = fx.pair().await;
    let resp = fx
        .app()
        .oneshot(
            Request::post("/api/auth/pair/new")
                .header(header::HOST, HOST)
                .header(header::ORIGIN, format!("http://{HOST}"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let url = json(resp).await["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&format!("http://{HOST}/#pair=")), "{url}");
    let token = url.rsplit("#pair=").next().unwrap().to_owned();
    assert_eq!(fx.pair_with(&token).await.status(), StatusCode::OK);
    let devices = fx.auth.list_devices().unwrap();
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().any(|d| d.paired_via == "session"));
    assert!(devices.iter().any(|d| d.paired_via == "socket"));
}

#[tokio::test]
async fn pending_pair_tokens_capped() {
    let fx = Fx::new();
    let tokens: Vec<String> = (0..4)
        .map(|_| fx.auth.mint_pair_token(PairedVia::Socket).unwrap())
        .collect();
    assert!(
        fx.auth.mint_pair_token(PairedVia::Socket).is_err(),
        "第 5 条被拒"
    );
    assert_eq!(fx.pair_with(&tokens[0]).await.status(), StatusCode::OK);
    assert!(
        fx.auth.mint_pair_token(PairedVia::Socket).is_ok(),
        "用掉一条后腾出位置"
    );
}

#[tokio::test]
async fn session_idle_and_absolute_expiry() {
    let fx = Fx::new();
    let cookie = fx.pair().await;
    assert_eq!(fx.get_devices(&cookie).await, StatusCode::OK);

    // 距最近使用 31 天 → 401。
    fx.db
        .conn()
        .execute(
            "UPDATE devices SET last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ','now','-31 days')",
            [],
        )
        .unwrap();
    assert_eq!(fx.get_devices(&cookie).await, StatusCode::UNAUTHORIZED);

    // 最近用过，但距配对 366 天 → 401（绝对上限）。
    fx.db
        .conn()
        .execute(
            "UPDATE devices SET last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ','now'),
                created_at = strftime('%Y-%m-%dT%H:%M:%SZ','now','-366 days')",
            [],
        )
        .unwrap();
    assert_eq!(fx.get_devices(&cookie).await, StatusCode::UNAUTHORIZED);

    // 29 天没用、配对 300 天：仍有效，且这次访问把 last_seen_at 刷到现在（每小时至多一次）。
    fx.db
        .conn()
        .execute(
            "UPDATE devices SET last_seen_at = strftime('%Y-%m-%dT%H:%M:%SZ','now','-29 days'),
                created_at = strftime('%Y-%m-%dT%H:%M:%SZ','now','-300 days')",
            [],
        )
        .unwrap();
    assert_eq!(fx.get_devices(&cookie).await, StatusCode::OK);
    let seen = fx.auth.list_devices().unwrap()[0].last_seen_at.clone();
    assert!(agora::clock::age_secs(&seen).unwrap() < 60, "{seen}");
}

#[tokio::test]
async fn revoked_device_rejected_immediately() {
    let fx = Fx::new();
    let a = fx.pair().await;
    let b = fx.pair().await;
    assert_eq!(fx.auth.list_devices().unwrap().len(), 2);
    // 用 b 吊销 a。列表按 id 排、id 随机，不能拿 devices[0] 当 a（2026-09-03 偶发过）：按哈希找。
    let a_id: String = fx
        .db
        .conn()
        .query_row(
            "SELECT id FROM devices WHERE session_sha256 = ?1",
            [agora::auth::sha256_hex(a.split_once('=').unwrap().1)],
            |r| r.get(0),
        )
        .unwrap();
    let resp = fx
        .app()
        .oneshot(
            Request::delete(format!("/api/auth/devices/{a_id}"))
                .header(header::HOST, HOST)
                .header(header::ORIGIN, format!("http://{HOST}"))
                .header(header::COOKIE, &b)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(fx.get_devices(&a).await, StatusCode::UNAUTHORIZED);
    assert_eq!(fx.get_devices(&b).await, StatusCode::OK);

    // logout 只删当前设备。
    let resp = fx
        .app()
        .oneshot(
            Request::post("/api/auth/logout")
                .header(header::HOST, HOST)
                .header("sec-fetch-site", "same-origin")
                .header(header::COOKIE, &b)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(fx.get_devices(&b).await, StatusCode::UNAUTHORIZED);
    assert!(fx
        .auth
        .list_devices()
        .unwrap()
        .iter()
        .all(|d| d.revoked_at.is_some()));
}

#[tokio::test]
async fn cross_origin_cookie_request_rejected() {
    let fx = Fx::new();
    let cookie = fx.pair().await;
    let post = |origin: Option<&str>| {
        let mut b = Request::post("/api/auth/pair/new")
            .header(header::HOST, HOST)
            .header(header::COOKIE, &cookie);
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        b.body(Body::empty()).unwrap()
    };
    let evil = fx
        .app()
        .oneshot(post(Some("http://evil.example")))
        .await
        .unwrap();
    assert_eq!(evil.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(evil).await["error"], "cross_origin");
    let none = fx.app().oneshot(post(None)).await.unwrap();
    assert_eq!(
        none.status(),
        StatusCode::FORBIDDEN,
        "没有 Origin 也没有 Sec-Fetch-Site → 拒绝"
    );
    let same = fx
        .app()
        .oneshot(post(Some(&format!("http://{HOST}"))))
        .await
        .unwrap();
    assert_eq!(same.status(), StatusCode::OK);
    // GET 不受影响。
    assert_eq!(fx.get_devices(&cookie).await, StatusCode::OK);
}

#[tokio::test]
async fn bearer_rejected_on_plaintext_listener() {
    let fx = Fx::new();
    let resp = fx
        .app()
        .oneshot(
            Request::get("/api/auth/devices")
                .header(header::HOST, HOST)
                .header(header::AUTHORIZATION, "Bearer apt_mac_whatever")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json(resp).await["error"], "bearer_requires_tls");
}

#[tokio::test]
async fn database_stores_only_hashes() {
    let fx = Fx::new();
    let cookie = fx.pair().await;
    let plain = cookie.split_once('=').unwrap().1;
    let stored: String = fx
        .db
        .conn()
        .query_row("SELECT session_sha256 FROM devices", [], |r| r.get(0))
        .unwrap();
    assert_ne!(stored, plain);
    assert_eq!(stored.len(), 64);
    assert_eq!(stored, agora::auth::sha256_hex(plain));
}

/// ADR-003 D2 的"配对一次后 30 天免登录"是**两个**窗口：服务端 `last_seen_at` 的滑动
/// 判定，和浏览器 cookie 的 `Max-Age`。原来只做了前者，cookie 不带 `Max-Age` 就是
/// session cookie，浏览器一关就丢，用户第二天打开还是要重新配对（agora-z8b）。
/// 老守卫 `session_idle_and_absolute_expiry` 抓不到它：那个测试全程拿 fixture 手里的
/// cookie 串发请求，相当于模拟了一个永远不丢 cookie 的浏览器，从没看过 `Set-Cookie`。
#[tokio::test]
async fn session_cookie_max_age_tracks_config_and_slides_with_use() {
    // 故意不用默认的 30 天：写死 30 天的实现会在这里变红。
    let idle = Duration::from_secs(7 * 86_400);
    let fx = Fx::with(AuthConfig {
        session_idle: idle,
        ..AuthConfig::default()
    });
    let cookie = fx.pair().await; // pair() 内部断言了 Max-Age=idle

    // 刚用过（idle 不到一小时）：不该每个响应都塞一个 Set-Cookie。
    let resp = fx.get_devices_resp(&cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get(header::SET_COOKIE).is_none(),
        "服务端没刷新 last_seen_at 时不必重发 cookie"
    );

    // 隔了两小时再用：服务端把 last_seen_at 往后推，浏览器那边的 Max-Age 也要跟着推，
    // 否则天天在用的人到第 8 天照样被浏览器丢掉 cookie。
    fx.set_idle(2 * 3600);
    let resp = fx.get_devices_resp(&cookie).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let set = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("刷新 last_seen_at 的那次请求要重发 cookie")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        set.starts_with(&cookie),
        "重发的是同一条 session，不换 token"
    );
    assert!(
        set.contains(&format!("Max-Age={}", idle.as_secs())),
        "续期的 Max-Age 也取配置值：{set}"
    );
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax"),
        "{set}"
    );

    // 紧接着再请求一次：last_seen_at 刚被推到现在，不该再发。
    let resp = fx.get_devices_resp(&cookie).await;
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
}

/// TLS 监听器上发的 cookie 要同时带 `Secure` 与 `Max-Age`——加 `Secure` 时别把
/// `Max-Age` 挤掉（两者是拼在同一个字符串里的）。
#[tokio::test]
async fn secure_cookie_still_carries_max_age() {
    let fx = Fx::with(AuthConfig {
        cookie_secure: true,
        ..AuthConfig::default()
    });
    let token = fx.auth.mint_pair_token(PairedVia::Socket).unwrap();
    let resp = fx.pair_with(&token).await;
    let set = resp.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set.contains("Secure"), "{set}");
    assert!(
        set.contains(&format!(
            "Max-Age={}",
            AuthConfig::default().session_idle.as_secs()
        )),
        "{set}"
    );
}
