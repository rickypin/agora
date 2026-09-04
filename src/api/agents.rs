//! `GET /api/agents`：New Agent 对话框的 Agent 下拉（MISSION §6.4 / §5.2）。
//!
//! 名字与默认命令是 Adapter 的启动侧事实（ADR-002 D9），配置里的 `agents.<name>.command`
//! 覆盖它。前端因此不写死任何 agent 名——写死的话，改一个默认命令要同时改两处，而
//! `tests/arch_boundary.rs` 只守得住 `src/`。

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use super::{ApiError, AppState};
use crate::adapter::ADAPTERS;
use crate::auth::Principal;

pub async fn list(
    _principal: Principal,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let agents: Vec<Value> = ADAPTERS
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name(),
                "command": command_for(&state, a.name(), a.default_command()),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "agents": agents })))
}

/// 配置覆盖 > Adapter 默认。与 `POST /api/sessions` 的缺省链是同一条，改一处要改两处。
fn command_for(state: &AppState, name: &str, fallback: &str) -> String {
    state
        .agents
        .get(name)
        .and_then(|a| a.command.clone())
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}
