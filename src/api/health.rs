//! `GET /api/health` 与 `GET /api/system`。
//!
//! health 未认证只回 `{ "status": "ok" }`（MISSION §10.3；ADR-003 D1 白名单），带 principal
//! 才是完整报告——这里刻意不让未认证请求看到任何能区分节点配置的字段。
//! 运行时的 degraded 判定随 agora-xqa.4 变成实时探测；现在是启动时的一次结论。

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use super::AppState;
use crate::auth::Principal;

/// 启动时得出的运行时健康（ADR-001 D7）。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeHealth {
    pub status: &'static str,
    pub reason: Option<String>,
    pub path_source: &'static str,
}

impl Default for RuntimeHealth {
    fn default() -> Self {
        RuntimeHealth {
            status: "ok",
            reason: None,
            path_source: "daemon",
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
    Json(json!({
        "status": "ok",
        "runtime": &*state.runtime_health,
        "database": database,
        "tls": Value::Null,
        "push": { "apple": false, "fcm": false },
        "peers": {},
    }))
}

/// 调用方版本不一致时必须识别并降级或提示，不得静默错读（MISSION §7.3）。
pub const API_VERSION: u32 = 1;

pub async fn system(_principal: Principal, State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "api_version": API_VERSION,
        "version": env!("CARGO_PKG_VERSION"),
        "node": &*state.node,
    }))
}
