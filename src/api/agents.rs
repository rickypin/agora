//! `GET /api/agents`：New Agent 对话框的 Agent 下拉（MISSION §6.4 / §5.2）。
//!
//! 名字与默认命令是 Adapter 的启动侧事实（ADR-002 D9），配置里的 `agents.<name>.command`
//! 覆盖它。前端因此不写死任何 agent 名——写死的话，改一个默认命令要同时改两处，而
//! `tests/arch_boundary.rs` 只守得住 `src/`。每项另带 `prompt: bool`——接不接受首条 prompt
//! （agora-h1k.2），同样是 Adapter 说的。

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
                // 接不接受首条 prompt（MISSION §6.4 从就绪任务起会话；A43）：对话框只对 true 的
                // agent 显示 Prompt 框；前端自己加的 custom 项没有 Adapter，视为 false。
                "prompt": a.accepts_initial_prompt(),
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
