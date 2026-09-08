//! 状态机与四层来源仲裁（ADR-002 D1；MISSION §4.4 §5.1 §5.3；A14；A36 不变量 10 的仲裁部分）。
//!
//! 每条裁决规则一个测试，注明关掉哪个守卫会红。时间用整数秒手工推进，不睡。

use std::path::PathBuf;
use std::time::Duration;

use agora::runtime::{Exit, RuntimeRef, RuntimeSession, Size};
use agora::status::{
    AgoraEvent, Assessment, DetectionResult, Liveness, Machine, MachineConfig, Observation, Source,
    Status,
};

fn cfg() -> MachineConfig {
    MachineConfig {
        idle_after: Duration::from_secs(60),
        silence_after: Duration::from_secs(600),
        unheard_after: Duration::from_secs(90),
        high_hold: Duration::from_secs(30),
        text_ticks: 2,
        tick: Duration::from_secs(2),
        startup_grace: Duration::from_secs(10),
        external_silent_after: Duration::from_secs(2 * 3600),
    }
}

fn rt(alive: bool, output_at: Option<i64>) -> RuntimeSession {
    RuntimeSession {
        r#ref: RuntimeRef("fake:agora:x".into()),
        name: "x".into(),
        pid: Some(1),
        alive,
        exit: if alive { None } else { Some(Exit::Code(0)) },
        exited_at: None,
        title: String::new(),
        cwd: PathBuf::from("/"),
        attached: false,
        size: Size { cols: 80, rows: 24 },
        managed: true,
        output_at,
    }
}

fn running() -> Assessment {
    Assessment::new(Status::Running, Source::Process, 1.0, None)
}

fn text(status: Status, reason: &str) -> DetectionResult {
    DetectionResult {
        status,
        confidence: 0.9,
        reason: reason.into(),
    }
}

/// 一个活着的 tick。
fn tick(m: &mut Machine, now: i64, rt: &RuntimeSession, t: Option<DetectionResult>) -> Assessment {
    m.observe(Observation {
        process: running(),
        liveness: Liveness::Alive,
        text: t,
        runtime: Some(rt),
        epoch: 1,
        now,
    })
}

#[test]
fn every_status_has_a_producer() {
    // A14：六态各有输出。RUNNING / WAITING / TURN_DONE 来自 hook，IDLE 来自活动，FINISHED / FAILED 来自进程。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    assert_eq!(m.current().status, Status::Starting);
    m.apply(&AgoraEvent::PromptSubmitted("do x".into()), 1, 1);
    assert_eq!(m.current().status, Status::Running);
    assert_eq!(m.detail(), Some("do x"));
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t".into(),
            summary: "Bash".into(),
        },
        1,
        2,
    );
    assert_eq!(
        (
            m.current().status,
            m.current().source,
            m.current().confidence
        ),
        (Status::Waiting, Source::Hook, 1.0)
    );
    m.apply(&AgoraEvent::DecisionResolved(Some("t".into())), 1, 3);
    assert_eq!(m.current().status, Status::Running);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 4);
    assert_eq!(m.current().status, Status::TurnDone);
    assert_eq!(m.detail(), Some("done"));

    let mut shell = Machine::new(cfg(), false, 1, 0);
    let r = rt(true, Some(0));
    tick(&mut shell, 0, &r, None);
    assert_eq!(tick(&mut shell, 61, &r, None).status, Status::Idle);

    for (exit, status) in [
        (Exit::Code(0), Status::Finished),
        (Exit::Code(3), Status::Failed),
    ] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        let mut dead = rt(false, None);
        dead.exit = Some(exit.clone());
        let a = agora::status::process_layer(Some(&dead), None, false);
        let got = m.observe(Observation {
            process: a,
            liveness: Liveness::Dead,
            text: None,
            runtime: Some(&dead),
            epoch: 1,
            now: 5,
        });
        assert_eq!(
            (got.status, got.source, got.confidence),
            (status, Source::Process, 1.0)
        );
    }
}

#[test]
fn text_cannot_raise_hooked_session() {
    // 守卫：有 hook 的会话文本层永远抬不到 WAITING / TURN_DONE，活动层不产生 IDLE。
    // 关掉：observe_hooked 里改走 observe_unhooked → 这里第一个断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let r = rt(true, Some(0));
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    for now in [2, 4, 6, 8] {
        let a = tick(&mut m, now, &r, Some(text(Status::Waiting, "prompt seen")));
        assert_eq!(
            (a.status, a.source),
            (Status::Running, Source::Hook),
            "t={now}"
        );
    }
    // 无输出很久也不是 IDLE。
    let a = tick(&mut m, 200, &r, None);
    assert_eq!(a.status, Status::Running);
    // 声明没有 hook 但收到过事件的会话同样受保护（采纳的未知会话装了 hook）。
    let mut m = Machine::new(cfg(), false, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(None), 1, 0);
    assert!(m.has_hooks());
    for now in [2, 4, 6] {
        let a = tick(&mut m, now, &r, Some(text(Status::Waiting, "prompt seen")));
        assert_eq!(a.status, Status::TurnDone, "t={now}");
    }
}

#[test]
fn silent_hooks_become_unknown() {
    // 守卫：进程活着、silence_after 无事件、屏幕像在等人 → UNKNOWN `hooks silent`，不猜 WAITING。
    // 关掉：observe_hooked 去掉 silent 分支 → 断言 UNKNOWN 红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let r = rt(true, Some(0));
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    // 沉默未满：屏幕像在等人也还是 RUNNING。
    let a = tick(
        &mut m,
        599,
        &r,
        Some(text(Status::Waiting, "permission prompt")),
    );
    assert_eq!(a.status, Status::Running);
    // 沉默满了但屏幕没说在等人：还是 RUNNING（agent 可能真在跑长任务）。
    let a = tick(
        &mut m,
        601,
        &r,
        Some(text(Status::Running, "output flowing")),
    );
    assert_eq!(a.status, Status::Running);
    let a = tick(
        &mut m,
        603,
        &r,
        Some(text(Status::Waiting, "permission prompt")),
    );
    assert_eq!((a.status, a.source), (Status::Unknown, Source::Text));
    assert!(
        a.reason.as_deref().unwrap().contains("hooks silent"),
        "{a:?}"
    );
    assert!(a.confidence < 0.8);
    // hook 一出声就恢复。
    m.apply(&AgoraEvent::TurnEnded(None), 1, 604);
    let a = tick(&mut m, 606, &r, Some(text(Status::Waiting, "prompt")));
    assert_eq!((a.status, a.source), (Status::TurnDone, Source::Hook));
    // 从来没有 hook 事件的会话：沉默从本代起算，同样退成 UNKNOWN。
    let mut m = Machine::new(cfg(), true, 1, 1000);
    let a = tick(&mut m, 1700, &r, Some(text(Status::Idle, "shell prompt")));
    assert_eq!(a.status, Status::Unknown);
}

#[test]
fn stale_epoch_dropped() {
    // 守卫：Restart 之前那代进程的 Stop 不能把新会话标成 TURN_DONE。
    // 关掉：apply 去掉 epoch < self.epoch 的丢弃 → 断言红。
    let mut m = Machine::new(cfg(), true, 2, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 2, 0);
    assert!(!m.apply(&AgoraEvent::TurnEnded(None), 1, 1));
    assert_eq!(m.current().status, Status::Running);
    // 更新的 epoch 到来 = Restart 了：旧状态作废，从这条事件重新开始。
    assert!(m.apply(&AgoraEvent::SessionStarted, 3, 2));
    assert_eq!(m.epoch(), 3);
    assert_eq!(m.current().status, Status::Starting);
    // observe 看到库里的 epoch 变大同样重置。
    let r = rt(true, Some(0));
    m.apply(&AgoraEvent::TurnEnded(None), 3, 3);
    let a = m.observe(Observation {
        process: running(),
        liveness: Liveness::Alive,
        text: None,
        runtime: Some(&r),
        epoch: 4,
        now: 4,
    });
    assert_eq!((a.status, a.source), (Status::Running, Source::Process));
}

#[test]
fn process_exit_overrides_hooks_and_later_events_are_metadata_only() {
    // 进程退出压倒一切；退出后的 hook 事件不改状态。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t".into(),
            summary: "Bash".into(),
        },
        1,
        0,
    );
    let mut dead = rt(false, None);
    dead.exit = Some(Exit::Signal("TERM".into()));
    let a = m.observe(Observation {
        process: agora::status::process_layer(Some(&dead), None, true),
        liveness: Liveness::Dead,
        text: None,
        runtime: Some(&dead),
        epoch: 1,
        now: 1,
    });
    assert_eq!(a.status, Status::Finished);
    assert!(a.reason.unwrap().contains("killed by user"));
    m.apply(&AgoraEvent::TurnEnded(None), 1, 2);
    assert_eq!(m.current().status, Status::Finished);
}

#[test]
fn text_waiting_needs_two_consecutive_ticks() {
    // 驻留：文本 WAITING 连续 2 tick 一致才算；中断就重来；置信度封顶 0.8。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let r = rt(true, Some(0));
    assert_eq!(tick(&mut m, 0, &r, None).status, Status::Running);
    assert_eq!(
        tick(&mut m, 2, &r, Some(text(Status::Waiting, "prompt"))).status,
        Status::Running
    );
    // 同一秒内再读一次不算第二个 tick。
    assert_eq!(
        tick(&mut m, 2, &r, Some(text(Status::Waiting, "prompt"))).status,
        Status::Running
    );
    let a = tick(&mut m, 4, &r, Some(text(Status::Waiting, "prompt")));
    assert_eq!(
        (a.status, a.source, a.confidence),
        (Status::Waiting, Source::Text, 0.8)
    );
    assert_eq!(a.reason.as_deref(), Some("prompt"));
    // 中断后重来。
    let mut m = Machine::new(cfg(), false, 1, 0);
    tick(&mut m, 0, &r, Some(text(Status::Waiting, "p")));
    tick(&mut m, 2, &r, None);
    assert_eq!(
        tick(&mut m, 4, &r, Some(text(Status::Waiting, "p"))).status,
        Status::Running
    );
}

#[test]
fn idle_after_no_output_and_back_to_running_when_output_resumes() {
    let mut m = Machine::new(cfg(), false, 1, 0);
    let mut r = rt(true, Some(0));
    tick(&mut m, 0, &r, None);
    assert_eq!(tick(&mut m, 59, &r, None).status, Status::Running);
    let a = tick(&mut m, 60, &r, None);
    assert_eq!(
        (a.status, a.source, a.confidence),
        (Status::Idle, Source::Activity, 0.6)
    );
    r.output_at = Some(61);
    assert_eq!(tick(&mut m, 61, &r, None).status, Status::Running);
    // 又 60 s 没输出：再次 IDLE，起点是最后一次输出。
    assert_eq!(tick(&mut m, 120, &r, None).status, Status::Running);
    assert_eq!(tick(&mut m, 121, &r, None).status, Status::Idle);
}

#[test]
fn redraw_from_resize_or_attach_is_not_activity() {
    // resize / attach / detach 引起的重绘不算活动：那一 tick 的输出时刻前进被忽略。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let mut r = rt(true, Some(0));
    tick(&mut m, 0, &r, None);
    assert_eq!(tick(&mut m, 60, &r, None).status, Status::Idle);
    r.size = Size {
        cols: 120,
        rows: 40,
    };
    r.output_at = Some(62);
    assert_eq!(
        tick(&mut m, 62, &r, None).status,
        Status::Idle,
        "resize 重绘"
    );
    r.attached = true;
    r.output_at = Some(64);
    assert_eq!(
        tick(&mut m, 64, &r, None).status,
        Status::Idle,
        "attach 重绘"
    );
    // 尺寸稳定后真有输出才恢复。
    r.output_at = Some(66);
    assert_eq!(tick(&mut m, 66, &r, None).status, Status::Running);
}

#[test]
fn lower_layer_does_not_override_higher_within_hold() {
    // 驻留：高层（hook）写入 30 s 内低层不覆盖；30 s 后可以。
    // 无 hook 的会话里最高层是 hook 事件本身不会出现，用 STARTING（进程层写入）演：
    let mut m = Machine::new(cfg(), false, 1, 0);
    let r = rt(true, Some(0));
    let starting = Assessment::new(Status::Starting, Source::Process, 1.0, None);
    m.observe(Observation {
        process: starting.clone(),
        liveness: Liveness::Alive,
        text: None,
        runtime: Some(&r),
        epoch: 1,
        now: 0,
    });
    // 文本层连续两 tick 说 WAITING，但 STARTING 是 30 s 内高层写的：不覆盖。
    for now in [2, 4] {
        let a = m.observe(Observation {
            process: starting.clone(),
            liveness: Liveness::Alive,
            text: Some(text(Status::Waiting, "prompt")),
            runtime: Some(&r),
            epoch: 1,
            now,
        });
        assert_eq!(a.status, Status::Starting, "t={now}");
    }
    let a = m.observe(Observation {
        process: starting,
        liveness: Liveness::Alive,
        text: Some(text(Status::Waiting, "prompt")),
        runtime: Some(&r),
        epoch: 1,
        now: 31,
    });
    assert_eq!(a.status, Status::Waiting);
    // 进程层的 RUNNING 是默认值，不享受驻留：文本可以立刻（满两 tick 后）覆盖。
    let mut m = Machine::new(cfg(), false, 1, 0);
    tick(&mut m, 0, &r, Some(text(Status::Waiting, "p")));
    assert_eq!(
        tick(&mut m, 2, &r, Some(text(Status::Waiting, "p"))).status,
        Status::Waiting
    );
}

#[test]
fn external_session_follows_hooks_only() {
    // 外部会话没有进程事实：hook 说什么就是什么；没有 hook 就 UNKNOWN。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let unknown = || Observation {
        process: Assessment::unknown("external session: no runtime, hook only"),
        liveness: Liveness::Unknown,
        text: None,
        runtime: None,
        epoch: 1,
        now: 1,
    };
    assert_eq!(m.observe(unknown()).status, Status::Unknown);
    m.apply(&AgoraEvent::TurnEnded(None), 1, 1);
    assert_eq!(m.observe(unknown()).status, Status::TurnDone);
}

#[test]
fn parallel_tools_stay_waiting_until_every_decision_is_answered() {
    // 并行工具各挂一个决定；答了一个还在 WAITING，其间别的工具活动也不算答了。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let need = |id: &str| AgoraEvent::DecisionNeeded {
        tool_use_id: id.into(),
        summary: "Bash".into(),
    };
    m.apply(&AgoraEvent::PromptSubmitted("x".into()), 1, 0);
    m.apply(&need("a"), 1, 1);
    m.apply(&need("b"), 1, 1);
    m.apply(&AgoraEvent::Activity("Read".into()), 1, 2);
    assert_eq!(m.current().status, Status::Waiting);
    m.apply(&AgoraEvent::DecisionResolved(Some("a".into())), 1, 3);
    assert_eq!(m.current().status, Status::Waiting);
    m.apply(&AgoraEvent::DecisionResolved(Some("b".into())), 1, 4);
    assert_eq!(m.current().status, Status::Running);
    // 提问也走同一套：同 id 解除才算答完。
    m.apply(
        &AgoraEvent::InputNeeded {
            tool_use_id: "q".into(),
            question: "which?".into(),
        },
        1,
        5,
    );
    assert_eq!(m.current().status, Status::Waiting);
    m.apply(&AgoraEvent::DecisionResolved(None), 1, 6);
    assert_eq!(m.current().status, Status::Running);
}

#[test]
fn status_since_is_when_the_state_was_set_not_the_last_tick() {
    // MISSION §6.3："waiting 3m"与同分按等待时长排序：起点是状态写入的那一刻，tick 不刷新它。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("do x".into()), 1, 1);
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t1".into(),
            summary: "Write /tmp/x".into(),
        },
        1,
        5,
    );
    assert_eq!(m.status_since(), 5);
    let r = rt(true, Some(40));
    tick(&mut m, 50, &r, None);
    assert_eq!(m.current().status, Status::Waiting);
    assert_eq!(m.status_since(), 5, "tick 没改状态就不能改起点");
    // 同一状态被同样的结论再写一次也不刷新；换了状态才刷新。
    m.apply(&AgoraEvent::DecisionResolved(Some("t1".into())), 1, 60);
    assert_eq!(
        (m.current().status, m.status_since()),
        (Status::Running, 60)
    );
}

#[test]
fn apply_at_moves_status_since_but_not_the_silence_clock() {
    // agora-h1k.4（A42）：重放一条三分钟前的事件——状态起点回到三分钟前，但 daemon 的沉默 /
    // 启动宽限时钟从"收到"算：宽限问的是 daemon 等了多久，不是事件多老。改成两者都用事件时刻，
    // 重放的 SessionStart 会当场衰减成 TURN_DONE，第二个断言就红。
    let mut m = Machine::new(cfg(), true, 1, 1000);
    m.apply_at(&AgoraEvent::SessionStarted, 1, 1000, 820);
    assert_eq!(m.status_since(), 820, "起点是事件时刻");
    let r = rt(true, None);
    assert_eq!(
        tick(&mut m, 1005, &r, None).status,
        Status::Starting,
        "宽限 10 s 从收到（1000）算，1005 还没到"
    );
    assert_eq!(tick(&mut m, 1011, &r, None).status, Status::TurnDone);
    // 事件时刻晚于收到时刻（时钟异常）按收到时刻；沉默阈值同样从收到算。
    m.apply_at(&AgoraEvent::PromptSubmitted("x".into()), 1, 1020, 1500);
    assert_eq!(m.status_since(), 1020);
    assert!(!m.hooks_silent(1020 + 599));
    assert!(m.hooks_silent(1020 + 600));
    // 实时路径：apply = apply_at(now, now)。
    m.apply(&AgoraEvent::TurnEnded(None), 1, 1030);
    assert_eq!(m.status_since(), 1030);
}

#[test]
fn hooks_never_heard_after_terminal_activity_is_flagged_but_not_at_startup() {
    // 守卫（agora-dvh.15）：装了 hook 的会话，启动宽限后终端有过活动、再过 unheard_after
    // 仍一条事件没有 → hooks_unheard 报沉默秒数；TUI 启动时那波输出不算起点（Codex 的
    // SessionStart 要到第一条 prompt 才 fire，2026-09-05 实测）；hook 一出声就撤。
    // 关掉：sample_activity 里不再记 first_activity_at → 第一个 Some 断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    // 起会话时 pane 立刻有输出（TUI 画界面）：不是起点。
    tick(&mut m, 1, &rt(true, Some(1)), None);
    tick(&mut m, 3, &rt(true, Some(3)), None);
    assert_eq!(m.hooks_unheard(200), None, "启动时的输出不能当用户已经提问");
    // 宽限过后终端又动了（用户敲了 prompt）：从这时起算。
    tick(&mut m, 30, &rt(true, Some(30)), None);
    assert_eq!(m.hooks_unheard(100), None);
    assert_eq!(m.hooks_unheard(120), Some(90));
    // 一条 hook 事件就撤销提示。
    m.apply(&AgoraEvent::SessionStarted, 1, 121);
    assert_eq!(m.hooks_unheard(300), None);
    // 没声明 hook 的会话（shell）从不提示。
    let mut m = Machine::new(cfg(), false, 1, 0);
    tick(&mut m, 30, &rt(true, Some(30)), None);
    assert_eq!(m.hooks_unheard(500), None);
    // 进程退出后不提示：没接上也没意义了。
    let mut m = Machine::new(cfg(), true, 1, 0);
    // 第一次采样只是"看见了"，不算活动（IDLE 同款规则）；第二次前进才是。
    tick(&mut m, 20, &rt(true, Some(20)), None);
    tick(&mut m, 30, &rt(true, Some(30)), None);
    assert_eq!(m.hooks_unheard(200), Some(170));
    m.observe(Observation {
        process: Assessment::new(Status::Finished, Source::Process, 1.0, None),
        liveness: Liveness::Dead,
        text: None,
        runtime: Some(&rt(false, Some(30))),
        epoch: 1,
        now: 201,
    });
    assert_eq!(m.hooks_unheard(202), None);
}

#[test]
fn an_injected_prompt_starts_a_turn_but_leaves_the_two_preview_lines_alone() {
    // agora-3s5：<task-notification> 之类是宿主注入的，不是人说的话——回 RUNNING、挂起过期，
    // 但 ❯ 行、↳ 行与 detail 都保持原样。关掉 PromptInjected 分支（当成 PromptSubmitted）会红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("do x".into()), 1, 1);
    m.apply(&AgoraEvent::TurnEnded(Some("done x".into())), 1, 2);
    assert_eq!(m.current().status, Status::TurnDone);
    m.apply(&AgoraEvent::PromptInjected, 1, 3);
    assert_eq!(m.current().status, Status::Running);
    assert_eq!(m.current().source, Source::Hook);
    assert_eq!(m.prompt(), Some("do x"));
    assert_eq!(m.progress(), Some("done x"));
    assert_eq!(m.detail(), Some("done x"));
    // 挂起的权限随新一轮过期（与人敲的 prompt 同一规则）。
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t".into(),
            summary: "Bash".into(),
        },
        1,
        4,
    );
    assert_eq!(m.current().status, Status::Waiting);
    m.apply(&AgoraEvent::PromptInjected, 1, 5);
    assert_eq!(m.current().status, Status::Running);
}

#[test]
fn hook_starting_decays_to_turn_done_awaiting_first_prompt() {
    // 守卫（agora-okr）：SessionStart 之后 startup_grace 内没有后续 hook 事件、进程活着 → TURN_DONE
    // `awaiting first prompt`，而不是永远 STARTING。关掉：observe_hooked 去掉衰减分支 → 第二个断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let r = rt(true, None);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    assert_eq!(m.current().status, Status::Starting);
    assert_eq!(
        tick(&mut m, 9, &r, None).status,
        Status::Starting,
        "宽限内不动"
    );
    let a = tick(&mut m, 10, &r, None);
    assert_eq!((a.status, a.source), (Status::TurnDone, Source::Hook));
    assert!(
        a.reason
            .as_deref()
            .unwrap()
            .contains("awaiting first prompt"),
        "{a:?}"
    );
    assert!(a.confidence < 1.0 && a.confidence >= 0.9);
    assert_eq!(m.status_since(), 10);
    // 第一条 prompt 照常开一轮。
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 12);
    assert_eq!(m.current().status, Status::Running);
    assert_eq!(tick(&mut m, 30, &r, None).status, Status::Running);

    // compact 一类：SessionStart 之后宽限内就有活动 → RUNNING，中间不出现 TURN_DONE。
    let mut m = Machine::new(cfg(), true, 1, 100);
    m.apply(&AgoraEvent::SessionStarted, 1, 100);
    assert_eq!(tick(&mut m, 101, &r, None).status, Status::Starting);
    m.apply(&AgoraEvent::Activity("PreToolUse Bash".into()), 1, 102);
    assert_eq!(m.current().status, Status::Running);
    assert_eq!(tick(&mut m, 120, &r, None).status, Status::Running);

    // 沉默规则优先：衰减成 TURN_DONE 之后 10 min 没事件且屏幕像在等人 → 仍是 UNKNOWN hooks silent。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    assert_eq!(tick(&mut m, 20, &r, None).status, Status::TurnDone);
    let a = tick(&mut m, 700, &r, Some(text(Status::Idle, "prompt")));
    assert_eq!(a.status, Status::Unknown);
    assert!(a.reason.as_deref().unwrap().contains("hooks silent"));
}

#[test]
fn restored_hook_starting_decays_too() {
    // 守卫（agora-okr；agora-9dj 的恢复路径）：daemon 重启从检查点恢复的 STARTING 在之后的
    // tick 同样衰减，不会因为"检查点说是 STARTING"就钉住。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    let snapshot = m.hook_snapshot().unwrap().clone();
    let mut restored = Machine::new(cfg(), true, 1, 50);
    restored.restore_hook(snapshot);
    assert_eq!(restored.current().status, Status::Starting);
    let a = tick(&mut restored, 60, &rt(true, None), None);
    assert_eq!((a.status, a.source), (Status::TurnDone, Source::Hook));
    assert!(a
        .reason
        .as_deref()
        .unwrap()
        .contains("awaiting first prompt"));
}

#[test]
fn external_session_ended_by_hook_is_finished_not_unknown() {
    // 守卫（agora-vfi）：external 会话没有进程事实，SessionEnd 就是它唯一的"结束"——apply 之后下一 tick
    // 是 FINISHED（source hook、conf 0.8、reason 说明来自 hook），两种活性都一样：Unknown（没有可信
    // 进程号：Codex Desktop 的 ppid 是共用 app-server）与 Alive（进程号活着但只是宿主）。
    // 关掉：apply 的 SessionEnded 改回只清 pending → 循环里第一个断言红（停在 UNKNOWN）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let obs = |liveness, now| Observation {
        process: Assessment::unknown("external session: hook only"),
        liveness,
        text: None,
        runtime: None,
        epoch: 1,
        now,
    };
    assert_eq!(m.observe(obs(Liveness::Unknown, 1)).status, Status::Unknown);
    assert_eq!(m.observe(obs(Liveness::Alive, 2)).status, Status::Unknown);
    // Codex 退出 / 结束线程时的 reason 是 other（0.152.1 实测）。
    assert!(m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 3));
    for (liveness, now) in [(Liveness::Unknown, 4), (Liveness::Alive, 5)] {
        let a = m.observe(obs(liveness, now));
        assert_eq!(
            (a.status, a.source),
            (Status::Finished, Source::Hook),
            "{a:?}"
        );
        assert!(a.reason.as_deref().unwrap().contains("hook"), "{a:?}");
        assert!(a.confidence < 1.0, "进程事实到了要能以 1.0 盖过它：{a:?}");
    }
    assert_eq!(m.status_since(), 3, "结束的起点是 SessionEnd 那一刻");
    // 没有 reason 的 SessionEnd 同样算结束；Grok 的 shutdown 也是。
    for reason in [None, Some("shutdown".to_owned())] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::TurnEnded(None), 1, 1);
        m.apply(&AgoraEvent::SessionEnded(reason.clone()), 1, 2);
        assert_eq!(m.current().status, Status::Finished, "reason={reason:?}");
    }
}

#[test]
fn session_end_reason_clear_keeps_the_session_alive() {
    // 守卫（agora-vfi）：Claude 的 /clear 发 SessionEnd(reason=clear)，进程活着、同一秒紧接着新 id 的
    // SessionStart(source=clear)（testdata/claude/2.1.261/hooks/clear.jsonl）。这条 SessionEnd 不改状态，
    // 只清挂起。关掉例外（一律 FINISHED）→ 第一个断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 1);
    assert_eq!(m.current().status, Status::Running);
    assert!(m.apply(&AgoraEvent::SessionEnded(Some("clear".into())), 1, 2));
    assert_eq!(
        (m.current().status, m.current().source, m.status_since()),
        (Status::Running, Source::Hook, 1),
        "clear 不改状态、不刷新起点"
    );
    let r = rt(true, None);
    assert_eq!(tick(&mut m, 3, &r, None).status, Status::Running);
    // pending 确实清了：WAITING 里挂着两个决定，clear 之后只解其中一个也能回 RUNNING——
    // 没清的话 "b" 还在，会停在 WAITING（parallel_tools_stay_waiting_until_every_decision_is_answered）。
    let need = |id: &str| AgoraEvent::DecisionNeeded {
        tool_use_id: id.into(),
        summary: "Bash".into(),
    };
    m.apply(&need("a"), 1, 4);
    m.apply(&need("b"), 1, 4);
    assert_eq!(m.current().status, Status::Waiting);
    m.apply(&AgoraEvent::SessionEnded(Some("clear".into())), 1, 5);
    assert_eq!(m.current().status, Status::Waiting, "状态不动");
    m.apply(&AgoraEvent::DecisionResolved(Some("a".into())), 1, 6);
    assert_eq!(
        m.current().status,
        Status::Running,
        "pending 已被 clear 清空"
    );
}

#[test]
fn session_start_after_session_end_restarts() {
    // 守卫（agora-vfi）：hook 层的 FINISHED 不是终态。Codex TUI 的 /new、Claude 的 /resume 之后同一进程
    // 再发 SessionStart，行要回到 STARTING（再经宽限衰减成 TURN_DONE），不能钉死在 FINISHED——只有
    // 进程层的 FINISHED / FAILED 才压倒一切（process_exit_overrides_hooks_and_later_events_are_metadata_only）。
    // 关掉：apply 开头的 early return 不再看 source == Process → STARTING 断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("bye".into())), 1, 1);
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 2);
    assert_eq!(
        (m.current().status, m.current().source),
        (Status::Finished, Source::Hook)
    );
    // 进程还活着（Claude 收到 SessionEnd 到真正退出有一两秒）：hook 说结束就是结束，进程层的 RUNNING
    // 不把它顶回去；真退出了由进程层以 1.0 接管（见上面点名的守卫）。
    let r = rt(true, None);
    assert_eq!(tick(&mut m, 3, &r, None).status, Status::Finished);
    assert!(m.apply(&AgoraEvent::SessionStarted, 1, 4));
    assert_eq!(
        (m.current().status, m.current().source, m.status_since()),
        (Status::Starting, Source::Hook, 4)
    );
    m.apply(&AgoraEvent::PromptSubmitted("again".into()), 1, 5);
    assert_eq!(m.current().status, Status::Running);
    assert_eq!(tick(&mut m, 6, &r, None).status, Status::Running);
}

#[test]
fn idle_status_since_survives_many_ticks() {
    // 守卫（agora-385）：无 hook 会话进入 IDLE 之后每 tick 的结论一字不差（reason 也一样），起点钉在
    // 进入 IDLE 的那个 tick——"idle 2m" 与同分按等待时长排序都靠它。2026-09-05 Shell-01 的反例：reason 是
    // `no output for {n}s`，每 tick 都不同，set() 连 reason 一起比，set_at 每 tick 刷新，侧栏永远 "idle 0s"。
    // 关掉：IDLE 的 reason 改回带秒数、或 set() 改回 `a != self.current` → 循环里的断言红。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let mut r = rt(true, Some(0));
    tick(&mut m, 0, &r, None);
    let first = tick(&mut m, 60, &r, None);
    assert_eq!(
        (first.status, first.source),
        (Status::Idle, Source::Activity)
    );
    assert_eq!(m.status_since(), 60);
    for now in [62, 64, 66, 68, 70, 72] {
        let a = tick(&mut m, now, &r, None);
        assert_eq!(a, first, "t={now}: 每 tick 的 Assessment 完全相等");
        assert_eq!(m.status_since(), 60, "t={now}: 起点是进入 IDLE 的那个 tick");
    }
    assert!(
        !first
            .reason
            .as_deref()
            .unwrap()
            .chars()
            .any(|c| c.is_ascii_digit()),
        "reason 里不嵌秒数: {:?}",
        first.reason
    );
    // 输出恢复 → RUNNING，起点跟着更新。
    r.output_at = Some(80);
    assert_eq!(tick(&mut m, 80, &r, None).status, Status::Running);
    assert_eq!(m.status_since(), 80);
}

#[test]
fn reason_only_changes_do_not_move_status_since() {
    // 守卫（agora-385）：status_since 的定义是"当前状态的起点"（docs/spec/api.md）。同状态同来源而 reason 变
    // （RUNNING 的 `prompt submitted` → `activity` → 换了个工具）不是换状态，起点不动；换了状态才刷新。
    // 关掉：set() 改回 `a != self.current` → 第二个断言红（起点变成 20）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("do x".into()), 1, 10);
    assert_eq!(
        (m.current().status, m.status_since()),
        (Status::Running, 10)
    );
    m.apply(&AgoraEvent::Activity("Bash".into()), 1, 20);
    assert_eq!(m.current().reason.as_deref(), Some("activity"));
    assert_eq!(
        (m.current().status, m.status_since()),
        (Status::Running, 10),
        "reason 变了、状态没变：起点不动"
    );
    m.apply(&AgoraEvent::Activity("Read".into()), 1, 30);
    assert_eq!(
        (m.current().status, m.status_since()),
        (Status::Running, 10)
    );
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t".into(),
            summary: "Write".into(),
        },
        1,
        40,
    );
    assert_eq!(
        (m.current().status, m.status_since()),
        (Status::Waiting, 40),
        "换了状态才刷新"
    );
    // 同状态换来源也算换：进程层的 RUNNING 被 hook 的 RUNNING 接管，起点是 hook 事件那一刻。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let r = rt(true, Some(0));
    tick(&mut m, 5, &r, None);
    assert_eq!((m.current().source, m.status_since()), (Source::Process, 5));
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 9);
    assert_eq!((m.current().source, m.status_since()), (Source::Hook, 9));
}

#[test]
fn silent_hooks_unknown_reason_is_stable_across_ticks() {
    // 守卫（agora-385）：hook 沉默 → UNKNOWN 之后屏幕不变，每 tick 的结论一字不差、起点不动；屏幕那半句
    // 变了（reason 变）起点也不动，因为 (status, source) 没变。关掉：reason 改回 `hooks silent for {n}s`
    // → 循环里的相等断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let r = rt(true, Some(0));
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let first = tick(
        &mut m,
        603,
        &r,
        Some(text(Status::Waiting, "permission prompt")),
    );
    assert_eq!(
        (first.status, first.source),
        (Status::Unknown, Source::Text)
    );
    assert_eq!(
        first.reason.as_deref(),
        Some("hooks silent; screen: permission prompt")
    );
    assert_eq!(m.status_since(), 603);
    for now in [605, 607, 609, 611] {
        let a = tick(
            &mut m,
            now,
            &r,
            Some(text(Status::Waiting, "permission prompt")),
        );
        assert_eq!(a, first, "t={now}");
        assert_eq!(m.status_since(), 603, "t={now}");
    }
    // 屏幕换了一句：reason 跟着变，状态与来源没变，起点仍是 603。
    let a = tick(&mut m, 613, &r, Some(text(Status::Idle, "shell prompt")));
    assert_eq!(
        a.reason.as_deref(),
        Some("hooks silent; screen: shell prompt")
    );
    assert_eq!(m.status_since(), 603);
    // hook 一出声：TURN_DONE，起点是事件那一刻。
    m.apply(&AgoraEvent::TurnEnded(None), 1, 620);
    let a = tick(&mut m, 622, &r, Some(text(Status::Waiting, "prompt")));
    assert_eq!((a.status, a.source), (Status::TurnDone, Source::Hook));
    assert_eq!(m.status_since(), 620);
}

#[test]
fn idle_notification_after_silent_hooks_unknown_lands_on_turn_done() {
    // 守卫（agora-01g）：D1 沉默规则给的 UNKNOWN(text, "hooks silent; …") 收到 Notification(idle_prompt)
    // → TURN_DONE(hook, "idle")：hook 活着、agent 停在提示符，与"提示消失"的 UNKNOWN 是同一条谓词
    // （unknown_from_screen）。不接它的话 Idle 在它身上不产状态，下一个 tick 沉默解除、进程层猜成 RUNNING。
    // 关掉：Idle 臂只认 RUNNING → 第一条 TURN_DONE 断言红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let r = rt(true, Some(0));
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let a = tick(
        &mut m,
        603,
        &r,
        Some(text(Status::Waiting, "permission prompt")),
    );
    assert_eq!((a.status, a.source), (Status::Unknown, Source::Text));
    assert!(
        a.reason.as_deref().unwrap().contains("hooks silent"),
        "{a:?}"
    );
    let at = 604;
    assert!(m.apply(&AgoraEvent::Idle, 1, at));
    let a = m.current();
    assert_eq!(
        (a.status, a.source),
        (Status::TurnDone, Source::Hook),
        "{a:?}"
    );
    assert_eq!(a.reason.as_deref(), Some("idle"));
    assert_eq!(m.status_since(), at);
    // 再 tick：屏幕文本随便，仍 TURN_DONE（hook 出声了，沉默解除；hook 说了算）。
    let a = tick(&mut m, 606, &r, Some(text(Status::Idle, "shell prompt")));
    assert_eq!(
        (a.status, a.source),
        (Status::TurnDone, Source::Hook),
        "{a:?}"
    );
    assert_eq!(m.status_since(), at);
    let a = tick(&mut m, 608, &r, None);
    assert_eq!((a.status, a.source), (Status::TurnDone, Source::Hook));
}

#[test]
fn handleless_external_silence_falls_to_unknown() {
    // agora-tql（2026-09-08 现场 11 行 external 僵尸）：没有 pane、没有可信进程号（Liveness::Unknown）
    // 的 external 行只跟 hook 走，agent 早退了也永远钉在 TURN_DONE。守卫：沉默 external_silent_after
    // 以上 → UNKNOWN（source hook，固定 reason），起点稳定；进程号活着（Liveness::Alive）的不动；
    // hook 一出声立刻回到事件给的状态。关掉 observe 第 1 步里的沉默兜底 → 第一段 UNKNOWN 断言红。
    let external = |liveness| Observation {
        process: Assessment::unknown("external session: no runtime, hook only"),
        liveness,
        text: None,
        runtime: None,
        epoch: 1,
        now: 0,
    };
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    let mut obs = external(Liveness::Unknown);
    obs.now = 7199;
    assert_eq!(m.observe(obs).status, Status::TurnDone, "阈值未到不动");
    let mut obs = external(Liveness::Unknown);
    obs.now = 7200;
    let a = m.observe(obs);
    assert_eq!(
        (a.status, a.source),
        (Status::Unknown, Source::Hook),
        "{a:?}"
    );
    assert_eq!(a.reason.as_deref(), Some("hooks silent; no process handle"));
    let since = m.status_since();
    for now in [7202, 7300, 90000] {
        let mut obs = external(Liveness::Unknown);
        obs.now = now;
        let a = m.observe(obs);
        assert_eq!(a.status, Status::Unknown);
        assert_eq!(m.status_since(), since, "起点不随 tick 漂（agora-385）");
    }
    // Idle 在这种 UNKNOWN 上是可信的证据：agent 停在提示符 → TURN_DONE。
    m.apply(&AgoraEvent::Idle, 1, 90001);
    assert_eq!(m.current().status, Status::TurnDone);
    // 进程号活着的：hook 说什么就是什么，几小时不出声也还是 TURN_DONE。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(None), 1, 0);
    let mut obs = external(Liveness::Alive);
    obs.now = 90000;
    assert_eq!(m.observe(obs).status, Status::TurnDone);
    // 已经结束的不再动：FINISHED(hook) 不会被沉默改成 UNKNOWN。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 0);
    let mut obs = external(Liveness::Unknown);
    obs.now = 90000;
    assert_eq!(m.observe(obs).status, Status::Finished);
}

#[test]
fn process_gone_does_not_overwrite_the_hook_session_end() {
    // agora-rzh（2026-09-08 现场：6 行 external 探针，5 行宿主发了 SessionEnd，行上却全是
    // `external process gone (no exit status)`）：external 行的进程层 FINISHED 与 hook 的 SessionEnd
    // 同状态同分（0.8），observe 第 1 步无条件 set 把 hook 的 reason 换掉了，人自己结束的与终端被关 /
    // 崩溃的分不开。守卫：hook FINISHED 之后进程消失，source 仍 hook、reason 仍 `session ended (hook)`、
    // 起点仍是 SessionEnd 那一刻；对照：从没收到 SessionEnd 的行 reason 是 `external process gone`；
    // agora 起的会话进程层带退出码（conf 1.0）照旧覆盖，FAILED 也照旧覆盖。
    // 关掉 observe 第 1 步的 process_fact_is_no_better 判断 → 第一段 source / reason 断言红。
    let gone = || {
        Assessment::new(
            Status::Finished,
            Source::Process,
            0.8,
            Some("external process gone (no exit status)"),
        )
    };
    let dead = |process, now| Observation {
        process,
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now,
    };
    let alive = |now| Observation {
        process: Assessment::unknown("external session: process alive, hook only"),
        liveness: Liveness::Alive,
        text: None,
        runtime: None,
        epoch: 1,
        now,
    };

    // 人在提示符上两次 Ctrl+C：Claude 发 SessionEnd(prompt_input_exit)，紧接着进程退出。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(None), 1, 1);
    assert_eq!(m.observe(alive(2)).status, Status::TurnDone);
    m.apply(
        &AgoraEvent::SessionEnded(Some("prompt_input_exit".into())),
        1,
        10,
    );
    for now in [11, 12, 600] {
        let a = m.observe(dead(gone(), now));
        assert_eq!(
            (a.status, a.source),
            (Status::Finished, Source::Hook),
            "{a:?}"
        );
        assert_eq!(a.reason.as_deref(), Some("session ended (hook)"), "{a:?}");
        assert_eq!(m.status_since(), 10, "结束的起点仍是 SessionEnd 那一刻");
    }

    // 对照：hook 从没说过结束（Codex 关窗口一个事件都不发），进程没了就只有进程层能说。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(None), 1, 1);
    let a = m.observe(dead(gone(), 11));
    assert_eq!(
        (a.status, a.source),
        (Status::Finished, Source::Process),
        "{a:?}"
    );
    assert_eq!(
        a.reason.as_deref(),
        Some("external process gone (no exit status)")
    );
    assert_eq!(m.status_since(), 11);

    // agora 起的会话：进程层带退出码、conf 1.0，SessionEnd 之后照旧被它覆盖（"进程退出压倒一切"不变）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 10);
    let exited = rt(false, Some(9));
    let a = m.observe(Observation {
        process: agora::status::process_layer(Some(&exited), None, false),
        liveness: Liveness::Dead,
        text: None,
        runtime: Some(&exited),
        epoch: 1,
        now: 11,
    });
    assert_eq!(
        (a.status, a.source, a.confidence),
        (Status::Finished, Source::Process, 1.0),
        "{a:?}"
    );
    // 状态不同（退出码非零 → FAILED）更不能被 hook 的 FINISHED 挡住。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 10);
    let a = m.observe(dead(
        Assessment::new(Status::Failed, Source::Process, 1.0, Some("exit code 3")),
        11,
    ));
    assert_eq!((a.status, a.source), (Status::Failed, Source::Process));
}
