//! `/api/sessions*`（docs/spec/api.md；MISSION §4.6 §7.3 §8）。
//!
//! 每个 handler 第一个参数是 `Principal`（ADR-003 D1）。Session Manager 的方法会起运行时
//! 子进程，一律放 `spawn_blocking`。会话 id 对外 `<node>:<id>`；写操作对 peer 会话经
//! `forward` 一跳转发到所属节点（agora-7ku.7），既不是本机也不是 peer 的前缀报 `node_unknown`。
//!
//! Kill / Restart 的确认跟着"杀"走（MISSION §8）：会杀且没带 `confirmed: true` →
//! 409 `needs_confirmation`；不会杀（FINISHED / FAILED / 会话已不在）直接执行。判断在
//! 会话所属节点做，转发节点不能代替。

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{forward, ApiError, AppState};
use crate::adapter::RestartPlan;
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
                "会话属于节点 {node}，本节点是 {}：既不是本机也不是可读的 peer 视图",
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
///
/// 人看到本机行 + 并入的全部 peer 行（离线 peer 的行仍在、带 `stale: true`，不变量 8）；**peer
/// 来拉只给本机行**——只导出本机会话、一跳防环（MISSION §3.5；docs/spec/api.md「peer 视图」；
/// 守卫 `tests/peer_view.rs::only_local_sessions_are_exported_one_hop`）。未登记会话同理只有本机的：
/// 采纳是写操作，peer 的未登记会话要采纳得去它自己的节点。
pub async fn list(
    principal: Principal,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let node = state.node.clone();
    let (views, unregistered) =
        blocking(&state.sessions, |s| Ok((s.list()?, s.unregistered()?))).await?;
    let mut sessions: Vec<Value> = views.iter().map(|v| export(&node, v)).collect();
    if matches!(principal, Principal::Human { .. }) {
        sessions.extend(state.peer_views.rows());
    }
    let unregistered: Vec<Value> = unregistered
        .iter()
        .map(|u| {
            let s = &u.session;
            serde_json::json!({
                "runtime_ref": s.r#ref.0,
                "name": s.name,
                "title": s.title,
                "alive": s.alive,
                "managed": s.managed,
                "working_directory": s.cwd,
                "agent_hint": u.agent_hint,
                "node": &*node,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "sessions": sessions,
        "unregistered": unregistered,
    })))
}

/// 本机会话直达 SessionManager；peer 会话给人看的是并入视图里的那一行（含 `stale`），不为一次
/// 读去转发。peer 来问 peer 的会话仍是 `node_unknown`（一跳，与 `list` 同一条规则）。
pub async fn get(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if let Some((node, _)) = gid.split_once(':') {
        let is_peer = matches!(
            state.registry.route(node),
            crate::peer::registry::Route::Peer(_)
        );
        if is_peer && matches!(principal, Principal::Human { .. }) {
            // 人点开 stale peer 的会话：插一次重试（agora-7ku.6，与 forward::hop 同一句）。读仍从
            // 视图来——给的是最后一眼（带 stale / last_seen），重连成功后事件流会把行刷新。
            if state.peer_views.is_stale(node) == Some(true) {
                state.peer_views.retry_now(node);
            }
            return state
                .peer_views
                .get(&gid)
                .map(Json)
                .ok_or_else(|| ApiError {
                    status: StatusCode::NOT_FOUND,
                    kind: "not_found",
                    message: format!("peer {node} 的视图里没有会话 {gid}"),
                });
        }
    }
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
    // 钉死对话 id（D7 识别顺序第二位）：hook 自报会覆盖它；没 hook 的会话靠它 resume。
    let st = state.clone();
    let view = blocking(&state.sessions, move |s| {
        let pin = crate::adapter::plan_pin(&new.agent_type, &new.command, |adapter, program| {
            st.probe_version(adapter, program)
        });
        let mut new = new;
        let pinned = pin.map(|(command, id)| {
            new.command = command;
            id
        });
        let view = s.create(&new)?;
        if let Some(id) = pinned {
            s.set_pinned_agent_session_id(&view.record.id, &id)?;
            return s.get(&view.record.id);
        }
        Ok(view)
    })
    .await?;
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

#[derive(Debug, Deserialize, Serialize)]
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
) -> Result<Response, ApiError> {
    let id = match forward::route(&state, &principal, &gid, Method::PATCH, "", Some(&body)).await? {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
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
    Ok(Json(session).into_response())
}

// ---------- 生命周期 ----------

#[derive(Debug, Default, Deserialize, Serialize)]
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
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::POST,
        "/kill",
        body.as_deref(),
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    require_confirmation(&state, &id, confirmed(&body), "Kill").await?;
    let view = blocking(&state.sessions, move |s| s.kill(&id)).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, "kill");
    Ok(Json(export(&state.node, &view)).into_response())
}

/// Restart 退化原因在 API 响应里的长度上限（字符）；日志不截。
const RESTART_REASON_MAX: usize = 120;

pub async fn restart(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
    body: MaybeConfirm,
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::POST,
        "/restart",
        body.as_deref(),
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    require_confirmation(&state, &id, confirmed(&body), "Restart").await?;
    // resume 命令按 Adapter 算（ADR-002 D7）：探版本会跑子进程，整段放 blocking 线程。
    let st = state.clone();
    let (view, plan) = blocking(&state.sessions, move |s| {
        let rec = s.get(&id)?.record;
        let plan = crate::adapter::plan_restart(
            &rec.agent_type,
            rec.command.as_deref().unwrap_or_default(),
            rec.agent_session_id.as_deref(),
            |adapter, program| st.probe_version(adapter, program),
        );
        let view = s.restart_with(&id, &[], Some(plan.command()))?;
        Ok((view, plan))
    })
    .await?;
    let resume = match &plan {
        RestartPlan::Resume {
            agent_session_id, ..
        } => {
            tracing::info!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, epoch = view.record.epoch, agent_session_id = %agent_session_id, "restart, resume");
            serde_json::json!({ "resumed": true, "agent_session_id": agent_session_id })
        }
        RestartPlan::Original { reason, .. } => {
            tracing::warn!(component = "api", principal = %principal.log_id(), session_id = %view.record.id, epoch = view.record.epoch, %reason, "restart 退化为原命令");
            // reason 可能带子进程的整段 stderr（fake 命令探版本时吐的 usage）：API 只给首行、截到
            // 120 字符，Dashboard 一行放得下；全文在上面那条日志里（agora-k9r，2026-09-05）。
            serde_json::json!({
                "resumed": false,
                "reason": crate::adapter::hooks::first_line_capped(reason, RESTART_REASON_MAX)
            })
        }
    };
    let mut out = export(&state.node, &view);
    out["restart"] = resume;
    Ok(Json(out).into_response())
}

/// 回收已退出会话的运行时会话与输出；活着 → 409 `still_alive`。
pub async fn cleanup(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::POST,
        "/cleanup",
        forward::NO_BODY,
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    let log_id = id.clone();
    blocking(&state.sessions, move |s| s.cleanup(&id)).await?;
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %log_id, "cleanup");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// 只删 metadata，绝不杀进程（MISSION §7.3）；已退出的顺手清理。
pub async fn delete(
    principal: Principal,
    State(state): State<AppState>,
    Path(gid): Path<String>,
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::DELETE,
        "",
        forward::NO_BODY,
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    let log_id = id.clone();
    blocking(&state.sessions, move |s| s.delete_metadata(&id)).await?;
    state.events.publish(Event::SessionRemoved {
        id: global_id(&state.node, &log_id),
    });
    tracing::info!(component = "api", principal = %principal.log_id(), session_id = %log_id, "删除 metadata");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/sessions/:id/input`（MISSION §7.3；ADR-002 D5）。
#[derive(Deserialize, Serialize)]
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
        /// Dashboard 必带展示的挂起实例 ID；工具名可能在下一次请求复用。
        #[serde(default)]
        request_id: Option<String>,
    },
    /// 经 PTY 写入：自由问答、下一条指令、无 hook 的 agent。
    Text { data: String },
}

#[derive(Deserialize, Serialize, Clone, Copy)]
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
) -> Result<Response, ApiError> {
    let id = match forward::route(
        &state,
        &principal,
        &gid,
        Method::POST,
        "/input",
        Some(&body),
    )
    .await?
    {
        forward::Routed::Local(id) => id,
        forward::Routed::Forwarded(resp) => return Ok(resp),
    };
    match body {
        InputBody::Decision {
            decision,
            message,
            tool_use_id,
            request_id,
        } => {
            let d = match decision {
                DecisionKind::Allow => crate::adapter::Decision::Allow,
                DecisionKind::Deny => crate::adapter::Decision::Deny { message },
            };
            let hooks = state.hooks.clone();
            let sid = id.clone();
            let key = tokio::task::spawn_blocking(move || {
                hooks
                    .as_ref()
                    .ok_or_else(|| crate::hook::RespondError::NoPendingDecision {
                        session: sid.clone(),
                        tool_use_id: String::new(),
                    })
                    .and_then(|h| match request_id.as_deref() {
                        Some(request_id) => h.respond_request(&sid, request_id, d),
                        None => h.respond(&sid, tool_use_id.as_deref(), d),
                    })
            })
            .await
            .map_err(|err| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                kind: "internal",
                message: format!("hook worker: {err}"),
            })?
            .map_err(|err| match err {
                crate::hook::RespondError::NoPendingDecision { .. } => ApiError {
                    status: StatusCode::CONFLICT,
                    kind: "no_pending_decision",
                    message: format!("会话 {id} 没有挂起的决定；在终端回答，或等它再问一次"),
                },
                crate::hook::RespondError::Checkpoint(message) => ApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    kind: "hook_state",
                    message,
                },
            })?;
            tracing::info!(component = "api", principal = %principal.log_id(), session_id = %id, tool_use_id = %key, "decision");
            Ok(Json(serde_json::json!({ "tool_use_id": key })).into_response())
        }
        InputBody::Text { data } => {
            let sid = id.clone();
            blocking(&state.sessions, move |s| s.send_input(&sid, &data)).await?;
            tracing::info!(component = "api", principal = %principal.log_id(), session_id = %id, "text");
            Ok(Json(serde_json::json!({})).into_response())
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
            SessionError::NoCommand(_) => (StatusCode::CONFLICT, "no_command"),
            SessionError::AlreadyRegistered(_) => (StatusCode::CONFLICT, "already_registered"),
            SessionError::Runtime(RuntimeError::NotFound(_)) => {
                (StatusCode::NOT_FOUND, "runtime_session_not_found")
            }
            SessionError::Runtime(RuntimeError::StillAlive(_)) => {
                (StatusCode::CONFLICT, "still_alive")
            }
            SessionError::Runtime(RuntimeError::ReadOnly(_)) => (StatusCode::CONFLICT, "read_only"),
            SessionError::Runtime(_) => (StatusCode::BAD_GATEWAY, "runtime"),
            SessionError::HookState(_) => (StatusCode::INTERNAL_SERVER_ERROR, "hook_state"),
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
