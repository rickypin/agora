//! hook 侧的 agent 特定知识（ADR-002 D2/D3/D4/D5；agora-dvh.3）。
//!
//! `agora hook` 与 daemon 的投递箱只知道"一个宿主、一坨 JSON"；宿主叫什么、payload 里
//! 会话 id 叫什么键、哪个事件要挂起等决定、决定要以什么形态写回 stdout——全在这里。
//! 本阶段先钉住投递链路需要的最小集合；完整的 `AgentHooks` trait（事件映射、install_spec）
//! 随 agora-dvh.5 补上，届时这些函数归入各 agent 的实现体。

use serde_json::Value;

/// 三个一等 agent 的宿主名（`--host` 的取值，也是投递箱的第一层目录）。
pub const HOSTS: &[&str] = &["claude", "grok", "codex"];

pub fn is_host(host: &str) -> bool {
    HOSTS.contains(&host)
}

/// 宿主自认（ADR-002 D4）：Grok 默认兼容加载 `~/.claude/settings.json` 里的 hooks，agora
/// 装给 Claude 的条目会被 Grok 以 `--host claude` 跑一遍。判定只看环境，不按 payload
/// 键名风格猜：`GROK_SESSION_ID` 存在而 `--host` 不是 grok → 不是自己；`--host grok`
/// 而它不存在 → 也不是自己。
pub fn host_matches_env(host: &str, has_grok_session: bool) -> bool {
    (host == "grok") == has_grok_session
}

/// 环境里哪些变量名前缀属于 agent 自己、值得进信封（`CLAUDE_PID`、`GROK_*`、`CODEX_*`）。
pub fn agent_env_prefixes() -> &'static [&'static str] {
    &["CLAUDE_", "GROK_", "CODEX_"]
}

/// 宿主的 hook 是否能替用户批准（ADR-002 D2 "没有的能力就写没有"）。
pub fn decision_via_hook(host: &str) -> bool {
    matches!(host, "claude" | "codex")
}

/// payload 里 agent 自报的对话 id：Claude / Codex 是 `session_id`，Grok 双写 camel / snake。
pub fn agent_session_id(payload: &Value) -> Option<String> {
    ["session_id", "sessionId"]
        .iter()
        .find_map(|k| payload.get(k).and_then(Value::as_str))
        .map(str::to_owned)
}

pub fn event_name(payload: &Value) -> Option<&str> {
    ["hook_event_name", "hookEventName"]
        .iter()
        .find_map(|k| payload.get(k).and_then(Value::as_str))
}

fn tool_use_id(payload: &Value) -> Option<String> {
    ["tool_use_id", "toolUseId", "call_id"]
        .iter()
        .find_map(|k| payload.get(k).and_then(Value::as_str))
        .map(str::to_owned)
}

/// 这条事件要不要挂起等 daemon 的决定：只有 `decision_via_hook` 宿主的 `PermissionRequest`。
/// 返回挂起键里的 `tool_use_id`（Claude 并行工具调用会同时 fire 多个，D5）。
pub fn hold_key(host: &str, payload: &Value) -> Option<String> {
    if !decision_via_hook(host) || event_name(payload) != Some("PermissionRequest") {
        return None;
    }
    // 没带 tool_use_id 的（不该发生）也挂，用事件名占位，免得一个会话里只能挂一个。
    Some(tool_use_id(payload).unwrap_or_else(|| "PermissionRequest".into()))
}

/// 一条事件到达时会解除哪些挂起（ADR-002 D5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// 同 `tool_use_id` 的 PostToolUse / PostToolUseFailure / PermissionDenied：终端里答了。
    ToolUse(String),
    /// Stop / SessionEnd：这一轮结束，所有挂起都过期。
    Session,
    None,
}

pub fn release_for(payload: &Value) -> Release {
    match event_name(payload) {
        Some("PostToolUse" | "PostToolUseFailure" | "PermissionDenied") => tool_use_id(payload)
            .map(Release::ToolUse)
            .unwrap_or(Release::None),
        Some("Stop" | "SessionEnd") => Release::Session,
        _ => Release::None,
    }
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

/// 决定写回 stdout 的形态。Claude Code 文档的 PermissionRequest 输出是
/// `hookSpecificOutput.decision.behavior`；Codex 文档同为 `decision.behavior`，本机未实测
/// （agora-dvh.2 / dvh.7 核实后若形态不同在这里分叉）。`None` → 不输出（fail-open）。
pub fn decision_output(host: &str, decision: &Decision) -> Option<String> {
    if !decision_via_hook(host) {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn host_self_recognition_follows_grok_env_only() {
        assert!(host_matches_env("claude", false));
        assert!(!host_matches_env("claude", true));
        assert!(host_matches_env("grok", true));
        assert!(!host_matches_env("grok", false));
        assert!(host_matches_env("codex", false));
    }

    #[test]
    fn only_permission_requests_of_capable_hosts_hold() {
        let pr = json!({ "hook_event_name": "PermissionRequest", "tool_use_id": "t1" });
        assert_eq!(hold_key("claude", &pr).as_deref(), Some("t1"));
        assert_eq!(hold_key("codex", &pr).as_deref(), Some("t1"));
        // Grok 的 allow 不能替用户批准：不挂。
        assert_eq!(hold_key("grok", &pr), None);
        let stop = json!({ "hook_event_name": "Stop" });
        assert_eq!(hold_key("claude", &stop), None);
    }

    #[test]
    fn releases_follow_adr_002_d5() {
        assert_eq!(
            release_for(&json!({ "hook_event_name": "PostToolUse", "tool_use_id": "t1" })),
            Release::ToolUse("t1".into())
        );
        assert_eq!(
            release_for(&json!({ "hookEventName": "PermissionDenied", "toolUseId": "t2" })),
            Release::ToolUse("t2".into())
        );
        assert_eq!(
            release_for(&json!({ "hook_event_name": "Stop" })),
            Release::Session
        );
        assert_eq!(
            release_for(&json!({ "hook_event_name": "SessionEnd" })),
            Release::Session
        );
        assert_eq!(
            release_for(&json!({ "hook_event_name": "PreToolUse" })),
            Release::None
        );
    }

    #[test]
    fn decision_output_is_the_documented_shape_or_nothing() {
        let out = decision_output("claude", &Decision::Allow).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let out = decision_output(
            "claude",
            &Decision::Deny {
                message: Some("no".into()),
            },
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["message"], "no");
        assert_eq!(decision_output("claude", &Decision::None), None);
        assert_eq!(decision_output("grok", &Decision::Allow), None);
    }

    #[test]
    fn session_id_accepts_both_key_styles() {
        assert_eq!(
            agent_session_id(&json!({ "session_id": "a" })).as_deref(),
            Some("a")
        );
        assert_eq!(
            agent_session_id(&json!({ "sessionId": "b" })).as_deref(),
            Some("b")
        );
        assert_eq!(agent_session_id(&json!({})), None);
    }
}
