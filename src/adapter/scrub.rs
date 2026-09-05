//! 录制脱敏（ADR-002 D10；agora-3la.1）：`agora hook --record` 落盘前把 payload 里的隐私抹掉，
//! 只留键与枚举值——fixture 要进 git，回放只认键名、事件名、枚举值与 id 之间的对应关系。
//!
//! 按键名分三类，三家的 camel / snake 拼法都列；不认识的键一律当自由文本抹掉，宁可多抹：
//! - 枚举键（事件名、来源、权限模式、工具名、错误类型……）：值是单个标识符 token 时原样保留，
//!   带空格的整句也抹——`error` 在 Grok 的 StopFailure 是枚举，在 Claude 的 PostToolUseFailure
//!   是一句话；
//! - id 键（`id`、`*_id`、`*Id`）：换成同一录制内稳定的哈希占位，形状跟着原值（uuid 仍是 uuid
//!   形状、`toolu_…` / `exec-…` 仍带前缀），PreToolUse / PostToolUse 的对应关系靠它保住；
//! - 路径（键名是路径，或值以 `/`、`~` 开头）：`/redacted/<hash>`；
//! - 其余字符串：`<键名>` 占位。布尔、数字、null、键名、数组结构原样。
//!
//! 哈希带录制盐（录制文件的时间零点）：同一录制里同值同占位，不同录制之间对不上号，路径这种
//! 猜得出来的值也猜不回去。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// 值是枚举 / 标识符的键：token 形态才保留。
const ENUM_KEYS: &[&str] = &[
    "hook_event_name",
    "hookEventName",
    "source",
    "permission_mode",
    "permissionMode",
    "notification_type",
    "notificationType",
    "reason",
    "tool_name",
    "toolName",
    "error_type",
    "errorType",
    "error",
    "matcher",
    "level",
    "effort",
    "behavior",
    "model",
    "trigger",
    "stop_reason",
    "stopReason",
    "status",
    "kind",
    "type",
    "decision",
    "subagent_type",
    "subagentType",
    "agent_type",
    "agentType",
    "role",
    "mode",
];

/// 值是路径的键。
const PATH_KEYS: &[&str] = &[
    "cwd",
    "transcript_path",
    "transcriptPath",
    "workspaceRoot",
    "workspace_root",
    "project_dir",
    "projectDir",
    "file_path",
    "filePath",
    "path",
    "root",
    "dir",
    "directory",
    "home",
];

fn is_id_key(key: &str) -> bool {
    key == "id" || key == "uuid" || key.ends_with("_id") || key.ends_with("Id")
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

fn digest(salt: &str, value: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update([0u8]);
    h.update(value.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn id_placeholder(salt: &str, value: &str) -> String {
    let h = digest(salt, value);
    if is_uuid(value) {
        return format!(
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        );
    }
    // `toolu_01ABC…` → `toolu_1a2b3c4d`、`exec-<uuid>` → `exec-1a2b3c4d`：前缀是形状的一部分。
    let prefix = value
        .find(['_', '-'])
        .map(|i| &value[..=i])
        .filter(|p| p.len() <= 12 && p[..p.len() - 1].chars().all(|c| c.is_ascii_alphanumeric()));
    match prefix {
        Some(p) => format!("{p}{}", &h[..8]),
        None => format!("id-{}", &h[..8]),
    }
}

fn path_placeholder(salt: &str, value: &str) -> String {
    let h = digest(salt, value);
    if value.starts_with('~') {
        format!("~/redacted/{}", &h[..8])
    } else {
        format!("/redacted/{}", &h[..8])
    }
}

fn scrub_str(key: &str, value: &str, salt: &str) -> Value {
    let out = if is_id_key(key) {
        id_placeholder(salt, value)
    } else if PATH_KEYS.contains(&key) || value.starts_with('/') || value.starts_with('~') {
        path_placeholder(salt, value)
    } else if ENUM_KEYS.contains(&key) && is_token(value) {
        value.to_owned()
    } else {
        format!("<{key}>")
    };
    Value::String(out)
}

fn scrub_value(key: &str, value: &Value, salt: &str) -> Value {
    match value {
        Value::String(s) => scrub_str(key, s, salt),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| scrub_value(key, v, salt)).collect())
        }
        Value::Object(map) => Value::Object(scrub_map(map, salt)),
        other => other.clone(),
    }
}

fn scrub_map(map: &Map<String, Value>, salt: &str) -> Map<String, Value> {
    map.iter()
        .map(|(k, v)| (k.clone(), scrub_value(k, v, salt)))
        .collect()
}

/// 脱敏一条 payload。`salt` 是这次录制的盐（同一录制内必须一致，id 才对得上）。
pub fn scrub(payload: &Value, salt: &str) -> Value {
    // 顶层不是对象（hook 送来的不是 JSON 时 cmd 会包成字符串）：整个当自由文本。
    scrub_value("payload", payload, salt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enums_survive_and_free_text_does_not() {
        let p = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "permission_mode": "default",
            "prompt": "delete /Users/me/secret please",
            "error": "interrupted by user after 3s",
            "tool_input": {"command": "cat ~/.ssh/id_rsa", "description": "read key"},
            "stop_hook_active": false,
            "n": 3,
            "nothing": null
        });
        let s = scrub(&p, "salt");
        assert_eq!(s["hook_event_name"], "PermissionRequest");
        assert_eq!(s["tool_name"], "Bash");
        assert_eq!(s["permission_mode"], "default");
        assert_eq!(s["prompt"], "<prompt>");
        // 枚举键里的整句不算枚举。
        assert_eq!(s["error"], "<error>");
        assert_eq!(s["tool_input"]["command"], "<command>");
        assert_eq!(s["tool_input"]["description"], "<description>");
        assert_eq!(s["stop_hook_active"], false);
        assert_eq!(s["n"], 3);
        assert!(s["nothing"].is_null());
        assert!(!s.to_string().contains("secret"));
        assert!(!s.to_string().contains("id_rsa"));
    }

    #[test]
    fn ids_keep_shape_and_stay_consistent_within_a_recording() {
        let sid = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
        let pre = json!({"session_id": sid, "sessionId": sid, "tool_use_id": "toolu_01XYZ", "call_id": "exec-abc-def"});
        let post = json!({"session_id": sid, "tool_use_id": "toolu_01XYZ"});
        let a = scrub(&pre, "t0");
        let b = scrub(&post, "t0");
        assert_eq!(a["session_id"], b["session_id"]);
        assert_eq!(a["tool_use_id"], b["tool_use_id"]);
        // 双写的两个键同值同占位。
        assert_eq!(a["session_id"], a["sessionId"]);
        let scrubbed_sid = a["session_id"].as_str().unwrap();
        assert!(is_uuid(scrubbed_sid) && scrubbed_sid != sid);
        assert!(a["tool_use_id"].as_str().unwrap().starts_with("toolu_"));
        assert!(a["call_id"].as_str().unwrap().starts_with("exec-"));
        assert_ne!(a["tool_use_id"], json!("toolu_01XYZ"));
        // 换盐就对不上号。
        assert_ne!(scrub(&pre, "other")["session_id"], a["session_id"]);
    }

    #[test]
    fn paths_are_hidden_whether_by_key_or_by_shape() {
        let p = json!({
            "cwd": "/Users/me/code/agora",
            "transcript_path": "~/.claude/projects/x/y.jsonl",
            "anything": "/etc/passwd",
            "workspaceRoot": "/Users/me/code/agora/"
        });
        let s = scrub(&p, "t0");
        assert!(s["cwd"].as_str().unwrap().starts_with("/redacted/"));
        assert!(s["transcript_path"]
            .as_str()
            .unwrap()
            .starts_with("~/redacted/"));
        assert!(s["anything"].as_str().unwrap().starts_with("/redacted/"));
        assert!(!s.to_string().contains("Users"));
    }

    #[test]
    fn arrays_and_nesting_keep_structure() {
        let p = json!({
            "tool_input": {"questions": [{"question": "A or B?", "options": [{"label": "A"}, {"label": "B"}]}]},
            "background_tasks": [],
            "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "ls"}]}]
        });
        let s = scrub(&p, "t0");
        assert_eq!(s["tool_input"]["questions"][0]["question"], "<question>");
        assert_eq!(
            s["tool_input"]["questions"][0]["options"][1]["label"],
            "<label>"
        );
        assert_eq!(s["background_tasks"], json!([]));
        assert_eq!(s["permission_suggestions"][0]["type"], "addRules");
        assert_eq!(
            s["permission_suggestions"][0]["rules"][0]["toolName"],
            "Bash"
        );
        assert_eq!(
            s["permission_suggestions"][0]["rules"][0]["ruleContent"],
            "<ruleContent>"
        );
    }

    #[test]
    fn non_object_payloads_become_a_placeholder() {
        assert_eq!(scrub(&json!("raw text"), "t0"), json!("<payload>"));
        assert_eq!(scrub(&json!(42), "t0"), json!(42));
    }
}
