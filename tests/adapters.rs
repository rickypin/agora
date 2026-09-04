//! Adapter 注册表与 Claude adapter 的对外承诺（ADR-002 D4/D7/D9；agora-dvh.5）。

use std::time::Duration;

use agora::adapter::{self, Version, VersionProbe};
use agora::hook::HOLD_TIMEOUT;

#[test]
fn hook_timeouts_outlive_the_daemon_hold() {
    // 安装的 PermissionRequest timeout 必须大于该宿主的挂起上限 + 客户端余量（30 s），
    // 否则 agent 先超时，hook 的 allow 会落在一个已经放弃的提示上。宿主的上限不超过默认值。
    for host in adapter::hosts() {
        let hooks = adapter::for_host(host).unwrap();
        assert!(hooks.hold_timeout() <= HOLD_TIMEOUT, "{host}");
        for h in hooks.install_spec() {
            if h.event == "PermissionRequest" {
                assert!(
                    h.timeout > hooks.hold_timeout() + Duration::from_secs(30),
                    "{host}"
                );
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

// ---------- Restart 的 resume 规划（ADR-002 D7；agora-dvh.13） ----------

use agora::adapter::{plan_pin, plan_restart, resume::splice_args, RestartPlan};

#[test]
fn restart_plan_resumes_only_with_self_reported_id_and_known_version() {
    let avail =
        |_: &dyn agora::adapter::Adapter, _: &str| VersionProbe::Available(Version(2, 1, 260));
    // 正路：自报 id + 版本表内 → --resume <id>，原有参数保留。
    let plan = plan_restart(
        "claude",
        "claude --permission-mode default",
        Some("conv-1"),
        avail,
    );
    assert_eq!(
        plan,
        RestartPlan::Resume {
            command: "claude --permission-mode default --resume conv-1".into(),
            agent_session_id: "conv-1".into(),
        }
    );
    // 没自报过 → 原命令，说明原因。
    let plan = plan_restart("claude", "claude", None, avail);
    assert!(
        matches!(&plan, RestartPlan::Original { command, reason } if command == "claude" && reason.contains("自报")),
        "{plan:?}"
    );
    let plan = plan_restart("claude", "claude", Some("unknown"), avail);
    assert!(matches!(plan, RestartPlan::Original { .. }));
    // 版本表外 → 不猜参数。
    let plan = plan_restart("claude", "claude", Some("conv-1"), |_, _| {
        VersionProbe::Unparsable("1.0.0 低于版本表首项".into())
    });
    assert!(
        matches!(&plan, RestartPlan::Original { command, reason } if command == "claude" && reason.contains("不可解析")),
        "{plan:?}"
    );
    let plan = plan_restart("claude", "claude", Some("conv-1"), |_, _| {
        VersionProbe::Missing
    });
    assert!(matches!(plan, RestartPlan::Original { .. }));
    // 没有 Adapter 的类型（custom）与没有对话概念的 shell → 原命令。
    let plan = plan_restart("myagent", "myagent -x", Some("conv-1"), avail);
    assert_eq!(plan.command(), "myagent -x");
    let plan = plan_restart("shell", "bash", Some("conv-1"), avail);
    assert!(
        matches!(&plan, RestartPlan::Original { reason, .. } if reason.contains("不支持")),
        "{plan:?}"
    );
}

#[test]
fn restart_plan_replaces_previous_conversation_flags_instead_of_stacking() {
    // 上一代已经是 --resume old（或起会话时 --session-id 钉的）：换成新 id，不叠加。
    let avail =
        |_: &dyn agora::adapter::Adapter, _: &str| VersionProbe::Available(Version(2, 1, 260));
    let plan = plan_restart(
        "claude",
        "claude --session-id pinned-0 --verbose",
        Some("conv-2"),
        avail,
    );
    assert_eq!(plan.command(), "claude --verbose --resume conv-2");
    let plan = plan_restart("claude", "claude --resume conv-1", Some("conv-2"), avail);
    assert_eq!(plan.command(), "claude --resume conv-2");
    assert_eq!(
        splice_args("x --a 1 --b", &["--a".into(), "it's".into()], &[]),
        "x --b --a 'it'\\''s'"
    );
}

#[test]
fn pin_only_when_version_known_and_user_did_not_pick_a_conversation() {
    let avail =
        |_: &dyn agora::adapter::Adapter, _: &str| VersionProbe::Available(Version(2, 1, 260));
    let (cmd, id) = plan_pin("claude", "claude", avail).unwrap();
    assert_eq!(cmd, format!("claude --session-id {id}"));
    assert_eq!(id.len(), 36, "uuid v4：{id}");
    assert_eq!(&id[14..15], "4");
    // 用户自己写了 --resume / --session-id → 尊重他。
    assert!(plan_pin("claude", "claude --resume abc", avail).is_none());
    assert!(plan_pin("claude", "claude --session-id abc", avail).is_none());
    // 版本不明、无 Adapter、shell → 不钉。
    assert!(plan_pin("claude", "claude", |_, _| VersionProbe::Missing).is_none());
    assert!(plan_pin("myagent", "myagent", avail).is_none());
    assert!(plan_pin("shell", "bash", avail).is_none());
}

// ---------- Grok adapter（ADR-002 D2/D4/D7；agora-dvh.6） ----------

#[test]
fn grok_is_first_class_but_answers_only_in_the_terminal() {
    let grok = adapter::find("grok").unwrap();
    assert_eq!(
        grok.version("grok 1.0.13 (5e9a58528b76)"),
        VersionProbe::Available(Version(1, 0, 13))
    );
    assert!(matches!(
        grok.version("grok 1.0.2 (abc)"),
        VersionProbe::Unparsable(_)
    ));
    // Restart 用自报的 id：grok --resume <id>；起会话钉 --session-id。
    let avail =
        |_: &dyn agora::adapter::Adapter, _: &str| VersionProbe::Available(Version(1, 0, 13));
    let plan = plan_restart(
        "grok",
        "grok --permission-mode default",
        Some("conv-g"),
        avail,
    );
    assert_eq!(
        plan.command(),
        "grok --permission-mode default --resume conv-g"
    );
    let (cmd, id) = plan_pin("grok", "grok", avail).unwrap();
    assert_eq!(cmd, format!("grok --session-id {id}"));
    // WAITING(decision) 只能在终端答：没有挂起、没有决定输出。
    let hooks = grok.hooks().unwrap();
    assert!(!hooks.decision_via_hook());
    let n = serde_json::json!({"hookEventName":"notification","notificationType":"permission_prompt","message":"Tool permission requested"});
    assert_eq!(hooks.hold_key(&n), None);
    assert_eq!(
        hooks.decision_output(&agora::adapter::Decision::Allow),
        None
    );
    // 实测的事件名是小写蛇形：通用表那种 CamelCase 匹配在 Grok 上一条都认不出。
    assert!(!hooks
        .parse(&serde_json::json!({"hookEventName":"stop","reason":"end_turn"}))
        .is_empty());
    assert!(hooks
        .parse(&serde_json::json!({"hookEventName":"stop","reason":"shutdown"}))
        .is_empty());
}

// ---------- Codex adapter（ADR-002 D2/D4/D5/D7；agora-dvh.7） ----------

#[test]
fn codex_answers_through_hooks_but_only_for_seconds() {
    let codex = adapter::find("codex").unwrap();
    assert_eq!(
        codex.version("codex-cli 0.152.1"),
        VersionProbe::Available(Version(0, 152, 1))
    );
    // Restart 是子命令 `codex resume <id>`，全局 -c 留在子命令前；再 Restart 不叠加。
    let avail =
        |_: &dyn agora::adapter::Adapter, _: &str| VersionProbe::Available(Version(0, 152, 1));
    let plan = plan_restart(
        "codex",
        "codex -c approval_policy=on-request",
        Some("conv-c"),
        avail,
    );
    assert_eq!(
        plan.command(),
        "codex -c approval_policy=on-request resume conv-c"
    );
    let again = plan_restart("codex", plan.command(), Some("conv-d"), avail);
    assert_eq!(
        again.command(),
        "codex -c approval_policy=on-request resume conv-d"
    );
    // 没有钉 id 的参数：起会话不钉，等自报。
    assert_eq!(plan_pin("codex", "codex", avail), None);
    // 实测 2026-09-05：挂起期间 TUI 不显示审批提示，终端答不了——所以上限是秒级，
    // 超时 fail-open 把提示交回终端；Claude 的并存模型才配得上 55 min。
    let hooks = codex.hooks().unwrap();
    assert!(hooks.decision_via_hook());
    assert!(hooks.hold_timeout() <= Duration::from_secs(60));
    assert!(adapter::for_host("claude").unwrap().hold_timeout() == HOLD_TIMEOUT);
    let pr = serde_json::json!({"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"}});
    assert_eq!(hooks.hold_key(&pr).as_deref(), Some("Bash"));
    assert!(hooks
        .decision_output(&agora::adapter::Decision::Allow)
        .unwrap()
        .contains("\"allow\""));
    // 未信任即静默：装完必须提示用户跑 /hooks。
    assert!(hooks.install_hint().unwrap().contains("/hooks"));
}
