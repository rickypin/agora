//! 源码边界守卫（ADR-001 D8 施工约束；ADR-002 规则 5/10；A36 不变量 9）。
//!
//! 每条规则一个测试，关掉哪条哪条变红。扫描的是 `src/` 与 `web/src/` 的文本，
//! 注释也算——边界不该靠"只是注释"来放行。

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "node_modules" || n == "dist")
            {
                continue;
            }
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn sources(sub: &str) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    walk(&root().join(sub), &mut files);
    files
        .into_iter()
        .filter_map(|p| fs::read_to_string(&p).ok().map(|s| (p, s)))
        .collect()
}

fn rel(p: &Path) -> String {
    p.strip_prefix(root())
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 断言：`needle` 只出现在 `allowed` 前缀下的文件里。
fn assert_only_under(sub: &str, needle: &str, allowed: &[&str], why: &str) {
    let offenders: Vec<String> = sources(sub)
        .into_iter()
        .filter(|(p, body)| {
            let r = rel(p);
            body.contains(needle) && !allowed.iter().any(|a| r.starts_with(a))
        })
        .map(|(p, _)| rel(&p))
        .collect();
    assert!(
        offenders.is_empty(),
        "`{needle}` 只允许出现在 {allowed:?}（{why}），越界文件: {offenders:?}"
    );
}

fn assert_absent(sub: &str, needle: &str, why: &str) {
    assert_only_under(sub, needle, &[], why);
}

#[test]
fn tmux_identifiers_only_in_runtime_tmux() {
    // ADR-001 D2：tmux 泄漏到 runtime/ 之外，第二运行时无处安放。
    assert_only_under(
        "src",
        "tmux",
        &["src/runtime/tmux/", "src/runtime/mod.rs"],
        "ADR-001 D2",
    );
}

#[test]
fn nobody_calls_kill_server() {
    // ADR-001 D3：kill-server 会把用户默认 socket 的会话陪葬。
    assert_absent("src", "kill-server", "ADR-001 D3");
}

#[test]
fn agent_specific_code_only_in_adapter() {
    // ADR-002 D2：核心层不知道任何具体 agent。
    for name in ["claude", "codex", "grok"] {
        assert_only_under("src", name, &["src/adapter/"], "ADR-002 D2");
    }
}

#[test]
fn no_help_parsing_anywhere() {
    // ADR-002 规则 10：只解析 --version，flag 有无由版本表回答。
    assert_absent("src", "--help", "ADR-002 规则 10");
}

#[test]
fn restart_never_uses_continue_or_last() {
    // ADR-002 D7：Restart 只用自报的对话 id resume。
    assert_absent("src", "--continue", "ADR-002 D7");
    assert_absent("src", "--last", "ADR-002 D7");
}

#[test]
fn core_layers_do_not_know_hook_payload_keys() {
    // ADR-002 规则 5：session/ status/ 不得引用 agent 的 payload 键名。
    for key in ["hook_event_name", "hookEventName"] {
        assert_only_under("src", key, &["src/adapter/", "src/hook/"], "ADR-002 规则 5");
    }
}

#[test]
fn no_anyhow() {
    // ADR-001 D8 施工约束 4：模块边界用错误枚举。
    assert_absent("src", "anyhow", "ADR-001 D8");
    let manifest = fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(!manifest.contains("anyhow"), "Cargo.toml 引入了 anyhow");
}

#[test]
fn locks_are_not_expected_unpoisoned() {
    // ADR-001 D8 施工约束 4：锁不 expect("poisoned")。
    assert_absent("src", "expect(\"poisoned", "ADR-001 D8");
}

#[test]
fn subprocesses_only_through_runtime_exec() {
    // ADR-001 D8 施工约束 2：子进程只经 runtime::exec 一个入口。
    for needle in ["process::Command", "tokio::process"] {
        assert_only_under("src", needle, &["src/runtime/exec.rs"], "ADR-001 D8");
    }
}

#[test]
fn frontend_only_talks_to_api_and_ws() {
    // A36 不变量 9：前端只经 /api 与 WS 和节点通信，没有客户端专用后门。
    let offenders: Vec<String> = sources("web/src")
        .into_iter()
        .filter(|(_, body)| {
            body.lines().any(|l| {
                let l = l.trim();
                (l.contains("fetch(") || l.contains("new WebSocket("))
                    && !l.contains("/api/")
                    && !l.starts_with("//")
            })
        })
        .map(|(p, _)| rel(&p))
        .collect();
    assert!(
        offenders.is_empty(),
        "前端直连了 /api 之外的地址: {offenders:?}"
    );
}
