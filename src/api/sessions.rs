//! `/api/sessions*`（docs/spec/api.md；MISSION §4.6 §7.3 §8）。
//!
//! 每个 handler 第一个参数是 `Principal`（ADR-003 D1）。Session Manager 的方法会起运行时
//! 子进程，一律放 `spawn_blocking`。会话 id 对外 `<node>:<id>`；不是本节点的 id 现在
//! 报 `node_unknown`（peer 转发随 ADR-004 的多节点阶段）。
//!
//! Kill / Restart 的确认跟着"杀"走（MISSION §8）：会杀且没带 `confirmed: true` →
//! 409 `needs_confirmation`；不会杀（FINISHED / FAILED / 会话已不在）直接执行。判断在
//! 会话所属节点做，转发节点不能代替。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::events::{export, global_id, Event};
use crate::runtime::Size;
use crate::session::{AdoptSession, NewSession, SessionError, SessionManager, SessionView};

/// `<node>:<id>` → 本机 id；节点不对就报错。
pub(super) fn local_id(state: &AppState, gid: &str) -> Result<String, ApiError> {
    match gid.split_once(':') {
        Some((node, id)) if node == &*state.node => Ok(id.to_owned()),
        Some((node, _)) => Err(ApiError {
            status: StatusCode::NOT_FOUND,
            kind: "node_unknown",
            message: format!(
                "会话属于节点 {node}，本节点是 {}；peer 转发尚未实现",
                state.node
            ),
        }),
        // 裸 id 视为本机：curl 手敲时少打一段。
        None => Ok(gid.to_owned()),
    }
}

/// 在 blocking 线程上跑 Session Manager 的一步。
pub(super) async fn blocking<T: Send + 'static>(
    sessions: &Arc<SessionManager>,
    f: impl FnOnce(&SessionManager) -> Result<T, SessionError> + Send + 'static,
) -> Result<T, ApiError> {
    let s = sessions.clone();
    match tokio::task::spawn_blocking(move || f(&s)).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(err) => Err(ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal",
            message: format!("blocking 任务异常: {err}"),
        }),
    }
}

// ---------- 读 ----------

/// `{ sessions: [...], unregistered: [...] }`：已登记的会话 + 运行时里未登记的（Unknown Agent，
/// 可采纳；A1 列表含全部运行时会话）。
pub async fn list(
    _principal: Principal,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let node = state.node.clone();
    let (views, unregistered) =
        blocking(&state.sessions, |s| Ok((s.list()?, s.unregistered()?))).await?;
    let sessions: Vec<Value> = views.iter().map(|v| export(&node, v)).collect();
    let unregistered: Vec<Value> = unregistered
        .iter()
        .map(|s| {
            serde_json::json!({
                "runtime_ref": s.r#ref.0,
                "name": s.name,
                "title": s.title,
                "alive": s.alive,
                "managed": s.managed,
                "working_directory": s.cwd,
                "node": &*node,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "sessions": sessions,
        "unregistered": unregistered,
    })))
}

pub async fn get(
    _principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = local_id(&state, &gid)?;
    let view = blocking(&state.sessions, move |s| s.get(&id)).await?;
    Ok(Json(export(&state.node, &view)))
}

// ---------- 创建 / 采纳 ----------

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub display_name: String,
    pub agent_type: String,
    pub working_directory: PathBuf,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub task_ref: Option<String>,
    /// 缺省链见 `create`；存的一律是可移植的裸命令名（ADR-001 D7）。
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

pub async fn create(
    principal: Principal,
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.display_name.trim().is_empty() || body.agent_type.trim().is_empty() {
        return Err(bad_request("display_name 与 agent_type 不能为空"));
    }
    // 缺省链：请求 > `agents.<type>.command` 覆盖 > Adapter 的 default_command >
    // agent_type 本身（`GET /api/agents` 走同一条链的前两段）。
    let command = body
        .command
        .filter(|c| !c.trim().is_empty())
        .or_else(|| {
            state
                .agents
                .get(&body.agent_type)
                .and_then(|a| a.command.clone())
        })
        .or_else(|| {
            crate::adapter::find(&body.agent_type)
                .map(|a| crate::adapter::AgentIdentity::default_command(a).to_owned())
        })
        .unwrap_or_else(|| body.agent_type.clone());
    let mut size = Size::default();
    if let (Some(c), Some(r)) = (body.cols, body.rows) {
        size = Size { cols: c, rows: r };
    }
    let working_directory = body.working_directory;
    let new = NewSession {
        display_name: body.display_name,
        agent_type: body.agent_type,
        working_directory: working_directory.clone(),
        worktree: body.worktree,
        task_ref: body.task_ref,
        command,
        env: vec![],
        size,
    };
    let view = blocking(&state.sessions, move |s| s.create(&new)).await?;
    touch_project(&state, working_directory).await;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, "创建会话");
    Ok((StatusCode::CREATED, Json(announce_created(&state, &view))))
}

#[derive(Debug, Deserialize)]
pub struct AdoptBody {
    pub runtime_ref: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    /// spec 里叫 project：会话的工作目录。
    #[serde(default, alias = "working_directory")]
    pub project: Option<PathBuf>,
}

pub async fn adopt(
    principal: Principal,
    State(state): State<AppState>,
    Json(body): Json<AdoptBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let spec = AdoptSession {
        runtime_ref: body.runtime_ref,
        display_name: body.display_name,
        agent_type: body.agent_type,
        working_directory: body.project,
    };
    let view = blocking(&state.sessions, move |s| s.adopt(&spec)).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, "采纳会话");
    Ok((StatusCode::CREATED, Json(announce_created(&state, &view))))
}

/// 项目列表的"最近使用"排序只在起会话时更新（MISSION §6.4）。记不上不该让创建失败：
/// 用户要的是会话，排序错一次下次就好了。
async fn touch_project(state: &AppState, path: PathBuf) {
    let projects = state.projects.clone();
    let res = tokio::task::spawn_blocking(move || projects.touch(&path)).await;
    if let Ok(Err(err)) = res {
        tracing::warn!(component = "api", %err, "更新项目最近使用失败");
    }
}

fn announce_created(state: &AppState, view: &SessionView) -> Value {
    let exported = export(&state.node, view);
    state.events.publish(Event::SessionCreated {
        id: global_id(&state.node, &view.record.id),
        session: exported.clone(),
    });
    exported
}

// ---------- 改 ----------

#[derive(Debug, Deserialize)]
pub struct PatchBody {
    #[serde(default)]
    pub display_name: Option<String>,
}

/// 现在只有改名（改成同名也落锁，§4.5）；Session Settings 的其它字段随 agora-xqa.11。
pub async fn patch(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    Json(body): Json<PatchBody>,
) -> Result<Json<Value>, ApiError> {
    let id = local_id(&state, &gid)?;
    let Some(name) = body.display_name else {
        return Err(bad_request("没有可改的字段（display_name）"));
    };
    if name.trim().is_empty() {
        return Err(bad_request("display_name 不能为空"));
    }
    let view = blocking(&state.sessions, move |s| s.rename(&id, &name)).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, "改名");
    let session = export(&state.node, &view);
    state.events.publish(Event::SessionUpdated {
        id: global_id(&state.node, &view.record.id),
        session: session.clone(),
    });
    Ok(Json(session))
}

// ---------- 生命周期 ----------

#[derive(Debug, Default, Deserialize)]
pub struct ConfirmBody {
    #[serde(default)]
    pub confirmed: bool,
}

/// 没带 body 的 POST 也要能用（curl -X POST），所以 body 可选。
type MaybeConfirm = Option<Json<ConfirmBody>>;

fn confirmed(body: &MaybeConfirm) -> bool {
    body.as_ref().is_some_and(|b| b.confirmed)
}

/// 会杀且未确认 → 409。判断用当前视图，执行前再看一次；两次之间进程自己退出了也无妨：
/// 那时 terminate 是空操作。
async fn require_confirmation(
    state: &AppState,
    id: &str,
    confirmed: bool,
    action: &'static str,
) -> Result<(), ApiError> {
    let id_owned = id.to_owned();
    let view = blocking(&state.sessions, move |s| s.get(&id_owned)).await?;
    if view.would_kill() && !confirmed {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            kind: "needs_confirmation",
            message: format!(
                "{action} 会杀掉正在运行的 agent（状态 {:?}）；确认后带 confirmed: true 重发",
                view.assessment.status
            ),
        });
    }
    Ok(())
}

pub async fn kill(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    body: MaybeConfirm,
) -> Result<Json<Value>, ApiError> {
    let id = local_id(&state, &gid)?;
    require_confirmation(&state, &id, confirmed(&body), "Kill").await?;
    let view = blocking(&state.sessions, move |s| s.kill(&id)).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, "kill");
    Ok(Json(export(&state.node, &view)))
}

pub async fn restart(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    body: MaybeConfirm,
) -> Result<Json<Value>, ApiError> {
    let id = local_id(&state, &gid)?;
    require_confirmation(&state, &id, confirmed(&body), "Restart").await?;
    let view = blocking(&state.sessions, move |s| s.restart(&id, &[])).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, epoch = view.record.epoch, "restart");
    Ok(Json(export(&state.node, &view)))
}

/// 回收已退出会话的运行时会话与输出；活着 → 409 `still_alive`。
pub async fn cleanup(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = local_id(&state, &gid)?;
    let log_id = id.clone();
    blocking(&state.sessions, move |s| s.cleanup(&id)).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %log_id, "cleanup");
    Ok(StatusCode::NO_CONTENT)
}

/// 只删 metadata，绝不杀进程（MISSION §7.3）；已退出的顺手清理。
pub async fn delete(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = local_id(&state, &gid)?;
    let log_id = id.clone();
    blocking(&state.sessions, move |s| s.delete_metadata(&id)).await?;
    state.events.publish(Event::SessionRemoved {
        id: global_id(&state.node, &log_id),
    });
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %log_id, "删除 metadata");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/sessions/:id/input`（MISSION §7.3；ADR-002 D5）。
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputBody {
    /// 经挂起的 hook 返回给 agent，不注入键击。
    Decision {
        decision: DecisionKind,
        #[serde(default)]
        message: Option<String>,
        /// 并行工具时指定答哪一个；缺省最早的。
        #[serde(default)]
        tool_use_id: Option<String>,
    },
    /// 经 PTY 写入：自由问答、下一条指令、无 hook 的 agent。
    Text { data: String },
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Allow,
    Deny,
}

pub async fn input(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    Json(body): Json<InputBody>,
) -> Result<Json<Value>, ApiError> {
    let id = local_id(&state, &gid)?;
    match body {
        InputBody::Decision {
            decision,
            message,
            tool_use_id,
        } => {
            let d = match decision {
                DecisionKind::Allow => crate::adapter::Decision::Allow,
                DecisionKind::Deny => crate::adapter::Decision::Deny { message },
            };
            let key = state
                .hooks
                .as_ref()
                .ok_or(())
                .and_then(|h| h.respond(&id, tool_use_id.as_deref(), d).map_err(|_| ()))
                .map_err(|()| ApiError {
                    status: StatusCode::CONFLICT,
                    kind: "no_pending_decision",
                    message: format!("会话 {id} 没有挂起的决定；在终端回答，或等它再问一次"),
                })?;
            tracing::info!(component = "api", principal = %principal.log_id(), session_id = %id, tool_use_id = %key, "decision");
            Ok(Json(serde_json::json!({ "tool_use_id": key })))
        }
        InputBody::Text { data } => {
            let sid = id.clone();
            blocking(&state.sessions, move |s| s.send_input(&sid, &data)).await?;
            tracing::info!(component = "api", principal = %principal.log_id(), session_id = %id, "text");
            Ok(Json(serde_json::json!({})))
        }
    }
}

fn bad_request(message: &str) -> ApiError {
    ApiError {
        status: StatusCode::BAD_REQUEST,
        kind: "bad_request",
        message: message.to_owned(),
    }
}

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        use crate::runtime::RuntimeError;
        let (status, kind) = match &e {
            SessionError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            SessionError::StillAlive(_) => (StatusCode::CONFLICT, "still_alive"),
            SessionError::NoRuntime(_) => (StatusCode::CONFLICT, "no_runtime"),
            SessionError::AlreadyRegistered(_) => (StatusCode::CONFLICT, "already_registered"),
            SessionError::Runtime(RuntimeError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "runtime_session_not_found")
            }
            SessionError::Runtime(RuntimeError::StillAlive(_)) => {
                (StatusCode::CONFLICT, "still_alive")
            }
            SessionError::Runtime(RuntimeError::ReadOnly(_)) => (StatusCode::CONFLICT, "read_only"),
            SessionError::Runtime(_) => (StatusCode::BAD_GATEWAY, "runtime"),
            SessionError::Db(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database"),
        };
        if status.is_server_error() {
            tracing::error!(component = "api", error = %e, "会话操作失败");
        }
        ApiError {
            status,
            kind,
            message: e.to_string(),
        }
    }
}
