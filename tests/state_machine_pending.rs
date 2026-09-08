//! 挂起期间的屏幕证据（ADR-002 D1 例外、D5 解除途径；agora-9cd）：Claude 2.1.261 在终端放行后按 Esc
//! 一个事件都不发（`testdata/claude/2.1.261/hooks/interrupted.jsonl`），挂起的权限没人解除。规则：有挂起、
//! hook 给的 WAITING、文本层见过提示、之后连续 `text_ticks` 个 tick 见不到 → 清挂起、降 UNKNOWN(text)。
//!
//! 辅助从 `tests/state_machine.rs` 复制（本批那份由别的任务追加，不改它）。时间用整数秒手工推进，不睡。

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

/// 权限提示在屏幕上：文本层的 WAITING。
fn prompt() -> Option<DetectionResult> {
    Some(text(Status::Waiting, "prompt: Do you want to proceed?"))
}

/// 起手：有 hook 的会话，prompt 提交 → 权限挂起 t1 → WAITING(hook)。
fn waiting_on_t1() -> Machine {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("do x".into()), 1, 1);
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t1".into(),
            summary: "Bash: sleep 20".into(),
        },
        1,
        2,
    );
    assert_eq!(
        (m.current().status, m.current().source),
        (Status::Waiting, Source::Hook)
    );
    assert!(m.has_pending());
    assert_eq!(m.pending_keys(), vec!["t1".to_owned()]);
    m
}

#[test]
fn pending_permission_is_released_when_the_prompt_leaves_the_screen() {
    // 守卫：见过提示、之后连续 text_ticks 个 tick 见不到 → 清挂起、UNKNOWN(text)、status_since 是翻转
    // 的那个 tick；随后 PostToolUse 的 DecisionResolved 把它抬回 RUNNING。
    // 关掉：observe_hooked 去掉"提示消失"块 → 一直 WAITING，第一条断言红；
    //       apply_at 的 DecisionResolved 臂去掉 released_by_screen → 最后一条断言红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    assert_eq!(tick(&mut m, 4, &r, prompt()).status, Status::Waiting);
    // 终端放行 / Esc：提示不在了。第一个 tick 还不够。
    let a = tick(&mut m, 6, &r, None);
    assert_eq!((a.status, a.source), (Status::Waiting, Source::Hook));
    assert!(m.has_pending());
    // 第二个 tick（text_ticks = 2）：翻转。
    let a = tick(&mut m, 8, &r, None);
    assert_eq!(
        (a.status, a.source),
        (Status::Unknown, Source::Text),
        "{a:?}"
    );
    assert!(
        a.reason.as_deref().unwrap().contains("prompt gone"),
        "{a:?}"
    );
    assert!(a.confidence < 0.8);
    assert!(!m.has_pending());
    assert!(m.pending_keys().is_empty());
    assert_eq!(m.status_since(), 8, "起点是翻转的那个 tick");
    // UNKNOWN 钉住：进程活着不等于在干活，进程层的 RUNNING 不得盖掉它（挂起清了就不再 capture，text=None）。
    let a = tick(&mut m, 10, &r, None);
    assert_eq!(
        (a.status, a.source),
        (Status::Unknown, Source::Text),
        "{a:?}"
    );
    assert_eq!(m.status_since(), 8);
    // 终端放行、工具跑完：PostToolUse 的 DecisionResolved 才到 → RUNNING(hook)。
    m.apply(&AgoraEvent::DecisionResolved(Some("t1".into())), 1, 12);
    let a = m.current();
    assert_eq!(
        (a.status, a.source),
        (Status::Running, Source::Hook),
        "{a:?}"
    );
    assert_eq!(a.reason.as_deref(), Some("decision resolved"));
    assert_eq!(m.status_since(), 12);
    // 之后 tick 保持 RUNNING（hook 说了算）。
    assert_eq!(tick(&mut m, 14, &r, None).status, Status::Running);
}

#[test]
fn pending_permission_stays_while_the_prompt_is_on_screen() {
    // 守卫：提示一直在屏幕上 → 一直 WAITING、status_since 不动、挂起还在。
    // 关掉：把 prompt_on_screen 分支也计数 → 第三个 tick 起红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    for i in 0..10 {
        let a = tick(&mut m, 4 + i * 2, &r, prompt());
        assert_eq!(
            (a.status, a.source),
            (Status::Waiting, Source::Hook),
            "tick {i}"
        );
        assert_eq!(m.status_since(), 2, "tick {i}");
        assert!(m.has_pending(), "tick {i}");
    }
}

#[test]
fn pending_permission_never_seen_on_screen_is_left_alone() {
    // 守卫："从没见过就不动"：检测器认不出这家的权限 UI 时宁可维持 WAITING，退回沉默兜底。
    // 关掉：去掉 pending_prompt_seen 的前置 → 第二个 tick 就翻 UNKNOWN，红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    for i in 0..20 {
        let a = tick(&mut m, 4 + i * 2, &r, None);
        assert_eq!(
            (a.status, a.source),
            (Status::Waiting, Source::Hook),
            "tick {i}"
        );
        assert!(m.has_pending(), "tick {i}");
    }
    assert_eq!(m.status_since(), 2);
}

#[test]
fn dashboard_or_terminal_resolution_still_wins() {
    // 守卫：见过提示、计数到 text_ticks - 1 时 DecisionResolved 到了（Dashboard 答了 / PostToolUse）→
    // RUNNING，计数与 seen 归零：之后 text=None 不会再翻 UNKNOWN；下一条挂起要重新见过提示才算。
    // 关掉：apply_at 里 pending 变了不 forget_screen_prompt → 下一条挂起的第一个 None tick 就凑齐两票，红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    assert_eq!(tick(&mut m, 4, &r, prompt()).status, Status::Waiting);
    assert_eq!(tick(&mut m, 6, &r, None).status, Status::Waiting); // 计数 1 = text_ticks - 1
    m.apply(&AgoraEvent::DecisionResolved(Some("t1".into())), 1, 7);
    assert_eq!(
        (m.current().status, m.current().source),
        (Status::Running, Source::Hook)
    );
    assert!(!m.has_pending());
    for i in 0..5 {
        let a = tick(&mut m, 8 + i * 2, &r, None);
        assert_eq!(
            (a.status, a.source),
            (Status::Running, Source::Hook),
            "tick {i}"
        );
    }
    // 下一条挂起：seen 已归零，从没见过它的提示 → text=None 再多也不动。
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t2".into(),
            summary: "Write".into(),
        },
        1,
        20,
    );
    for i in 0..5 {
        let a = tick(&mut m, 22 + i * 2, &r, None);
        assert_eq!(
            (a.status, a.source),
            (Status::Waiting, Source::Hook),
            "tick {i}"
        );
        assert!(m.has_pending(), "tick {i}");
    }
    // 见过一次、再消失两个 tick：这一条才解除。
    tick(&mut m, 40, &r, prompt());
    tick(&mut m, 42, &r, None);
    let a = tick(&mut m, 44, &r, None);
    assert_eq!((a.status, a.source), (Status::Unknown, Source::Text));
    assert!(!m.has_pending());
}

#[test]
fn same_second_reads_count_as_one_tick() {
    // 守卫：几个客户端同一秒同时刷新不能凑齐两个 tick（同 observe_unhooked 的 text_streak）。
    // 关掉：去掉 counted 的间隔判定 → 第二次同秒读就翻 UNKNOWN，红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    tick(&mut m, 4, &r, prompt());
    for _ in 0..5 {
        let a = tick(&mut m, 6, &r, None);
        assert_eq!((a.status, a.source), (Status::Waiting, Source::Hook));
    }
    // 间隔不足 tick（2 s）也不算。
    assert_eq!(tick(&mut m, 7, &r, None).status, Status::Waiting);
    assert_eq!(tick(&mut m, 8, &r, None).status, Status::Unknown);
}

#[test]
fn a_new_prompt_clears_pending_and_the_screen_evidence() {
    // 守卫：PromptSubmitted 清挂起（原有规则）后，屏幕证据一并归零，RUNNING(hook) 不受 text=None 影响。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    tick(&mut m, 4, &r, prompt());
    tick(&mut m, 6, &r, None);
    m.apply(&AgoraEvent::PromptSubmitted("never mind".into()), 1, 7);
    assert!(!m.has_pending());
    for i in 0..3 {
        let a = tick(&mut m, 8 + i * 2, &r, None);
        assert_eq!(
            (a.status, a.source),
            (Status::Running, Source::Hook),
            "tick {i}"
        );
    }
}

#[test]
fn idle_notification_after_the_prompt_is_gone_lands_on_turn_done() {
    // 守卫（agora-01g）："提示消失"给的 UNKNOWN(text) 收到 Notification(idle_prompt) → TURN_DONE(hook)。
    // 反例：终端放行后按 Esc，Claude 2.1.261 一个事件都不发，行降成 UNKNOWN 钉住；约 60 s 后 Claude 发
    // idle_prompt，只认 RUNNING 的 Idle 臂在它身上不产状态，行一直 UNKNOWN 到用户的下一条 prompt——而
    // Esc 之后 Claude 确实停在提示符等下一条指令。
    // 关掉：apply_at 的 Idle 臂去掉 unknown_from_screen() → 第一条 TURN_DONE 断言红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    assert_eq!(tick(&mut m, 4, &r, prompt()).status, Status::Waiting);
    tick(&mut m, 6, &r, None);
    let a = tick(&mut m, 8, &r, None);
    assert_eq!(
        (a.status, a.source),
        (Status::Unknown, Source::Text),
        "{a:?}"
    );
    assert!(
        a.reason.as_deref().unwrap().contains("prompt gone"),
        "{a:?}"
    );
    assert!(!m.has_pending());
    // 停在提示符约 60 s：idle_prompt 到。
    let at = 70;
    assert!(m.apply(&AgoraEvent::Idle, 1, at));
    let a = m.current();
    assert_eq!(
        (a.status, a.source),
        (Status::TurnDone, Source::Hook),
        "{a:?}"
    );
    assert_eq!(a.reason.as_deref(), Some("idle"));
    assert!((a.confidence - 0.9).abs() < f32::EPSILON, "{a:?}");
    assert_eq!(m.status_since(), at);
    assert!(!m.has_pending());
    // 之后 tick（text=None、进程活着）仍是 TURN_DONE：hook 说了算，不被进程层盖回 RUNNING。
    for i in 0..5 {
        let a = tick(&mut m, 72 + i * 2, &r, None);
        assert_eq!(
            (a.status, a.source),
            (Status::TurnDone, Source::Hook),
            "tick {i}: {a:?}"
        );
        assert_eq!(m.status_since(), at, "tick {i}");
    }
}

#[test]
fn idle_notification_while_the_prompt_is_still_pending_is_ignored() {
    // 守卫：挂起还在、提示还在屏幕上的 WAITING(hook) 收到 Idle → 不动（钉住"WAITING 不动"，防有人把
    // Idle 臂的谓词再放宽到 WAITING：agent 在等人答权限，不是"一轮做完"，放掉挂起会让 Dashboard 的
    // Allow / Deny 失效）。
    // 关掉：Idle 臂对 WAITING 也产 TURN_DONE → 第一条断言红。
    let mut m = waiting_on_t1();
    let r = rt(true, Some(0));
    assert_eq!(tick(&mut m, 4, &r, prompt()).status, Status::Waiting);
    assert!(m.apply(&AgoraEvent::Idle, 1, 5));
    let a = m.current();
    assert_eq!(
        (a.status, a.source),
        (Status::Waiting, Source::Hook),
        "{a:?}"
    );
    assert_eq!(a.reason.as_deref(), Some("permission"));
    assert!(m.has_pending());
    assert_eq!(m.pending_keys(), vec!["t1".to_owned()]);
    assert_eq!(m.status_since(), 2, "status_since 不动");
    // 提示还在屏幕上，之后 tick 仍 WAITING。
    let a = tick(&mut m, 6, &r, prompt());
    assert_eq!((a.status, a.source), (Status::Waiting, Source::Hook));
    assert_eq!(m.status_since(), 2);
}
