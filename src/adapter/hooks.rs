//! 各宿主 `AgentHooks` 实现共用的零件（ADR-002 D2/D3/D5）。
//!
//! 决定的形态（[`Decision`]）、解除的形态（[`Release`]）、payload 取键的小工具与挂起判定。
//! 三家的表各在自己的文件（`claude.rs` / `codex.rs` / `grok.rs`）：键名双写 camel / snake 都认，
//! 因为 Grok 双写、Claude 与 Codex 用 snake；事件名 Claude / Codex 是 CamelCase，Grok 实测是
//! 小写蛇形（2026-09-04），所以没有一张能三家通用的表。

use serde_json::Value;

/// 默认的挂起上限（ADR-002 D5）：安装的 hook timeout 3600 s 减余量，agora 永远先于 agent 退出。
pub const DEFAULT_HOLD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(55 * 60);

/// 环境里哪些变量名前缀属于 agent 自己、值得进信封（`CLAUDE_PID`、`GROK_*`、`CODEX_*`）。
pub fn agent_env_prefixes() -> &'static [&'static str] {
    &["CLAUDE_", "GROK_", "CODEX_"]
}

/// Dashboard 的决定（`decision.behavior`），经挂起的 hook 同步返回给 agent。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny {
        message: Option<String>,
    },
    /// 没有决定（超时、上限、终端答了、进程退出）：hook 静默退出，TUI 的提示还在。
    None,
}

/// 一条事件到达时会解除哪些挂起（ADR-002 D5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// 同工具的 PostToolUse / PostToolUseFailure / PermissionDenied：终端里答了。列表是候选键：
    /// `tool_use_id` 与 `tool_name`——实测 2026-09-04 Claude 2.1.260 的 PermissionRequest **不带**
    /// `tool_use_id`，挂起只能按 `tool_name` 键，解除时两个键都试。
    ToolUse(Vec<String>),
    /// Stop / SessionEnd：这一轮结束，所有挂起都过期。
    Session,
    None,
}

pub(crate) fn str_of<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| payload.get(k).and_then(Value::as_str))
}

pub(crate) fn event_name(payload: &Value) -> Option<&str> {
    str_of(payload, &["hook_event_name", "hookEventName"])
}

/// payload 的 `cwd`（Claude / Codex / Grok 三家的事件都带）。
pub(crate) fn cwd(payload: &Value) -> Option<std::path::PathBuf> {
    str_of(payload, &["cwd"]).map(std::path::PathBuf::from)
}

pub(crate) fn session_id(payload: &Value) -> Option<String> {
    str_of(payload, &["session_id", "sessionId"]).map(str::to_owned)
}

pub(crate) fn tool_use_id(payload: &Value) -> Option<String> {
    str_of(payload, &["tool_use_id", "toolUseId", "call_id"]).map(str::to_owned)
}

/// `PermissionRequest` 决定写回 stdout 的形态：`hookSpecificOutput.decision.behavior`，Claude
/// 与 Codex 0.152.1（2026-09-05 实测放行）都认。`None` → 不输出（fail-open）：Codex 要等 hook
/// 退出后才弹自己的审批提示，所以 None 必须尽快退出。
pub(crate) fn permission_output(decision: &Decision) -> Option<String> {
    let decision = match decision {
        Decision::Allow => serde_json::json!({ "behavior": "allow" }),
        Decision::Deny { message } => match message {
            Some(m) => serde_json::json!({ "behavior": "deny", "message": m }),
            None => serde_json::json!({ "behavior": "deny" }),
        },
        Decision::None => return None,
    };
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": decision,
            }
        })
        .to_string(),
    )
}

/// 挂起 / 待决的键：`tool_use_id`，没有就 `tool_name`（Claude 2.1.260 的 PermissionRequest
/// 实测只有后者），再没有用事件名占位，免得一个会话里只能挂一个。按 tool_name 键的代价：
/// 并行的两个同名工具分不开，后到的接替先到的（`Receiver::hold` 同键接替）。
pub(crate) fn decision_key(payload: &Value) -> String {
    tool_use_id(payload)
        .or_else(|| str_of(payload, &["tool_name", "toolName"]).map(str::to_owned))
        .unwrap_or_else(|| "PermissionRequest".into())
}

/// 通用的挂起判定：只有能替用户批准的宿主的 `PermissionRequest`。
pub(crate) fn generic_hold_key(decision_via_hook: bool, payload: &Value) -> Option<String> {
    if !decision_via_hook || event_name(payload) != Some("PermissionRequest") {
        return None;
    }
    Some(decision_key(payload))
}

/// PostToolUse 等解除时的候选键：id 与工具名都算。
pub(crate) fn release_keys(payload: &Value) -> Vec<String> {
    let mut keys: Vec<String> = tool_use_id(payload).into_iter().collect();
    if let Some(name) = str_of(payload, &["tool_name", "toolName"]) {
        keys.push(name.to_owned());
    }
    keys
}

pub(crate) fn generic_release(payload: &Value) -> Release {
    match event_name(payload) {
        Some("PostToolUse" | "PostToolUseFailure" | "PermissionDenied") => {
            let keys = release_keys(payload);
            if keys.is_empty() {
                Release::None
            } else {
                Release::ToolUse(keys)
            }
        }
        Some("Stop" | "SessionEnd") => Release::Session,
        _ => Release::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_permission_requests_of_capable_hosts_hold() {
        let pr = json!({ "hook_event_name": "PermissionRequest", "tool_use_id": "t1" });
        assert_eq!(generic_hold_key(true, &pr).as_deref(), Some("t1"));
        assert_eq!(generic_hold_key(false, &pr), None);
        assert_eq!(
            generic_hold_key(true, &json!({ "hook_event_name": "Stop" })),
            None
        );
        // 2.1.260 实测：没有 tool_use_id，按 tool_name 键；什么都没有才用事件名占位。
        assert_eq!(
            generic_hold_key(
                true,
                &json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" })
            )
            .as_deref(),
            Some("Bash")
        );
        assert_eq!(
            generic_hold_key(true, &json!({ "hook_event_name": "PermissionRequest" })).as_deref(),
            Some("PermissionRequest")
        );
    }

    #[test]
    fn releases_follow_adr_002_d5() {
        assert_eq!(
            generic_release(
                &json!({ "hook_event_name": "PostToolUse", "tool_use_id": "t1", "tool_name": "Bash" })
            ),
            Release::ToolUse(vec!["t1".into(), "Bash".into()])
        );
        assert_eq!(
            generic_release(&json!({ "hookEventName": "PermissionDenied", "toolUseId": "t2" })),
            Release::ToolUse(vec!["t2".into()])
        );
        assert_eq!(
            generic_release(&json!({ "hook_event_name": "Stop" })),
            Release::Session
        );
        assert_eq!(
            generic_release(&json!({ "hook_event_name": "SessionEnd" })),
            Release::Session
        );
        assert_eq!(
            generic_release(&json!({ "hook_event_name": "PreToolUse" })),
            Release::None
        );
    }

    #[test]
    fn decision_output_is_the_documented_shape_or_nothing() {
        let out = permission_output(&Decision::Allow).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let out = permission_output(&Decision::Deny {
            message: Some("no".into()),
        })
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["message"], "no");
        assert_eq!(permission_output(&Decision::None), None);
    }
}
