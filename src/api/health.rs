//! `GET /api/health` 与 `GET /api/system`。
//!
//! health 未认证只回 `{ "status": "ok" }`（MISSION §10.3；ADR-003 D1 白名单），带 principal
//! 才是完整报告——这里刻意不让未认证请求看到任何能区分节点配置的字段。
//! 运行时的 degraded 是**实时**结论：每次读运行时的成败都记进 `RuntimeStatus`，这里现算
//! （agora-xqa.4）。所以运行时升级导致的失明会在 server 换代后自愈，不必重启 daemon。

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
        "tls": Value::Null,
        "push": { "apple": false, "fcm": false },
        "peers": {},
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
