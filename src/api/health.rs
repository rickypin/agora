//! `GET /api/health` 与 `GET /api/system`。
//!
//! health 未认证只回 `{ "status": "ok" }`（MISSION §10.3；ADR-003 D1 白名单），带 principal
//! 才是完整报告——这里刻意不让未认证请求看到任何能区分节点配置的字段。
//! 运行时的 degraded 是**实时**结论：每次读运行时的成败都记进 `RuntimeStatus`，这里现算
//! （agora-xqa.4）。所以运行时升级导致的失明会在 server 换代后自愈，不必重启 daemon。
//! peers 段是 `AppState::peers` 的快照（src/peer/state.rs）：peer 客户端在连接成败时写，
//! 这里只读——health 不去连任何 peer。

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use super::version::{SystemInfo, API_VERSION};
use super::AppState;
use crate::auth::Principal;

/// `/api/health` 里 runtime 那一段（ADR-001 D7）。status / reason 每次请求现算，
/// path_source 是启动时探到的、不会变。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeHealth {
    pub status: &'static str,
    pub reason: Option<String>,
    pub path_source: &'static str,
}

impl RuntimeHealth {
    fn now(reason: Option<String>, path_source: &'static str) -> Self {
        RuntimeHealth {
            status: if reason.is_some() { "degraded" } else { "ok" },
            reason,
            path_source,
        }
    }
}

pub async fn health(principal: Option<Principal>, State(state): State<AppState>) -> Json<Value> {
    let Some(_principal) = principal else {
        return Json(json!({ "status": "ok" }));
    };
    let database = state
        .sessions
        .db()
        .conn()
        .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
        .is_ok();
    let runtime = RuntimeHealth::now(
        state.sessions.runtime_status().reason(),
        state.runtime_path_source,
    );
    Json(json!({
        "status": "ok",
        "runtime": runtime,
        "database": database,
        // "self-signed" / "external" / null（没开 TLS 监听器）；`main.rs` 按 tls.mode 填（agora-ltb）。
        "tls": state.tls_mode,
        "push": { "apple": false, "fcm": false },
        // 节点名 → PeerState（src/peer/state.rs）：online / last_seen / retrying / last_error，
        // 错误按类型；配置了却还没连上的 peer 也在（agora-7ku.12）。
        "peers": state.peers.snapshot(),
    }))
}

/// `GET /api/system`：调用方读任何业务数据之前先拿这个比版本（MISSION §7.3；
/// 规则与判定在 `super::version`，docs/spec/api.md「api_version 兼容规则」）。
/// 响应是有类型的 [`SystemInfo`]，peer 客户端用同一个类型反序列化，形态只在一处定义。
pub async fn system(_principal: Principal, State(state): State<AppState>) -> Json<SystemInfo> {
    Json(SystemInfo {
        api_version: API_VERSION,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        node: state.node.to_string(),
    })
}
