//! `GET /api/presets`：手机「新建」一屏的按钮列表（agora-prdg.4；epic agora-hxva；MISSION §6.9）。
//!
//! **只读**。"能起什么"冻结在桌面侧：预设只在终端里用 `agora preset`（S1）增删改，这里没有
//! 任何写端点——路由只注册 GET，`POST` / `PUT` / `PATCH` / `DELETE` 由 axum 按方法不匹配回
//! 405 METHOD_NOT_ALLOWED（不是 404：端点有、写没有）。就算这台手机被临时拿到，也只能选已经
//! 批准过的那几条。手机上的起会话走 `POST /api/sessions {preset}`（[`super::sessions::create`]
//! 里的 preset 分支），那边同样只在本机展开。
//!
//! 预设属于**承载节点**（本机）这一份：peer 的会话行会出现在手机上（MISSION §3.5），但预设
//! 列表没有 peer 版本、也没有 `?node=`——跨节点起会话留 V2（承接 agora-ch7a）。
//!
//! 列表顺序用 S1 `preset::list` 的既有排序（按名字，确定、好扫）；"updated_at 倒序是不是更好"
//! 是 S2 留给人的那条人眼项，改的时候只动这里与 `docs/spec/ux.md`。

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::Value;

use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::session::preset::{self, Preset, PresetError};

/// `GET /api/presets`（docs/spec/api.md「预设」）。契约只承诺这六项，逐条从 [`Preset`] 取。
pub async fn list(
    _principal: Principal,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let rows = load_all(&state).await?;
    let presets: Vec<Value> = rows
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "agent_type": p.agent_type,
                "working_directory": p.working_directory,
                "args": p.args,
                "prompt": p.prompt,
                "updated_at": p.updated_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "presets": presets })))
}

/// 库里全部预设。SQLite 是阻塞调用，走 blocking 线程（与其它 handler 同一条纪律）。
async fn load_all(state: &AppState) -> Result<Vec<Preset>, ApiError> {
    let db = state.sessions.db_handle();
    match tokio::task::spawn_blocking(move || preset::list(&db)).await {
        Ok(Ok(rows)) => Ok(rows),
        Ok(Err(err)) => Err(db_error(err)),
        Err(err) => Err(join_error(err)),
    }
}

/// 单条；不存在 → 404 `preset_unknown`（手机上的 404 提示按这个类型分支）。
/// `POST /api/sessions {preset}` 的展开也走这里，两种消费方读同一个语义。
pub(super) async fn load(state: &AppState, name: &str) -> Result<Preset, ApiError> {
    let db = state.sessions.db_handle();
    let asked = name.to_owned();
    let key = name.to_owned();
    // 与 `sessions::blocking` 同一个形状，但 preset 层有自己的错误枚举（不并进 SessionError）。
    match tokio::task::spawn_blocking(move || preset::get(&db, &key)).await {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(PresetError::NotFound(_))) => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            kind: "preset_unknown",
            message: format!(
                "未知预设 {asked}；在终端用 `agora preset list` 看看有哪些（手机上只能选已经批准过的那几条）"
            ),
        }),
        Ok(Err(err)) => Err(db_error(err)),
        Err(err) => Err(join_error(err)),
    }
}

fn db_error(err: PresetError) -> ApiError {
    tracing::error!(component = "api", error = %err, "读预设失败");
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        kind: "database",
        message: err.to_string(),
    }
}

/// 与 [`sessions::blocking`] 里那句同一形状（那是 `pub(super)`，这里是 preset 层自己的）。
fn join_error(err: tokio::task::JoinError) -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        kind: "internal",
        message: format!("blocking 任务异常: {err}"),
    }
}
