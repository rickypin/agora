//! `POST /api/sessions/:id/images`（agora-lmz2；MISSION §6.9；docs/spec/api.md「附图」）。
//!
//! 手机 composer 的截图先单独传到会话所属节点、落进会话工作目录（`session::images`），响应给出
//! 绝对路径；客户端再把 `[image: <路径>]` 接进下一条文本走 `input`。分两步而不是给 `input`
//! 加字段：老节点不认识新端点会回 405、页面明确报错；若塞进 `input` 的新字段，老节点会静默丢图、
//! 只把文字交出去（api.md「绝不静默错读」）。peer 会话同 `input` 一样经 `forward` 一跳转发。

use std::path::PathBuf;
use std::time::SystemTime;

use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::{forward, ApiError, AppState};
use crate::auth::Principal;
use crate::session::images::{self, ImageError};
use crate::session::SessionError;

/// 请求体上限：base64 后的 [`images::MAX_BYTES`] 再留一点 JSON 外壳的余量。超了由 axum 回 413。
pub(super) const BODY_LIMIT: usize = images::MAX_BYTES / 3 * 4 + 4096;

#[derive(Deserialize, Serialize)]
pub struct ImageBody {
    /// 图片字节的标准 base64（不带 `data:` 前缀）；格式由节点按魔数认，不收客户端报的类型。
    data: String,
}

pub async fn upload(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    Json(body): Json<ImageBody>,
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::POST,
        "/images",
        Some(&body),
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    let sessions = state.sessions.clone();
    let sid = id.clone();
    let view = tokio::task::spawn_blocking(move || sessions.get(&sid))
        .await
        .map_err(join_error)?
        .map_err(ApiError::from)?;
    // 收了图却没有通道把路径交出去，只会在工作目录里留垃圾：与 `input` 的 text 同一道门。
    if view.text_via == "none" {
        return Err(SessionError::NoRuntime(id).into());
    }
    let Some(wd) = view
        .record
        .working_directory
        .clone()
        .filter(|s| !s.is_empty())
    else {
        return Err(no_working_directory(&id));
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.data.as_bytes())
        .map_err(|_| ApiError {
            status: StatusCode::BAD_REQUEST,
            kind: "bad_image",
            message: "data 不是标准 base64".to_owned(),
        })?;
    let size = bytes.len();
    let saved = tokio::task::spawn_blocking(move || {
        images::save(&PathBuf::from(wd), &bytes, SystemTime::now())
    })
    .await
    .map_err(join_error)?;
    let path = saved.map_err(|e| match e {
        ImageError::Empty | ImageError::NotAnImage => ApiError {
            status: StatusCode::BAD_REQUEST,
            kind: "bad_image",
            message: e.to_string(),
        },
        ImageError::TooLarge(_) => ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            kind: "image_too_large",
            message: e.to_string(),
        },
        ImageError::NoWorkingDirectory(_) => no_working_directory(&id),
        ImageError::UnsafeDir(_) => ApiError {
            status: StatusCode::CONFLICT,
            kind: "unsafe_upload_dir",
            message: e.to_string(),
        },
        ImageError::Io(_) => {
            tracing::error!(component = "api", session_id = %id, error = %e, "写附图失败");
            ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                kind: "internal",
                message: e.to_string(),
            }
        }
    })?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %id,
        bytes = size, path = %path.display(), "image");
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "path": path.to_string_lossy() })),
    )
        .into_response())
}

fn no_working_directory(id: &str) -> ApiError {
    ApiError {
        status: StatusCode::CONFLICT,
        kind: "no_working_directory",
        message: format!("{id} 没有可写的工作目录，图没处放"),
    }
}

fn join_error(err: tokio::task::JoinError) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        kind: "internal",
        message: format!("blocking 任务异常: {err}"),
    }
}
