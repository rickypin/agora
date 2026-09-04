//! Adapter 注册表与 Claude adapter 的对外承诺（ADR-002 D4/D7/D9；agora-dvh.5）。

use std::time::Duration;

use agora::adapter::{self, Version, VersionProbe};
use agora::hook::HOLD_TIMEOUT;

#[test]
fn hook_timeouts_outlive_the_daemon_hold() {
    // 安装的 PermissionRequest timeout 必须大于 daemon 的挂起超时 + 客户端余量（55 min + 30 s），
    // 否则 agent 先超时，hook 的 allow 会落在一个已经放弃的提示上。
    for host in adapter::hosts() {
        let hooks = adapter::for_host(host).unwrap();
        for h in hooks.install_spec() {
            if h.event == "PermissionRequest" {
                assert!(h.timeout > HOLD_TIMEOUT + Duration::from_secs(30), "{host}");
            }
            // 所有 hook 的配置文件都相对 HOME，安装器不会写到别处。
            assert!(h.file.is_relative(), "{host}: {}", h.file.display());
        }
    }
}

#[test]
fn only_capable_hosts_answer_through_hooks() {
    assert!(adapter::for_host("claude").unwrap().decision_via_hook());
    assert!(adapter::for_host("codex").unwrap().decision_via_hook());
    assert!(!adapter::for_host("grok").unwrap().decision_via_hook());
    assert!(adapter::for_host("shell").is_none());
    assert!(adapter::for_host("nope").is_none());
}

#[test]
fn availability_is_three_valued() {
    let claude = adapter::find("claude").unwrap();
    assert_eq!(
        claude.version("2.1.258 (Claude Code)"),
        VersionProbe::Available(Version(2, 1, 258))
    );
    assert!(matches!(
        claude.version("1.0.0 (Claude Code)"),
        VersionProbe::Unparsable(_)
    ));
    assert_eq!(
        adapter::probe(
            claude,
            "/nonexistent/agora-no-such-agent",
            Duration::from_secs(2)
        ),
        VersionProbe::Missing
    );
    // 没有版本表的 agent：认不出就是不可解析，不会假装可用。
    let shell = adapter::find("shell").unwrap();
    assert!(matches!(
        shell.version("zsh 5.9"),
        VersionProbe::Unparsable(_)
    ));
    assert_eq!(shell.resume_args(Version(5, 9, 0), "x"), None);
}

#[test]
fn restart_arguments_never_guess_the_conversation() {
    let claude = adapter::find("claude").unwrap();
    let args = claude.resume_args(Version(2, 1, 260), "sess-1").unwrap();
    assert_eq!(args, vec!["--resume", "sess-1"]);
    let pin = claude.pin_args(Version(2, 1, 260), "new-1").unwrap();
    assert_eq!(pin, vec!["--session-id", "new-1"]);
    for a in args.iter().chain(pin.iter()) {
        assert!(!a.starts_with("--cont") && !a.starts_with("--la"), "{a}");
    }
}
