//! `/api/projects*`：New Agent 对话框的数据源（MISSION §6.4；docs/spec/api.md）。
//!
//! 列项目、列该仓库现有的 worktree、列该仓库的就绪任务（`GET /api/projects/tasks`，读 `bd ready`，
//! A43，agora-h1k.2）都只读；`POST /api/projects/worktrees` 是唯一的写——
//! `git worktree add -b`，§1.4 Git GUI 边界的唯一例外（A44，agora-h1k.1；`project::worktree`）。

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

#[derive(Debug, Deserialize)]
pub struct CreateWorktreeBody {
    /// 已知项目（与 GET 的 `?path=` 同一校验）。
    pub path: PathBuf,
    /// 既是目录名也是分支名。
    pub name: String,
    /// 起点分支；缺省链见 `project::worktree`（主 worktree 当前分支 → origin/HEAD → main）。
    #[serde(default)]
    pub base: Option<String>,
}

/// `POST /api/projects/worktrees` → 201，响应体与 GET 的每项同形（`main: false`）。
/// Peer 也能调：New Agent 选 peer 节点时经一跳转发在那边建（MISSION §6.4）。
pub async fn create_worktree(
    principal: Principal,
    State(state): State<AppState>,
    Json(body): Json<CreateWorktreeBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let CreateWorktreeBody { path, name, base } = body;
    let log_name = name.clone();
    let worktree = blocking(&state.projects, move |p| {
        p.create_worktree(&path, &name, base.as_deref())
    })
    .await?;
    tracing::info!(component = "api", principal = %principal.log_id(), worktree = %log_name, "新建 worktree");
    Ok((StatusCode::CREATED, Json(serde_json::json!(worktree))))
}

#[derive(Debug, Deserialize)]
pub struct TasksQuery {
    /// 已知项目（与 `GET /api/projects/worktrees` 同一校验）。
    pub path: PathBuf,
}

/// `GET /api/projects/tasks?path=<repo>`：该仓库 `bd ready --json` 里可起会话的任务
/// （MISSION §6.4 从就绪任务起会话；A43；agora-h1k.2）。
///
/// 永远 200：`{ tasks: [{ id, title, priority, type }], reason: null | "no_bd" | "no_beads" |
/// "timeout" | "bad_output" }`——没装 bd、仓库没有 beads 都不是错误，对话框据 `reason` 的
/// **类型**给一行灰字并把 Task 退回一句话（MISSION §2.3 规则 10）。只有 `path` 不是已知项目
/// 才 400 `bad_request`（同 worktrees：这个端点不该回答任意目录的存在性）。
///
/// agora 对 beads 零写入（不变量 12）：整条链路敲到 bd 的只有 `ready --json`，没有 `--claim`
/// （守卫 `tests/task_pick.rs`、`tests/arch_boundary.rs::beads_is_read_only_and_lives_in_task`）。
pub async fn tasks(
    _principal: Principal,
    State(state): State<AppState>,
    Query(q): Query<TasksQuery>,
) -> Result<Json<Value>, ApiError> {
    let path = q.path;
    let known_path = path.clone();
    blocking(&state.projects, move |p| {
        if p.is_known(&known_path) {
            Ok(())
        } else {
            Err(ProjectError::Unknown(
                known_path.to_string_lossy().into_owned(),
            ))
        }
    })
    .await?;
    let index = state.sessions.task_index();
    let listed = tokio::task::spawn_blocking(move || index.ready(&path))
        .await
        .map_err(|err| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal",
            message: format!("blocking 任务异常: {err}"),
        })?;
    let (tasks, reason) = match listed {
        Ok(tasks) => (tasks, None),
        Err(reason) => (Vec::new(), Some(reason)),
    };
    Ok(Json(
        serde_json::json!({ "tasks": tasks, "reason": reason }),
    ))
}

impl From<ProjectError> for ApiError {
    fn from(e: ProjectError) -> Self {
        let (status, kind) = match &e {
            // 未知路径不是 404：回 404 等于确认"这个路径不存在"，而这个端点不该回答
            // 任意路径的存在性问题。
            ProjectError::Unknown(_) | ProjectError::InvalidName(_) => {
                (StatusCode::BAD_REQUEST, "bad_request")
            }
            ProjectError::WorktreeExists(_) => (StatusCode::CONFLICT, "worktree_exists"),
            ProjectError::BranchExists(_) => (StatusCode::CONFLICT, "branch_exists"),
            ProjectError::PathExists(_) => (StatusCode::CONFLICT, "path_exists"),
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
