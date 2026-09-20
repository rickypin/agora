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

use std::path::PathBuf;
use std::time::Duration;

use agora::runtime::{Exit, RuntimeRef, RuntimeSession, Size};
use agora::status::{
    AgoraEvent, Assessment, DetectionResult, EndCause, HostEndReason, Liveness, Machine,
    MachineConfig, Observation, RuntimeGone, Source, Status, UnknownCause,
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
        process: Assessment::unknown("external session: hook only")
            .with_unknown(UnknownCause::NoObservation),
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

// ─────────────────── agora-5gg.6: 每一格的"为什么"是一个枚举值 ───────────────────
//
// 上面那些行只断言到 `reason` 那句话为止，而 `reason` 是给人看的：同一件"结束"在行上好几种说法
// （`session ended (hook)`、`superseded: …`、`external process gone (no exit status)`、
// `runtime session gone (…)`），宿主 SessionEnd 自带的 reason（clear / resume / logout / …）在
// `machine.rs` 那里整个被吞掉。调用方（`src/events.rs` 的通知规则、前端的"这一格给什么出口"）
// 只能对一句人话做 `starts_with`——MISSION §2.3 规则 10 禁的正是这个形状。
//
// 于是每一格还多问一句：**结束的原因 / 说不清的原因是不是封闭集合里的一个值**。这三条守卫钉的是
// 一件事：`reason` 的措辞从此可以随便改，程序读的那一栏改不了也漏得掉。

/// 真值表认得的结束原因（封闭集合的成员表）。新增一档要同时改四处：`src/status/mod.rs` 的
/// `EndCause`、这张表、`docs/spec/api.md`「会话形态」、`src/api/version.rs` 的 minor（§7.3）。
const END_CAUSE_KINDS: &[&str] = &[
    "exit_code",
    "signal",
    "killed_by_user",
    "host_session_end",
    "superseded",
    "process_gone",
    "runtime_gone",
];
/// `host_session_end` 的 value：宿主 SessionEnd 的 reason 归一化之后的五档。三家的原话
/// （`prompt_input_exit`、`shutdown` 那一类）不进枚举，原话留在 `reason` 里。
const HOST_END_REASONS: &[&str] = &["clear", "resume", "logout", "exit", "other"];
/// 说不清的原因（封闭集合），升格规则同上。
const UNKNOWN_CAUSES: &[&str] = &[
    "runtime_unavailable",
    "hooks_silent_screen",
    "prompt_gone",
    "hooks_silent_no_handle",
    "no_observation",
    "exit_status_missing",
];

/// 这一格的结束原因，写成 api.md 里那个形态的短字符串（`kind`，带 value 的附上 value）。
/// 顺带做完两件事：`is_some` 与"成员表里查得到"——缺一个都 panic，成员表拦的是"私自加一档"。
fn end_tag(a: &Assessment) -> String {
    let cause = a
        .end_cause
        .as_ref()
        .unwrap_or_else(|| panic!("FINISHED / FAILED 行没带 end_cause: {a:?}"));
    let v = serde_json::to_value(cause).unwrap();
    let kind = v["kind"].as_str().unwrap_or_default();
    assert!(
        END_CAUSE_KINDS.contains(&kind),
        "end_cause.kind={kind:?} 不在封闭集合 {END_CAUSE_KINDS:?} 里：新增一档要同步 api.md 与 api_version"
    );
    match &v["value"] {
        serde_json::Value::Null => kind.to_owned(),
        serde_json::Value::String(s) => {
            if kind == "host_session_end" {
                assert!(
                    HOST_END_REASONS.contains(&s.as_str()),
                    "host_session_end 的 value={s:?} 不在 {HOST_END_REASONS:?} 里：宿主的原话不该漏进枚举"
                );
            }
            format!("{kind}:{s}")
        }
        other => format!("{kind}:{other}"),
    }
}

/// UNKNOWN 那一格的"为什么说不清"。
fn unknown_tag(a: &Assessment) -> String {
    let cause = a
        .unknown_cause
        .as_ref()
        .unwrap_or_else(|| panic!("UNKNOWN 行没带 unknown_cause: {a:?}"));
    let v = serde_json::to_value(cause).unwrap();
    let s = v.as_str().unwrap_or_default();
    assert!(
        UNKNOWN_CAUSES.contains(&s),
        "unknown_cause={s:?} 不在封闭集合 {UNKNOWN_CAUSES:?} 里"
    );
    s.to_owned()
}

/// 每一格都过一遍：`end_cause` 与 `unknown_cause` 各归各的状态，不并存、不都空。
fn assert_cause_matches_status(a: &Assessment, cell: &str) {
    match a.status {
        Status::Finished | Status::Failed => {
            assert!(a.end_cause.is_some(), "{cell}: {a:?}");
            assert!(
                a.unknown_cause.is_none(),
                "{cell}: 结束了就不该带 unknown_cause: {a:?}"
            );
        }
        Status::Unknown => {
            assert!(a.unknown_cause.is_some(), "{cell}: {a:?}");
            assert!(
                a.end_cause.is_none(),
                "{cell}: 没结束就不该带 end_cause: {a:?}"
            );
        }
        _ => assert!(
            a.end_cause.is_none() && a.unknown_cause.is_none(),
            "{cell}: 还在跑的行两个都不该有: {a:?}"
        ),
    }
}

/// 进程层的一个事实：`exit` 是运行时报的退出码 / 信号，None = 还没收集到。
fn rt_exit(exit: Option<Exit>, killed_by_user: bool) -> Assessment {
    let session = RuntimeSession {
        r#ref: RuntimeRef("fake:agora:x".into()),
        name: "x".into(),
        pid: Some(1),
        alive: false,
        exit,
        exited_at: None,
        title: String::new(),
        cwd: PathBuf::from("/"),
        attached: false,
        size: Size { cols: 80, rows: 24 },
        managed: true,
        output_at: None,
    };
    agora::status::process_layer(Some(&session), None, killed_by_user)
}

/// 先跑起来、再喂一条进程事实：结束与"说不清"的那些行都从这个形状起步（observe 第 1 步：
/// 进程事实压倒一切）。不起 tmux——`RuntimeSession` 只是一个数据，真值表不碰运行时。
fn after_running(fact: Assessment) -> Assessment {
    let mut m = Machine::new(cfg(), false, 1, 0);
    m.observe(Observation {
        process: Assessment::new(Status::Running, Source::Process, 1.0, None),
        liveness: Liveness::Alive,
        text: None,
        runtime: None,
        epoch: 1,
        now: 10,
    });
    m.observe(Observation {
        process: fact,
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 20,
    })
}

/// 一格的结束原因：`feed` 怎么把这行喂出来，`want` 是该落在哪个值上。
struct EndRow {
    why: &'static str,
    feed: fn() -> Assessment,
    want: &'static str,
}

fn end_rows() -> Vec<EndRow> {
    /// 喂一条 hook 事件后取状态机的结论。
    fn hook(e: AgoraEvent) -> Assessment {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        m.apply(&e, 1, 1);
        m.current().clone()
    }
    /// 喂一条 external 事件的 reason（含 receiver 给无句柄行改写后的长句，agora-s3r）。
    fn session_end(reason: Option<&str>) -> Assessment {
        hook(AgoraEvent::SessionEnded(reason.map(str::to_owned)))
    }
    vec![
        EndRow {
            why: "退出码 0 = 干净结束",
            feed: || after_running(rt_exit(Some(Exit::Code(0)), false)),
            want: "exit_code:0",
        },
        EndRow {
            why: "退出码非 0 = FAILED，值就是那个码（分不开两种退法就白报了）",
            feed: || after_running(rt_exit(Some(Exit::Code(3)), false)),
            want: "exit_code:3",
        },
        EndRow {
            why: "运行时报的是信号不是码",
            feed: || after_running(rt_exit(Some(Exit::Signal("hup".into())), false)),
            want: "signal:hup",
        },
        EndRow {
            why: "人在 Dashboard 按过 Kill：哪个码不重要，通知要静音（§4.6）",
            feed: || after_running(rt_exit(Some(Exit::Code(143)), true)),
            want: "killed_by_user",
        },
        EndRow {
            why: "Kill 的另一半形状：运行时直接报信号",
            feed: || after_running(rt_exit(Some(Exit::Signal("term".into())), true)),
            want: "killed_by_user",
        },
        EndRow {
            why: "server 在、只有这一个会话没了（agora-u5p）",
            feed: || after_running(agora::status::runtime_gone(RuntimeGone::Session, false)),
            want: "runtime_gone:session",
        },
        EndRow {
            why: "整个 tmux server 连不上：一屋子行同时结束，值要说得出是哪一种",
            feed: || after_running(agora::status::runtime_gone(RuntimeGone::Server, false)),
            want: "runtime_gone:server",
        },
        EndRow {
            why: "按过 Kill 之后的 runtime gone 只报 Kill：session/server 那半句留在 reason 里",
            feed: || after_running(agora::status::runtime_gone(RuntimeGone::Session, true)),
            want: "killed_by_user",
        },
        EndRow {
            why: "external 行只剩探活：号没了就是结束，没有退出码可分",
            feed: || after_running(agora::status::external_process_gone()),
            want: "process_gone",
        },
        EndRow {
            why: "人在提示符上退出的（宿主的 prompt_input_exit 归一到 exit）",
            feed: || session_end(Some("prompt_input_exit")),
            want: "host_session_end:exit",
        },
        EndRow {
            why: "宿主自己收尾关闭的（shutdown 同归 exit：动作一样，这行不用再管）",
            feed: || session_end(Some("shutdown")),
            want: "host_session_end:exit",
        },
        EndRow {
            why: "登出",
            feed: || session_end(Some("logout")),
            want: "host_session_end:logout",
        },
        EndRow {
            why: "换对话：这就是 5gg.6 立论的那一行——以前它与人自己退出的一模一样",
            feed: || session_end(Some("resume")),
            want: "host_session_end:resume",
        },
        EndRow {
            why: "宿主说了 other",
            feed: || session_end(Some("other")),
            want: "host_session_end:other",
        },
        EndRow {
            why: "宿主没给 reason：认不出的一律 other，不自造一个值",
            feed: || session_end(None),
            want: "host_session_end:other",
        },
        EndRow {
            why: "以后某家添的新词：照单收进 other，不该让 agora 读不懂这一行",
            feed: || session_end(Some("org_policy_revoked")),
            want: "host_session_end:other",
        },
        EndRow {
            why: "无句柄 external 行的 /clear：身份是被清掉的那个对话 id，旧行到此为止（agora-s3r）。\
                   receiver 改写的那句长话归一化只看第一个词，枚举里仍是 clear",
            feed: || session_end(Some("clear (external row: the new id lands on a new row)")),
            want: "host_session_end:clear",
        },
        EndRow {
            why: "同一进程换到了新对话（不发 SessionEnd 的那两家）",
            feed: || hook(AgoraEvent::Superseded),
            want: "superseded",
        },
    ]
}

#[test]
fn every_finished_and_failed_row_names_its_end_cause() {
    // 关掉任何一处 `.with_end(..)` → 那一格红（end_cause 是 None）；把 `HostEndReason::from_host`
    // 的表改窄（比如认漏 shutdown）→ 那一格落到 host_session_end:other 而红。
    for row in end_rows() {
        let a = (row.feed)();
        assert!(
            matches!(a.status, Status::Finished | Status::Failed),
            "{}: 这一格该是结束的行，实际 {a:?}",
            row.why
        );
        assert_cause_matches_status(&a, row.why);
        assert_eq!(end_tag(&a), row.want, "{}: {a:?}", row.why);
    }
}

#[test]
fn a_hook_that_spoke_first_keeps_its_end_cause_when_the_process_agrees() {
    // 同状态、同把握的进程事实不抢 hook 的说法（agora-rzh）。这条规则现在也要保住 end_cause：
    // 人在终端里自己退出的那一行，后来的"进程没了"只把 process 三态换成 gone，不许把
    // host_session_end 换成 process_gone —— 换成 process_gone 之后 external 行会开始弹通知
    // （`src/events.rs` 的 notification_for 按 end_cause 分档，agora-5gg.6）。
    // 关掉 observe 里的 process_fact_is_no_better → 两条断言都红（reason 与 end_cause 一起被抢）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(
        &AgoraEvent::SessionEnded(Some("prompt_input_exit".into())),
        1,
        1,
    );
    let a = m.observe(Observation {
        process: agora::status::external_process_gone(),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 5,
    });
    assert_eq!(a.status, Status::Finished, "{a:?}");
    assert_eq!(a.source, Source::Hook, "{a:?}");
    assert_eq!(a.reason.as_deref(), Some("session ended (hook)"), "{a:?}");
    assert_eq!(end_tag(&a), "host_session_end:exit", "{a:?}");
}

/// UNKNOWN 的一行：喂法 + 该落在哪个值。
struct UnknownRow {
    why: &'static str,
    feed: fn() -> Assessment,
    want: &'static str,
}

fn unknown_rows() -> Vec<UnknownRow> {
    /// hook 沉默到阈值、屏幕像在等人（ADR-002 D1 的沉默规则）。
    fn hooks_silent_screen() -> Assessment {
        let session = RuntimeSession {
            r#ref: RuntimeRef("fake:agora:x".into()),
            name: "x".into(),
            pid: Some(1),
            alive: true,
            exit: None,
            exited_at: None,
            title: String::new(),
            cwd: PathBuf::from("/"),
            attached: false,
            size: Size { cols: 80, rows: 24 },
            managed: true,
            output_at: Some(0),
        };
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        let obs = |m: &mut Machine, now: i64| {
            m.observe(Observation {
                process: Assessment::new(Status::Running, Source::Process, 1.0, None),
                liveness: Liveness::Alive,
                text: Some(DetectionResult {
                    status: Status::Waiting,
                    confidence: 0.7,
                    reason: "permission prompt".to_owned(),
                }),
                runtime: Some(&session),
                epoch: 1,
                now,
            })
        };
        let a = obs(&mut m, 601);
        assert_eq!(a.status, Status::Unknown, "{a:?}");
        a
    }
    /// 挂起的权限提示从屏幕上消失了（agora-9cd）。
    fn prompt_gone() -> Assessment {
        let session = RuntimeSession {
            r#ref: RuntimeRef("fake:agora:x".into()),
            name: "x".into(),
            pid: Some(1),
            alive: true,
            exit: None,
            exited_at: None,
            title: String::new(),
            cwd: PathBuf::from("/"),
            attached: false,
            size: Size { cols: 80, rows: 24 },
            managed: true,
            output_at: Some(0),
        };
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("do x".into()), 1, 0);
        m.apply(
            &AgoraEvent::DecisionNeeded {
                tool_use_id: "t1".into(),
                summary: "Bash: sleep 20".into(),
            },
            1,
            1,
        );
        let obs = |m: &mut Machine, now: i64, on_screen: bool| {
            m.observe(Observation {
                process: Assessment::new(Status::Running, Source::Process, 1.0, None),
                liveness: Liveness::Alive,
                text: on_screen.then(|| DetectionResult {
                    status: Status::Waiting,
                    confidence: 0.7,
                    reason: "prompt: Do you want to proceed?".to_owned(),
                }),
                runtime: Some(&session),
                epoch: 1,
                now,
            })
        };
        obs(&mut m, 3, true);
        obs(&mut m, 5, false);
        let a = obs(&mut m, 7, false);
        assert_eq!(a.status, Status::Unknown, "{a:?}");
        a
    }
    vec![
        UnknownRow {
            why: "刚建起来、一条观测都没有：说不清，但没有'说不清的细节'可说",
            feed: || Machine::new(cfg(), true, 1, 0).current().clone(),
            want: "no_observation",
        },
        UnknownRow {
            why: "hook 沉默到阈值、只有屏幕可看（D1）：宁可 UNKNOWN 也不猜 WAITING",
            feed: hooks_silent_screen,
            want: "hooks_silent_screen",
        },
        UnknownRow {
            why: "挂着的提示从屏幕上没了（终端里答了 / Esc 中断，宿主一个事件都不发）",
            feed: prompt_gone,
            want: "prompt_gone",
        },
        UnknownRow {
            why: "无句柄 external 行沉默到 hooks.external_silent_after（agora-tql）",
            feed: || {
                let mut m = Machine::new(cfg(), true, 1, 0);
                m.apply(&AgoraEvent::SessionStarted, 1, 0);
                m.observe(external(Liveness::Unknown, 7201))
            },
            want: "hooks_silent_no_handle",
        },
        UnknownRow {
            why: "运行时整体读不到 ≠ 会话没了（ADR-001 D7），process 同时是 unknown",
            feed: || {
                after_running(agora::status::runtime_unavailable(
                    "protocol version mismatch",
                ))
            },
            want: "runtime_unavailable",
        },
        UnknownRow {
            why: "运行时报了'退了'、退出码还没收集到：下一 tick 补上，不猜 FINISHED 也不猜 FAILED",
            feed: || after_running(rt_exit(None, false)),
            want: "exit_status_missing",
        },
    ]
}

#[test]
fn every_unknown_row_names_why_it_is_unknown() {
    // 关掉任何一处 `.with_unknown(..)` → 那一格红。这一条还拦"私自加一档"：新值没进
    // UNKNOWN_CAUSES 就会被 unknown_tag 的成员表拦下。
    for row in unknown_rows() {
        let a = (row.feed)();
        assert_eq!(a.status, Status::Unknown, "{}: 实际 {a:?}", row.why);
        assert_cause_matches_status(&a, row.why);
        assert_eq!(unknown_tag(&a), row.want, "{}: {a:?}", row.why);
    }
}

#[test]
fn cause_wire_vocabulary_is_the_locked_set() {
    // 调用方按这些字面量分支（api.md「会话形态」），所以它们是 API 形态：改(rename_all) 或改变体名
    // 都会红这里。带 value 的三种写成完整 JSON，一眼看得出线长什么样。
    assert_eq!(
        serde_json::to_value(EndCause::ExitCode(3)).unwrap(),
        serde_json::json!({ "kind": "exit_code", "value": 3 })
    );
    assert_eq!(
        serde_json::to_value(EndCause::Signal("hup".to_owned())).unwrap(),
        serde_json::json!({ "kind": "signal", "value": "hup" })
    );
    assert_eq!(
        serde_json::to_value(EndCause::KilledByUser).unwrap(),
        serde_json::json!({ "kind": "killed_by_user" })
    );
    assert_eq!(
        serde_json::to_value(EndCause::HostSessionEnd(HostEndReason::Clear)).unwrap(),
        serde_json::json!({ "kind": "host_session_end", "value": "clear" })
    );
    assert_eq!(
        serde_json::to_value(EndCause::Superseded).unwrap(),
        serde_json::json!({ "kind": "superseded" })
    );
    assert_eq!(
        serde_json::to_value(EndCause::ProcessGone).unwrap(),
        serde_json::json!({ "kind": "process_gone" })
    );
    assert_eq!(
        serde_json::to_value(EndCause::RuntimeGone(RuntimeGone::Server)).unwrap(),
        serde_json::json!({ "kind": "runtime_gone", "value": "server" })
    );
    for reason in ["clear", "resume", "logout", "exit", "other"] {
        let v = serde_json::to_value(EndCause::HostSessionEnd(
            agora::status::HostEndReason::from_host(Some(reason)),
        ))
        .unwrap();
        assert_eq!(v["kind"], "host_session_end");
        assert_eq!(v["value"], reason, "归一化把 {reason} 换掉了");
    }
    assert_eq!(
        serde_json::to_value(UnknownCause::HooksSilentNoHandle).unwrap(),
        serde_json::json!("hooks_silent_no_handle")
    );
    // 反方向也要走得通：peer 与页面读到的就是这个 JSON。
    let back: EndCause = serde_json::from_value(serde_json::json!({
        "kind": "host_session_end", "value": "resume"
    }))
    .unwrap();
    assert_eq!(back, EndCause::HostSessionEnd(HostEndReason::Resume));
    // 老节点（这条修复之前）不发 end_cause：缺失读成 None，不是读成"没有原因"。
    let none: agora::status::Assessment = serde_json::from_value(serde_json::json!({
        "status": "finished", "source": "hook", "confidence": 0.8, "reason": "session ended (hook)"
    }))
    .unwrap();
    assert_eq!(none.end_cause, None);
    assert_eq!(none.unknown_cause, None);
}
