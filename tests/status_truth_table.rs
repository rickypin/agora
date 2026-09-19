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
    // 把衰减移到兜底之后不会红（两者同一个 quiet 时钟，2026-09-19 实测逐行同色）。
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
