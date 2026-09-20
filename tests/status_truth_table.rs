//! 状态真值表守卫（epic agora-5gg；本文件由 agora-rkl 起第一行，agora-5gg.16 往里补齐）。
//!
//! 一行 = 「origin × 进程事实 × hook 来源」的某种组合喂进状态机之后**该**吐出什么。为什么单开
//! 一个文件而不塞进 `tests/state_machine.rs`：那边按"每条裁决规则一个测试"组织，讲为什么；这里
//! 按"每一行长什么样"组织，讲的是对人眼承诺的口径（MISSION §4.3 的三问：进程还在不在、它要我
//! 做什么、说不清的话为什么说不清）。2026-09-18 的盘点（`docs/analysis/session-status-audit-2026-09-18.md`）
//! 之所以能发现一批错行，靠的就是把 79 行摊成一张表逐条问——表落成守卫之后，同样的核对不该再靠
//! 人肉做一遍。
//!
//! 只喂 `Machine`，不起运行时、不起 daemon：真值表要在几毫秒内重跑完，才有人改一行就肯全跑。

use std::time::Duration;

use agora::status::{
    AgoraEvent, Assessment, Liveness, Machine, MachineConfig, Observation, Source, Status,
};

/// 表用生产默认值跑（宽限 10 s、external 沉默兜底 2 h）：真值表记的是**默认口径**，
/// 阈值本身被改是配置的事，规则之间的关系不该跟着变。
fn cfg() -> MachineConfig {
    MachineConfig {
        startup_grace: Duration::from_secs(10),
        external_silent_after: Duration::from_secs(2 * 3600),
        ..Default::default()
    }
}

/// external（无运行时句柄）行的一个观测：进程层恒为 `Source::None`——`SessionManager::view()`
/// 对 external 行三种情况都只说"不知道"（活着 / 没了 / 根本没有进程号），所以它到不了
/// `observe_hooked`，规则要在 `observe` 第 1 步的分支里落（agora-rkl）。
fn external(liveness: Liveness, now: i64) -> Observation<'static> {
    Observation {
        process: Assessment::unknown("external session: hook only"),
        liveness,
        text: None,
        runtime: None,
        epoch: 1,
        now,
    }
}

/// 一行真值：活性、SessionStart 之后多久、该是什么、为什么是它。
struct Row {
    liveness: Liveness,
    at: i64,
    want: Status,
    why: &'static str,
}

const EXTERNAL_STARTING: &[Row] = &[
    Row {
        liveness: Liveness::Alive,
        at: 9,
        want: Status::Starting,
        why: "宽限内不动：agent 可能真在起",
    },
    Row {
        liveness: Liveness::Alive,
        at: 10,
        want: Status::TurnDone,
        why: "起好了、停在提示符等第一条指令（zuan ef0e50 曾钉在 starting 180 h）",
    },
    Row {
        liveness: Liveness::Alive,
        at: 2 * 3600,
        want: Status::TurnDone,
        why: "进程号活着：沉默兜底够不着它，只有衰减能把它送到 TURN_DONE",
    },
    Row {
        liveness: Liveness::Unknown,
        at: 9,
        want: Status::Starting,
        why: "无句柄（Codex Desktop、丢了进程号的旧检查点）：宽限内同样不动",
    },
    Row {
        liveness: Liveness::Unknown,
        at: 10,
        want: Status::TurnDone,
        why: "同一条衰减不分 origin、也不分活没活着（MISSION §4.3）",
    },
    Row {
        liveness: Liveness::Unknown,
        at: 2 * 3600,
        want: Status::Unknown,
        why:
            "无句柄 + 沉默过 external_silent_after：兜底说\"看不清\"，比\"等指令\"诚实（agora-tql）",
    },
];

#[test]
fn external_starting_decays_to_turn_done() {
    // 关掉 observe 第 1 步 external 分支里的 decay_starting → 两行 TurnDone 断言红。
    // 关掉同一步里的沉默兜底（agora-tql 那条）→ 最后那行从 Unknown 变 TurnDone 红；
    // 把衰减移到兜底之后不会红：兜底一旦落 UNKNOWN，衰减的门就关上不再开；两条从 agora-5gg.2 起用
    // 的不是同一个 quiet 时钟（衰减按收到时刻、兜底按事件时刻），但兜底成立必然也在宽限之外，
    // 所以仍逐行同色（2026-09-19 实测）。
    for row in EXTERNAL_STARTING {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        let a = m.observe(external(row.liveness, row.at));
        assert_eq!(
            (a.status, a.source),
            (row.want, Source::Hook),
            "external STARTING + {:?} + {} s → {:?}（{}），实际 {a:?}",
            row.liveness,
            row.at,
            row.want,
            row.why
        );
    }
}

#[test]
fn external_starting_is_never_a_dead_end() {
    // STARTING 不许是没有出口的筐（epic agora-5gg 的立论）：进程号活着与不知道的两档 external 行，
    // 衰减之后要么离开 STARTING、要么被进程事实说结束，不会自己再回去。
    // 关掉 observe 第 1 步 external 分支里的 decay_starting → 第一段（Tick 之后仍是 Starting）红。
    for liveness in [Liveness::Alive, Liveness::Unknown] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        m.observe(external(liveness, 3600));
        assert_ne!(m.current().status, Status::Starting, "{liveness:?}");
        for at in [3601, 7201, 100_000] {
            let a = m.observe(external(liveness, at));
            assert_ne!(a.status, Status::Starting, "{liveness:?} @ {at} → {a:?}");
        }
    }
    // 进程事实到了仍然压倒 hook：没有"先衰减成 TURN_DONE 才说结束"这一说。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    let gone = Assessment::new(
        Status::Finished,
        Source::Process,
        0.8,
        Some("external process gone (no exit status)"),
    );
    let a = m.observe(Observation {
        process: gone,
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 3600,
    });
    assert_eq!(
        (a.status, a.source),
        (Status::Finished, Source::Process),
        "{a:?}"
    );
}

/// 有 `runtime_ref`、运行时此刻正常应答、而列表里找不到这一行——进程层给出的事实由
/// `SessionManager::view` 的上半段算好后喂进来（真值表不起运行时，见文件头）。这一格过去是
/// UNKNOWN `runtime session missing`，每一行钉在 ? 没有出口；agora-u5p 判它是结束的事实。
/// 降级那行放进来是为了对照：同一条读路径，"运行时答话说没有"与"运行时读不到"不是一件事。
fn gone(gone: agora::status::RuntimeGone, killed_by_user: bool) -> Assessment {
    agora::status::runtime_gone(gone, killed_by_user)
}

/// 一行真值：进程层给什么、该落成什么、reason 里必须有的那半句、为什么是它。
/// 表做成函数而不是 const：`gone()` 要 format! 出 reason，编译期算不出来（上面
/// `EXTERNAL_STARTING` 那张表只放字段值，放得下 const）。
#[derive(Debug)]
struct GoneRow {
    given: Assessment,
    want: (Status, Source),
    reason: &'static str,
    why: &'static str,
}

fn runtime_session_gone_rows() -> Vec<GoneRow> {
    vec![
        GoneRow {
            given: gone(agora::status::RuntimeGone::Session, false),
            want: (Status::Finished, Source::Process),
            reason: "runtime session gone (session gone",
            why: "会话连同 pane 一起没了 ⇒ pane 进程收 SIGHUP，agent 确定不在（Mac 2026-09-18 A2）",
        },
        GoneRow {
            given: gone(agora::status::RuntimeGone::Server, false),
            want: (Status::Finished, Source::Process),
            reason: "runtime session gone (server gone",
            why: "整个 server 连不上：一屋子行同时结束，reason 要说得出是哪一种没了",
        },
        GoneRow {
            given: gone(agora::status::RuntimeGone::Session, true),
            want: (Status::Finished, Source::Process),
            reason: "killed by user (runtime session gone",
            why: "人在 Dashboard 按过 Kill：结论一样，口径按他做的那件事写，通知静音（§4.6）",
        },
        GoneRow {
            given: Assessment::unknown("runtime unavailable: protocol version mismatch"),
            want: (Status::Unknown, Source::None),
            reason: "runtime unavailable",
            why: "运行时整体读不到 ≠ 会话没了（ADR-001 D7）：这一格不许抬成结束，也不写 ended_at",
        },
    ]
}

#[test]
fn runtime_session_gone_is_a_fact_with_an_exit_not_a_dead_end() {
    // 每一行都从 RUNNING 起步：求差器第一轮只建基线（notes ④），这里要看的是"运行中死了怎么落"。
    // 关掉 status::runtime_gone 的 Status::Finished（改回 unknown）→ 前三行的 Finished 红；
    // 删掉降级那一行对应的 UNKNOWN 分支（把读不到当已死）→ 第四行红。
    for row in runtime_session_gone_rows() {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        m.observe(Observation {
            process: Assessment::new(Status::Running, Source::Process, 1.0, None),
            liveness: Liveness::Alive,
            text: None,
            runtime: None,
            epoch: 1,
            now: 30,
        });
        let a = m.observe(Observation {
            process: row.given.clone(),
            liveness: Liveness::Dead,
            text: None,
            runtime: None,
            epoch: 1,
            now: 60,
        });
        assert_eq!(
            (a.status, a.source),
            row.want,
            "{row:?} → {a:?}（{}）",
            row.why
        );
        assert!(
            a.reason.as_deref().unwrap_or_default().contains(row.reason),
            "{row:?} → {a:?}"
        );
    }
}

#[test]
fn runtime_session_gone_does_not_outvote_a_hook_that_spoke_first() {
    // 0.8 不是 1.0（agora-rzh 同一条理由）：宿主自己说了 `session ended (hook)` 的行，后来的
    // "会话没了"只把进程事实带上行，不抢 hook 的说法。把 conf 抬到 1.0 → reason 被抢成
    // `runtime session gone …` 而红（`Machine::process_fact_is_no_better` 不再成立）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.observe(Observation {
        process: Assessment::new(Status::Running, Source::Process, 1.0, None),
        liveness: Liveness::Alive,
        text: None,
        runtime: None,
        epoch: 1,
        now: 30,
    });
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 40);
    let a = m.observe(Observation {
        process: gone(agora::status::RuntimeGone::Session, false),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
    });
    assert_eq!(a.status, Status::Finished, "{a:?}");
    assert_eq!(a.source, Source::Hook, "{a:?}");
    assert_eq!(a.reason.as_deref(), Some("session ended (hook)"), "{a:?}");
}
