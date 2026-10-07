//! `/api/push/subscriptions`（agora-thc.6）：浏览器把 Web Push 订阅交给承载节点。
//!
//! 订阅属于**设备**（Principal::Human 的 device id），不是属于页面会话：同一台手机重订就是更新
//! 同一端点（或换端点新增一条），吊销设备后发送侧立刻看不到它（`PushStore::list_active` 联查
//! `devices.revoked_at`）。端点只收 Apple 的推送服务，别的地址一律 400：这个字段是浏览器给的，
//! 恶意页面塞任意 https 等于拿节点当 SSRF 跳板（`validate_endpoint`）。

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::push::crypto::b64decode;
use crate::push::sender::{host_of_endpoint, validate_endpoint};

#[derive(Debug, Deserialize)]
pub struct SubscribeBody {
    pub endpoint: String,
    pub keys: SubscribeKeys,
}

#[derive(Debug, Deserialize)]
pub struct SubscribeKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Serialize)]
pub struct SubscribeReply {
    pub endpoint: String,
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        kind: "bad_request",
        message: message.into(),
    }
}

/// 注册 / 更新一份订阅。幂等：同一端点再来就是换密钥。
pub async fn subscribe(
    principal: Principal,
    State(state): State<AppState>,
    Json(body): Json<SubscribeBody>,
) -> Result<(StatusCode, Json<SubscribeReply>), ApiError> {
    let device = match &principal {
        Principal::Human { device } => device.clone(),
        Principal::Peer { .. } => return Err(crate::auth::AuthError::PeerForbidden.into()),
    };
    if let Err(why) = validate_endpoint(&body.endpoint) {
        return Err(bad_request(why));
    }
    // 密钥长度按 RFC 8291：p256dh 是 65 字节未压缩点，auth 是 16 字节秘密。形状不对不如现在
    // 就拒——发送时才发现的话，每来一条通知都要重试一遍坏订阅。
    let p256dh =
        b64decode(&body.keys.p256dh).map_err(|_| bad_request("keys.p256dh 不是合法 base64url"))?;
    if p256dh.len() != 65 {
        return Err(bad_request(format!(
            "keys.p256dh 应是 65 字节，收到 {}",
            p256dh.len()
        )));
    }
    let auth =
        b64decode(&body.keys.auth).map_err(|_| bad_request("keys.auth 不是合法 base64url"))?;
    if auth.len() != 16 {
        return Err(bad_request(format!(
            "keys.auth 应是 16 字节，收到 {}",
            auth.len()
        )));
    }
    let existed = state
        .push_store
        .upsert(&device, &body.endpoint, &body.keys.p256dh, &body.keys.auth)
        .map_err(|e| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "database",
            message: e.to_string(),
        })?;
    tracing::info!(
        component = "push",
        principal = %principal.log_id(),
        endpoint = %host_of_endpoint(&body.endpoint),
        existed,
        "推送订阅已登记"
    );
    Ok((
        if existed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(SubscribeReply {
            endpoint: body.endpoint,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeBody {
    pub endpoint: String,
}

/// 按端点删除（幂等）：浏览器清站点数据、用户关掉通知都会走到这里。已有的 404 / 410 也是同一条
/// 路（发送侧删）。
pub async fn unsubscribe(
    principal: Principal,
    State(state): State<AppState>,
    Json(body): Json<UnsubscribeBody>,
) -> Result<StatusCode, ApiError> {
    match &principal {
        Principal::Human { .. } => {}
        Principal::Peer { .. } => return Err(crate::auth::AuthError::PeerForbidden.into()),
    }
    state
        .push_store
        .remove(&body.endpoint)
        .map_err(|e| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "database",
            message: e.to_string(),
        })?;
    tracing::info!(
        component = "push",
        principal = %principal.log_id(),
        endpoint = %host_of_endpoint(&body.endpoint),
        "推送订阅已删除"
    );
    Ok(StatusCode::NO_CONTENT)
}
