//! `/api/auth/*` 与 Principal 提取器（ADR-003 D1 / D2 / D7）。

use axum::extract::{FromRequestParts, OptionalFromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState, ClientAddr};
use crate::auth::{Auth, AuthConfig, AuthError, Device, PairedVia, Principal, COOKIE_NAME};

/// 每个 handler 的签名都要它；缺了就是免认证端点（守卫：tests/auth.rs）。
impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Bearer 优先解析；明文监听器上一律拒绝（TLS 监听器与 peer token 随 agora-7ku.2）。
        if parts.headers.contains_key(header::AUTHORIZATION) {
            return Err(AuthError::BearerRequiresTls.into());
        }
        let token = cookie_value(&parts.headers, COOKIE_NAME).ok_or(AuthError::Unauthenticated)?;
        // 一次索引命中的 SQLite 查询，微秒级；不值得为它 spawn_blocking。
        let auth = state.auth.authenticate_session(&token)?;
        let principal = auth.principal;
        // 服务端刚把 last_seen_at 往后推了，就把浏览器那边的 Max-Age 也推一下，
        // 两个 30 天窗口才是同一个窗口（RenewSlot 的注释里有为什么不能只在配对时发）。
        if auth.renewed {
            if let Some(slot) = parts.extensions.get::<RenewSlot>() {
                slot.set(session_cookie(&token, state.auth.config()));
            }
        }
        // 请求日志那一行的 principal 栏（api::log_request 开的 span）。
        tracing::Span::current().record("principal", tracing::field::display(principal.log_id()));
        // CSRF（ADR-003 D7）：cookie 认证的非安全方法必须同源。
        if !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS)
            && !same_origin(&parts.headers)
        {
            tracing::warn!(component = "auth", principal = %principal.log_id(), "拒绝跨站写请求");
            return Err(AuthError::CrossOrigin.into());
        }
        Ok(principal)
    }
}

/// 白名单端点（health）用它区分公开子集与完整形态：没凭据 → None；凭据形式上就
/// 不该出现（明文监听器上的 Bearer）→ 照样报错，不能靠"降级成公开子集"绕过。
impl OptionalFromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, ApiError> {
        match <Principal as FromRequestParts<AppState>>::from_request_parts(parts, state).await {
            Ok(p) => Ok(Some(p)),
            Err(e) if e.kind == "unauthenticated" => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// 滑动续期的回传格子。提取器认证成功、且这次刷新了 `last_seen_at` 时把要重发的
/// cookie 放进来，`api::renew_session_cookie` 层在响应上补一个 `Set-Cookie`。
///
/// 为什么绕这一道：axum 的提取器只能读请求、动不了响应，而中间件手里没有提取器
/// 认证时查出来的结果。另一条路是让中间件自己再认一次，那就是每个请求两次 SQLite
/// 查询、两份过期判定逻辑，将来改一处忘一处。共享格子只多一个 Arc。
#[derive(Clone, Default)]
pub(super) struct RenewSlot(std::sync::Arc<std::sync::Mutex<Option<HeaderValue>>>);

impl RenewSlot {
    fn set(&self, cookie: HeaderValue) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(cookie);
        }
    }

    pub(super) fn take(&self) -> Option<HeaderValue> {
        self.0.lock().ok().and_then(|mut g| g.take())
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
        .next()
}

/// `Sec-Fetch-Site: same-origin`，或 `Origin` 的 authority 与 `Host` 相同。两者都没有 → 不同源。
pub(super) fn same_origin(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("same-origin"))
    {
        return true;
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let authority = origin.split_once("://").map(|(_, a)| a).unwrap_or(origin);
    authority.eq_ignore_ascii_case(host)
}

// ---------- handlers ----------

#[derive(Debug, Deserialize)]
pub struct PairBody {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct PairReply {
    pub device: Device,
}

/// 唯一的未认证写端点：一次性链接换 cookie。
pub async fn pair(
    State(state): State<AppState>,
    ClientAddr(addr): ClientAddr,
    headers: HeaderMap,
    Json(body): Json<PairBody>,
) -> Result<Response, ApiError> {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let from = addr.map(|a| a.to_string());
    let (device, session) = state.auth.redeem(&body.token, ua, from.as_deref())?;
    let cookie = session_cookie(&session, state.auth.config());
    Ok(([(header::SET_COOKIE, cookie)], Json(PairReply { device })).into_response())
}

#[derive(Debug, Serialize)]
pub struct PairLink {
    pub url: String,
}

/// 已认证的 session 铸造新链接（Dashboard "配对新设备"）；origin 取自 Host。
pub async fn pair_new(
    principal: Principal,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<PairLink>, ApiError> {
    let token = state.auth.mint_pair_token(PairedVia::Session)?;
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1");
    let origin = format!("http://{host}");
    tracing::info!(component = "auth", principal = %principal.log_id(), "铸造配对链接");
    Ok(Json(PairLink {
        url: Auth::pair_link(&origin, &token),
    }))
}

/// 只删当前设备。
pub async fn logout(
    principal: Principal,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    if let Principal::Human { device } = &principal {
        state.auth.revoke(device)?;
    }
    Ok((
        [(header::SET_COOKIE, clear_cookie())],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn devices(
    _principal: Principal,
    State(state): State<AppState>,
) -> Result<Json<Vec<Device>>, ApiError> {
    Ok(Json(state.auth.list_devices()?))
}

pub async fn revoke_device(
    principal: Principal,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.auth.revoke(&id)?;
    tracing::info!(component = "auth", principal = %principal.log_id(), device = %id, "吊销设备");
    Ok(StatusCode::NO_CONTENT)
}

/// `Max-Age` 必须发：不发就是 session cookie，浏览器一关就丢（RFC 6265 5.3），
/// ADR-003 承诺的"配对一次后 30 天免登录"在浏览器侧根本不成立——服务端的
/// `session_idle` 判定再对也没用，因为凭据已经不在了（agora-z8b，2026-09-03 实测：
/// 配对应答只有 `HttpOnly; SameSite=Lax; Path=/`）。取值跟着 `auth.session_idle` 走，
/// 不写死 30 天：两个窗口一旦对不上，要么服务端先拒（用户看到莫名其妙的 401），
/// 要么浏览器先丢（用户看到莫名其妙的重新配对）。
fn session_cookie(session: &str, cfg: &AuthConfig) -> HeaderValue {
    let mut s = format!(
        "{COOKIE_NAME}={session}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        cfg.session_idle.as_secs()
    );
    if cfg.cookie_secure {
        s.push_str("; Secure");
    }
    HeaderValue::from_str(&s).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static("agora_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parsing_finds_ours_among_others() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            "a=1; agora_session=tok; b=2".parse().unwrap(),
        );
        assert_eq!(cookie_value(&h, COOKIE_NAME).as_deref(), Some("tok"));
        assert_eq!(cookie_value(&h, "nope"), None);
    }

    #[test]
    fn same_origin_compares_authority_with_host() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "127.0.0.1:7680".parse().unwrap());
        assert!(
            !same_origin(&h),
            "没有 Origin 也没有 Sec-Fetch-Site → 不同源"
        );
        h.insert(header::ORIGIN, "http://127.0.0.1:7680".parse().unwrap());
        assert!(same_origin(&h));
        h.insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert!(!same_origin(&h));
        h.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(same_origin(&h));
    }
}
