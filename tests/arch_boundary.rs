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

/// 把 JS/TS 源码里的注释换成空白，字符串与模板串原样保留。
///
/// 守卫扫的是"会执行的代码"：`/* /api/ */` 这种注释曾经让 `fetch(u, i)`
/// 满足"同行含 /api/"，守卫等于没有（agora-gwm）。反过来，注释里出现
/// `new WebSocket(` 也不该让守卫去敲一行说明文字。
///
/// 必须认字符串：WS 的 URL 里有 `${proto}//${host}/api/…`，按 `//` 一刀切会把 `/api/`
/// 砍掉，守卫反而误报。
fn strip_js_comments(src: &str) -> String {
    #[derive(PartialEq)]
    enum St {
        Code,
        Line,
        Block,
        Str(char),
    }
    let mut out = String::with_capacity(src.len());
    let mut st = St::Code;
    let mut chars = src.chars().peekable();
    let mut escaped = false;
    while let Some(c) = chars.next() {
        match st {
            St::Code => match (c, chars.peek()) {
                ('/', Some('/')) => {
                    chars.next();
                    st = St::Line;
                    out.push_str("  ");
                }
                ('/', Some('*')) => {
                    chars.next();
                    st = St::Block;
                    out.push_str("  ");
                }
                ('"', _) | ('\'', _) | ('`', _) => {
                    st = St::Str(c);
                    escaped = false;
                    out.push(c);
                }
                _ => out.push(c),
            },
            St::Line => {
                if c == '\n' {
                    st = St::Code;
                    out.push(c);
                } else {
                    out.push(' ');
                }
            }
            St::Block => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    st = St::Code;
                    out.push_str("  ");
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                }
            }
            St::Str(q) => {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    st = St::Code;
                }
            }
        }
    }
    out
}

#[test]
fn tmux_identifiers_only_in_runtime_tmux() {
    // ADR-001 D2：tmux 泄漏到 runtime/ 之外，第二运行时无处安放。
    // main.rs 是组装根：选哪个运行时只能在那里发生，session/ status/ api/ 仍看不见 tmux。
    assert_only_under(
        "src",
        "tmux",
        &["src/runtime/tmux/", "src/runtime/mod.rs", "src/main.rs"],
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
fn beads_is_read_only_and_lives_in_task() {
    // 不变量 12：agora 对 beads 零写入。起 `bd` 的地方只有 `src/task/`（子命令白名单由
    // `tests/task_beads.rs` 用假 bd 录下核对）；别处连 "bd" 这个命令名都不许出现。
    assert_only_under("src", "\"bd\"", &["src/task/"], "MISSION 不变量 12");
    for write in [
        "\"update\"",
        "\"close\"",
        "\"create\"",
        "\"claim\"",
        "\"dep\"",
    ] {
        assert_only_under("src/task", write, &[], "对 beads 只读");
    }
}

#[test]
fn subprocesses_only_through_runtime_exec() {
    // ADR-001 D8 施工约束 2：子进程只经 runtime::exec 一个入口。
    for needle in ["process::Command", "tokio::process"] {
        assert_only_under("src", needle, &["src/runtime/exec.rs"], "ADR-001 D8");
    }
    // PTY 里的 attach 是第二个子进程入口，D8 显式列它为例外（长进程 + PTY，塞不进
    // "跑完拿输出"的 exec 模型）。守卫原先只认 Command / tokio::process，gateway 从旁边
    // 走过去谁也没拦（agora-gwm）：现在这个入口也被钉在 gateway 一个文件里。
    for needle in ["spawn_command", "native_pty_system"] {
        assert_only_under(
            "src",
            needle,
            &["src/gateway/"],
            "ADR-001 D8 约束 2 的显式例外",
        );
    }
}

#[test]
fn the_terminal_installs_the_key_handler() {
    // agora-xqa.3：键位判断在 keys.ts 里测得再全，没装到 xterm 上也是白的——Shift+Enter
    // 照样发裸 CR、Cmd+← 照样让浏览器后退。xterm 在 jsdom 里开不起来（canvas），
    // 前端测不到这一行，所以在这里扫（剥掉注释，写在注释里不算）。
    let body =
        fs::read_to_string(root().join("web/src/TerminalView.tsx")).expect("TerminalView 存在");
    let code = strip_js_comments(&body);
    assert!(
        code.contains("attachCustomKeyEventHandler") && code.contains("handleTerminalKey("),
        "TerminalView 必须把 keys.ts 的 handleTerminalKey 装成 xterm 的 custom key handler"
    );
}

/// 唯一允许出现 `fetch(` 的前端文件。
const NET_EXIT: &str = "web/src/net.ts";

#[test]
fn frontend_only_talks_to_api_and_ws() {
    // A36 不变量 9：前端只经 /api 与 WS 和节点通信，没有客户端专用后门。
    //
    // 旧版逐行找 `fetch(` 并要求同行含 `/api/`，`fetch(u, i) /* /api/ */` 一句注释就能
    // 满足它——守卫等于没有（agora-gwm）。现在扫的是剥掉注释后的代码：`fetch(` 收在
    // net.ts 一个出口，WebSocket 的 URL 字面量必须同行以 /api/ 开头。
    let offenders: Vec<String> = sources("web/src")
        .into_iter()
        .filter(|(p, body)| rel(p) != NET_EXIT && strip_js_comments(body).contains("fetch("))
        .map(|(p, _)| rel(&p))
        .collect();
    assert!(
        offenders.is_empty(),
        "`fetch(` 只允许出现在 {NET_EXIT}（前端唯一的网络出口），越界文件: {offenders:?}"
    );

    let ws_offenders: Vec<String> = sources("web/src")
        .into_iter()
        .filter(|(_, body)| {
            strip_js_comments(body)
                .lines()
                .any(|l| l.contains("new WebSocket(") && !l.contains("/api/"))
        })
        .map(|(p, _)| rel(&p))
        .collect();
    assert!(
        ws_offenders.is_empty(),
        "WebSocket 的 URL 必须同行以 /api/ 开头: {ws_offenders:?}"
    );
}

#[test]
fn the_single_network_exit_checks_the_prefix_at_runtime() {
    // 文本守卫拦得住"新写一个 fetch"，拦不住"经 net.ts 请求 /api/ 之外的地址"。
    // 出口里必须有真正会执行的前缀校验——剥掉注释再找，写在注释里不算（agora-gwm）。
    let body = fs::read_to_string(root().join(NET_EXIT)).expect("net.ts 存在");
    let code = strip_js_comments(&body);
    assert!(
        code.contains(r#"API_PREFIX = "/api/""#),
        "{NET_EXIT} 必须定义 API_PREFIX = \"/api/\""
    );
    assert!(
        code.contains("startsWith(API_PREFIX)") && code.contains("throw"),
        "{NET_EXIT} 必须在发请求前校验前缀并抛错"
    );
}

#[test]
fn comment_stripping_keeps_strings_and_drops_comments() {
    // 剥离器自身的守卫：它错一点，上面两条就会误报或漏报。
    let src = r#"const u = `${p}//${h}/api/events`; // fetch("/evil")
/* new WebSocket("/evil") */ const s = "a/*b";"#;
    let out = strip_js_comments(src);
    assert!(out.contains("/api/events"), "模板串里的 // 不是注释: {out}");
    assert!(!out.contains("/evil"), "注释没被剥掉: {out}");
    assert!(out.contains(r#""a/*b""#), "字符串里的 /* 不是注释: {out}");
}
