//! `/api/projects*`：New Agent 对话框的数据源（MISSION §6.4；docs/spec/api.md）。
//!
//! 两个端点都只读：列项目、列该仓库现有的 worktree。新建 worktree 归 M3 A44。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::project::{ProjectError, Projects};

/// 在 blocking 线程上跑一步项目查询（扫目录 / 起 git 子进程，ADR-001 D8）。
async fn blocking<T: Send + 'static>(
    projects: &Arc<Projects>,
    f: impl FnOnce(&Projects) -> Result<T, ProjectError> + Send + 'static,
) -> Result<T, ApiError> {
    let p = projects.clone();
    match tokio::task::spawn_blocking(move || f(&p)).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(err) => Err(ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal",
            message: format!("blocking 任务异常: {err}"),
        }),
    }
}

/// `project_roots` 扫描结果 ∪ 用过的项目，按最近使用排序（§6.4）。
pub async fn list(
    _principal: Principal,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let projects = blocking(&state.projects, |p| p.list()).await?;
    Ok(Json(serde_json::json!({ "projects": projects })))
}

#[derive(Debug, Deserialize)]
pub struct WorktreeQuery {
    pub path: PathBuf,
}

pub async fn worktrees(
    _principal: Principal,
    State(state): State<AppState>,
    Query(q): Query<WorktreeQuery>,
) -> Result<Json<Value>, ApiError> {
    let worktrees = blocking(&state.projects, move |p| p.worktrees(&q.path)).await?;
    Ok(Json(serde_json::json!({ "worktrees": worktrees })))
}

impl From<ProjectError> for ApiError {
    fn from(e: ProjectError) -> Self {
        let (status, kind) = match &e {
            // 未知路径不是 404：回 404 等于确认"这个路径不存在"，而这个端点不该回答
            // 任意路径的存在性问题。
            ProjectError::Unknown(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ProjectError::Git(_) | ProjectError::Exec(_) => (StatusCode::BAD_GATEWAY, "git"),
            ProjectError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database"),
        };
        if status.is_server_error() {
            tracing::error!(component = "api", error = %e, "项目查询失败");
        }
        ApiError {
            status,
            kind,
            message: e.to_string(),
        }
    }
}
