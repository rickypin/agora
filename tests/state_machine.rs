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
        high_hold: Duration::from_secs(30),
        text_ticks: 2,
        tick: Duration::from_secs(2),
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
    m.apply(&AgoraEvent::DecisionResolved, 1, 3);
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
