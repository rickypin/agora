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

// ═══════════════════ 真值表本身（docs/spec/status.md 第 2 / 3 节）═══════════════════
//
// 上面那些测试按**规则**组织（衰减、沉默、end_cause……），这一节按**格**组织：docs/spec/status.md
// 的表里每一行 = 一个 id = 一个 `#[test]`，测试名以 id 开头，于是"表里这一行有没有守卫"是 grep
// 一下就有答案的事。两边的对账由 `spec_rows_and_code_rows_agree` 做——它读那张 markdown 表与下面
// 的 `ROWS`，逐行核对 id、判定符号、三列文本与守卫测试名：新增一行不写守卫、删掉一行不删表、
// 把 ✗ 改成 ✓，都会红这里（2026-09-21 定，agora-5gg.16）。
//
// 每一行断言三列：`status` × `source` × `process`。前两列是状态机的结论，第三列是调用方看到的
// 进程三态，由 `ProcessState::derive` 从结论 + `Liveness` + "运行时整体读不到"导出（裁决
// agora-5gg.4 选 A、实施 agora-5gg.18）——表里 `process` 那一列写的是它，不是状态机内部那个
// `Liveness`；两者不一致的两行（a14 / x08）在"含义"列里点明了。
//
// 喂进去的是数据、不起运行时也不起 daemon（见文件头）：`RuntimeSession` 只是一个结构体，
// `tick_rt` / `tick_external` 把 `SessionManager::view()` 上半段那套"按 (origin, 运行时) 算进程
// 事实"的规则在这里复现一遍，视图那一段的等价性另有集成测试兜着（`tests/session_manager.rs`、
// `tests/hooks_external.rs`，本节的"守卫"列点名了它们的那些行）。

use agora::session::Origin;
use agora::status::ProcessState;

/// 判定符号（docs/spec/status.md 第 1 节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// ✓ 合法
    Legal,
    /// ◐ 合法但只许短暂
    Brief,
    /// ✗ 不可能
    Never,
}

impl Verdict {
    fn symbol(self) -> &'static str {
        match self {
            Verdict::Legal => "✓",
            Verdict::Brief => "◐",
            Verdict::Never => "✗",
        }
    }
}

/// 表里的一格。喂法写在各自的 `#[test]` 里，这里只留对账要的三样。
/// （名字不叫 `Row`：文件前半段那张 `EXTERNAL_STARTING` 已经占了它。）
struct Cell {
    id: &'static str,
    verdict: Verdict,
    /// `status | process | source`，与 docs/spec/status.md 表里那三列逐字相同（对账比这个）。
    cell: &'static str,
    /// 为什么是它 / 为什么不可能（表里"含义"列的浓缩，红的时候印出来）。
    why: &'static str,
}

const ROWS: &[Cell] = &[
    // ── 第 2 节：origin = agora / adopted（有运行时句柄）──
    Cell {
        id: "a01",
        verdict: Verdict::Legal,
        cell: "STARTING | alive | process（本代起始 < 2 s）",
        why: "刚起，等一下",
    },
    Cell {
        id: "a02",
        verdict: Verdict::Legal,
        cell: "STARTING | alive | hook（SessionStart 之后 ≤ 10 s）",
        why: "刚起，等一下：宽限内不许衰减",
    },
    Cell {
        id: "a03",
        verdict: Verdict::Never,
        cell: "STARTING | alive | hook（> 10 s）",
        why: "起好了停在提示符等第一条指令，该归 TURN_DONE（agora-okr）",
    },
    Cell {
        id: "a04",
        verdict: Verdict::Legal,
        cell: "RUNNING | alive | process",
        why: "在干活，不用管",
    },
    Cell {
        id: "a05",
        verdict: Verdict::Legal,
        cell: "RUNNING | alive | hook",
        why: "在干活，不用管",
    },
    Cell {
        id: "a06",
        verdict: Verdict::Legal,
        cell: "WAITING | alive | hook",
        why: "答它：能经 hook 答的按 respond_via = hook，否则打开终端",
    },
    Cell {
        id: "a07",
        verdict: Verdict::Never,
        cell: "WAITING | alive | text，agent 有 hook",
        why: "ADR-002 D1：文本抬不起 WAITING，只有 hook 沉默兜底能降它",
    },
    Cell {
        id: "a08",
        verdict: Verdict::Legal,
        cell: "TURN_DONE | alive | hook",
        why: "看结果、给下一条",
    },
    Cell {
        id: "a09",
        verdict: Verdict::Never,
        cell: "TURN_DONE | alive | text 或 activity",
        why: "「一轮做完」只有宿主自己说得出来（Stop / idle）",
    },
    Cell {
        id: "a10",
        verdict: Verdict::Legal,
        cell: "IDLE | alive | activity（无 hook 的行）",
        why: "安静了一阵，没人说为什么——IDLE 只出现在兜底路径",
    },
    Cell {
        id: "a11",
        verdict: Verdict::Never,
        cell: "IDLE | alive | activity，agent 有 hook",
        why: "活动层不产 IDLE：hook 活着而不说话是说不清（a18），不是安静",
    },
    Cell {
        id: "a12",
        verdict: Verdict::Legal,
        cell: "FINISHED | gone | process（退出码 0 / killed by user）",
        why: "退了，看结果或清理；按过 Kill 的不弹通知",
    },
    Cell {
        id: "a13",
        verdict: Verdict::Legal,
        cell: "FINISHED | gone | process（`runtime_gone`）",
        why: "运行时会话没了是事实：Restart 重建或删除（agora-u5p）",
    },
    Cell {
        id: "a14",
        verdict: Verdict::Brief,
        cell: "FINISHED | gone | hook（SessionEnd 先于进程退出被观测）",
        why: "下一 tick 进程层带着退出码换掉 source（同状态同分不抢，agora-rzh）",
    },
    Cell {
        id: "a15",
        verdict: Verdict::Legal,
        cell: "FAILED | gone | process（exit ≠ 0 / signal）",
        why: "出错了，看终端；end_cause 带的是那个码或信号",
    },
    Cell {
        id: "a16",
        verdict: Verdict::Never,
        cell: "FINISHED / FAILED | alive，持续 | 任何",
        why: "进程退出压倒一切 + Q4：结束了的行不许报出 alive",
    },
    Cell {
        id: "a17",
        verdict: Verdict::Legal,
        cell: "UNKNOWN | unknown | process（`runtime unavailable`）",
        why: "agora 失明，横幅说明；恢复即自愈，绝不写 ended_at（ADR-001 D7）",
    },
    Cell {
        id: "a18",
        verdict: Verdict::Legal,
        cell: "UNKNOWN | alive | text（`hooks silent; screen: …` / `permission prompt gone`）",
        why: "hook 没声音而屏幕像在等人：打开终端，或按 hooks_unheard 去查 hook",
    },
    Cell {
        id: "a19",
        verdict: Verdict::Never,
        cell: "UNKNOWN | gone | 任何（旧版的 `runtime session missing`）",
        why: "运行时会话没了是事实、不是看不清（a13）",
    },
    Cell {
        id: "a20",
        verdict: Verdict::Never,
        cell: "UNKNOWN | 任何 | hook",
        why: "有句柄的行里 UNKNOWN 只有 a17 / a18 两格；hook 的词表不含它",
    },
    // ── 第 3 节：origin = external / headless（无运行时句柄）──
    Cell {
        id: "x01",
        verdict: Verdict::Legal,
        cell: "STARTING | alive 或 unknown | hook（≤ 10 s）",
        why: "刚起：无句柄行也一样，进程号在不在都不影响宽限",
    },
    Cell {
        id: "x02",
        verdict: Verdict::Never,
        cell: "STARTING | 任何 | hook（> 10 s）",
        why: "衰减不分 origin（现场 zuan ef0e50 钉过 180 h，agora-rkl）",
    },
    Cell {
        id: "x03",
        verdict: Verdict::Legal,
        cell: "RUNNING / WAITING / TURN_DONE | alive | hook",
        why: "hook 说什么就是什么：进程号活着不会改它的说法",
    },
    Cell {
        id: "x04",
        verdict: Verdict::Legal,
        cell: "RUNNING / WAITING / TURN_DONE | unknown | hook",
        why: "同上，但 agora 说不上进程在不在（Codex Desktop）",
    },
    Cell {
        id: "x05",
        verdict: Verdict::Never,
        cell: "RUNNING / WAITING / TURN_DONE | gone | 任何",
        why: "进程号探不到了就是结束，落在 x07",
    },
    Cell {
        id: "x06",
        verdict: Verdict::Never,
        cell: "IDLE | 任何 | 任何",
        why: "无句柄行没有活动来源（没有 pane 可采输出）",
    },
    Cell {
        id: "x07",
        verdict: Verdict::Legal,
        cell: "FINISHED | gone | process（`process_gone`）",
        why: "一个事件都没来、进程消失：崩溃 / 关窗口；通知一次",
    },
    Cell {
        id: "x08",
        verdict: Verdict::Legal,
        cell: "FINISHED | gone | hook（`host_session_end{clear|resume|logout|exit|other}` / `superseded`）",
        why: "人自己结束的或换了对话：不通知；号还在跑新对话也报 gone（Q4）",
    },
    Cell {
        id: "x09",
        verdict: Verdict::Never,
        cell: "FAILED | 任何 | 任何",
        why: "没有退出码可拿，分不出两种退法：结束只有 FINISHED",
    },
    Cell {
        id: "x10",
        verdict: Verdict::Brief,
        cell: "UNKNOWN | unknown | hook（`hooks silent; no process handle`）",
        why: "暂态：下一条 hook 恢复，否则满 sessions.external_unknown_ttl 走 DELETE（agora-e08）",
    },
    Cell {
        id: "x11",
        verdict: Verdict::Never,
        cell: "UNKNOWN | alive 或 gone | hook / text / process",
        why: "行上说过话、或探到了进程事实，就不该说不清",
    },
    Cell {
        id: "x12",
        verdict: Verdict::Brief,
        cell: "UNKNOWN | 任何 | none（`no observation yet` / `external session: … hook only`）",
        why: "只允许在检查点恢复 / 重放完成前的瞬间：第一条 hook 事件一到就走",
    },
    Cell {
        id: "x13",
        verdict: Verdict::Legal,
        cell: "本节每一格 | 同 external | 同 external",
        why: "headless 与 external 同一张表：代码里一律问 Origin::is_handleless()",
    },
];

fn cell(id: &str) -> &'static Cell {
    ROWS.iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("真值表里没有 {id} 这一格：加一格要先加进 ROWS"))
}

/// 表里 `process` 那一列：无句柄行按进程号探活的结果直译（FINISHED 之后的 gone 由 derive 的
/// 第一条规则管，见 a16 / x08）。
fn expected_process(liveness: Liveness) -> ProcessState {
    match liveness {
        Liveness::Alive => ProcessState::Alive,
        Liveness::Dead => ProcessState::Gone,
        Liveness::Unknown => ProcessState::Unknown,
    }
}

/// 一次喂进状态机的完整输入：结论 + 导出 `process` 要的那两个事实。
struct Fed {
    a: Assessment,
    liveness: Liveness,
    /// 运行时整体读不到（`view()` 的"降级 + 有句柄"那一支）。
    unreadable: bool,
}

impl Fed {
    fn new(a: Assessment, liveness: Liveness) -> Self {
        Fed {
            a,
            liveness,
            unreadable: false,
        }
    }

    /// 这一行报给调用方的进程三态（`GET /api/sessions` 的 `process`）。
    fn process(&self) -> ProcessState {
        ProcessState::derive(self.a.status, self.liveness, self.unreadable)
    }
}

/// 一个活着的运行时会话。
fn pane(output_at: Option<i64>) -> RuntimeSession {
    RuntimeSession {
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
        output_at,
    }
}

/// 退了、带退出事实的运行时会话。
fn pane_exit(exit: Option<Exit>) -> RuntimeSession {
    RuntimeSession {
        exit,
        alive: false,
        ..pane(None)
    }
}

/// 有句柄的行喂一个 tick：`process` 与 `liveness` 按 `SessionManager::view()` 上半段算。
fn tick_rt(
    m: &mut Machine,
    rt: &RuntimeSession,
    spawn_age: Option<u64>,
    killed: bool,
    text: Option<&DetectionResult>,
    now: i64,
) -> Fed {
    let liveness = if rt.alive {
        Liveness::Alive
    } else {
        Liveness::Dead
    };
    let a = m.observe(Observation {
        process: agora::status::process_layer(Some(rt), spawn_age, killed),
        liveness,
        text: text.cloned(),
        runtime: Some(rt),
        epoch: 1,
        now,
    });
    Fed {
        a,
        liveness,
        unreadable: false,
    }
}

/// 无句柄的行喂一个 tick：进程事实取 `view()` 的 `(origin, None)` 那一支的三种情况，
/// 活动样本恒为 None（没有 pane 可采）。
fn tick_external(m: &mut Machine, liveness: Liveness, now: i64) -> Fed {
    let process = match liveness {
        Liveness::Alive => Assessment::unknown("external session: process alive, hook only")
            .with_unknown(UnknownCause::NoObservation),
        Liveness::Unknown => Assessment::unknown("external session: no runtime, hook only")
            .with_unknown(UnknownCause::NoObservation),
        Liveness::Dead => agora::status::external_process_gone(),
    };
    let a = m.observe(Observation {
        process,
        liveness,
        text: None,
        runtime: None,
        epoch: 1,
        now,
    });
    Fed {
        a,
        liveness,
        unreadable: false,
    }
}

/// 先跑起来（一个活着的 tick），再喂一条**现成的**进程事实（`runtime_gone` /
/// `runtime_unavailable` / 退出码那一类由 `view()` 上半段算出来的结论）。`declared_hooks`
/// 按调用方给的：有 hook 的行与没有 hook 的行在同一条规则上不该有差别，有几格正是钉这点。
fn running_then_fact(
    declared_hooks: bool,
    fact: Assessment,
    liveness: Liveness,
    unreadable: bool,
) -> Fed {
    let mut m = Machine::new(cfg(), declared_hooks, 1, 0);
    let rt = pane(Some(0));
    tick_rt(&mut m, &rt, Some(3600), false, None, 30);
    let a = m.observe(Observation {
        process: fact,
        liveness,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
    });
    Fed {
        a,
        liveness,
        unreadable,
    }
}

/// 宿主先说了结束、进程号还活着的那一瞬（a14 / a16 的喂法）。
fn host_end_while_the_process_is_alive(end: AgoraEvent) -> Fed {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(&end, 1, 1);
    let rt = pane(Some(0));
    // 进程层报的是"活着"：行上是 hook 说的结束，而 pane 里的号还在（旧代码在这里报 finished +
    // alive:true，agora-5gg.18 改掉）。
    tick_rt(&mut m, &rt, Some(3600), false, None, 2)
}

/// 不许出现的东西。
#[derive(Debug, Clone, Copy)]
enum Ban {
    /// 这个状态整行不许出现（不看 source）。
    Status(Status),
    /// 这个 (status, source) 组合不许出现。
    Cell(Status, Source),
    /// 调用方看到的进程事实不许是这个值。
    Process(ProcessState),
}

/// 正向：这一格喂出来就该落在 (status, source, process) 上。
fn lands(id: &str, fed: &Fed, status: Status, source: Source, process: ProcessState) {
    let r = cell(id);
    assert_ne!(
        r.verdict,
        Verdict::Never,
        "{id} 在表里是 ✗（不可能），该用 forbid 断言它"
    );
    assert_eq!(
        (fed.a.status, fed.a.source, fed.process()),
        (status, source, process),
        "{id}「{}」该落在 {status:?} × {source:?} × {process:?}（{}），实际 {a:?}",
        r.cell,
        r.why,
        a = fed.a
    );
    assert_cause_matches_status(&fed.a, id);
}

/// 反向：这一格不许落在 `bans` 指出的任何一栏上。✗ 行同样要钉住它实际落在哪（`instead`），
/// 否则"不许"会变成空话——`None` 只用在喂法本身会落到好几格合法位置的行上（a20 / x09）。
fn forbid(id: &str, fed: &Fed, bans: &[Ban], instead: Option<(Status, Source, ProcessState)>) {
    let r = cell(id);
    assert_eq!(r.verdict, Verdict::Never, "{id} 不是 ✗ 行，不该用 forbid");
    assert!(!bans.is_empty(), "{id}: ✗ 行一条反向断言都没写");
    let got = (fed.a.status, fed.a.source, fed.process());
    for ban in bans {
        match ban {
            Ban::Status(s) => assert_ne!(
                got.0,
                *s,
                "{id}「{}」不该出现 {s:?}（{}），实际 {a:?}",
                r.cell,
                r.why,
                a = fed.a
            ),
            Ban::Cell(s, src) => assert_ne!(
                (got.0, got.1),
                (*s, *src),
                "{id}「{}」不该出现 {s:?} × {src:?}（{}），实际 {a:?}",
                r.cell,
                r.why,
                a = fed.a
            ),
            Ban::Process(p) => assert_ne!(
                got.2,
                *p,
                "{id}「{}」不该报出 {p:?}（{}），实际 {a:?}",
                r.cell,
                r.why,
                a = fed.a
            ),
        }
    }
    if let Some(want) = instead {
        assert_eq!(
            got,
            want,
            "{id}「{}」该落在 {want:?}（{}），实际 {a:?}",
            r.cell,
            r.why,
            a = fed.a
        );
    }
    assert_cause_matches_status(&fed.a, id);
}

// ───────────────────────── 第 2 节：有运行时句柄的行 ─────────────────────────

#[test]
fn a01_starting_from_the_runtime_inside_the_starting_window() {
    // 关掉 `process_layer` 里 `spawn_age < STARTING_WINDOW_SECS` 那一判 → 红（报成 RUNNING）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, Some(0), false, None, 1);
    lands(
        "a01",
        &fed,
        Status::Starting,
        Source::Process,
        ProcessState::Alive,
    );
}

#[test]
fn a02_starting_from_a_hook_inside_the_startup_grace() {
    // 把 startup_grace 调成 0（或删掉 observe_hooked 里那条衰减的门）→ 这一格与 a03 一起红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, None, false, None, 9);
    lands(
        "a02",
        &fed,
        Status::Starting,
        Source::Hook,
        ProcessState::Alive,
    );
}

#[test]
fn a03_hook_starting_never_outlives_the_startup_grace() {
    // 关掉 observe_hooked 的 decay_starting → 每一格都钉在 Starting 而红。现行守卫另有
    // `tests/state_machine.rs::hook_starting_decays_to_turn_done_awaiting_first_prompt`（同一条规则
    // 按规则组织的那一份）。
    let rt = pane(None);
    for now in [10, 11, 3600] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        let fed = tick_rt(&mut m, &rt, None, false, None, now);
        forbid(
            "a03",
            &fed,
            &[Ban::Cell(Status::Starting, Source::Hook)],
            Some((Status::TurnDone, Source::Hook, ProcessState::Alive)),
        );
    }
}

#[test]
fn a04_running_from_the_process_layer() {
    let mut m = Machine::new(cfg(), true, 1, 0);
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, Some(3600), false, None, 3600);
    lands(
        "a04",
        &fed,
        Status::Running,
        Source::Process,
        ProcessState::Alive,
    );
}

#[test]
fn a05_running_from_a_hook() {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, None, false, None, 1);
    lands(
        "a05",
        &fed,
        Status::Running,
        Source::Hook,
        ProcessState::Alive,
    );
}

#[test]
fn a06_waiting_from_a_hook() {
    // 挂起的权限：进程号活着，答它的窗口开着（respond_via 由 `pending_decision` 决定，与这一格同源）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(
        &AgoraEvent::DecisionNeeded {
            tool_use_id: "t1".into(),
            summary: "Bash: sleep 20".into(),
        },
        1,
        1,
    );
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, None, false, None, 2);
    lands(
        "a06",
        &fed,
        Status::Waiting,
        Source::Hook,
        ProcessState::Alive,
    );
}

#[test]
fn a07_text_cannot_raise_a_hooked_row_to_waiting() {
    // 关掉 observe_hooked 的"hook 写过状态就原样返回"（改成走 observe_unhooked）→ 红。
    // `tests/state_machine.rs::text_cannot_raise_hooked_session` 是同一条规则的另一份。
    let rt = pane(None);
    let waiting = DetectionResult {
        status: Status::Waiting,
        confidence: 0.9,
        reason: "permission prompt".to_owned(),
    };
    for now in [2, 4, 6, 8] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        let fed = tick_rt(&mut m, &rt, None, false, Some(&waiting), now);
        forbid(
            "a07",
            &fed,
            &[Ban::Cell(Status::Waiting, Source::Text)],
            Some((Status::Running, Source::Hook, ProcessState::Alive)),
        );
    }
}

#[test]
fn a08_turn_done_from_a_hook() {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 1);
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, None, false, None, 2);
    lands(
        "a08",
        &fed,
        Status::TurnDone,
        Source::Hook,
        ProcessState::Alive,
    );
}

#[test]
fn a09_neither_text_nor_activity_can_produce_turn_done() {
    let done = DetectionResult {
        status: Status::TurnDone,
        confidence: 0.9,
        reason: "prompt visible".to_owned(),
    };
    let bans = [
        Ban::Cell(Status::TurnDone, Source::Text),
        Ban::Cell(Status::TurnDone, Source::Activity),
    ];
    // 有 hook 的行：屏幕看到"停在提示符"也不改口。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let rt = pane(None);
    let fed = tick_rt(&mut m, &rt, None, false, Some(&done), 4);
    forbid(
        "a09",
        &fed,
        &bans,
        Some((Status::Running, Source::Hook, ProcessState::Alive)),
    );
    // 没有 hook 的行（shell / custom）：文本层只认 WAITING，屏幕上的"像是答完了"不算；
    // 活动层能给的只有 IDLE。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let rt = pane(Some(0));
    let mut fed = tick_rt(&mut m, &rt, Some(3600), false, Some(&done), 1);
    for now in [61, 62] {
        fed = tick_rt(&mut m, &rt, Some(3600), false, Some(&done), now);
    }
    forbid(
        "a09",
        &fed,
        &bans,
        Some((Status::Idle, Source::Activity, ProcessState::Alive)),
    );
}

#[test]
fn a10_idle_from_activity_when_no_hook_ever_spoke() {
    // 关掉 observe_unhooked 的 idle_after 分支 → 红。第一次看到输出只记时刻、不追溯（agora-385）。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let rt = pane(Some(0));
    let mut fed = tick_rt(&mut m, &rt, Some(3600), false, None, 0);
    assert_eq!(fed.a.status, Status::Running, "{:?}", fed.a);
    fed = tick_rt(&mut m, &rt, Some(3600), false, None, 61);
    lands(
        "a10",
        &fed,
        Status::Idle,
        Source::Activity,
        ProcessState::Alive,
    );
}

#[test]
fn a11_activity_never_produces_idle_for_a_hooked_row() {
    // 关掉 observe_hooked 的"hook 写过状态就返回"→ 61 s 那一 tick 会拿活动层的 IDLE 覆盖 → 红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let rt = pane(Some(0));
    let mut fed = tick_rt(&mut m, &rt, Some(3600), false, None, 1);
    for now in [61, 121, 181] {
        fed = tick_rt(&mut m, &rt, Some(3600), false, None, now);
    }
    forbid(
        "a11",
        &fed,
        &[Ban::Status(Status::Idle)],
        Some((Status::Running, Source::Hook, ProcessState::Alive)),
    );
}

#[test]
fn a12_finished_from_the_process_layer() {
    // 退干净的一行：source 从 hook 换成 process、confidence 1.0、end_cause 带退出码。
    // 关掉 `process_layer` 的 Code(0) → Finished 分支 → 红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let dead = pane_exit(Some(Exit::Code(0)));
    let fed = tick_rt(&mut m, &dead, None, false, None, 20);
    lands(
        "a12",
        &fed,
        Status::Finished,
        Source::Process,
        ProcessState::Gone,
    );
    // 按过 Kill 的另一半：143 经壳包装也算用户自己干的（agora-3ib 实测）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let dead = pane_exit(Some(Exit::Code(143)));
    let fed = tick_rt(&mut m, &dead, None, true, None, 20);
    lands(
        "a12",
        &fed,
        Status::Finished,
        Source::Process,
        ProcessState::Gone,
    );
}

#[test]
fn a13_finished_when_the_runtime_session_is_gone() {
    // server 还在应答、只有这一个会话没了；对照 server 连不上；再对照按过 Kill 的那一行
    // （结论同 Finished，end_cause 只报 killed_by_user）。关掉 `runtime_gone` 的 Status::Finished
    // （改回 unknown）→ 三格都红。视图那一段的等价性：
    // `tests/session_manager.rs::missing_runtime_session_finishes_the_row_and_writes_ended_at_once`、
    // `::server_gone_and_session_gone_are_told_apart_in_the_reason`。
    for (gone, killed) in [
        (RuntimeGone::Session, false),
        (RuntimeGone::Server, false),
        (RuntimeGone::Session, true),
    ] {
        let fed = running_then_fact(true, gone_fact(gone, killed), Liveness::Dead, false);
        lands(
            "a13",
            &fed,
            Status::Finished,
            Source::Process,
            ProcessState::Gone,
        );
    }
}

/// `runtime_gone` 的两个值走同一个构造器（表里 a13 / a19 两行都要它，测试里少写一次导入形状）。
fn gone_fact(gone: RuntimeGone, killed_by_user: bool) -> Assessment {
    agora::status::runtime_gone(gone, killed_by_user)
}

#[test]
fn a14_a_host_end_seen_before_the_process_exit() {
    // ◐ 的那一瞬：宿主先说了结束，进程层还没来得及报退出。旧代码在这里报出 finished + alive:true
    // （2026-09-18 Mac 10 行），关掉 `ProcessState::derive` 的第一条规则就红。
    let fed = host_end_while_the_process_is_alive(AgoraEvent::SessionEnded(Some(
        "prompt_input_exit".into(),
    )));
    lands(
        "a14",
        &fed,
        Status::Finished,
        Source::Hook,
        ProcessState::Gone,
    );
}

#[test]
fn a15_failed_names_the_exit_that_caused_it() {
    // 退出码与信号两种退法都到 FAILED；end_cause 带的是那一个值（守卫
    // `every_finished_and_failed_row_names_its_end_cause` 逐值钉）。关掉 `process_layer` 的
    // `Code(n)` → Failed 分支 → 红。
    for fact in [
        rt_exit(Some(Exit::Code(3)), false),
        rt_exit(Some(Exit::Signal("hup".into())), false),
    ] {
        let fed = running_then_fact(true, fact, Liveness::Dead, false);
        lands(
            "a15",
            &fed,
            Status::Failed,
            Source::Process,
            ProcessState::Gone,
        );
    }
}

#[test]
fn a16_an_ended_row_never_reports_an_alive_process() {
    // 表里的 ✗ 行：结束了却报 process=alive 的形状，靠 derive 的第一条规则挡住（对话结束即不再谈
    // 进程），而不是靠"进程层下一 tick 会覆盖"——后者对 hook 先说结束的行并不成立（有 hook 的行
    // hook 说什么就是什么，直到进程层报出不同的状态）。
    for end in [
        AgoraEvent::SessionEnded(Some("prompt_input_exit".into())),
        AgoraEvent::Superseded,
    ] {
        let fed = host_end_while_the_process_is_alive(end);
        forbid(
            "a16",
            &fed,
            &[Ban::Process(ProcessState::Alive)],
            Some((Status::Finished, Source::Hook, ProcessState::Gone)),
        );
    }
}

#[test]
fn a17_an_unreadable_runtime_is_unknown_not_gone() {
    // ADR-001 D7：读不到 ≠ 已死。`view()` 在降级时给 Liveness::Dead 只是内部编码（让状态机别把
    // 失明当活着），导出时必须还原成 unknown——关掉 derive 的 unreadable 分支就报出一条假的 gone。
    // 视图那一段：`tests/session_manager.rs::degraded_runtime_never_becomes_runtime_gone`。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let a = m.observe(Observation {
        process: agora::status::runtime_unavailable("protocol version mismatch"),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
    });
    let fed = Fed {
        a,
        liveness: Liveness::Dead,
        unreadable: true,
    };
    lands(
        "a17",
        &fed,
        Status::Unknown,
        Source::None,
        ProcessState::Unknown,
    );
}

#[test]
fn a18_a_screen_only_unknown_still_reports_an_alive_process() {
    // 喂法与 `every_unknown_row_names_why_it_is_unknown` 里那两格同一条（不复制一份状态机序列）：
    // 屏幕给的 UNKNOWN 两条规则都在"pane 活着"的前提下才采得到屏幕。
    for want in ["hooks_silent_screen", "prompt_gone"] {
        let r = unknown_rows()
            .into_iter()
            .find(|r| r.want == want)
            .unwrap_or_else(|| panic!("unknown_rows 里少了 {want}"));
        let fed = Fed::new((r.feed)(), Liveness::Alive);
        lands(
            "a18",
            &fed,
            Status::Unknown,
            Source::Text,
            ProcessState::Alive,
        );
    }
}

#[test]
fn a19_a_missing_runtime_session_is_not_unknown() {
    // 旧版这一格是 UNKNOWN `runtime session missing`：每一行钉在 ? 没有出口、终端面板给一个永远连不上的
    // 「重新连接」（agora-u5p，ADR-001 D4 于 2026-09-19 据此修订）。两种形状都要钉住：
    let bans = [
        Ban::Cell(Status::Unknown, Source::None),
        Ban::Cell(Status::Unknown, Source::Process),
        Ban::Cell(Status::Unknown, Source::Text),
    ];
    // ① 正在跑的行，运行时列表里再也找不到它：落在 a13 那一格。
    let fed = running_then_fact(
        true,
        gone_fact(RuntimeGone::Session, false),
        Liveness::Dead,
        false,
    );
    forbid(
        "a19",
        &fed,
        &bans,
        Some((Status::Finished, Source::Process, ProcessState::Gone)),
    );
    // ② hook 先说了结束、之后会话才没：结论仍是结束，但 source 留 hook（同状态同分不抢，
    //    agora-rzh；对照 `runtime_session_gone_does_not_outvote_a_hook_that_spoke_first`）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let rt = pane(Some(0));
    tick_rt(&mut m, &rt, Some(3600), false, None, 30);
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 40);
    let a = m.observe(Observation {
        process: gone_fact(RuntimeGone::Session, false),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
    });
    let fed = Fed::new(a, Liveness::Dead);
    forbid(
        "a19",
        &fed,
        &bans,
        Some((Status::Finished, Source::Hook, ProcessState::Gone)),
    );
    // ③ 还有一格长得几乎一样却不该算结束：本代刚起、运行时这一 tick 还没报到它（STARTING 窗口内）。
    //    那一格 `process_layer(None)` 给的是 UNKNOWN `no_observation`，而且**不写 ended_at**；
    //    把 view() 里那条 `!starting` 的豁免拆掉，每次起会话都会先给自己写一个 ended_at 再报
    //    FINISHED——红在 `tests/session_manager.rs::starting_window_exempts_a_row_that_is_still_starting`。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let a = m.observe(Observation {
        process: agora::status::process_layer(None, Some(0), false),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 1,
    });
    let fed = Fed::new(a, Liveness::Dead);
    assert_eq!(
        (fed.a.status, fed.a.source),
        (Status::Unknown, Source::None),
        "STARTING 窗口里的「没看见」不等于「没了」: {:?}",
        fed.a
    );
    assert_eq!(
        fed.a.unknown_cause,
        Some(UnknownCause::NoObservation),
        "{:?}",
        fed.a
    );
}

#[test]
fn a20_the_hook_layer_never_writes_unknown() {
    // 反向断言的形状：把每一类 hook 事件各喂一次，每一个落点都不许是 UNKNOWN × hook——屏幕给的两格
    // UNKNOWN 的 source 是 text，无句柄沉默那格的 source 才是 hook（x10，第 3 节）。
    // 落点不钉死（每一类事件各落合法格），只钉"没有一类 hook 事件能把有句柄的行写成 UNKNOWN × hook"。
    // 反向说：把 observe_hooked 的沉默分支的 source 从 Text 改成 Hook → 红在沉默那一格。
    let events = [
        AgoraEvent::SessionStarted,
        AgoraEvent::SessionId("s".into()),
        AgoraEvent::PromptSubmitted("go".into()),
        AgoraEvent::PromptInjected,
        AgoraEvent::Activity("Bash".into()),
        AgoraEvent::InputNeeded {
            tool_use_id: "t1".into(),
            question: "which one?".into(),
        },
        AgoraEvent::DecisionNeeded {
            tool_use_id: "t1".into(),
            summary: "Bash: sleep 20".into(),
        },
        AgoraEvent::DecisionResolved(Some("t1".into())),
        AgoraEvent::TurnEnded(Some("done".into())),
        AgoraEvent::TurnFailed("api_error".into()),
        AgoraEvent::Idle,
        AgoraEvent::SessionEnded(Some("other".into())),
        AgoraEvent::Superseded,
    ];
    let rt = pane(Some(0));
    for event in &events {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(event, 1, 0);
        let fed = tick_rt(&mut m, &rt, Some(3600), false, None, 1);
        forbid(
            "a20",
            &fed,
            &[Ban::Cell(Status::Unknown, Source::Hook)],
            None,
        );
    }
}

// ───────────────────── 第 3 节：无运行时句柄的行（external / headless）─────────────────────

#[test]
fn x01_handleless_starting_inside_the_grace() {
    for lv in [Liveness::Alive, Liveness::Unknown] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        let fed = tick_external(&mut m, lv, 9);
        lands(
            "x01",
            &fed,
            Status::Starting,
            Source::Hook,
            expected_process(lv),
        );
    }
}

#[test]
fn x02_handleless_starting_never_outlives_the_startup_grace() {
    // 关掉 observe 第 1 步 external 分支里的 decay_starting（agora-rkl 那一处）→ 四格全红。
    // 现场：zuan ef0e50 钉在 starting 180 h、a3a2a0 10 h，Mac db5d48 2 d、4aa42b 15 h。
    for lv in [Liveness::Alive, Liveness::Unknown] {
        for at in [10, 3600] {
            let mut m = Machine::new(cfg(), true, 1, 0);
            m.apply(&AgoraEvent::SessionStarted, 1, 0);
            let fed = tick_external(&mut m, lv, at);
            forbid(
                "x02",
                &fed,
                &[Ban::Cell(Status::Starting, Source::Hook)],
                Some((Status::TurnDone, Source::Hook, expected_process(lv))),
            );
        }
    }
}

#[test]
fn x03_hook_states_stay_put_while_the_handle_is_alive() {
    for (event, want) in [
        (AgoraEvent::PromptSubmitted("go".into()), Status::Running),
        (
            AgoraEvent::DecisionNeeded {
                tool_use_id: "t1".into(),
                summary: "Bash: sleep 20".into(),
            },
            Status::Waiting,
        ),
        (AgoraEvent::TurnEnded(Some("done".into())), Status::TurnDone),
    ] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&event, 1, 0);
        let fed = tick_external(&mut m, Liveness::Alive, 5);
        lands("x03", &fed, want, Source::Hook, ProcessState::Alive);
    }
}

#[test]
fn x04_hook_states_stay_put_without_a_handle() {
    // Codex Desktop 那一格：进程号不可信，行照旧只由 hook 说话（盘点里那 7 行 turn_done +
    // alive:false 就是它，旧形态把"不知道"压成 false）。
    for (event, want) in [
        (AgoraEvent::PromptSubmitted("go".into()), Status::Running),
        (
            AgoraEvent::DecisionNeeded {
                tool_use_id: "t1".into(),
                summary: "Bash: sleep 20".into(),
            },
            Status::Waiting,
        ),
        (AgoraEvent::TurnEnded(Some("done".into())), Status::TurnDone),
    ] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&event, 1, 0);
        let fed = tick_external(&mut m, Liveness::Unknown, 5);
        lands("x04", &fed, want, Source::Hook, ProcessState::Unknown);
    }
}

#[test]
fn x05_a_gone_handle_ends_the_row_it_cannot_leave_it_working() {
    for event in [
        AgoraEvent::PromptSubmitted("go".into()),
        AgoraEvent::DecisionNeeded {
            tool_use_id: "t1".into(),
            summary: "Bash: sleep 20".into(),
        },
        AgoraEvent::TurnEnded(Some("done".into())),
    ] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&event, 1, 0);
        tick_external(&mut m, Liveness::Alive, 5);
        let fed = tick_external(&mut m, Liveness::Dead, 20);
        forbid(
            "x05",
            &fed,
            &[
                Ban::Status(Status::Running),
                Ban::Status(Status::Waiting),
                Ban::Status(Status::TurnDone),
            ],
            Some((Status::Finished, Source::Process, ProcessState::Gone)),
        );
    }
}

#[test]
fn x06_no_activity_layer_means_no_idle() {
    // 无句柄行没有 pane 可采输出：`observe` 第 1 步就返回了，活动层与文本层都够不着它。
    // 关掉第 1 步那个提前返回（让 external 行走 observe_unhooked）→ 61 s 那一 tick 会落 IDLE 而红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let mut fed = tick_external(&mut m, Liveness::Alive, 5);
    for at in [61, 3600] {
        fed = tick_external(&mut m, Liveness::Alive, at);
    }
    forbid(
        "x06",
        &fed,
        &[Ban::Status(Status::Idle)],
        Some((Status::Running, Source::Hook, ProcessState::Alive)),
    );
    // 对照：声明没有 hook 的无句柄行（custom / unknown 类型跑着装了 hook 的东西）也不产 IDLE，
    // 它连"没有观测"这格都出不去（x12）。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let fed = tick_external(&mut m, Liveness::Unknown, 3600);
    forbid(
        "x06",
        &fed,
        &[Ban::Status(Status::Idle)],
        Some((Status::Unknown, Source::None, ProcessState::Unknown)),
    );
}

#[test]
fn x07_a_silent_row_ends_when_its_process_goes() {
    // 一个事件都没来、只剩探活：这是 external 行唯一一种"没有宿主说法"的结束，通知按它弹一次。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let fed = tick_external(&mut m, Liveness::Dead, 20);
    lands(
        "x07",
        &fed,
        Status::Finished,
        Source::Process,
        ProcessState::Gone,
    );
    // 有过对话的行同一条：进程号没了就是结束，不管它先前停在哪个 hook 状态。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    let fed = tick_external(&mut m, Liveness::Dead, 20);
    lands(
        "x07",
        &fed,
        Status::Finished,
        Source::Process,
        ProcessState::Gone,
    );
}

#[test]
fn x08_a_row_ended_by_its_host_or_by_a_new_conversation_reports_gone() {
    // 盘点 B1 的那 10 行：宿主说了结束（或同一进程换到了新对话），而那个号还在跑新对话。
    // `process` 一律 gone（Q4），hook 的 reason / end_cause / 起点留着（agora-rzh）。
    // 视图那一段的等价性：`tests/hooks_external.rs::hook_session_end_survives_the_agent_process_going_away`、
    // `::a_new_conversation_on_the_same_process_supersedes_the_old_external_row`。
    for end in [
        AgoraEvent::SessionEnded(Some("prompt_input_exit".into())),
        AgoraEvent::SessionEnded(Some("resume".into())),
        AgoraEvent::Superseded,
    ] {
        for lv in [Liveness::Alive, Liveness::Dead] {
            let mut m = Machine::new(cfg(), true, 1, 0);
            m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
            m.apply(&end, 1, 1);
            let fed = tick_external(&mut m, lv, 5);
            lands(
                "x08",
                &fed,
                Status::Finished,
                Source::Hook,
                ProcessState::Gone,
            );
        }
    }
}

#[test]
fn x09_no_exit_code_means_no_failed() {
    // 无句柄行拿不到退出码：崩溃与干净退出在探活上长得一样，所以结束只有 FINISHED 一种。
    // 反过来说也不行：不许因为"没有码"就退化成 UNKNOWN 永远占着列表（那是 e08 的出口）。
    let events = [
        AgoraEvent::SessionStarted,
        AgoraEvent::PromptSubmitted("go".into()),
        AgoraEvent::TurnEnded(Some("done".into())),
        AgoraEvent::TurnFailed("api_error".into()),
        AgoraEvent::DecisionNeeded {
            tool_use_id: "t1".into(),
            summary: "Bash".into(),
        },
        AgoraEvent::SessionEnded(Some("other".into())),
        AgoraEvent::Superseded,
    ];
    for event in &events {
        for lv in [Liveness::Alive, Liveness::Unknown, Liveness::Dead] {
            let mut m = Machine::new(cfg(), true, 1, 0);
            m.apply(event, 1, 0);
            let fed = tick_external(&mut m, lv, 5);
            forbid("x09", &fed, &[Ban::Status(Status::Failed)], None);
        }
    }
}

#[test]
fn x10_a_silent_handleless_row_falls_to_unknown_on_the_event_clock() {
    // 沉默时钟按**事件自己的时刻**算（agora-5gg.2）：停机 3.5 天 + 重放不能把"沉默了多久"清零。
    // 现场：Mac 0e26ad / 14d791 / eb129d / b939bb 沉默 62 h 仍 turn_done（重放过的那批），
    // 同一批没被重放、从检查点恢复的 4 行却当场 UNKNOWN——两种结果差在用了哪只表。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply_at(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201, 1);
    let fed = tick_external(&mut m, Liveness::Unknown, 7201);
    lands(
        "x10",
        &fed,
        Status::Unknown,
        Source::Hook,
        ProcessState::Unknown,
    );
    // 对照：同一格如果拿 daemon 的收到时刻算，事件三小时前发生也才"刚刚收到"，就还是 TURN_DONE
    // ——把 handleless_quiet_since 改回 last_hook_at 就红在这一格。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201);
    let fed = tick_external(&mut m, Liveness::Unknown, 7201);
    lands(
        "x10",
        &fed,
        Status::TurnDone,
        Source::Hook,
        ProcessState::Unknown,
    );
    // ◐ 说的"暂态"两半：下一条 hook 事件即恢复（这一条是状态机给的出口），
    // 另一半分两条在代码里，本表的"出口"列点名：
    //   · 满 sessions.external_unknown_ttl 走 DELETE：`tests/external_expiry.rs::handleless_unknown_external_rows_expire_and_emit_session_removed`
    //   · 无头行不论状态满 external_finished_ttl：`tests/hooks_external.rs::a_headless_session_registers_as_headless_and_expires_whatever_its_status_is`
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply_at(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201, 1);
    tick_external(&mut m, Liveness::Unknown, 7201);
    let fed = {
        m.apply(&AgoraEvent::TurnEnded(Some("later".into())), 1, 7205);
        tick_external(&mut m, Liveness::Unknown, 7205)
    };
    lands(
        "x10",
        &fed,
        Status::TurnDone,
        Source::Hook,
        ProcessState::Unknown,
    );
}

#[test]
fn x11_a_handle_or_a_spoken_hook_rules_out_unknown() {
    // 沉默兜底只对"无句柄 + 无进程号"那一格开（agora-tql）：进程号活着的行沉默几小时是正常的，
    // 人离开再回来而已。关掉 `obs.liveness == Liveness::Unknown` 那个条件 → 第一格红（Alive 的行
    // 也被打成 UNKNOWN）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    let fed = tick_external(&mut m, Liveness::Alive, 7201);
    forbid(
        "x11",
        &fed,
        &[
            Ban::Cell(Status::Unknown, Source::Hook),
            Ban::Cell(Status::Unknown, Source::Text),
            Ban::Cell(Status::Unknown, Source::Process),
            Ban::Cell(Status::Unknown, Source::None),
        ],
        Some((Status::TurnDone, Source::Hook, ProcessState::Alive)),
    );
    // 探到号没了：说的是结束，不是说不清。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    let fed = tick_external(&mut m, Liveness::Dead, 7201);
    forbid(
        "x11",
        &fed,
        &[
            Ban::Cell(Status::Unknown, Source::Hook),
            Ban::Cell(Status::Unknown, Source::Process),
        ],
        Some((Status::Finished, Source::Process, ProcessState::Gone)),
    );
}

#[test]
fn x12_no_observation_yet_leaves_as_soon_as_a_hook_speaks() {
    // ◐：这一格只在检查点恢复 / 重放完成前的那一瞬间合法。它自己不会把行送到任何地方——
    // 沉默兜底要的是"hook 说过话"（current.source == Hook），所以一条 hook 事件一到就走。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let fed = tick_external(&mut m, Liveness::Alive, 5);
    lands(
        "x12",
        &fed,
        Status::Unknown,
        Source::None,
        ProcessState::Alive,
    );
    let mut m = Machine::new(cfg(), true, 1, 0);
    let fed = tick_external(&mut m, Liveness::Unknown, 5);
    lands(
        "x12",
        &fed,
        Status::Unknown,
        Source::None,
        ProcessState::Unknown,
    );
    // 一条 hook 事件都没的无句柄行会**留**在这一格（连沉默兜底都不落）：持续出现 = 检查点丢了，
    // 属 bug（表里 ◐ 那半句说的就是它，出口是 e08 的 TTL 只管落进沉默兜底的那一格）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    tick_external(&mut m, Liveness::Unknown, 7201);
    let fed = {
        m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7202);
        tick_external(&mut m, Liveness::Unknown, 7202)
    };
    lands(
        "x12",
        &fed,
        Status::TurnDone,
        Source::Hook,
        ProcessState::Unknown,
    );
}

#[test]
fn x13_headless_shares_every_cell_of_the_handleless_table() {
    // 真值表按"有没有句柄"分两节，而不是按四个 origin 分四节，靠的是代码一律问
    // `Origin::is_handleless()`（`SessionManager::view` 的 match 臂、探活、supersede、`ended_at`、
    // 过期扫描都是）。新增一档 origin 时这里编译不过：先回答它有没有句柄，再决定它进第 2 节还是第 3 节。
    match Origin::Agora {
        Origin::Agora | Origin::Adopted | Origin::External | Origin::Headless => {}
    }
    for (origin, handleless) in [
        (Origin::Agora, false),
        (Origin::Adopted, false),
        (Origin::External, true),
        (Origin::Headless, true),
    ] {
        assert_eq!(origin.is_handleless(), handleless, "{origin:?}");
        assert_eq!(Origin::parse(origin.as_str()), Some(origin), "{origin:?}");
    }
    assert_eq!(Origin::parse("wsl"), None);
}

// ───────────────────────────── 表与守卫的对账 ─────────────────────────────

fn repo_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// markdown 表格的一行拆成单元格：先躲开 `\|`（表格里的转义竖线，`host_session_end{clear\|resume…}`），
/// 再按 `|` 切。
fn row_cells(line: &str) -> Vec<String> {
    const ESC: char = '\u{0}';
    let esc = line.trim().replace("\\|", &ESC.to_string());
    let esc = esc.strip_prefix('|').unwrap_or(&esc);
    let esc = esc.strip_suffix('|').unwrap_or(esc);
    esc.split('|')
        .map(|c| c.replace(ESC, "|").trim().to_owned())
        .collect()
}

/// 一格里的 `` `file::test_fn` `` 引用：返回 (文件, 测试名)。`…::name` 沿用上一条的行所文件。
fn guard_refs(cell: &str, default_file: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut last_file = default_file.to_owned();
    for span in cell.split('`').skip(1).step_by(2) {
        let Some((file, test)) = span.split_once("::") else {
            continue;
        };
        let file = if file.starts_with('…') {
            last_file.clone()
        } else {
            last_file = file.to_owned();
            file.to_owned()
        };
        out.push((file, test.to_owned()));
    }
    out
}

#[test]
fn spec_rows_and_code_rows_agree() {
    // 这张表之所以是"守卫"而不是"文档"：本文件里的每一行与 docs/spec/status.md 表里的每一行
    // 一一对得上。四个方向都查——
    //   表里有、测试里没有      → 红（新增一格不写守卫）
    //   测试里有、表里没有      → 红（删了一格忘了删守卫，或写了张表外的私货）
    //   三列文本 / 判定符号不一致 → 红（口径改了、测试名没改，或反过来）
    //   守卫点名的测试不存在      → 红（改名忘了同步表；表里那些 `tests/*.rs::fn` 是真的引用）
    let md =
        std::fs::read_to_string(repo_file("docs/spec/status.md")).expect("docs/spec/status.md");
    let me = std::fs::read_to_string(repo_file("tests/status_truth_table.rs")).expect("本文件");
    let mut ids: Vec<String> = Vec::new();
    let mut cells = std::collections::HashMap::new();
    let mut verdicts = std::collections::HashMap::new();
    let mut in_id_table = false;
    for line in md.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            in_id_table = false;
            continue;
        }
        let cells_row = row_cells(line);
        if cells_row.first().map(String::as_str) == Some("格") {
            in_id_table = true;
            continue;
        }
        if !in_id_table || cells_row.len() < 7 || cells_row[0].starts_with('-') {
            continue;
        }
        let id = cells_row[0].clone();
        assert!(!id.is_empty(), "表里有一行没有 id：{line}");
        assert!(!ids.contains(&id), "id 重复：{id}");
        ids.push(id.clone());
        assert!(
            ["✓", "◐", "✗"].contains(&cells_row[4].as_str()),
            "{id} 的判定符号不是 ✓ / ◐ / ✗：{}",
            cells_row[4]
        );
        cells.insert(
            id.clone(),
            format!("{} | {} | {}", cells_row[1], cells_row[2], cells_row[3]),
        );
        verdicts.insert(id.clone(), cells_row[4].clone());
        // 守卫点名的测试必须真的存在，且第一个测试名以 id 为前缀（"表里这一行"要能在测试名里检索到）。
        let refs = guard_refs(&cells_row[6], "tests/status_truth_table.rs");
        assert!(
            !refs.is_empty(),
            "{id} 的守卫格没有 `tests/*.rs::fn` 引用：{}",
            cells_row[6]
        );
        for (file, test) in &refs {
            let src = std::fs::read_to_string(repo_file(file))
                .unwrap_or_else(|e| panic!("{id} 的守卫引用的文件不存在: {file}（{e}）"));
            assert!(
                src.contains(&format!("fn {test}(")) || src.contains(&format!("async fn {test}(")),
                "{id} 的守卫点名的测试不存在: {file}::{test}"
            );
        }
        assert!(
            refs[0].1.starts_with(&format!("{id}_")),
            "{id} 的守卫第一个点名的测试应当叫 `{id}_…`，实际 {}",
            refs[0].1
        );
        assert_eq!(
            refs[0].0, "tests/status_truth_table.rs",
            "{id} 的守卫第一格点名的测试应当在本文件里"
        );
    }
    // 每一行都能在测试名里检索到。
    for id in &ids {
        assert!(
            me.contains(&format!("fn {id}_")),
            "表里的 {id} 在本文件里没有以它为前缀的测试（表里每一行要在测试名里可检索）"
        );
    }
    for r in ROWS {
        let Some(cell) = cells.get(r.id) else {
            panic!(
                "ROWS 里有 {}，docs/spec/status.md 的表里没有：删一格要连守卫一起删",
                r.id
            );
        };
        assert_eq!(
            cell,
            &r.cell.replace("\\|", "|"),
            "{} 的三列与表里不一致（表里改了、测试里没改，或反之）",
            r.id
        );
        assert_eq!(
            verdicts.get(r.id).map(String::as_str),
            Some(r.verdict.symbol()),
            "{} 的判定符号与测试里的 verdict 不一致：表里 {}，测试里 {}",
            r.id,
            verdicts.get(r.id).map(String::as_str).unwrap_or_default(),
            r.verdict.symbol()
        );
    }
}
