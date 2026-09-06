//! ADR-003 "什么会让它变危险"里点名的机器 token 守卫（A31；agora-7ku.2）：未签发 token 拒绝
//! 一切 Bearer、Bearer 只上 TLS 监听器、吊销即时、库里没有明文、轮换让旧的失效、token_file
//! 权限过宽是配置错误。
//!
//! "TLS 监听器"在这里是请求扩展里的 `api::TlsListener` 标记（监听器侧盖上去的，线上伪造不了）；
//! 真的 TLS 监听器随 agora-7ku.10，与本文件无关——这里守的是"标记在 / 不在"两种情况下的提取器。

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use agora::api::{self, AppState, TlsListener, PUBLIC_ROUTES, ROUTES};
use agora::auth::peer_token::{self, PeerTokenError, TokenFileError};
use agora::auth::{sha256_hex, Auth, AuthConfig, Principal};
use agora::runtime::Runtime;
use agora::session::{Db, SessionManager};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:7681";

struct Fx {
    auth: Arc<Auth>,
    db: Arc<Db>,
}

impl Fx {
    fn new() -> Self {
        Self::with_db(Arc::new(Db::open_in_memory().unwrap()))
    }

    fn with_db(db: Arc<Db>) -> Self {
        let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
        Fx { auth, db }
    }

    fn app(&self) -> Router {
        let rt = Arc::new(common::FakeRuntime::default());
        let sessions = Arc::new(SessionManager::new(self.db.clone(), rt as Arc<dyn Runtime>));
        api::router(AppState::new(self.auth.clone(), sessions, common::NODE))
    }

    /// 带 `Authorization` 的请求；`tls` 决定盖不盖 TLS 监听器的标记。
    fn request(&self, method: &str, path: &str, authorization: &str, tls: bool) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, authorization);
        if tls {
            b = b.extension(TlsListener);
        }
        b.body(Body::from("{}")).unwrap()
    }

    async fn call(&self, req: Request<Body>) -> axum::response::Response {
        self.app().oneshot(req).await.unwrap()
    }

    /// TLS 监听器上、带 Bearer 的 `GET /api/auth/devices`——需要 principal 的最普通一条。
    async fn devices_tls(&self, token: &str) -> axum::response::Response {
        self.call(self.request("GET", "/api/auth/devices", &format!("Bearer {token}"), true))
            .await
    }

    fn rows(&self) -> i64 {
        self.db
            .conn()
            .query_row("SELECT count(*) FROM peer_tokens", [], |r| r.get(0))
            .unwrap()
    }
}

async fn json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// A31 前半：没有签发过任何 token 的节点拒绝一切 Bearer——形态合法但未签发的、乱写的、空的，
/// 对每一条需要 principal 的路由都是 401 `unauthenticated`；health 只给公开子集。
#[tokio::test]
async fn no_token_issued_rejects_all_bearer() {
    let fx = Fx::new();
    assert_eq!(fx.rows(), 0, "前置：表里一行都没有");
    // 形态完全合法、连 name 都像真的——只是没人签过。
    let unissued = format!("apt_zuan_{}", agora::auth::random_token());
    assert_eq!(peer_token::parse(&unissued), Some("zuan"));
    let bearers = [
        format!("Bearer {unissued}"),
        "Bearer garbage".to_owned(),
        "Bearer ".to_owned(),
        "Bearer apt__".to_owned(),
        format!("Basic {unissued}"),
    ];
    for (method, path) in ROUTES {
        if PUBLIC_ROUTES.contains(&(*method, *path)) {
            continue;
        }
        let concrete = path.replace("{id}", "someid");
        for bearer in &bearers {
            let resp = fx.call(fx.request(method, &concrete, bearer, true)).await;
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} 带 {bearer:?} 必须 401"
            );
            assert_eq!(
                json(resp).await["error"],
                "unauthenticated",
                "{method} {path}"
            );
        }
    }
    // 白名单里的 health：坏 Bearer 不能换来完整形态。
    let resp = fx
        .call(fx.request("GET", "/api/health", &format!("Bearer {unissued}"), true))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json(resp).await, serde_json::json!({ "status": "ok" }));
}

/// A31 后半：同一个真 token，明文监听器（没有标记）401 `bearer_requires_tls`，TLS 监听器 200；
/// 明文监听器上任何 `Authorization` 头都拒——连 scheme 都不看，先拒再说。
#[tokio::test]
async fn bearer_rejected_on_plaintext_listener() {
    let fx = Fx::new();
    let token = peer_token::create(&fx.db, "zuan", false).unwrap();

    let plain = fx
        .call(fx.request(
            "GET",
            "/api/auth/devices",
            &format!("Bearer {token}"),
            false,
        ))
        .await;
    assert_eq!(plain.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json(plain).await["error"], "bearer_requires_tls");

    let basic = fx
        .call(fx.request("GET", "/api/auth/devices", "Basic abc", false))
        .await;
    assert_eq!(basic.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json(basic).await["error"], "bearer_requires_tls");

    // health 也不例外：明文监听器上带 Bearer 不能降级成公开子集蒙混过去。
    let health = fx
        .call(fx.request("GET", "/api/health", &format!("Bearer {token}"), false))
        .await;
    assert_eq!(health.status(), StatusCode::UNAUTHORIZED);

    let tls = fx.devices_tls(&token).await;
    assert_eq!(
        tls.status(),
        StatusCode::OK,
        "同一 token 在 TLS 监听器上可用"
    );
    // TLS 监听器上 health 给完整形态。
    let health = fx
        .call(fx.request("GET", "/api/health", &format!("Bearer {token}"), true))
        .await;
    assert_eq!(health.status(), StatusCode::OK);
    assert!(json(health).await.get("runtime").is_some());
}

/// 吊销即时：同一个 Auth、同一个 Router，不重启、不清缓存，吊销后的下一个请求就是 401。
#[tokio::test]
async fn revoked_token_rejected_immediately() {
    let fx = Fx::new();
    let token = peer_token::create(&fx.db, "zuan", false).unwrap();
    assert_eq!(fx.devices_tls(&token).await.status(), StatusCode::OK);

    peer_token::revoke(&fx.db, "zuan").unwrap();
    let resp = fx.devices_tls(&token).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json(resp).await["error"], "unauthenticated");
    let rows = peer_token::list(&fx.db).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].revoked_at.is_some());

    // 再吊销一次是 no-op；吊销没签过的 → NotFound。
    peer_token::revoke(&fx.db, "zuan").unwrap();
    assert!(matches!(
        peer_token::revoke(&fx.db, "nobody"),
        Err(PeerTokenError::NotFound(_))
    ));
    // 吊销后再签是正常流程，不需要 --rotate；新 token 可用、旧的仍拒。
    let again = peer_token::create(&fx.db, "zuan", false).unwrap();
    assert_ne!(again, token);
    assert_eq!(fx.devices_tls(&again).await.status(), StatusCode::OK);
    assert_eq!(
        fx.devices_tls(&token).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

/// 库里只有 SHA-256：表的每个文本格子、落盘的 db 与 WAL 文件里都找不到明文（连随机段都不行）。
#[test]
fn plaintext_never_stored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agora.db");
    let token = {
        let db = Db::open(&path).unwrap();
        let token = peer_token::create(&db, "zuan", false).unwrap();
        // 用一次，让 last_used_at 也写进去——每一列都不该带明文。
        peer_token::authenticate(&db, &format!("Bearer {token}")).unwrap();
        let conn = db.conn();
        let mut stmt = conn.prepare("SELECT * FROM peer_tokens").unwrap();
        let ncols = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        let mut cells = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..ncols {
                if let Ok(Some(s)) = row.get::<_, Option<String>>(i) {
                    cells.push(s);
                }
            }
        }
        assert!(!cells.is_empty());
        let secret = &token[token.len() - 43..];
        for c in &cells {
            assert!(
                !c.contains(&token) && !c.contains(secret),
                "明文进了库: {c}"
            );
        }
        assert!(
            cells.contains(&sha256_hex(&token)),
            "存的应是整串的 SHA-256"
        );
        token
    };
    // 连接已关（WAL 已 checkpoint），扫原始字节。
    let secret = &token.as_bytes()[token.len() - 43..];
    for f in ["agora.db", "agora.db-wal"] {
        let p = dir.path().join(f);
        if let Ok(bytes) = std::fs::read(&p) {
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret),
                "{f} 里有明文 token"
            );
        }
    }
}

/// 轮换：新的生效、旧的立即失效；不带 --rotate 时已有有效 token 的 name 拒绝再签。
#[tokio::test]
async fn rotate_invalidates_old_token() {
    let fx = Fx::new();
    let t1 = peer_token::create(&fx.db, "zuan", false).unwrap();
    assert_eq!(fx.devices_tls(&t1).await.status(), StatusCode::OK);
    assert!(matches!(
        peer_token::create(&fx.db, "zuan", false),
        Err(PeerTokenError::Exists(_))
    ));
    assert_eq!(
        fx.devices_tls(&t1).await.status(),
        StatusCode::OK,
        "拒绝再签不影响旧的"
    );

    let t2 = peer_token::create(&fx.db, "zuan", true).unwrap();
    assert_ne!(t1, t2);
    assert_eq!(fx.devices_tls(&t1).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(fx.devices_tls(&t2).await.status(), StatusCode::OK);
    assert_eq!(fx.rows(), 1, "一个 peer 一行");

    // principal 是 token 里的 name。
    assert_eq!(
        fx.auth
            .authenticate_bearer(&format!("bearer {t2}"))
            .unwrap(),
        Principal::Peer {
            name: "zuan".into()
        },
        "scheme 大小写不敏感"
    );
    // 别的 peer 的 token 换不来 zuan 的身份：按 name 查行、哈希比对。
    let mac = peer_token::create(&fx.db, "mac", false).unwrap();
    let forged = format!("apt_zuan_{}", &mac[mac.len() - 43..]);
    assert_eq!(
        fx.devices_tls(&forged).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(matches!(
        peer_token::create(&fx.db, "zu an", false),
        Err(PeerTokenError::InvalidName(_))
    ));
}

/// 持有方的 `token_file`：group / other 有任何位、不属于自己、内容不像 token——都是配置错误
/// （不是离线、不是未授权）；0600 才读得出来。
#[test]
fn token_file_too_open_is_config_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zuan.token");
    let token = format!("apt_zuan_{}", agora::auth::random_token());
    std::fs::write(&path, format!("{token}\n")).unwrap();

    for mode in [0o644u32, 0o640, 0o604, 0o660, 0o666, 0o610, 0o601] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let err = peer_token::load_token_file(&path).unwrap_err();
        assert!(
            matches!(err, TokenFileError::TooOpen { mode: m, .. } if m == mode),
            "{mode:o}: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("chmod 600") && msg.contains("权限过宽"),
            "{msg}"
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(peer_token::load_token_file(&path).unwrap(), token);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(
        peer_token::load_token_file(&path).unwrap(),
        token,
        "只读也行"
    );

    // 内容不合形态：同样是配置错误。
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, "not a token\n").unwrap();
    assert!(matches!(
        peer_token::load_token_file(&path),
        Err(TokenFileError::Malformed { .. })
    ));
    // 文件不存在。
    assert!(matches!(
        peer_token::load_token_file(&dir.path().join("missing")),
        Err(TokenFileError::Read { .. })
    ));
}
