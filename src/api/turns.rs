//! `GET /api/sessions/:id/turns`（agora-2mff；MISSION §6.9 / A53；docs/spec/api.md「轮次」）。
//!
//! 节点为每个会话记下的最近几轮「人话 + 最终回复」（`session::turns`），手机会话卡拿它画「更早 N 轮」。
//! peer 会话经 `forward::route` 一跳转发（GET 也走它，`limit` 拼进转发路径）。老节点不认识这个子路径，
//! GET 落到 SPA fallback，`/api/` 前缀一律空 body 的 404（`spa::serve`）；页面对任何非 2xx 都按
//! 「没有上文」处理，不必分清是老节点还是行不在。

use axum::extract::{Path, Query, State};
use axum::http::Method;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::{forward, sessions, ApiError, AppState};
use crate::auth::Principal;
use crate::session::turns::KEEP;

#[derive(Debug, Default, Deserialize)]
pub struct TurnsQuery {
    /// 最多几轮，1..=20；缺省 20。
    limit: Option<usize>,
}

pub async fn list(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    Query(q): Query<TurnsQuery>,
) -> Result<Response, ApiError> {
    let limit = q.limit.unwrap_or(KEEP).clamp(1, KEEP);
    let suffix = format!("/turns?limit={limit}");
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::GET,
        &suffix,
        forward::NO_BODY,
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    let turns = sessions::blocking(&state.sessions, move |s| s.turns(&id, limit)).await?;
    Ok(Json(serde_json::json!({ "turns": turns, "keep": KEEP })).into_response())
}
