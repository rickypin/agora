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

/// 权限摘要的上限（字符数）：Respond 区一行能读完的量，多的换行也没意义。
pub(crate) const PERMISSION_SUMMARY_MAX: usize = 200;

/// `tool_input` 里"这次调用到底要干什么"的键，按先后取第一个有字符串值的（agora-pzi）：
/// Bash / run_terminal_command 是 `command`，Write / Edit / Read / NotebookEdit 是 `file_path`
/// （Codex 的 apply_patch 只有 `patch`），MCP 与网页类工具通常有 `url` / `pattern` / `prompt`。
/// `description` 放最后：Bash 同时带 command 与 description，人要看的是命令本身。
const PRIMARY_INPUT_KEYS: &[&str] = &[
    "command",
    "file_path",
    "notebook_path",
    "path",
    "url",
    "pattern",
    "patch",
    "prompt",
    "query",
    "description",
];

/// `PermissionRequest` 给 Dashboard 看的摘要：`<tool>: <主参数首行>`，超长截断加 `…`；没有
/// `tool_input` 就只剩工具名（agora-pzi，2026-09-05）。只给工具名时用户在 Dashboard 上看不出
/// agent 要执行什么，只能盲点 Allow 或去开终端，MISSION A15"不打开终端即可回答"就名存实亡。
/// 主参数取 [`PRIMARY_INPUT_KEYS`]（AskUserQuestion 另取 `questions[0].question`），都没有就
/// 把整个 `tool_input` 压成紧凑 JSON 再截——总比只剩工具名多一点线索。
pub(crate) fn permission_summary(payload: &Value) -> String {
    let tool = str_of(payload, &["tool_name", "toolName"]).unwrap_or("tool");
    let Some(input) = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .filter(|i| !i.is_null())
    else {
        return tool.to_owned();
    };
    let raw = primary_input(input).unwrap_or_else(|| input.to_string());
    let arg = first_line_capped(&raw, PERMISSION_SUMMARY_MAX);
    if arg.is_empty() {
        tool.to_owned()
    } else {
        format!("{tool}: {arg}")
    }
}

fn primary_input(input: &Value) -> Option<String> {
    if let Some(s) = input.as_str() {
        return Some(s.to_owned());
    }
    let question = input
        .get("questions")
        .and_then(|q| q.get(0))
        .and_then(|q| q.get("question"))
        .and_then(Value::as_str);
    question
        .or_else(|| {
            PRIMARY_INPUT_KEYS
                .iter()
                .find_map(|k| input.get(k).and_then(Value::as_str))
        })
        .map(str::to_owned)
}

/// 第一个非空行，按字符数截到 `max`；被截或还有后续行就在末尾加 `…`，让人知道没看全。
pub(crate) fn first_line_capped(text: &str, max: usize) -> String {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return String::new();
    };
    let more_lines = lines.next().is_some();
    let mut out: String = first.chars().take(max).collect();
    if more_lines || first.chars().count() > max {
        out.push('…');
    }
    out
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

    #[test]
    fn permission_summary_shows_what_the_tool_is_about_to_do() {
        // Bash：命令本身，不是 description（agora-pzi：只剩 "Bash" 时 Dashboard 上没法判断）。
        let pr = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": { "command": "git push origin main", "description": "Push" } });
        assert_eq!(permission_summary(&pr), "Bash: git push origin main");
        // 文件类工具：路径。
        let pr = json!({ "tool_name": "Write", "tool_input": { "file_path": "/tmp/x", "content": "hi" } });
        assert_eq!(permission_summary(&pr), "Write: /tmp/x");
        // AskUserQuestion：问题文本。
        let pr = json!({ "tool_name": "AskUserQuestion",
            "tool_input": { "questions": [{ "question": "which?", "header": "h" }] } });
        assert_eq!(permission_summary(&pr), "AskUserQuestion: which?");
        // 不认识的键：紧凑 JSON，总比只剩工具名多一点线索。
        let pr = json!({ "tool_name": "mcp__x__do", "tool_input": { "thing": 1 } });
        assert_eq!(permission_summary(&pr), "mcp__x__do: {\"thing\":1}");
        // 没有 tool_input / 空对象也不崩：退回工具名。
        assert_eq!(permission_summary(&json!({ "tool_name": "Bash" })), "Bash");
        assert_eq!(
            permission_summary(
                &json!({ "tool_name": "Bash", "tool_input": { "command": "  \n" } })
            ),
            "Bash"
        );
        assert_eq!(permission_summary(&json!({})), "tool");
        // Grok 风格的 camelCase 键也认。
        let pr = json!({ "toolName": "run_terminal_command", "toolInput": { "command": "ls" } });
        assert_eq!(permission_summary(&pr), "run_terminal_command: ls");
    }

    #[test]
    fn permission_summary_is_one_line_and_capped() {
        let pr = json!({ "tool_name": "Bash", "tool_input": { "command": "echo a\necho b" } });
        assert_eq!(permission_summary(&pr), "Bash: echo a…");
        let long = "x".repeat(PERMISSION_SUMMARY_MAX + 5);
        let pr = json!({ "tool_name": "Bash", "tool_input": { "command": long } });
        let got = permission_summary(&pr);
        assert_eq!(
            got.chars().count(),
            "Bash: ".len() + PERMISSION_SUMMARY_MAX + 1
        );
        assert!(got.ends_with('…'));
        // 刚好到上限不加省略号；按字符不按字节截，多字节不会被劈开。
        let exact: String = "中".repeat(PERMISSION_SUMMARY_MAX);
        let pr = json!({ "tool_name": "Bash", "tool_input": { "command": exact } });
        assert!(!permission_summary(&pr).ends_with('…'));
    }
}
