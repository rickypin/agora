//! `/api/auth/*` 与 Principal 提取器（ADR-003 D1 / D2 / D7）。

use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState, ClientAddr};
use crate::auth::{Auth, AuthError, Device, PairedVia, Principal, COOKIE_NAME};

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
        let principal = state.auth.authenticate_session(&token)?;
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
fn same_origin(headers: &HeaderMap) -> bool {
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
    let cookie = session_cookie(&session, state.auth.config().cookie_secure);
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

fn session_cookie(session: &str, secure: bool) -> HeaderValue {
    let mut s = format!("{COOKIE_NAME}={session}; HttpOnly; SameSite=Lax; Path=/");
    if secure {
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
