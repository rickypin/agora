//! 状态真值表守卫（epic agora-5gg；本文件由 agora-rkl 起第一行，agora-5gg.16 往里补齐，
//! agora-fhez 把六套表结构收口成一张表 + 一套枚举断言）。
//!
//! 一行 = 「origin × 进程事实 × hook 来源」的某种组合喂进状态机之后**该**吐出什么。为什么单开
//! 一个文件而不塞进 `tests/state_machine.rs`：那边按"每条裁决规则一个测试"组织，讲为什么；这里
//! 按"每一行长什么样"组织，讲的是对人眼承诺的口径（MISSION §4.3 的三问：进程还在不在、它要我
//! 做什么、说不清的话为什么说不清）。2026-09-18 的盘点（`docs/analysis/session-status-audit-2026-09-18.md`）
//! 之所以能发现一批错行，靠的就是把 79 行摊成一张表逐条问——表落成守卫之后，同样的核对不该再靠
//! 人肉做一遍。
//!
//! **收口（agora-fhez，2026-10-08）**：以前这里并存六套表结构——agora-rkl 的 `Row`（external
//! STARTING 衰减）、agora-u5p 的 `GoneRow`、5gg.6 的 `EndRow` / `UnknownRow`、5gg.16 的
//! `Cell`/`ROWS` 与喂法包装 `Fed`。现在只剩一张 `ROWS`：一行要么占 docs/spec/status.md 的一格
//! （`spec: Some`），要么是格子下面的一档组合（词表 / 分片矩阵 / expire arm，`spec: None`）；
//! 每行的期望都是枚举（status / source / process + `Cause`），断言只有 `check_row` 一套，且
//! `every_row_in_the_table_is_asserted` 会真的把每一行喂一遍——「只声明不断言」的格子会红那里。
//!
//! 只喂 `Machine`，不起运行时、不起 daemon：真值表要在几毫秒内重跑完，才有人改一行就肯全跑。
//! 唯一破例是文件末尾的 `expire_arms_and_the_local_seen_anchor_are_pinned`：agora-fhez 点名要
//! expire 的三 arm 组合与 91vy 锚的两态进本文件，而 `expire_external_finished` 要 Db，只好用
//! `Db::open_in_memory()` + `common::FakeRuntime`（不开 daemon、不跑 tmux、不碰真会话）；它单独
//! 带 `#[test]`，主表仍按毫秒级跑。

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use agora::clock;
use agora::runtime::{Exit, Runtime, RuntimeRef, RuntimeSession, Size};
use agora::session::{Db, ExternalSession, Origin, SessionManager};
use agora::status::{
    AgoraEvent, Assessment, DetectionResult, EndCause, HostEndReason, Liveness, Machine,
    MachineConfig, Observation, ProcessState, RuntimeGone, Source, Status, UnknownCause,
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

// ─────────────────────────────── 喂法 ───────────────────────────────

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
        at: None,
    }
}

/// `runtime_gone` 的两个值走同一个构造器（真值表里 a13 / a19 两处用它，测试里少写一次导入形状）。
fn gone_fact(gone: RuntimeGone, killed_by_user: bool) -> Assessment {
    agora::status::runtime_gone(gone, killed_by_user)
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
        at: None,
    });
    m.observe(Observation {
        process: fact,
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 20,
        at: None,
    })
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

/// 一次喂进状态机的完整输入：结论 + 导出 `process` 要的那两个事实。
#[derive(Debug, Clone)]
struct Fed {
    a: Assessment,
    liveness: Liveness,
    /// 有句柄而这一 tick 没有运行时事实（`view()` 的"降级 / socket 没扫成 / 本代还在
    /// STARTING 窗口"那几支；就是 `ProcessState::derive` 的第三个参数，agora-86nk）。
    runtime_fact_missing: bool,
}

impl Fed {
    fn new(a: Assessment, liveness: Liveness) -> Self {
        Fed {
            a,
            liveness,
            runtime_fact_missing: false,
        }
    }

    /// 这一行报给调用方的进程三态（`GET /api/sessions` 的 `process`）。
    fn process(&self) -> ProcessState {
        ProcessState::derive(self.a.status, self.liveness, self.runtime_fact_missing)
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
        at: None,
    });
    Fed {
        a,
        liveness,
        runtime_fact_missing: false,
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
        at: None,
    });
    Fed {
        a,
        liveness,
        runtime_fact_missing: false,
    }
}

/// 先跑起来（一个活着的 tick），再喂一条**现成的**进程事实（`runtime_gone` /
/// `runtime_unavailable` / 退出码那一类由 `view()` 上半段算出来的结论）。
fn running_then_fact(
    declared_hooks: bool,
    fact: Assessment,
    liveness: Liveness,
    runtime_fact_missing: bool,
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
        at: None,
    });
    Fed {
        a,
        liveness,
        runtime_fact_missing,
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

// ─────────────────────── 统一的一格：期望全是枚举 ───────────────────────

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

/// 这一格该带的原因，**全是封闭集合里的枚举成员**：结束行给 `end_cause`，说不清的行给
/// `unknown_cause`，还在跑的行两个都不带。写成枚举而不是 reason 字面量——reason 只是给人看的一句话，
/// 措辞可以随便改，程序读的这一栏改不了也漏得掉（MISSION §2.3 规则 10，agora-5gg.6）。
/// 生产方 / 线形态的常量（`EXTERNAL_SILENT_REASON`、`HOST_END_CLEAR` 那一类）不经过这里，
/// 它们是契约，保持字面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// 还在跑 / 没有原因可说的行：`end_cause` 与 `unknown_cause` 都必须是 None。
    None,
    ExitCode(i32),
    Signal(&'static str),
    KilledByUser,
    /// 宿主 SessionEnd 归一化之后的五档之一（clear / resume / logout / exit / other）。
    HostEnd(&'static str),
    Superseded,
    ProcessGone,
    /// session / server。
    RuntimeGone(&'static str),
    CheckpointUnrecorded,
    /// `unknown_cause` 的封闭成员。
    Unknown(&'static str),
}

/// 正向落点：调用方看到的四个值都写死。
#[derive(Debug, Clone, Copy)]
struct Landing {
    status: Status,
    source: Source,
    process: ProcessState,
    cause: Cause,
}

const fn at(status: Status, source: Source, process: ProcessState, cause: Cause) -> Landing {
    Landing {
        status,
        source,
        process,
        cause,
    }
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

/// 一格的期望：✓/◐ 写正向落点；✗ 写一条（或几条）反向禁令，能钉住实际落点就一并写。
/// 喂法会落好几处合法位置的 ✗ 行（a20 / x09）不写 `instead`——但那一条禁令本身就是枚举断言。
#[derive(Debug, Clone, Copy)]
enum Want {
    Lands(Landing),
    Forbid {
        bans: &'static [Ban],
        instead: Option<Landing>,
    },
}

const fn lands(status: Status, source: Source, process: ProcessState, cause: Cause) -> Want {
    Want::Lands(at(status, source, process, cause))
}

const fn forbid(bans: &'static [Ban], instead: Option<Landing>) -> Want {
    Want::Forbid { bans, instead }
}

/// 这一行属于哪道既有守卫：spec 格走各自的 `aNN_…` / `xNN_…` 测试，组合行走四道老守卫
/// （starting / gone / end / unknown）与收口新加的两组（shard / expire）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Cell,
    Starting,
    Gone,
    End,
    Unknown,
    Shard,
    Expire,
}

/// spec 格的三列与判定（对账用；组合行没有 spec）。
#[derive(Debug, Clone, Copy)]
struct Spec {
    verdict: Verdict,
    /// `status | process | source`，与 docs/spec/status.md 表里那三列逐字相同（对账比这个）。
    cell: &'static str,
}

/// 这一行怎么喂出来。主路是 `Feds`：返回 1..n 个观测，每个观测在 `want` 里有一条对应的枚举期望
/// （而 `want.len() == 1` 时对每个观测都生效，给 a20 / x09 这类"每类事件喂一次"的行用）。
/// `Direct` 只给不是"一次状态机落点"的契约行（x13 的 origin 分类）：它自己带断言，
/// 自检会真的把它跑一遍，防"只声明不断言"。
#[derive(Clone, Copy)]
enum Feed {
    Feds(fn() -> Vec<Fed>),
    Direct(fn()),
}

/// 表里的一行。
struct Row {
    id: &'static str,
    /// Some = 这一行占 docs/spec/status.md 表里的一格；None = 格子下面的一档组合。
    spec: Option<Spec>,
    group: Group,
    /// 为什么是它 / 为什么不可能（表里"含义"列的浓缩，红的时候印出来）。
    why: &'static str,
    feed: Feed,
    want: &'static [Want],
}

impl Row {
    fn spec(&self) -> &Spec {
        self.spec
            .as_ref()
            .unwrap_or_else(|| panic!("{} 不是 spec 格", self.id))
    }
}

/// 这一格的结束原因，写成 api.md 里那个形态的短字符串（`kind`，带 value 的附上 value）。
/// 顺带做完两件事：`is_some` 与"成员表里查得到"——缺一个都 panic，成员表拦的是"私自加一档"。
const END_CAUSE_KINDS: &[&str] = &[
    "exit_code",
    "signal",
    "killed_by_user",
    "host_session_end",
    "superseded",
    "process_gone",
    "runtime_gone",
    "checkpoint_unrecorded",
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

fn assert_member(value: &str, members: &[&str], what: &str, id: &str) {
    assert!(
        members.contains(&value),
        "{id}: {what}={value:?} 不在封闭集合 {members:?} 里：新增一档要同步 api.md 与 api_version"
    );
}

fn end_tag(a: &Assessment, id: &str) -> String {
    let cause = a
        .end_cause
        .as_ref()
        .unwrap_or_else(|| panic!("{id}: FINISHED / FAILED 行没带 end_cause: {a:?}"));
    let v = serde_json::to_value(cause).unwrap();
    let kind = v["kind"].as_str().unwrap_or_default();
    assert_member(kind, END_CAUSE_KINDS, "end_cause.kind", id);
    match &v["value"] {
        serde_json::Value::Null => kind.to_owned(),
        serde_json::Value::String(s) => {
            if kind == "host_session_end" {
                assert_member(s, HOST_END_REASONS, "host_session_end.value", id);
            }
            format!("{kind}:{s}")
        }
        other => format!("{kind}:{other}"),
    }
}

fn unknown_tag(a: &Assessment, id: &str) -> String {
    let cause = a
        .unknown_cause
        .as_ref()
        .unwrap_or_else(|| panic!("{id}: UNKNOWN 行没带 unknown_cause: {a:?}"));
    let v = serde_json::to_value(cause).unwrap();
    let s = v.as_str().unwrap_or_default();
    assert_member(s, UNKNOWN_CAUSES, "unknown_cause", id);
    s.to_owned()
}

/// 每一格都过一遍：`end_cause` 与 `unknown_cause` 各归各的状态，不并存、不都空。
fn assert_cause_matches_status(a: &Assessment, id: &str) {
    match a.status {
        Status::Finished | Status::Failed => {
            assert!(a.end_cause.is_some(), "{id}: {a:?}");
            assert!(
                a.unknown_cause.is_none(),
                "{id}: 结束了就不该带 unknown_cause: {a:?}"
            );
        }
        Status::Unknown => {
            assert!(a.unknown_cause.is_some(), "{id}: {a:?}");
            assert!(
                a.end_cause.is_none(),
                "{id}: 没结束就不该带 end_cause: {a:?}"
            );
        }
        _ => assert!(
            a.end_cause.is_none() && a.unknown_cause.is_none(),
            "{id}: 还在跑的行两个都不该有: {a:?}"
        ),
    }
}

/// 这一格的原因与表里写的枚举值对得上。
fn check_cause(a: &Assessment, want: Cause, id: &str) {
    match want {
        Cause::None => assert!(
            a.end_cause.is_none() && a.unknown_cause.is_none(),
            "{id}: 这一格不该带原因: {a:?}"
        ),
        Cause::ExitCode(n) => assert_eq!(end_tag(a, id), format!("exit_code:{n}"), "{id}: {a:?}"),
        Cause::Signal(s) => assert_eq!(end_tag(a, id), format!("signal:{s}"), "{id}: {a:?}"),
        Cause::KilledByUser => assert_eq!(end_tag(a, id), "killed_by_user", "{id}: {a:?}"),
        Cause::HostEnd(v) => {
            assert_eq!(
                end_tag(a, id),
                format!("host_session_end:{v}"),
                "{id}: {a:?}"
            )
        }
        Cause::Superseded => assert_eq!(end_tag(a, id), "superseded", "{id}: {a:?}"),
        Cause::ProcessGone => assert_eq!(end_tag(a, id), "process_gone", "{id}: {a:?}"),
        Cause::RuntimeGone(v) => {
            assert_eq!(end_tag(a, id), format!("runtime_gone:{v}"), "{id}: {a:?}")
        }
        Cause::CheckpointUnrecorded => {
            assert_eq!(end_tag(a, id), "checkpoint_unrecorded", "{id}: {a:?}")
        }
        Cause::Unknown(v) => assert_eq!(unknown_tag(a, id), v, "{id}: {a:?}"),
    }
}

fn check_one(r: &Row, fed: &Fed, want: &Want) {
    assert_cause_matches_status(&fed.a, r.id);
    let got = (fed.a.status, fed.a.source, fed.process());
    match want {
        Want::Lands(l) => {
            assert_eq!(
                got,
                (l.status, l.source, l.process),
                "{}「{}」该落在 {:?} × {:?} × {:?}（{}），实际 {:?}",
                r.id,
                r.spec.map(|s| s.cell).unwrap_or(r.why),
                l.status,
                l.source,
                l.process,
                r.why,
                fed.a
            );
            check_cause(&fed.a, l.cause, r.id);
        }
        Want::Forbid { bans, instead } => {
            assert!(!bans.is_empty(), "{}: ✗ 行一条反向断言都没写", r.id);
            for ban in *bans {
                match ban {
                    Ban::Status(s) => assert_ne!(
                        got.0,
                        *s,
                        "{}「{}」不该出现 {s:?}（{}），实际 {:?}",
                        r.id,
                        r.spec.map(|s| s.cell).unwrap_or(r.why),
                        r.why,
                        fed.a
                    ),
                    Ban::Cell(s, src) => assert_ne!(
                        (got.0, got.1),
                        (*s, *src),
                        "{}「{}」不该出现 {s:?} × {src:?}（{}），实际 {:?}",
                        r.id,
                        r.spec.map(|s| s.cell).unwrap_or(r.why),
                        r.why,
                        fed.a
                    ),
                    Ban::Process(p) => assert_ne!(
                        got.2,
                        *p,
                        "{}「{}」不该报出 {p:?}（{}），实际 {:?}",
                        r.id,
                        r.spec.map(|s| s.cell).unwrap_or(r.why),
                        r.why,
                        fed.a
                    ),
                }
            }
            if let Some(l) = instead {
                assert_eq!(
                    got,
                    (l.status, l.source, l.process),
                    "{}「{}」该落在 {:?} × {:?} × {:?}（{}），实际 {:?}",
                    r.id,
                    r.spec.map(|s| s.cell).unwrap_or(r.why),
                    l.status,
                    l.source,
                    l.process,
                    r.why,
                    fed.a
                );
                check_cause(&fed.a, l.cause, r.id);
            }
        }
    }
}

/// 把一行喂出来并逐观测断言（`want.len() == 1` 时同一条期望管所有观测）。
fn check_row(r: &Row) {
    match r.feed {
        Feed::Feds(f) => {
            let feds = f();
            assert!(!feds.is_empty(), "{}: 一格都没喂出来", r.id);
            if r.want.len() == 1 {
                for fed in &feds {
                    check_one(r, fed, &r.want[0]);
                }
            } else {
                assert_eq!(
                    feds.len(),
                    r.want.len(),
                    "{}: 喂出来的观测数与枚举期望数对不上（{} vs {}）",
                    r.id,
                    feds.len(),
                    r.want.len()
                );
                for (fed, want) in feds.iter().zip(r.want) {
                    check_one(r, fed, want);
                }
            }
        }
        Feed::Direct(f) => {
            assert!(r.want.is_empty(), "{}: Direct 行不该带落点期望", r.id);
            f();
        }
    }
}

/// 某一格的全部观测（一个 spec id 对应恰好一行，组合行没有 spec id 检索）。
fn check_id(id: &str) {
    let rows: Vec<&Row> = ROWS.iter().filter(|r| r.id == id).collect();
    assert!(!rows.is_empty(), "真值表里没有 {id} 这一格");
    for r in rows {
        check_row(r);
    }
}

/// 某道老守卫吃的那一组行（名字保持不变，见每组自己的注释）。
fn check_group(group: Group) {
    let rows: Vec<&Row> = ROWS.iter().filter(|r| r.group == group).collect();
    assert!(!rows.is_empty(), "{group:?} 组一格都没有");
    for r in rows {
        check_row(r);
    }
}
// ───────────────────── 喂法：每一格/每一档怎么来 ─────────────────────

/// 一条文本证据（屏幕给的那个 DetectionResult）。
fn det(status: Status, reason: &str) -> DetectionResult {
    DetectionResult {
        status,
        confidence: 0.9,
        reason: reason.into(),
    }
}

// ── 第 2 节：有运行时句柄的行（a01…a24）──

fn f_a01() -> Vec<Fed> {
    // 关掉 `process_layer` 里 `spawn_age < STARTING_WINDOW_SECS` 那一判 → 红（报成 RUNNING）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    vec![tick_rt(&mut m, &pane(None), Some(0), false, None, 1)]
}

fn f_a02() -> Vec<Fed> {
    // 把 startup_grace 调成 0（或删掉 observe_hooked 里那条衰减的门）→ 这一格与 a03 一起红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    vec![tick_rt(&mut m, &pane(None), None, false, None, 9)]
}

fn f_a03() -> Vec<Fed> {
    // 关掉 observe_hooked 的 decay_starting → 每一格都钉在 Starting 而红。
    let mut out = Vec::new();
    for now in [10, 11, 3600] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        out.push(tick_rt(&mut m, &pane(None), None, false, None, now));
    }
    out
}

fn f_a04() -> Vec<Fed> {
    let mut m = Machine::new(cfg(), true, 1, 0);
    vec![tick_rt(&mut m, &pane(None), Some(3600), false, None, 3600)]
}

fn f_a05() -> Vec<Fed> {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    vec![tick_rt(&mut m, &pane(None), None, false, None, 1)]
}

fn f_a06() -> Vec<Fed> {
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
    vec![tick_rt(&mut m, &pane(None), None, false, None, 2)]
}

fn f_a07() -> Vec<Fed> {
    // 关掉 observe_hooked 的"hook 写过状态就原样返回"（改成走 observe_unhooked）→ 红。
    let waiting = det(Status::Waiting, "permission prompt");
    let mut out = Vec::new();
    for now in [2, 4, 6, 8] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        out.push(tick_rt(
            &mut m,
            &pane(None),
            None,
            false,
            Some(&waiting),
            now,
        ));
    }
    out
}

fn f_a08() -> Vec<Fed> {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 1);
    vec![tick_rt(&mut m, &pane(None), None, false, None, 2)]
}

fn f_a09() -> Vec<Fed> {
    let done = det(Status::TurnDone, "prompt visible");
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    out.push(tick_rt(&mut m, &pane(None), None, false, Some(&done), 4));
    let mut m = Machine::new(cfg(), false, 1, 0);
    let rt = pane(Some(0));
    for now in [1, 61, 62] {
        out.push(tick_rt(&mut m, &rt, Some(3600), false, Some(&done), now));
    }
    out
}

fn f_a10() -> Vec<Fed> {
    // 关掉 observe_unhooked 的 idle_after 分支 → 红。第一次看到输出只记时刻、不追溯（agora-385）。
    let mut m = Machine::new(cfg(), false, 1, 0);
    let rt = pane(Some(0));
    let first = tick_rt(&mut m, &rt, Some(3600), false, None, 0);
    assert_eq!(first.a.status, Status::Running, "{:?}", first.a);
    let second = tick_rt(&mut m, &rt, Some(3600), false, None, 61);
    vec![first, second]
}

fn f_a11() -> Vec<Fed> {
    // 关掉 observe_hooked 的"hook 写过状态就返回"→ 61 s 那一 tick 会拿活动层的 IDLE 覆盖 → 红。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let rt = pane(Some(0));
    let mut out = Vec::new();
    for now in [1, 61, 121, 181] {
        out.push(tick_rt(&mut m, &rt, Some(3600), false, None, now));
    }
    out
}

fn f_a12() -> Vec<Fed> {
    // 关掉 `process_layer` 的 Code(0) → Finished 分支 → 红；Kill 的另一半：143 经壳包装也算用户自己干的。
    let mut out = Vec::new();
    for (exit, killed) in [(Exit::Code(0), false), (Exit::Code(143), true)] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        let dead = pane_exit(Some(exit));
        out.push(tick_rt(&mut m, &dead, None, killed, None, 20));
    }
    out
}

fn f_a13() -> Vec<Fed> {
    // 关掉 `runtime_gone` 的 Status::Finished（改回 unknown）→ 三格都红。
    let mut out = Vec::new();
    for (gone, killed) in [
        (RuntimeGone::Session, false),
        (RuntimeGone::Server, false),
        (RuntimeGone::Session, true),
    ] {
        out.push(running_then_fact(
            true,
            gone_fact(gone, killed),
            Liveness::Dead,
            false,
        ));
    }
    out
}

fn f_a14() -> Vec<Fed> {
    // 关掉 `ProcessState::derive` 的第一条规则就红（旧代码报 finished + alive:true）。
    vec![host_end_while_the_process_is_alive(
        AgoraEvent::SessionEnded(Some("prompt_input_exit".into())),
    )]
}

fn f_a15() -> Vec<Fed> {
    // 关掉 `process_layer` 的 `Code(n)` → Failed 分支 → 红。
    let mut out = Vec::new();
    for fact in [
        rt_exit(Some(Exit::Code(3)), false),
        rt_exit(Some(Exit::Signal("hup".into())), false),
    ] {
        out.push(running_then_fact(true, fact, Liveness::Dead, false));
    }
    out
}

fn f_a16() -> Vec<Fed> {
    // 结束了的行不许报 alive：靠 derive 的第一条规则挡住。
    vec![
        host_end_while_the_process_is_alive(AgoraEvent::SessionEnded(Some(
            "prompt_input_exit".into(),
        ))),
        host_end_while_the_process_is_alive(AgoraEvent::Superseded),
    ]
}

fn f_a17() -> Vec<Fed> {
    // 关掉 derive 的 runtime_fact_missing 分支就报出一条假的 gone。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let a = m.observe(Observation {
        process: agora::status::runtime_unavailable("protocol version mismatch"),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
        at: None,
    });
    vec![Fed {
        a,
        liveness: Liveness::Dead,
        runtime_fact_missing: true,
    }]
}

fn f_a18() -> Vec<Fed> {
    // 屏幕给的两格 UNKNOWN 都在"pane 活着"的前提下采得到屏幕。
    vec![
        Fed::new(a_u02(), Liveness::Alive),
        Fed::new(a_u03(), Liveness::Alive),
    ]
}

fn f_a19() -> Vec<Fed> {
    // ① 运行中、列表里找不到它 → a13；② hook 先说了结束、之后会话才没 → source 留 hook。
    let mut out = Vec::new();
    out.push(running_then_fact(
        true,
        gone_fact(RuntimeGone::Session, false),
        Liveness::Dead,
        false,
    ));
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    tick_rt(&mut m, &pane(Some(0)), Some(3600), false, None, 30);
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 40);
    let a = m.observe(Observation {
        process: gone_fact(RuntimeGone::Session, false),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
        at: None,
    });
    out.push(Fed::new(a, Liveness::Dead));
    out
}

fn f_a20() -> Vec<Fed> {
    // 每一类 hook 事件各喂一次：没有一类能把有句柄的行写成 UNKNOWN × hook。
    let rt = pane(Some(0));
    let mut out = Vec::new();
    for event in hook_events() {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&event, 1, 0);
        out.push(tick_rt(&mut m, &rt, Some(3600), false, None, 1));
    }
    out
}

fn f_a21() -> Vec<Fed> {
    // 第一个 tick：streak 才 1，还不够——不许一看到提示就抬；第二个 tick 才落 WAITING × text。
    let waiting = det(Status::Waiting, "permission prompt");
    let mut m = Machine::new(cfg(), false, 1, 0);
    let rt = pane(Some(0));
    let first = tick_rt(&mut m, &rt, Some(3600), false, Some(&waiting), 10);
    let second = tick_rt(&mut m, &rt, Some(3600), false, Some(&waiting), 12);
    vec![first, second]
}

fn f_a22() -> Vec<Fed> {
    // ◐：运行时报了"退了"、退出码下一 tick 才补得上；不许猜。出口：下一 tick 落 a12 / a15。
    let mut out = Vec::new();
    {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        let rt = pane(Some(0));
        tick_rt(&mut m, &rt, Some(3600), false, None, 30);
        out.push(tick_rt(&mut m, &pane_exit(None), None, false, None, 40));
    }
    for exit in [Exit::Code(0), Exit::Code(3)] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
        let rt = pane(Some(0));
        tick_rt(&mut m, &rt, Some(3600), false, None, 30);
        tick_rt(&mut m, &pane_exit(None), None, false, None, 40);
        out.push(tick_rt(
            &mut m,
            &pane_exit(Some(exit)),
            None,
            false,
            None,
            42,
        ));
    }
    out
}

fn f_a23() -> Vec<Fed> {
    // 有句柄、运行时应答正常、这一 tick 列表还没报到它：不是"没了"；process 也是 unknown 不是 gone。
    let mut m = Machine::new(cfg(), true, 1, 0);
    let a = m.observe(Observation {
        process: agora::status::process_layer(None, Some(0), false),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 1,
        at: None,
    });
    let first = Fed {
        a,
        liveness: Liveness::Dead,
        runtime_fact_missing: true,
    };
    // 下一 tick 运行时报到它了，就落 a01：这一格自己不会把行送到结束那一档。
    let second = tick_rt(&mut m, &pane(None), Some(0), false, None, 2);
    vec![first, second]
}

fn f_a24() -> Vec<Fed> {
    // 进程层不盖 hook 的结论：两条 tick 都停在 IDLE，离开这一格的证据是下一条 hook 事件。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.apply(&AgoraEvent::IdleReported, 1, 0);
    vec![
        tick_rt(&mut m, &pane(Some(0)), Some(3600), false, None, 1),
        tick_rt(&mut m, &pane(Some(0)), Some(3600), false, None, 3600),
    ]
}

// ── 第 3 节：无运行时句柄的行（x01…x14）──

fn f_x01() -> Vec<Fed> {
    let mut out = Vec::new();
    for lv in [Liveness::Alive, Liveness::Unknown] {
        let mut m = Machine::new(cfg(), true, 1, 0);
        m.apply(&AgoraEvent::SessionStarted, 1, 0);
        out.push(tick_external(&mut m, lv, 9));
    }
    out
}

fn f_x02() -> Vec<Fed> {
    // 关掉 observe 第 1 步 external 分支里的 decay_starting（agora-rkl 那一处）→ 四格全红。
    let mut out = Vec::new();
    for lv in [Liveness::Alive, Liveness::Unknown] {
        for at in [10, 3600] {
            let mut m = Machine::new(cfg(), true, 1, 0);
            m.apply(&AgoraEvent::SessionStarted, 1, 0);
            out.push(tick_external(&mut m, lv, at));
        }
    }
    out
}

fn f_x03() -> Vec<Fed> {
    let mut out = Vec::new();
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
        out.push(tick_external(&mut m, Liveness::Alive, 5));
    }
    out
}

fn f_x04() -> Vec<Fed> {
    // Codex Desktop 那一格：进程号不可信，行照旧只由 hook 说话。
    let mut out = Vec::new();
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
        out.push(tick_external(&mut m, Liveness::Unknown, 5));
    }
    out
}

fn f_x05() -> Vec<Fed> {
    let mut out = Vec::new();
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
        out.push(tick_external(&mut m, Liveness::Dead, 20));
    }
    out
}

fn f_x06() -> Vec<Fed> {
    // 关掉第 1 步那个提前返回（让 external 行走 observe_unhooked）→ 61 s 那一 tick 会落 IDLE 而红。
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    for at in [5, 61, 3600] {
        out.push(tick_external(&mut m, Liveness::Alive, at));
    }
    // 对照：声明没有 hook 的无句柄行也不产 IDLE，它连"没有观测"这格都出不去（x12）。
    let mut m = Machine::new(cfg(), false, 1, 0);
    out.push(tick_external(&mut m, Liveness::Unknown, 3600));
    out
}

fn f_x07() -> Vec<Fed> {
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    out.push(tick_external(&mut m, Liveness::Dead, 20));
    // 有过对话的行同一条：进程号没了就是结束，不管它先前停在哪个 hook 状态。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    out.push(tick_external(&mut m, Liveness::Dead, 20));
    out
}

fn f_x08() -> Vec<Fed> {
    // 盘点 B1 的那 10 行：宿主说了结束（或换了对话），而那个号还在跑新对话；process 一律 gone。
    let mut out = Vec::new();
    for end in [
        AgoraEvent::SessionEnded(Some("prompt_input_exit".into())),
        AgoraEvent::SessionEnded(Some("resume".into())),
        AgoraEvent::Superseded,
    ] {
        for lv in [Liveness::Alive, Liveness::Dead] {
            let mut m = Machine::new(cfg(), true, 1, 0);
            m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
            m.apply(&end, 1, 1);
            out.push(tick_external(&mut m, lv, 5));
        }
    }
    out
}

fn f_x09() -> Vec<Fed> {
    // 无句柄行拿不到退出码：结束只有 FINISHED 一种，也不许因为"没有码"退化成 UNKNOWN。
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
    let mut out = Vec::new();
    for event in &events {
        for lv in [Liveness::Alive, Liveness::Unknown, Liveness::Dead] {
            let mut m = Machine::new(cfg(), true, 1, 0);
            m.apply(event, 1, 0);
            out.push(tick_external(&mut m, lv, 5));
        }
    }
    out
}

fn f_x10() -> Vec<Fed> {
    // 沉默时钟按**事件自己的时刻**算（agora-5gg.2）：停机 3.5 天 + 重放不能把"沉默了多久"清零。
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply_at(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201, 1);
    out.push(tick_external(&mut m, Liveness::Unknown, 7201));
    // 对照：拿 daemon 的收到时刻算就还是 TURN_DONE——把 quiet 时钟改回 last_hook_at 就红在这一格。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201);
    out.push(tick_external(&mut m, Liveness::Unknown, 7201));
    // ◐ 的出口：下一条 hook 事件即恢复。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply_at(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201, 1);
    tick_external(&mut m, Liveness::Unknown, 7201);
    m.apply(&AgoraEvent::TurnEnded(Some("later".into())), 1, 7205);
    out.push(tick_external(&mut m, Liveness::Unknown, 7205));
    out
}

fn f_x11() -> Vec<Fed> {
    // 关掉 `obs.liveness == Liveness::Unknown` 那个条件 → 第一格红（Alive 的行也被打成 UNKNOWN）。
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    out.push(tick_external(&mut m, Liveness::Alive, 7201));
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    out.push(tick_external(&mut m, Liveness::Dead, 7201));
    out
}

fn f_x12() -> Vec<Fed> {
    // ◐：只在检查点恢复 / 重放完成前的那一瞬间合法，一条 hook 事件一到就走。
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    out.push(tick_external(&mut m, Liveness::Alive, 5));
    let mut m = Machine::new(cfg(), true, 1, 0);
    out.push(tick_external(&mut m, Liveness::Unknown, 5));
    // 一条 hook 事件都没的无句柄行会留在这格（持续出现 = 检查点丢了，属 bug）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    tick_external(&mut m, Liveness::Unknown, 7201);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7202);
    out.push(tick_external(&mut m, Liveness::Unknown, 7202));
    out
}

fn x13_contract() {
    // 真值表按"有没有句柄"分两节，靠的是代码一律问 `Origin::is_handleless()`。新增一档 origin
    // 时这里编译不过：先回答它有没有句柄，再决定它进第 2 节还是第 3 节。
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

fn f_x14() -> Vec<Fed> {
    // 登记即空闲：reload 一个跑了 N 小时的会话，STARTING / TURN_DONE 都是假话。
    let mut out = Vec::new();
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.apply(&AgoraEvent::IdleReported, 1, 0);
    out.push(tick_external(&mut m, Liveness::Alive, 1));
    out.push(tick_external(&mut m, Liveness::Alive, 2 * 3600));
    // 没有进程号的（丢了号的旧检查点）：沉默满 external_silent_after 走 x10。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.apply(&AgoraEvent::IdleReported, 1, 0);
    out.push(tick_external(&mut m, Liveness::Unknown, 2 * 3600));
    // 投递乱序的反面：已经 RUNNING 的行晚收到这条登记，不许被降成空闲。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(&AgoraEvent::IdleReported, 1, 0);
    out.push(tick_external(&mut m, Liveness::Alive, 1));
    out
}

// ── external STARTING 衰减（旧 Row 组，agora-rkl）──

fn external_starting(liveness: Liveness, at: i64) -> Fed {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    let a = m.observe(external(liveness, at));
    Fed::new(a, liveness)
}

// ── runtime gone（旧 GoneRow 组，agora-u5p）──

fn gone_row(gone: RuntimeGone, killed: bool) -> Fed {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.observe(Observation {
        process: Assessment::new(Status::Running, Source::Process, 1.0, None),
        liveness: Liveness::Alive,
        text: None,
        runtime: None,
        epoch: 1,
        now: 30,
        at: None,
    });
    let a = m.observe(Observation {
        process: gone_fact(gone, killed),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
        at: None,
    });
    Fed::new(a, Liveness::Dead)
}

// ── 结束原因词表（旧 EndRow 组，agora-5gg.6）──

/// 喂一条 hook 事件后取状态机的结论（进程号还活着的两态由 `Liveness::Alive` 表达，
/// 结束行一律 gone 是 derive 的第一条规则）。
fn hook_end(e: AgoraEvent) -> Fed {
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    m.apply(&e, 1, 1);
    Fed::new(m.current().clone(), Liveness::Alive)
}

fn session_end(reason: Option<&str>) -> Fed {
    hook_end(AgoraEvent::SessionEnded(reason.map(str::to_owned)))
}

/// 这条修复之前写下的 hook 检查点恢复出来的结束行（`end_cause` 还没有落盘，agora-ohvn）：
/// `restore_hook` 要补一档显式的「检查点未记录原因」，不能让 None 一路冒到 API。
fn checkpoint_finished_without_end_cause() -> Assessment {
    let mut m = Machine::new(cfg(), true, 1, 0);
    let snapshot: agora::status::machine::HookSnapshot =
        serde_json::from_value(serde_json::json!({
            "version": 3,
            "epoch": 1,
            "current": {
                "status": "finished",
                "source": "hook",
                "confidence": 0.8,
                "reason": "session ended (hook)",
            },
            "set_at": 5,
            "last_hook_at": 5,
            "pending": [],
        }))
        .unwrap();
    m.restore_hook(snapshot);
    m.current().clone()
}

// ── 说不清原因词表（旧 UnknownRow 组，agora-5gg.6）──

fn a_u01() -> Assessment {
    // 刚建起来、一条观测都没有：说不清，但没有"说不清的细节"可说。
    Machine::new(cfg(), true, 1, 0).current().clone()
}

fn a_u02() -> Assessment {
    // hook 沉默到阈值、屏幕像在等人（ADR-002 D1 的沉默规则）。
    let session = RuntimeSession {
        output_at: Some(0),
        ..pane(None)
    };
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    let waiting = DetectionResult {
        status: Status::Waiting,
        confidence: 0.7,
        reason: "permission prompt".to_owned(),
    };
    let a = m.observe(Observation {
        process: Assessment::new(Status::Running, Source::Process, 1.0, None),
        liveness: Liveness::Alive,
        text: Some(waiting),
        runtime: Some(&session),
        epoch: 1,
        now: 601,
        at: None,
    });
    assert_eq!(a.status, Status::Unknown, "{a:?}");
    a
}

fn a_u03() -> Assessment {
    // 挂起的权限提示从屏幕上消失了（agora-9cd）。
    let session = RuntimeSession {
        output_at: Some(0),
        ..pane(None)
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
            at: None,
        })
    };
    obs(&mut m, 3, true);
    obs(&mut m, 5, false);
    let a = obs(&mut m, 7, false);
    assert_eq!(a.status, Status::Unknown, "{a:?}");
    a
}

fn a_u04() -> Assessment {
    // 无句柄 external 行沉默到 hooks.external_silent_after（agora-tql）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.observe(external(Liveness::Unknown, 7201))
}

fn a_u05() -> Assessment {
    after_running(agora::status::runtime_unavailable(
        "protocol version mismatch",
    ))
}

fn a_u06() -> Assessment {
    // 运行时报了"退了"、退出码还没收集到：下一 tick 补上，不猜 FINISHED 也不猜 FAILED。
    after_running(rt_exit(None, false))
}

// ── 分片（shard）行的 process 断言（并入 agora-3lc）──
//
// 真值表里 process 相关的格以前由 `src/status/mod.rs` 的四条单测（与 api 侧的投影断言）
// 分片覆盖；这里把它们的每一条都收进统一的枚举断言。src 侧的单测保留不动（它们是同一行为的
// 另一份守卫），这里保证"每格带枚举值"的那张表也管得住这些形状。

fn sh_finished_and_failed_always_report_gone() -> Vec<Fed> {
    // `ProcessState::derive` 第一条规则：结束行的 process 与 liveness、与"读不到"都无关。
    let fin = after_running(rt_exit(Some(Exit::Code(0)), false));
    let fail = after_running(rt_exit(Some(Exit::Code(3)), false));
    let mut out = Vec::new();
    for a in [fin, fail] {
        for liveness in [Liveness::Alive, Liveness::Dead, Liveness::Unknown] {
            for runtime_fact_missing in [false, true] {
                out.push(Fed {
                    a: a.clone(),
                    liveness,
                    runtime_fact_missing,
                });
            }
        }
    }
    out
}

fn sh_unreadable_runtime_is_unknown_not_gone() -> Vec<Fed> {
    // 第二条规则：没有运行时事实 → unknown（ADR-001 D7 与 agora-86nk 的窗口格同一条）。
    let a = a_u05();
    let mut out = Vec::new();
    for liveness in [Liveness::Alive, Liveness::Dead, Liveness::Unknown] {
        out.push(Fed {
            a: a.clone(),
            liveness,
            runtime_fact_missing: true,
        });
    }
    out
}

fn sh_liveness_maps_straight_through() -> Vec<Fed> {
    // 第三条规则：其余按三值直译（没有可信进程号 ≠ 没了）。
    vec![
        Fed::new(
            Assessment::new(Status::Running, Source::Process, 1.0, None),
            Liveness::Alive,
        ),
        Fed::new(
            Assessment::new(Status::TurnDone, Source::Hook, 1.0, None),
            Liveness::Unknown,
        ),
        Fed::new(a_u06(), Liveness::Dead),
    ]
}

fn sh_wire_vocabulary() {
    // `process` 的取值是 API 形态（docs/spec/api.md「会话形态」）：改 rename_all 或改变体名都红这里。
    assert_eq!(
        serde_json::to_value(ProcessState::Alive).unwrap(),
        serde_json::json!("alive")
    );
    assert_eq!(
        serde_json::to_value(ProcessState::Gone).unwrap(),
        serde_json::json!("gone")
    );
    assert_eq!(
        serde_json::to_value(ProcessState::Unknown).unwrap(),
        serde_json::json!("unknown")
    );
    assert_eq!(
        serde_json::from_value::<ProcessState>(serde_json::json!("gone")).unwrap(),
        ProcessState::Gone
    );
}

// ── expire_external_finished 的三 arm：它们吃的状态形状（含 91vy 锚两态，见文件尾的真跑）──
//
// 三条 arm 的**判据**是状态形状 + 原因枚举，由状态机在这一行拍板：
//   arm 1（finished_ttl，时钟 ended_at）：external 的 FINISHED（hook 结束 / superseded / 探活没了）；
//   arm 2（unknown_ttl，时钟 status_since）：external 的 UNKNOWN 且 `hooks_silent_no_handle`——
//     只有这一档；"没有观测"（x12）与"运行时读不到"（a17）另有出口，不归它；
//   arm 3（finished_ttl，不论状态）：headless 的任意一格（含 TURN_DONE / RUNNING / IDLE）。
// 真正调用 `expire_external_finished`（三 arm × 两只时钟 × 两个 ttl × 锚两态）在文件尾的
// `expire_arms_and_the_local_seen_anchor_are_pinned`。
//
// 91vy 的两态：`local_seen_at` 锚**在**且现在的号比锚新 → 探活判死 → 行落 q02 这一格（arm 1 看得见）；
// 锚**缺**（升级前的检查点）→ 跳过复用判据、号还在 → 行留在 q06 这一格（arm 1 看不见它）。

fn f_q01() -> Vec<Fed> {
    // arm 1 的一档：宿主自己结束（ended_at = SessionEnd 那一刻）。
    vec![session_end(Some("other"))]
}

fn f_q02() -> Vec<Fed> {
    // arm 1 的另一档：探活判死（ended_at 近似）——锚在且号被复用就走这条。
    let mut m = Machine::new(cfg(), true, 1, 0);
    vec![tick_external(&mut m, Liveness::Dead, 20)]
}

fn f_q03() -> Vec<Fed> {
    // arm 2 唯一认得的一档。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply_at(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7201, 1);
    vec![tick_external(&mut m, Liveness::Unknown, 7201)]
}

fn f_q04() -> Vec<Fed> {
    // arm 2 不认的一档：UNKNOWN 但没有"沉默"那句原因（没有观测，x12）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    vec![tick_external(&mut m, Liveness::Unknown, 5)]
}

fn f_q05() -> Vec<Fed> {
    // arm 2 也不认：运行时读不到（a17），另有降级自愈的出口。
    vec![Fed {
        a: a_u05(),
        liveness: Liveness::Dead,
        runtime_fact_missing: true,
    }]
}

fn f_q06() -> Vec<Fed> {
    // arm 3：headless 的 TURN_DONE（还挂在等人回看的格上）——锚缺、号还在时这里是它的归宿。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 0);
    vec![tick_external(&mut m, Liveness::Alive, 5)]
}

fn f_q07() -> Vec<Fed> {
    // arm 3：headless 的 RUNNING（拿状态当门槛就永远删不掉）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
    vec![tick_external(&mut m, Liveness::Alive, 5)]
}

fn f_q08() -> Vec<Fed> {
    // arm 3：headless 的 IDLE。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.apply(&AgoraEvent::IdleReported, 1, 0);
    vec![tick_external(&mut m, Liveness::Alive, 1)]
}

// ─────────────────────────────── 唯一的表 ───────────────────────────────

// 每一行：id / spec（占不占 docs/spec/status.md 的格）/ 组 / 为什么 / 喂法 / 枚举期望。
// 表按 docs/spec/status.md 的顺序排；词表与组合行跟在自己服务的格之后。
const ROWS: &[Row] = &[
    // ── 第 2 节：origin = agora / adopted（有运行时句柄）──
    Row {
        id: "a01",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "STARTING | alive | process（本代起始 < 2 s）" }),
        group: Group::Cell,
        why: "刚起，等一下",
        feed: Feed::Feds(f_a01),
        want: &[lands(Status::Starting, Source::Process, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "a02",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "STARTING | alive | hook（SessionStart 之后 ≤ 10 s）" }),
        group: Group::Cell,
        why: "刚起，等一下：宽限内不许衰减",
        feed: Feed::Feds(f_a02),
        want: &[lands(Status::Starting, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "a03",
        spec: Some(Spec { verdict: Verdict::Never, cell: "STARTING | alive | hook（> 10 s）" }),
        group: Group::Cell,
        why: "起好了停在提示符等第一条指令，该归 TURN_DONE（agora-okr）",
        feed: Feed::Feds(f_a03),
        want: &[forbid(
            &[Ban::Cell(Status::Starting, Source::Hook)],
            Some(at(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)),
        )],
    },
    Row {
        id: "a04",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "RUNNING | alive | process" }),
        group: Group::Cell,
        why: "在干活，不用管",
        feed: Feed::Feds(f_a04),
        want: &[lands(Status::Running, Source::Process, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "a05",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "RUNNING | alive | hook" }),
        group: Group::Cell,
        why: "在干活，不用管",
        feed: Feed::Feds(f_a05),
        want: &[lands(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "a06",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "WAITING | alive | hook" }),
        group: Group::Cell,
        why: "答它：能经 hook 答的按 respond_via = hook，否则打开终端",
        feed: Feed::Feds(f_a06),
        want: &[lands(Status::Waiting, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "a07",
        spec: Some(Spec { verdict: Verdict::Never, cell: "WAITING | alive | text，agent 有 hook" }),
        group: Group::Cell,
        why: "ADR-002 D1：文本抬不起 WAITING，只有 hook 沉默兜底能降它",
        feed: Feed::Feds(f_a07),
        want: &[forbid(
            &[Ban::Cell(Status::Waiting, Source::Text)],
            Some(at(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)),
        )],
    },
    Row {
        id: "a08",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "TURN_DONE | alive | hook" }),
        group: Group::Cell,
        why: "看结果、给下一条",
        feed: Feed::Feds(f_a08),
        want: &[lands(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "a09",
        spec: Some(Spec { verdict: Verdict::Never, cell: "TURN_DONE | alive | text 或 activity" }),
        group: Group::Cell,
        why: "「一轮做完」只有宿主自己说得出来（Stop / idle）",
        feed: Feed::Feds(f_a09),
        want: &[
            forbid(
                &[Ban::Cell(Status::TurnDone, Source::Text), Ban::Cell(Status::TurnDone, Source::Activity)],
                Some(at(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Cell(Status::TurnDone, Source::Text), Ban::Cell(Status::TurnDone, Source::Activity)],
                Some(at(Status::Running, Source::Process, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Cell(Status::TurnDone, Source::Text), Ban::Cell(Status::TurnDone, Source::Activity)],
                Some(at(Status::Idle, Source::Activity, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Cell(Status::TurnDone, Source::Text), Ban::Cell(Status::TurnDone, Source::Activity)],
                Some(at(Status::Idle, Source::Activity, ProcessState::Alive, Cause::None)),
            ),
        ],
    },
    Row {
        id: "a10",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "IDLE | alive | activity（无 hook 的行）" }),
        group: Group::Cell,
        why: "安静了一阵，没人说为什么——IDLE 只出现在兜底路径",
        feed: Feed::Feds(f_a10),
        want: &[
            lands(Status::Running, Source::Process, ProcessState::Alive, Cause::None),
            lands(Status::Idle, Source::Activity, ProcessState::Alive, Cause::None),
        ],
    },
    Row {
        id: "a11",
        spec: Some(Spec { verdict: Verdict::Never, cell: "IDLE | alive | activity，agent 有 hook" }),
        group: Group::Cell,
        why: "活动层不产 IDLE：hook 活着而不说话是说不清（a18），不是安静",
        feed: Feed::Feds(f_a11),
        want: &[forbid(
            &[Ban::Status(Status::Idle)],
            Some(at(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)),
        )],
    },
    Row {
        id: "a12",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "FINISHED | gone | process（退出码 0 / killed by user）" }),
        group: Group::Cell,
        why: "退了，看结果或清理；按过 Kill 的不弹通知",
        feed: Feed::Feds(f_a12),
        want: &[
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::KilledByUser),
        ],
    },
    Row {
        id: "a13",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "FINISHED | gone | process（`runtime_gone`）" }),
        group: Group::Cell,
        why: "运行时会话没了是事实：Restart 重建或删除（agora-u5p）",
        feed: Feed::Feds(f_a13),
        want: &[
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("session")),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("server")),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::KilledByUser),
        ],
    },
    Row {
        id: "a14",
        spec: Some(Spec { verdict: Verdict::Brief, cell: "FINISHED | gone | hook（SessionEnd 先于进程退出被观测）" }),
        group: Group::Cell,
        why: "下一 tick 进程层带着退出码换掉 source（同状态同分不抢，agora-rzh）",
        feed: Feed::Feds(f_a14),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("exit"))],
    },
    Row {
        id: "a15",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "FAILED | gone | process（exit ≠ 0 / signal）" }),
        group: Group::Cell,
        why: "出错了，看终端；end_cause 带的是那个码或信号",
        feed: Feed::Feds(f_a15),
        want: &[
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::Signal("hup")),
        ],
    },
    Row {
        id: "a16",
        spec: Some(Spec { verdict: Verdict::Never, cell: "FINISHED / FAILED | alive，持续 | 任何" }),
        group: Group::Cell,
        why: "进程退出压倒一切 + Q4：结束了的行不许报出 alive",
        feed: Feed::Feds(f_a16),
        want: &[
            forbid(
                &[Ban::Process(ProcessState::Alive)],
                Some(at(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("exit"))),
            ),
            forbid(
                &[Ban::Process(ProcessState::Alive)],
                Some(at(Status::Finished, Source::Hook, ProcessState::Gone, Cause::Superseded)),
            ),
        ],
    },
    Row {
        id: "a17",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "UNKNOWN | unknown | none（`runtime unavailable`）" }),
        group: Group::Cell,
        why: "agora 失明，横幅说明；恢复即自愈，绝不写 ended_at（ADR-001 D7）；source 是 none",
        feed: Feed::Feds(f_a17),
        want: &[lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable"))],
    },
    Row {
        id: "a18",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "UNKNOWN | alive | text（`hooks silent; screen: …` / `permission prompt gone`）" }),
        group: Group::Cell,
        why: "hook 没声音而屏幕像在等人：打开终端，或按 hooks_unheard 去查 hook",
        feed: Feed::Feds(f_a18),
        want: &[
            lands(Status::Unknown, Source::Text, ProcessState::Alive, Cause::Unknown("hooks_silent_screen")),
            lands(Status::Unknown, Source::Text, ProcessState::Alive, Cause::Unknown("prompt_gone")),
        ],
    },
    Row {
        id: "a19",
        spec: Some(Spec { verdict: Verdict::Never, cell: "UNKNOWN | gone | none（旧版 `runtime session missing`：本代已过 STARTING 窗口）" }),
        group: Group::Cell,
        why: "运行时会话没了是事实、不是看不清（a13）；还在窗口里的那格另算（a23）",
        feed: Feed::Feds(f_a19),
        want: &[
            forbid(
                &[
                    Ban::Cell(Status::Unknown, Source::None),
                    Ban::Cell(Status::Unknown, Source::Process),
                    Ban::Cell(Status::Unknown, Source::Text),
                ],
                Some(at(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("session"))),
            ),
            forbid(
                &[
                    Ban::Cell(Status::Unknown, Source::None),
                    Ban::Cell(Status::Unknown, Source::Hook),
                    Ban::Cell(Status::Unknown, Source::Process),
                    Ban::Cell(Status::Unknown, Source::Text),
                ],
                Some(at(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("other"))),
            ),
        ],
    },
    Row {
        id: "a20",
        spec: Some(Spec { verdict: Verdict::Never, cell: "UNKNOWN | 任何 | hook" }),
        group: Group::Cell,
        why: "有句柄的行里 UNKNOWN 只有 a17 / a18 / a22 / a23 四格；hook 的词表不含它",
        feed: Feed::Feds(f_a20),
        want: &[forbid(&[Ban::Cell(Status::Unknown, Source::Hook)], None)],
    },
    Row {
        id: "a21",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "WAITING | alive | text（agent 无 hook）" }),
        group: Group::Cell,
        why: "答它：这一行没有 hook 可回，只能打开终端；与 a07 是同一层在两种行上的两种命运",
        feed: Feed::Feds(f_a21),
        want: &[
            lands(Status::Running, Source::Process, ProcessState::Alive, Cause::None),
            lands(Status::Waiting, Source::Text, ProcessState::Alive, Cause::None),
        ],
    },
    Row {
        id: "a22",
        spec: Some(Spec { verdict: Verdict::Brief, cell: "UNKNOWN | gone | process（`process exited, exit status not yet collected`）" }),
        group: Group::Cell,
        why: "只许停一个 tick：码到了落 a12 / a15，永远补不上落 a13；不猜 FINISHED 也不猜 FAILED",
        feed: Feed::Feds(f_a22),
        want: &[
            lands(Status::Unknown, Source::Process, ProcessState::Gone, Cause::Unknown("exit_status_missing")),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
        ],
    },
    Row {
        id: "a23",
        spec: Some(Spec { verdict: Verdict::Brief, cell: "UNKNOWN | unknown | none（`runtime session missing`，本代还在 STARTING 窗口 < 2 s）" }),
        group: Group::Cell,
        why: "运行时这一 tick 还没报到它，不等于没了：不写 ended_at、process 也不报假的 gone",
        feed: Feed::Feds(f_a23),
        want: &[
            lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("no_observation")),
            lands(Status::Starting, Source::Process, ProcessState::Alive, Cause::None),
        ],
    },
    Row {
        id: "a24",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "IDLE | alive | hook（登记即空闲）" }),
        group: Group::Cell,
        why: "宿主自报 ctx.isIdle()（pi 的 /reload、agora create pi 的托管行）；进程层不盖 hook 的结论",
        feed: Feed::Feds(f_a24),
        want: &[lands(Status::Idle, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    // ── 第 3 节：origin = external / headless（无运行时句柄）──
    Row {
        id: "x01",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "STARTING | alive 或 unknown | hook（≤ 10 s）" }),
        group: Group::Cell,
        why: "刚起：无句柄行也一样，进程号在不在都不影响宽限",
        feed: Feed::Feds(f_x01),
        want: &[
            lands(Status::Starting, Source::Hook, ProcessState::Alive, Cause::None),
            lands(Status::Starting, Source::Hook, ProcessState::Unknown, Cause::None),
        ],
    },
    Row {
        id: "x02",
        spec: Some(Spec { verdict: Verdict::Never, cell: "STARTING | 任何 | hook（> 10 s）" }),
        group: Group::Cell,
        why: "衰减不分 origin（现场 zuan ef0e50 钉过 180 h，agora-rkl）",
        feed: Feed::Feds(f_x02),
        want: &[
            forbid(
                &[Ban::Cell(Status::Starting, Source::Hook)],
                Some(at(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Cell(Status::Starting, Source::Hook)],
                Some(at(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Cell(Status::Starting, Source::Hook)],
                Some(at(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None)),
            ),
            forbid(
                &[Ban::Cell(Status::Starting, Source::Hook)],
                Some(at(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None)),
            ),
        ],
    },
    Row {
        id: "x03",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "RUNNING / WAITING / TURN_DONE | alive | hook" }),
        group: Group::Cell,
        why: "hook 说什么就是什么：进程号活着不会改它的说法",
        feed: Feed::Feds(f_x03),
        want: &[
            lands(Status::Running, Source::Hook, ProcessState::Alive, Cause::None),
            lands(Status::Waiting, Source::Hook, ProcessState::Alive, Cause::None),
            lands(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None),
        ],
    },
    Row {
        id: "x04",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "RUNNING / WAITING / TURN_DONE | unknown | hook" }),
        group: Group::Cell,
        why: "同上，但 agora 说不上进程在不在（Codex Desktop）",
        feed: Feed::Feds(f_x04),
        want: &[
            lands(Status::Running, Source::Hook, ProcessState::Unknown, Cause::None),
            lands(Status::Waiting, Source::Hook, ProcessState::Unknown, Cause::None),
            lands(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None),
        ],
    },
    Row {
        id: "x05",
        spec: Some(Spec { verdict: Verdict::Never, cell: "RUNNING / WAITING / TURN_DONE | gone | 任何" }),
        group: Group::Cell,
        why: "进程号探不到了就是结束，落在 x07",
        feed: Feed::Feds(f_x05),
        want: &[forbid(
            &[Ban::Status(Status::Running), Ban::Status(Status::Waiting), Ban::Status(Status::TurnDone)],
            Some(at(Status::Finished, Source::Process, ProcessState::Gone, Cause::ProcessGone)),
        )],
    },
    Row {
        id: "x06",
        spec: Some(Spec { verdict: Verdict::Never, cell: "IDLE | 任何 | activity / text / none" }),
        group: Group::Cell,
        why: "无句柄行没有活动来源（没有 pane 可采输出）；宿主自报空闲是 x14 那一格",
        feed: Feed::Feds(f_x06),
        want: &[
            forbid(
                &[Ban::Status(Status::Idle)],
                Some(at(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Status(Status::Idle)],
                Some(at(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Status(Status::Idle)],
                Some(at(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[Ban::Status(Status::Idle)],
                Some(at(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("no_observation"))),
            ),
        ],
    },
    Row {
        id: "x07",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "FINISHED | gone | process（`process_gone`）" }),
        group: Group::Cell,
        why: "一个事件都没来、进程消失：崩溃 / 关窗口；通知一次",
        feed: Feed::Feds(f_x07),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ProcessGone)],
    },
    Row {
        id: "x08",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "FINISHED | gone | hook（`host_session_end{clear\\|resume\\|logout\\|exit\\|other}` / `superseded`）" }),
        group: Group::Cell,
        why: "人自己结束的或换了对话：不通知；号还在跑新对话也报 gone（Q4）",
        feed: Feed::Feds(f_x08),
        want: &[
            lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("exit")),
            lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("exit")),
            lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("resume")),
            lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("resume")),
            lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::Superseded),
            lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::Superseded),
        ],
    },
    Row {
        id: "x09",
        spec: Some(Spec { verdict: Verdict::Never, cell: "FAILED | 任何 | 任何" }),
        group: Group::Cell,
        why: "没有退出码可拿，分不出两种退法：结束只有 FINISHED",
        feed: Feed::Feds(f_x09),
        want: &[forbid(&[Ban::Status(Status::Failed)], None)],
    },
    Row {
        id: "x10",
        spec: Some(Spec { verdict: Verdict::Brief, cell: "UNKNOWN | unknown | hook（`hooks silent; no process handle`）" }),
        group: Group::Cell,
        why: "暂态：下一条 hook 恢复，否则满 sessions.external_unknown_ttl 走 DELETE（agora-e08）",
        feed: Feed::Feds(f_x10),
        want: &[
            lands(Status::Unknown, Source::Hook, ProcessState::Unknown, Cause::Unknown("hooks_silent_no_handle")),
            lands(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None),
            lands(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None),
        ],
    },
    Row {
        id: "x11",
        spec: Some(Spec { verdict: Verdict::Never, cell: "UNKNOWN | alive 或 gone | hook / text / process" }),
        group: Group::Cell,
        why: "行上说过话、或探到了进程事实，就不该说不清",
        feed: Feed::Feds(f_x11),
        want: &[
            forbid(
                &[
                    Ban::Cell(Status::Unknown, Source::Hook),
                    Ban::Cell(Status::Unknown, Source::Text),
                    Ban::Cell(Status::Unknown, Source::Process),
                    Ban::Cell(Status::Unknown, Source::None),
                ],
                Some(at(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)),
            ),
            forbid(
                &[
                    Ban::Cell(Status::Unknown, Source::Hook),
                    Ban::Cell(Status::Unknown, Source::Process),
                    Ban::Cell(Status::Unknown, Source::Text),
                    Ban::Cell(Status::Unknown, Source::None),
                ],
                Some(at(Status::Finished, Source::Process, ProcessState::Gone, Cause::ProcessGone)),
            ),
        ],
    },
    Row {
        id: "x12",
        spec: Some(Spec { verdict: Verdict::Brief, cell: "UNKNOWN | 任何 | none（`no observation yet` / `external session: … hook only`）" }),
        group: Group::Cell,
        why: "只允许在检查点恢复 / 重放完成前的瞬间：第一条 hook 事件一到就走",
        feed: Feed::Feds(f_x12),
        want: &[
            lands(Status::Unknown, Source::None, ProcessState::Alive, Cause::Unknown("no_observation")),
            lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("no_observation")),
            lands(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None),
        ],
    },
    Row {
        id: "x13",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "本节每一格 | 同 external | 同 external" }),
        group: Group::Cell,
        why: "headless 与 external 同一张表：代码里一律问 Origin::is_handleless()",
        feed: Feed::Direct(x13_contract),
        want: &[],
    },
    Row {
        id: "x14",
        spec: Some(Spec { verdict: Verdict::Legal, cell: "IDLE | 任何 | hook（登记即空闲：`session_start` 自报 `ctx.isIdle()`）" }),
        group: Group::Cell,
        why: "进程在、没在跑、也不等人；只从 STARTING 吃（晚到的登记不许把在跑的行降下来）",
        feed: Feed::Feds(f_x14),
        want: &[
            lands(Status::Idle, Source::Hook, ProcessState::Alive, Cause::None),
            lands(Status::Idle, Source::Hook, ProcessState::Alive, Cause::None),
            lands(Status::Unknown, Source::Hook, ProcessState::Unknown, Cause::Unknown("hooks_silent_no_handle")),
            lands(Status::Running, Source::Hook, ProcessState::Alive, Cause::None),
        ],
    },
    // ── external STARTING 衰减（原 Row 表；每一档都落在 x01 / x02 / x10 三格里）──
    Row {
        id: "s01",
        spec: None,
        group: Group::Starting,
        why: "宽限内不动：agent 可能真在起",
        feed: Feed::Feds(|| vec![external_starting(Liveness::Alive, 9)]),
        want: &[lands(Status::Starting, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "s02",
        spec: None,
        group: Group::Starting,
        why: "起好了、停在提示符等第一条指令（zuan ef0e50 曾钉在 starting 180 h）",
        feed: Feed::Feds(|| vec![external_starting(Liveness::Alive, 10)]),
        want: &[lands(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "s03",
        spec: None,
        group: Group::Starting,
        why: "进程号活着：沉默兜底够不着它，只有衰减能把它送到 TURN_DONE",
        feed: Feed::Feds(|| vec![external_starting(Liveness::Alive, 2 * 3600)]),
        want: &[lands(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "s04",
        spec: None,
        group: Group::Starting,
        why: "无句柄（Codex Desktop、丢了进程号的旧检查点）：宽限内同样不动",
        feed: Feed::Feds(|| vec![external_starting(Liveness::Unknown, 9)]),
        want: &[lands(Status::Starting, Source::Hook, ProcessState::Unknown, Cause::None)],
    },
    Row {
        id: "s05",
        spec: None,
        group: Group::Starting,
        why: "同一条衰减不分 origin、也不分活没活着（MISSION §4.3）",
        feed: Feed::Feds(|| vec![external_starting(Liveness::Unknown, 10)]),
        want: &[lands(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None)],
    },
    Row {
        id: "s06",
        spec: None,
        group: Group::Starting,
        why: "无句柄 + 沉默过 external_silent_after：兜底说\"看不清\"，比\"等指令\"诚实（agora-tql）",
        feed: Feed::Feds(|| vec![external_starting(Liveness::Unknown, 2 * 3600)]),
        want: &[lands(Status::Unknown, Source::Hook, ProcessState::Unknown, Cause::Unknown("hooks_silent_no_handle"))],
    },
    // ── runtime gone（原 GoneRow 表）──
    Row {
        id: "g01",
        spec: None,
        group: Group::Gone,
        why: "会话连同 pane 一起没了 ⇒ pane 进程收 SIGHUP，agent 确定不在（Mac 2026-09-18 A2）",
        feed: Feed::Feds(|| vec![gone_row(RuntimeGone::Session, false)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("session"))],
    },
    Row {
        id: "g02",
        spec: None,
        group: Group::Gone,
        why: "整个 server 连不上：一屋子行同时结束，reason 要说得出是哪一种没了",
        feed: Feed::Feds(|| vec![gone_row(RuntimeGone::Server, false)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("server"))],
    },
    Row {
        id: "g03",
        spec: None,
        group: Group::Gone,
        why: "人在 Dashboard 按过 Kill：结论一样，口径按他做的那件事写，通知静音（§4.6）",
        feed: Feed::Feds(|| vec![gone_row(RuntimeGone::Session, true)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::KilledByUser)],
    },
    Row {
        id: "g04",
        spec: None,
        group: Group::Gone,
        why: "运行时整体读不到 ≠ 会话没了（ADR-001 D7）：不许抬成结束，也不写 ended_at",
        feed: Feed::Feds(|| vec![Fed { a: a_u05(), liveness: Liveness::Dead, runtime_fact_missing: true }]),
        want: &[lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable"))],
    },
    // ── 结束原因词表（原 EndRow 表）──
    Row {
        id: "e01",
        spec: None,
        group: Group::End,
        why: "退出码 0 = 干净结束",
        feed: Feed::Feds(|| vec![Fed::new(after_running(rt_exit(Some(Exit::Code(0)), false)), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0))],
    },
    Row {
        id: "e02",
        spec: None,
        group: Group::End,
        why: "退出码非 0 = FAILED，值就是那个码（分不开两种退法就白报了）",
        feed: Feed::Feds(|| vec![Fed::new(after_running(rt_exit(Some(Exit::Code(3)), false)), Liveness::Dead)]),
        want: &[lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3))],
    },
    Row {
        id: "e03",
        spec: None,
        group: Group::End,
        why: "运行时报的是信号不是码",
        feed: Feed::Feds(|| vec![Fed::new(after_running(rt_exit(Some(Exit::Signal("hup".into())), false)), Liveness::Dead)]),
        want: &[lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::Signal("hup"))],
    },
    Row {
        id: "e04",
        spec: None,
        group: Group::End,
        why: "人在 Dashboard 按过 Kill：哪个码不重要，通知要静音（§4.6）",
        feed: Feed::Feds(|| vec![Fed::new(after_running(rt_exit(Some(Exit::Code(143)), true)), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::KilledByUser)],
    },
    Row {
        id: "e05",
        spec: None,
        group: Group::End,
        why: "Kill 的另一半形状：运行时直接报信号",
        feed: Feed::Feds(|| vec![Fed::new(after_running(rt_exit(Some(Exit::Signal("term".into())), true)), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::KilledByUser)],
    },
    Row {
        id: "e06",
        spec: None,
        group: Group::End,
        why: "server 在、只有这一个会话没了（agora-u5p）",
        feed: Feed::Feds(|| vec![Fed::new(after_running(gone_fact(RuntimeGone::Session, false)), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("session"))],
    },
    Row {
        id: "e07",
        spec: None,
        group: Group::End,
        why: "整个 tmux server 连不上：一屋子行同时结束，值要说得出是哪一种",
        feed: Feed::Feds(|| vec![Fed::new(after_running(gone_fact(RuntimeGone::Server, false)), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::RuntimeGone("server"))],
    },
    Row {
        id: "e08",
        spec: None,
        group: Group::End,
        why: "按过 Kill 之后的 runtime gone 只报 Kill：session/server 那半句留在 reason 里",
        feed: Feed::Feds(|| vec![Fed::new(after_running(gone_fact(RuntimeGone::Session, true)), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::KilledByUser)],
    },
    Row {
        id: "e09",
        spec: None,
        group: Group::End,
        why: "external 行只剩探活：号没了就是结束，没有退出码可分",
        feed: Feed::Feds(|| vec![Fed::new(after_running(agora::status::external_process_gone()), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ProcessGone)],
    },
    Row {
        id: "e10",
        spec: None,
        group: Group::End,
        why: "人在提示符上退出的（宿主的 prompt_input_exit 归一到 exit）",
        feed: Feed::Feds(|| vec![session_end(Some("prompt_input_exit"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("exit"))],
    },
    Row {
        id: "e11",
        spec: None,
        group: Group::End,
        why: "宿主自己收尾关闭的（shutdown 同归 exit：动作一样，这行不用再管）",
        feed: Feed::Feds(|| vec![session_end(Some("shutdown"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("exit"))],
    },
    Row {
        id: "e12",
        spec: None,
        group: Group::End,
        why: "登出",
        feed: Feed::Feds(|| vec![session_end(Some("logout"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("logout"))],
    },
    Row {
        id: "e13",
        spec: None,
        group: Group::End,
        why: "换对话：这就是 5gg.6 立论的那一行——以前它与人自己退出的一模一样",
        feed: Feed::Feds(|| vec![session_end(Some("resume"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("resume"))],
    },
    Row {
        id: "e14",
        spec: None,
        group: Group::End,
        why: "宿主说了 other",
        feed: Feed::Feds(|| vec![session_end(Some("other"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("other"))],
    },
    Row {
        id: "e15",
        spec: None,
        group: Group::End,
        why: "宿主没给 reason：认不出的一律 other，不自造一个值",
        feed: Feed::Feds(|| vec![session_end(None)]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("other"))],
    },
    Row {
        id: "e16",
        spec: None,
        group: Group::End,
        why: "以后某家添的新词：照单收进 other，不该让 agora 读不懂这一行",
        feed: Feed::Feds(|| vec![session_end(Some("org_policy_revoked"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("other"))],
    },
    Row {
        id: "e17",
        spec: None,
        group: Group::End,
        why: "无句柄 external 行的 /clear：身份是被清掉的那个对话 id，旧行到此为止（agora-s3r）",
        feed: Feed::Feds(|| vec![session_end(Some("clear (external row: the new id lands on a new row)"))]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("clear"))],
    },
    Row {
        id: "e18",
        spec: None,
        group: Group::End,
        why: "同一进程换到了新对话（不发 SessionEnd 的那两家）",
        feed: Feed::Feds(|| vec![hook_end(AgoraEvent::Superseded)]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::Superseded)],
    },
    Row {
        id: "e19",
        spec: None,
        group: Group::End,
        why: "修复前写下的 hook 检查点：恢复出来的结束行没有 end_cause，要补一档显式的「检查点未记录原因」（agora-ohvn）",
        feed: Feed::Feds(|| vec![Fed::new(checkpoint_finished_without_end_cause(), Liveness::Dead)]),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::CheckpointUnrecorded)],
    },
    // ── 说不清原因词表（原 UnknownRow 表）──
    Row {
        id: "u01",
        spec: None,
        group: Group::Unknown,
        why: "刚建起来、一条观测都没有：说不清，但没有\"说不清的细节\"可说",
        feed: Feed::Feds(|| vec![Fed::new(a_u01(), Liveness::Unknown)]),
        want: &[lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("no_observation"))],
    },
    Row {
        id: "u02",
        spec: None,
        group: Group::Unknown,
        why: "hook 沉默到阈值、只有屏幕可看（D1）：宁可 UNKNOWN 也不猜 WAITING",
        feed: Feed::Feds(|| vec![Fed::new(a_u02(), Liveness::Alive)]),
        want: &[lands(Status::Unknown, Source::Text, ProcessState::Alive, Cause::Unknown("hooks_silent_screen"))],
    },
    Row {
        id: "u03",
        spec: None,
        group: Group::Unknown,
        why: "挂着的提示从屏幕上没了（终端里答了 / Esc 中断，宿主一个事件都不发）",
        feed: Feed::Feds(|| vec![Fed::new(a_u03(), Liveness::Alive)]),
        want: &[lands(Status::Unknown, Source::Text, ProcessState::Alive, Cause::Unknown("prompt_gone"))],
    },
    Row {
        id: "u04",
        spec: None,
        group: Group::Unknown,
        why: "无句柄 external 行沉默到 hooks.external_silent_after（agora-tql）",
        feed: Feed::Feds(|| vec![Fed::new(a_u04(), Liveness::Unknown)]),
        want: &[lands(Status::Unknown, Source::Hook, ProcessState::Unknown, Cause::Unknown("hooks_silent_no_handle"))],
    },
    Row {
        id: "u05",
        spec: None,
        group: Group::Unknown,
        why: "运行时整体读不到 ≠ 会话没了（ADR-001 D7），process 同时是 unknown",
        feed: Feed::Feds(|| vec![Fed { a: a_u05(), liveness: Liveness::Dead, runtime_fact_missing: true }]),
        want: &[lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable"))],
    },
    Row {
        id: "u06",
        spec: None,
        group: Group::Unknown,
        why: "运行时报了'退了'、退出码还没收集到：下一 tick 补上，不猜 FINISHED 也不猜 FAILED",
        feed: Feed::Feds(|| vec![Fed::new(a_u06(), Liveness::Dead)]),
        want: &[lands(Status::Unknown, Source::Process, ProcessState::Gone, Cause::Unknown("exit_status_missing"))],
    },
    // ── 分片行的 process 断言（并入 agora-3lc）──
    Row {
        id: "p01",
        spec: None,
        group: Group::Shard,
        why: "FINISHED / FAILED 一律 gone：三档 liveness × 有没有运行时事实全过一遍（原 src/status/mod.rs 分片）",
        feed: Feed::Feds(sh_finished_and_failed_always_report_gone),
        want: &[
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ExitCode(0)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
            lands(Status::Failed, Source::Process, ProcessState::Gone, Cause::ExitCode(3)),
        ],
    },
    Row {
        id: "p02",
        spec: None,
        group: Group::Shard,
        why: "没有运行时事实一律 unknown（ADR-001 D7 / agora-86nk）：三档 liveness 都还原",
        feed: Feed::Feds(sh_unreadable_runtime_is_unknown_not_gone),
        want: &[
            lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable")),
            lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable")),
            lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable")),
        ],
    },
    Row {
        id: "p03",
        spec: None,
        group: Group::Shard,
        why: "其余三值直译（原 src/status/mod.rs 的 liveness_maps_straight_through_otherwise）",
        feed: Feed::Feds(sh_liveness_maps_straight_through),
        want: &[
            lands(Status::Running, Source::Process, ProcessState::Alive, Cause::None),
            lands(Status::TurnDone, Source::Hook, ProcessState::Unknown, Cause::None),
            lands(Status::Unknown, Source::Process, ProcessState::Gone, Cause::Unknown("exit_status_missing")),
        ],
    },
    Row {
        id: "p04",
        spec: None,
        group: Group::Shard,
        why: "process 的线形态锁定（原 src/status/mod.rs 的 process_state_wire_names_are_the_locked_vocabulary）",
        feed: Feed::Direct(sh_wire_vocabulary),
        want: &[],
    },
    // ── expire_external_finished 的三 arm 吃的状态形状 ──
    Row {
        id: "q01",
        spec: None,
        group: Group::Expire,
        why: "arm 1（finished_ttl）：external 的 hook 结束，时钟 ended_at",
        feed: Feed::Feds(f_q01),
        want: &[lands(Status::Finished, Source::Hook, ProcessState::Gone, Cause::HostEnd("other"))],
    },
    Row {
        id: "q02",
        spec: None,
        group: Group::Expire,
        why: "arm 1：探活判死（锚在、号被复用）→ process_gone，时钟 ended_at 近似",
        feed: Feed::Feds(f_q02),
        want: &[lands(Status::Finished, Source::Process, ProcessState::Gone, Cause::ProcessGone)],
    },
    Row {
        id: "q03",
        spec: None,
        group: Group::Expire,
        why: "arm 2（unknown_ttl，时钟 status_since）：只有 hooks_silent_no_handle 这一档",
        feed: Feed::Feds(f_q03),
        want: &[lands(Status::Unknown, Source::Hook, ProcessState::Unknown, Cause::Unknown("hooks_silent_no_handle"))],
    },
    Row {
        id: "q04",
        spec: None,
        group: Group::Expire,
        why: "arm 2 不认：UNKNOWN「没有观测」另有出口（第一条 hook 事件）",
        feed: Feed::Feds(f_q04),
        want: &[lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("no_observation"))],
    },
    Row {
        id: "q05",
        spec: None,
        group: Group::Expire,
        why: "arm 2 不认：运行时读不到另有降级自愈的出口（ADR-001 D7）",
        feed: Feed::Feds(f_q05),
        want: &[lands(Status::Unknown, Source::None, ProcessState::Unknown, Cause::Unknown("runtime_unavailable"))],
    },
    Row {
        id: "q06",
        spec: None,
        group: Group::Expire,
        why: "arm 3（finished_ttl，不论状态）：headless 的 TURN_DONE——锚缺、号还在时留在这里",
        feed: Feed::Feds(f_q06),
        want: &[lands(Status::TurnDone, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "q07",
        spec: None,
        group: Group::Expire,
        why: "arm 3：headless 的 RUNNING（拿状态当门槛就永远删不掉）",
        feed: Feed::Feds(f_q07),
        want: &[lands(Status::Running, Source::Hook, ProcessState::Alive, Cause::None)],
    },
    Row {
        id: "q08",
        spec: None,
        group: Group::Expire,
        why: "arm 3：headless 的 IDLE",
        feed: Feed::Feds(f_q08),
        want: &[lands(Status::Idle, Source::Hook, ProcessState::Alive, Cause::None)],
    },
];
// ═══════════════════ 格与守卫：表里每一行都被断言到 ═══════════════════

/// 表里每一行都真的被喂了一遍：喂法非空、枚举期望与判定符号一致、id 不重复；顺带把每一组
/// 都点名一次（哪一组一格都没有 = 有人把格子删空了）。这是「不许有格子只声明不断言」的自检。
#[test]
fn every_row_in_the_table_is_asserted() {
    let mut ids = std::collections::HashSet::new();
    for r in ROWS {
        assert!(ids.insert(r.id), "id 重复：{}", r.id);
        if let Some(spec) = r.spec {
            match spec.verdict {
                Verdict::Never => assert!(
                    r.want.iter().all(|w| matches!(w, Want::Forbid { .. })),
                    "{} 在表里是 ✗，期望必须是反向断言",
                    r.id
                ),
                Verdict::Legal | Verdict::Brief => assert!(
                    r.want.iter().all(|w| matches!(w, Want::Lands(_))),
                    "{} 不是 ✗，期望必须是正向落点",
                    r.id
                ),
            }
        }
        if r.want.is_empty() {
            assert!(
                matches!(r.feed, Feed::Direct(_)),
                "{} 只声明不断言：非 Direct 行必须带枚举期望",
                r.id
            );
        }
        check_row(r);
    }
    for group in [
        Group::Cell,
        Group::Starting,
        Group::Gone,
        Group::End,
        Group::Unknown,
        Group::Shard,
        Group::Expire,
    ] {
        assert!(
            ROWS.iter().any(|r| r.group == group),
            "{group:?} 组一格都没有"
        );
    }
}

// ── 第 2 节：每一格一个测试（测试名以 id 开头，spec 表按它检索）──

#[test]
fn a01_starting_from_the_runtime_inside_the_starting_window() {
    // 关掉 `process_layer` 里 `spawn_age < STARTING_WINDOW_SECS` 那一判 → 红（报成 RUNNING）。
    check_id("a01");
}

#[test]
fn a02_starting_from_a_hook_inside_the_startup_grace() {
    // 把 startup_grace 调成 0（或删掉 observe_hooked 里那条衰减的门）→ 这一格与 a03 一起红。
    check_id("a02");
}

#[test]
fn a03_hook_starting_never_outlives_the_startup_grace() {
    // 关掉 observe_hooked 的 decay_starting → 每一格都钉在 Starting 而红。现行守卫另有
    // `tests/state_machine.rs::hook_starting_decays_to_turn_done_awaiting_first_prompt`。
    check_id("a03");
}

#[test]
fn a04_running_from_the_process_layer() {
    check_id("a04");
}

#[test]
fn a05_running_from_a_hook() {
    check_id("a05");
}

#[test]
fn a06_waiting_from_a_hook() {
    // 挂起的权限：进程号活着，答它的窗口开着（respond_via 由 `pending_decision` 决定）。
    check_id("a06");
}

#[test]
fn a07_text_cannot_raise_a_hooked_row_to_waiting() {
    // 关掉 observe_hooked 的"hook 写过状态就原样返回"→ 红。
    // `tests/state_machine.rs::text_cannot_raise_hooked_session` 是同一条规则的另一份。
    check_id("a07");
}

#[test]
fn a08_turn_done_from_a_hook() {
    check_id("a08");
}

#[test]
fn a09_neither_text_nor_activity_can_produce_turn_done() {
    check_id("a09");
}

#[test]
fn a10_idle_from_activity_when_no_hook_ever_spoke() {
    // 关掉 observe_unhooked 的 idle_after 分支 → 红。第一次看到输出只记时刻、不追溯（agora-385）。
    check_id("a10");
}

#[test]
fn a11_activity_never_produces_idle_for_a_hooked_row() {
    // 关掉 observe_hooked 的"hook 写过状态就返回"→ 61 s 那一 tick 会拿活动层的 IDLE 覆盖 → 红。
    check_id("a11");
}

#[test]
fn a12_finished_from_the_process_layer() {
    // 关掉 `process_layer` 的 Code(0) → Finished 分支 → 红；按过 Kill 的 143 经壳包装也算。
    check_id("a12");
}

#[test]
fn a13_finished_when_the_runtime_session_is_gone() {
    // 关掉 `runtime_gone` 的 Status::Finished（改回 unknown）→ 三格都红。视图那一段的等价性：
    // `tests/session_manager.rs::missing_runtime_session_finishes_the_row_and_writes_ended_at_once`、
    // `::server_gone_and_session_gone_are_told_apart_in_the_reason`。
    check_id("a13");
}

#[test]
fn a14_a_host_end_seen_before_the_process_exit() {
    // ◐ 的那一瞬：宿主先说了结束，进程层还没来得及报退出。关掉 `ProcessState::derive` 的第一条规则就红。
    check_id("a14");
}

#[test]
fn a15_failed_names_the_exit_that_caused_it() {
    // 退出码与信号两种退法都到 FAILED；end_cause 带的是那一个值。关掉 `process_layer` 的
    // `Code(n)` → Failed 分支 → 红。
    check_id("a15");
}

#[test]
fn a16_an_ended_row_never_reports_an_alive_process() {
    // 表里的 ✗ 行：结束了却报 process=alive 的形状，靠 derive 的第一条规则挡住（对话结束即不再谈进程）。
    check_id("a16");
}

#[test]
fn a17_an_unreadable_runtime_is_unknown_not_gone() {
    // ADR-001 D7：读不到 ≠ 已死。关掉 derive 的 runtime_fact_missing 分支就报出一条假的 gone。
    // 视图那一段：`tests/session_manager.rs::degraded_runtime_never_becomes_runtime_gone`。
    check_id("a17");
}

#[test]
fn a18_a_screen_only_unknown_still_reports_an_alive_process() {
    // 喂法与 u02 / u03 同一条（不复制状态机序列）：屏幕给的 UNKNOWN 两条规则都在"pane 活着"下才采得到。
    check_id("a18");
}

#[test]
fn a19_a_missing_runtime_session_is_not_unknown() {
    // 旧版这一格是 UNKNOWN `runtime session missing`：每一行钉在 ? 没有出口（agora-u5p）。
    check_id("a19");
}

#[test]
fn a20_the_hook_layer_never_writes_unknown() {
    // 把每一类 hook 事件各喂一次，每一个落点都不许是 UNKNOWN × hook（无句柄沉默那格在第 3 节）。
    check_id("a20");
}

#[test]
fn a21_text_raises_waiting_only_on_a_row_without_hooks() {
    // 关掉 `observe_unhooked` 的 text_waiting 分支 → 红。另一半（有 hook 抬不起来）在 a07，现行守卫另有
    // `tests/state_machine.rs::text_waiting_needs_two_consecutive_ticks`。
    check_id("a21");
}

#[test]
fn a22_a_missing_exit_status_is_unknown_not_a_guess() {
    // 关掉 `process_layer` 的 `None =>` 分支（改成 Code(0)）→ 第一格红；出口在表里（后两格）。
    check_id("a22");
}

#[test]
fn a23_a_row_the_runtime_has_not_reported_yet_is_not_gone() {
    // 把 view() 里那条 `!starting` 豁免拆掉，每次起会话都会先给自己写一个 ended_at 再报 FINISHED
    // ——视图那一侧红在 `tests/session_manager.rs::starting_window_exempts_a_row_that_is_still_starting`。
    check_id("a23");
}

#[test]
fn a24_a_handle_reports_idle_at_registration_and_stays_idle() {
    check_id("a24");
}

// ── 第 3 节：每一格一个测试 ──

#[test]
fn x01_handleless_starting_inside_the_grace() {
    check_id("x01");
}

#[test]
fn x02_handleless_starting_never_outlives_the_startup_grace() {
    // 关掉 observe 第 1 步 external 分支里的 decay_starting（agora-rkl 那一处）→ 四格全红。
    // 现场：zuan ef0e50 钉在 starting 180 h、a3a2a0 10 h，Mac db5d48 2 d、4aa42b 15 h。
    check_id("x02");
}

#[test]
fn x03_hook_states_stay_put_while_the_handle_is_alive() {
    check_id("x03");
}

#[test]
fn x04_hook_states_stay_put_without_a_handle() {
    // Codex Desktop 那一格：进程号不可信，行照旧只由 hook 说话（盘点里那 7 行 turn_done + alive:false）。
    check_id("x04");
}

#[test]
fn x05_a_gone_handle_ends_the_row_it_cannot_leave_it_working() {
    check_id("x05");
}

#[test]
fn x06_no_activity_layer_means_no_idle() {
    // 关掉第 1 步那个提前返回（让 external 行走 observe_unhooked）→ 61 s 那一 tick 会落 IDLE 而红。
    check_id("x06");
}

#[test]
fn x07_a_silent_row_ends_when_its_process_goes() {
    // 一个事件都没来、只剩探活：这是 external 行唯一一种"没有宿主说法"的结束，通知按它弹一次。
    check_id("x07");
}

#[test]
fn x08_a_row_ended_by_its_host_or_by_a_new_conversation_reports_gone() {
    // 盘点 B1 的那 10 行：`process` 一律 gone（Q4），hook 的 reason / end_cause / 起点留着（agora-rzh）。
    // 视图那一段：`tests/hooks_external.rs::hook_session_end_survives_the_agent_process_going_away`、
    // `::a_new_conversation_on_the_same_process_supersedes_the_old_external_row`。
    check_id("x08");
}

#[test]
fn x09_no_exit_code_means_no_failed() {
    // 无句柄行拿不到退出码：结束只有 FINISHED 一种，也不许退化成 UNKNOWN 永远占着列表。
    check_id("x09");
}

#[test]
fn x10_a_silent_handleless_row_falls_to_unknown_on_the_event_clock() {
    // 沉默时钟按**事件自己的时刻**算（agora-5gg.2）：停机 3.5 天 + 重放不能把"沉默了多久"清零。
    // ◐ 的另一半出口在 `tests/external_expiry.rs::handleless_unknown_external_rows_expire_and_emit_session_removed`、
    // `tests/hooks_external.rs::a_headless_session_registers_as_headless_and_expires_whatever_its_status_is`。
    check_id("x10");
}

#[test]
fn x11_a_handle_or_a_spoken_hook_rules_out_unknown() {
    // 关掉 `obs.liveness == Liveness::Unknown` 那个条件 → 第一格红（Alive 的行也被打成 UNKNOWN）。
    check_id("x11");
}

#[test]
fn x12_no_observation_yet_leaves_as_soon_as_a_hook_speaks() {
    // ◐：只在检查点恢复 / 重放完成前的一瞬间合法；持续出现 = 检查点丢了，属 bug。
    check_id("x12");
}

#[test]
fn x13_headless_shares_every_cell_of_the_handleless_table() {
    // 新增一档 origin 时这里编译不过：先回答它有没有句柄，再决定它进第 2 节还是第 3 节。
    check_id("x13");
}

#[test]
fn x14_a_host_reporting_idle_at_registration_lands_on_idle() {
    // 登记即空闲（pi 在 `session_start` 里报 `ctx.isIdle()`，agora-wmrq）；乱序的登记不许把在跑的行降下来。
    check_id("x14");
}

// ── 四道老守卫（名字与验收点保留；现在是同一张表的四个组）──

#[test]
fn external_starting_decays_to_turn_done() {
    // 原 agora-rkl 那张表（s01…s06）：每一档都在 x01 / x02 / x10 三格里，只是喂法按 liveness × at
    // 摊开。关掉 observe 第 1 步 external 分支里的 decay_starting → 两行 TurnDone 断言红；关掉同一步里的
    // 沉默兜底（agora-tql 那条）→ 最后一档从 Unknown 变 TurnDone 红。
    check_group(Group::Starting);
}

#[test]
fn external_starting_is_never_a_dead_end() {
    // STARTING 不许是没有出口的筐（epic agora-5gg 的立论）：进程号活着与不知道的两档 external 行，
    // 衰减之后要么离开 STARTING、要么被进程事实说结束，不会自己再回去。
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
    let a = m.observe(Observation {
        process: agora::status::external_process_gone(),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 3600,
        at: None,
    });
    assert_eq!(
        (a.status, a.source),
        (Status::Finished, Source::Process),
        "{a:?}"
    );
    assert_eq!(
        end_tag(&a, "external-starting-exit"),
        "process_gone",
        "{a:?}"
    );
}

#[test]
fn runtime_session_gone_is_a_fact_with_an_exit_not_a_dead_end() {
    // 原 agora-u5p 那张表（g01…g04）：每一档都从 RUNNING 起步；前三行落 a13、降级那行落 a17。
    // 关掉 status::runtime_gone 的 Status::Finished（改回 unknown）→ 前三行的 Finished 红。
    check_group(Group::Gone);
}

#[test]
fn runtime_session_gone_does_not_outvote_a_hook_that_spoke_first() {
    // 0.8 不是 1.0（agora-rzh 同一条理由）：宿主自己说了 `session ended (hook)` 的行，后来的
    // "会话没了"只把进程事实带上行，不抢 hook 的说法。把 conf 抬到 1.0 → end_cause 被抢成
    // `runtime_gone`（`Machine::process_fact_is_no_better` 不再成立）。
    let mut m = Machine::new(cfg(), true, 1, 0);
    m.apply(&AgoraEvent::SessionStarted, 1, 0);
    m.observe(Observation {
        process: Assessment::new(Status::Running, Source::Process, 1.0, None),
        liveness: Liveness::Alive,
        text: None,
        runtime: None,
        epoch: 1,
        now: 30,
        at: None,
    });
    m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 40);
    let a = m.observe(Observation {
        process: gone_fact(RuntimeGone::Session, false),
        liveness: Liveness::Dead,
        text: None,
        runtime: None,
        epoch: 1,
        now: 60,
        at: None,
    });
    assert_eq!(a.status, Status::Finished, "{a:?}");
    assert_eq!(a.source, Source::Hook, "{a:?}");
    assert_eq!(
        end_tag(&a, "not-outvoted"),
        "host_session_end:other",
        "{a:?}"
    );
}

#[test]
fn every_finished_and_failed_row_names_its_end_cause() {
    // 原 agora-5gg.6 那张表（e01…e19）：每一档都断言 `end_cause` 是封闭集合里的一个值。
    // 关掉任何一处 `.with_end(..)` → 那一档红；把 `HostEndReason::from_host` 的表改窄 → 那一档红。
    check_group(Group::End);
}

#[test]
fn a_hook_that_spoke_first_keeps_its_end_cause_when_the_process_agrees() {
    // 同状态、同把握的进程事实不抢 hook 的说法（agora-rzh）。这条规则现在也要保住 end_cause：
    // 人在终端里自己退出的那一行，后来的"进程没了"只把 process 三态换成 gone，不许把
    // host_session_end 换成 process_gone —— 换成 process_gone 之后 external 行会开始弹通知。
    // 关掉 observe 里的 process_fact_is_no_better → 两条断言都红（source 与 end_cause 一起被抢）。
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
        at: None,
    });
    assert_eq!(a.status, Status::Finished, "{a:?}");
    assert_eq!(a.source, Source::Hook, "{a:?}");
    assert_eq!(end_tag(&a, "hook-first"), "host_session_end:exit", "{a:?}");
}

#[test]
fn every_unknown_row_names_why_it_is_unknown() {
    // 原 agora-5gg.6 那张表（u01…u06）：`unknown_cause` 是封闭集合里的一个值。
    // 关掉任何一处 `.with_unknown(..)` → 那一档红；新值没进 UNKNOWN_CAUSES 会被成员表拦下。
    check_group(Group::Unknown);
}

#[test]
fn shard_process_assertions_are_all_in_the_table() {
    // 并入 agora-3lc：process 相关的分片断言（原 src/status/mod.rs 的四条单测与 api 侧投影那批）
    // 现在也在这张表里逐档断言（p01…p04）。线形态 p04 是 Direct：词表锁定不看状态机落点。
    check_group(Group::Shard);
}

#[test]
fn expire_arms_are_shapes_in_the_table() {
    // q01…q08 是 expire_external_finished 三条 arm 吃的状态形状 + 原因枚举；真跑删行见文件尾的
    // `expire_arms_and_the_local_seen_anchor_are_pinned`（Db 行，单独一个测试）。
    check_group(Group::Expire);
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
    assert_eq!(
        serde_json::to_value(EndCause::CheckpointUnrecorded).unwrap(),
        serde_json::json!({ "kind": "checkpoint_unrecorded" })
    );
    for reason in ["clear", "resume", "logout", "exit", "other"] {
        let v = serde_json::to_value(EndCause::HostSessionEnd(HostEndReason::from_host(Some(
            reason,
        ))))
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
    let none: Assessment = serde_json::from_value(serde_json::json!({
        "status": "finished", "source": "hook", "confidence": 0.8, "reason": "session ended (hook)"
    }))
    .unwrap();
    assert_eq!(none.end_cause, None);
    assert_eq!(none.unknown_cause, None);
}

// ───────────────────────── 表与守卫的对账 ─────────────────────────

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
/// 不像路径的那一段（`Machine::decay_starting`、`ProcessState::derive` 那一类）不是引用，跳过。
fn guard_refs(cell: &str, default_file: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut last_file = default_file.to_owned();
    for span in cell.split('`').skip(1).step_by(2) {
        let Some((file, test)) = span.split_once("::") else {
            continue;
        };
        if !file.contains('/') && !file.starts_with('…') {
            continue;
        }
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

fn spec_rows() -> Vec<&'static Row> {
    ROWS.iter().filter(|r| r.spec.is_some()).collect()
}

#[test]
fn spec_rows_and_code_rows_agree() {
    // 这张表之所以是"守卫"而不是"文档"：本文件里的每一格与 docs/spec/status.md 表里的每一格
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
        let check = |file: &str, test: &str| {
            let src = std::fs::read_to_string(repo_file(file))
                .unwrap_or_else(|e| panic!("{id} 引用的文件不存在: {file}（{e}）"));
            assert!(
                src.contains(&format!("fn {test}(")) || src.contains(&format!("async fn {test}(")),
                "{id} 点名的测试不存在: {file}::{test}"
            );
        };
        for (file, test) in &refs {
            check(file, test);
        }
        // "含义"列里点名的也是引用：写错的测试名当场红，而不是留给下一个人去 grep。
        for (file, test) in guard_refs(&cells_row[5], "tests/status_truth_table.rs") {
            check(&file, &test);
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
    // 每一格都能在测试名里检索到。
    for id in &ids {
        assert!(
            me.contains(&format!("fn {id}_")),
            "表里的 {id} 在本文件里没有以它为前缀的测试（表里每一行要在测试名里可检索）"
        );
    }
    for r in spec_rows() {
        let spec = r.spec();
        let Some(cell) = cells.get(r.id) else {
            panic!(
                "ROWS 里有 {}，docs/spec/status.md 的表里没有：删一格要连守卫一起删",
                r.id
            );
        };
        assert_eq!(
            cell,
            &spec.cell.replace("\\|", "|"),
            "{} 的三列与表里不一致（表里改了、测试里没改，或反之）",
            r.id
        );
        assert_eq!(
            verdicts.get(r.id).map(String::as_str),
            Some(spec.verdict.symbol()),
            "{} 的判定符号与测试里的 verdict 不一致：表里 {}，测试里 {}",
            r.id,
            verdicts.get(r.id).map(String::as_str).unwrap_or_default(),
            spec.verdict.symbol()
        );
    }
}

// ═══════════════════ 表的闭合性：喂得出的落点必须在表里 ═══════════════════
//
// `spec_rows_and_code_rows_agree` 查的是"表与守卫是不是同一份"，这一节查反方向：状态机真喂得
// 出来的落点，有没有在表上没人认领的。做法是把喂法摊成一个固定的集合（15 类 hook 事件 × 屏幕证据
// × 进程事实 × 有无 hook × 进程号三种状态），把每一个 tick 的落点拿去查同一档（有句柄查第 2 节、
// 无句柄查第 3 节）的表：找不到一格就红。
//
// 只查"落点有人认领"，不把 ✗ 行拿来当场判红：✗ 那一格可能与 ✓ 那一格三列一模一样、只差前置条件
// （a19 与 a23 都是 `UNKNOWN | gone | none`，分别要求"已过"与"还在" STARTING 窗口），三列拆不出
// 这种差别。✗ 行靠自己的反向断言钉（`check_row` 的 Forbid），闭合守卫只保证"喂得出来的东西在表上有
// 一格"。

/// 线长什么样：三列写成调用方看到的字面量（`api.md`「会话形态」那三个字段）。
fn wire_triple(fed: &Fed) -> [String; 3] {
    let w = |v: serde_json::Value| v.as_str().unwrap_or_default().to_owned();
    [
        w(serde_json::to_value(fed.a.status).unwrap()).to_ascii_uppercase(),
        w(serde_json::to_value(fed.process()).unwrap()),
        w(serde_json::to_value(fed.a.source).unwrap()),
    ]
}

/// 把 `status | process | source` 那一列拆成三组取值：` 或 ` 与 ` / ` 都算并列，中文括号与逗号之后
/// 是给人看的说明（不参与匹配），`任何` = 全集。拆不出三列的行（x13「本节每一格」）返回 None，
/// 意思是"这一行不是一张三列的格"，不参与闭合守卫。
fn cell_columns(spec: &str) -> Option<[Vec<String>; 3]> {
    let cols: Vec<Vec<String>> = spec
        .split(" | ")
        .map(|col| {
            let head = col.split('（').next().unwrap_or(col);
            let head = head.split('，').next().unwrap_or(head);
            head.replace(" 或 ", " / ")
                .split(" / ")
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .collect()
        })
        .collect();
    if cols.len() != 3 {
        return None;
    }
    Some([cols[0].clone(), cols[1].clone(), cols[2].clone()])
}

/// 这一格接不接受某个落点。
fn cell_accepts(spec: &Spec, trip: &[String; 3]) -> bool {
    let Some(cols) = cell_columns(spec.cell) else {
        return false;
    };
    let hits = |vals: &[String], got: &str| vals.iter().any(|v| v == "任何" || v == got);
    hits(&cols[0], &trip[0]) && hits(&cols[1], &trip[1]) && hits(&cols[2], &trip[2])
}

/// hook 事件：每一类都能单独把一行喂到某个落点，闭合守卫逐类过一遍。
fn hook_events() -> Vec<AgoraEvent> {
    vec![
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
        AgoraEvent::IdleReported,
        AgoraEvent::SessionEnded(Some("other".into())),
        AgoraEvent::SessionEnded(Some("clear".into())),
        AgoraEvent::Superseded,
    ]
}

/// 屏幕证据的四种说法（Adapter 的兜底只会给这几种）。
fn screen_evidence() -> Vec<Option<DetectionResult>> {
    vec![
        None,
        Some(DetectionResult {
            status: Status::Waiting,
            confidence: 0.7,
            reason: "permission prompt".to_owned(),
        }),
        Some(DetectionResult {
            status: Status::TurnDone,
            confidence: 0.7,
            reason: "shell prompt".to_owned(),
        }),
        Some(DetectionResult {
            status: Status::Idle,
            confidence: 0.7,
            reason: "idle".to_owned(),
        }),
        Some(DetectionResult {
            status: Status::Running,
            confidence: 0.7,
            reason: "working".to_owned(),
        }),
    ]
}

/// 进程层能报出的全部事实。第二个值 = 运行时整体读不到（只影响导出的 `process`，不影响结论）。
fn process_facts() -> Vec<(Assessment, bool)> {
    let mut v: Vec<(Assessment, bool)> = vec![];
    for killed in [false, true] {
        for exit in [
            None,
            Some(Exit::Code(0)),
            Some(Exit::Code(3)),
            Some(Exit::Signal("hup".into())),
            Some(Exit::Signal("kill".into())),
        ] {
            v.push((rt_exit(exit, killed), false));
        }
        for gone in [RuntimeGone::Session, RuntimeGone::Server] {
            v.push((gone_fact(gone, killed), false));
        }
        for age in [Some(0u64), Some(3600), None] {
            v.push((agora::status::process_layer(None, age, killed), true));
        }
    }
    v.push((
        agora::status::runtime_unavailable("protocol version mismatch"),
        true,
    ));
    v.push((agora::status::external_process_gone(), false));
    v
}

/// 有句柄的行：第 2 节能喂的形状。
fn probe_handle_rows(v: &mut Vec<Fed>) {
    for declared in [true, false] {
        // ① 一条 hook 事件 + 三个 tick（宽限内 / 沉默阈值之后 / 驻留过期之后），屏幕证据四态。
        for ev in hook_events() {
            for (spawn, now) in [(Some(0u64), 1i64), (Some(3600), 60i64)] {
                for text in screen_evidence() {
                    let mut m = Machine::new(cfg(), declared, 1, 0);
                    m.apply(&ev, 1, 0);
                    v.push(tick_rt(
                        &mut m,
                        &pane(Some(0)),
                        spawn,
                        false,
                        text.as_ref(),
                        now,
                    ));
                    v.push(tick_rt(
                        &mut m,
                        &pane(Some(0)),
                        spawn,
                        false,
                        text.as_ref(),
                        now + 600,
                    ));
                    v.push(tick_rt(
                        &mut m,
                        &pane(Some(0)),
                        spawn,
                        false,
                        None,
                        now + 90,
                    ));
                }
            }
        }
        // ② 每一条进程事实：既喂给"hook 还没说话"的行，也喂给"hook 先说了结束"的行。
        for (fact, runtime_fact_missing) in process_facts() {
            for declared2 in [true, false] {
                v.push(running_then_fact(
                    declared2,
                    fact.clone(),
                    Liveness::Dead,
                    runtime_fact_missing,
                ));
                let mut m = Machine::new(cfg(), true, 1, 0);
                m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
                tick_rt(&mut m, &pane(Some(0)), Some(3600), false, None, 30);
                m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 40);
                let a = m.observe(Observation {
                    process: fact.clone(),
                    liveness: Liveness::Dead,
                    text: None,
                    runtime: None,
                    epoch: 1,
                    now: 60,
                    at: None,
                });
                v.push(Fed {
                    a,
                    liveness: Liveness::Dead,
                    runtime_fact_missing,
                });
            }
        }
        // ③ 活动层：有输出 / 没输出走到 IDLE。
        for text in screen_evidence() {
            let mut m = Machine::new(cfg(), declared, 1, 0);
            v.push(tick_rt(
                &mut m,
                &pane(None),
                Some(3600),
                false,
                text.as_ref(),
                1,
            ));
            v.push(tick_rt(
                &mut m,
                &pane(None),
                Some(3600),
                false,
                text.as_ref(),
                200,
            ));
            v.push(tick_rt(
                &mut m,
                &pane(Some(200)),
                Some(3600),
                false,
                text.as_ref(),
                202,
            ));
        }
        // ④ 挂起的权限：提示在屏幕上、从屏幕上消失、之后被 hook 答掉。
        for on_screen in [true, false] {
            let mut m = Machine::new(cfg(), declared, 1, 0);
            m.apply(&AgoraEvent::PromptSubmitted("go".into()), 1, 0);
            m.apply(
                &AgoraEvent::DecisionNeeded {
                    tool_use_id: "t1".into(),
                    summary: "Bash: sleep 20".into(),
                },
                1,
                1,
            );
            for (i, now) in [3i64, 5, 7, 9].iter().enumerate() {
                let prompt = if on_screen || i < 2 {
                    Some(DetectionResult {
                        status: Status::Waiting,
                        confidence: 0.7,
                        reason: "prompt: Do you want to proceed?".to_owned(),
                    })
                } else {
                    None
                };
                v.push(tick_rt(
                    &mut m,
                    &pane(Some(0)),
                    Some(3600),
                    false,
                    prompt.as_ref(),
                    *now,
                ));
            }
            m.apply(&AgoraEvent::DecisionResolved(Some("t1".into())), 1, 11);
            v.push(tick_rt(&mut m, &pane(Some(0)), Some(3600), false, None, 11));
        }
    }
}

/// 无句柄的行（第 3 节）：进程层恒说"不知道"，只有 hook 与探活能说话。
fn probe_handleless_rows(v: &mut Vec<Fed>) {
    for ev in hook_events() {
        for lv in [Liveness::Alive, Liveness::Unknown, Liveness::Dead] {
            for at in [1i64, 11, 3600, 7201] {
                let mut m = Machine::new(cfg(), true, 1, 0);
                m.apply(&ev, 1, 0);
                v.push(tick_external(&mut m, lv, at));
                v.push(tick_external(&mut m, lv, at + 600));
            }
        }
    }
    for lv in [Liveness::Alive, Liveness::Unknown, Liveness::Dead] {
        // 一条 hook 事件都没有的行（检查点刚恢复）；与沉默满 2 h 的那一格。
        let mut m = Machine::new(cfg(), true, 1, 0);
        v.push(tick_external(&mut m, lv, 1));
        v.push(tick_external(&mut m, lv, 7201));
        m.apply(&AgoraEvent::TurnEnded(Some("done".into())), 1, 7202);
        v.push(tick_external(&mut m, lv, 7202));
        // 宿主说了结束之后再喂沉默：不许被沉默兜底捞回 UNKNOWN。
        m.apply(&AgoraEvent::SessionEnded(Some("other".into())), 1, 7203);
        v.push(tick_external(&mut m, lv, 20000));
    }
}

#[test]
fn every_reachable_cell_is_in_the_table() {
    let mut handle: Vec<Fed> = Vec::new();
    let mut handleless: Vec<Fed> = Vec::new();
    probe_handle_rows(&mut handle);
    probe_handleless_rows(&mut handleless);
    assert!(
        handle.len() > 100,
        "喂法太少（{}），闭合守卫会退化成空话",
        handle.len()
    );
    assert!(handleless.len() > 50, "喂法太少（{}）", handleless.len());

    for (prefix, section, fed) in [
        ("a", "第 2 节（有运行时句柄）", &handle),
        ("x", "第 3 节（无运行时句柄）", &handleless),
    ] {
        for f in fed {
            assert_cause_matches_status(&f.a, "闭合守卫");
            let trip = wire_triple(f);
            let claimed = ROWS.iter().any(|r| {
                r.id.starts_with(prefix)
                    && r.spec.as_ref().is_some_and(|s| s.verdict != Verdict::Never)
                    && cell_accepts(r.spec(), &trip)
            });
            assert!(
                claimed,
                "状态机在{section}喂出表上没有的一格 {} | {} | {}（reason {:?}、cause {:?}）：\
                 要么这一格该进 docs/spec/status.md 与 ROWS，要么这个落点是 bug",
                trip[0],
                trip[1],
                trip[2],
                f.a.reason.as_deref().unwrap_or_default(),
                f.a.unknown_cause,
            );
        }
    }
}

// ═════════ 破例的一个 Db 测试：expire 三 arm × 两钟 × 两 ttl × 91vy 锚两态 ═════════
//
// 文件头立了"真值表不碰 Db"的纪律（agora-uhdf），这一条是 agora-fhez 点名的收口内容：
// expire_external_finished 的三条 arm × 两只时钟 × 两个 ttl 的门，以及 agora-91vy 之后
// `local_seen_at` 锚的存在 / 缺失两态——它们决定探活判死还是判活，从而决定 arm 1 看不看得见这一行。
// 只用 `Db::open_in_memory()` / 临时目录里的 sqlite + `common::FakeRuntime`：不开 daemon、
// 不起 tmux、不碰真会话，毫秒级仍是主表的事。

fn expire_mgr(finished_secs: i64, unknown_secs: i64) -> (SessionManager, Arc<Db>) {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let m = SessionManager::new(Arc::clone(&db), rt as Arc<dyn Runtime>)
        .with_external_finished_ttl(Duration::from_secs(finished_secs as u64))
        .with_external_unknown_ttl(Duration::from_secs(unknown_secs as u64));
    (m, db)
}

fn register_external_row(m: &SessionManager, agent_session: &str, origin: Origin) -> String {
    m.register_external(&ExternalSession {
        agent_type: "claude".into(),
        agent_session_id: agent_session.into(),
        runtime_ref: None,
        working_directory: Some(PathBuf::from("/work/agora")),
        created_at: None,
        origin,
    })
    .unwrap()
}

/// 直接改库里的 ended_at（把两只时钟拆开用；`None` = 清成老库行的样子）。
fn set_ended_at(db: &Db, id: &str, value: Option<&str>) {
    let n = db
        .conn()
        .execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
            rusqlite::params![id, value],
        )
        .unwrap();
    assert_eq!(n, 1, "改行命中：{id}");
}

fn checkpoint_path(home: &std::path::Path, id: &str) -> PathBuf {
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    home.join("hooks/state").join(format!("{key}.json"))
}

fn manager_with_checkpoints(home: &std::path::Path, finished_secs: i64) -> SessionManager {
    let db = Arc::new(Db::open(&home.join("agora.db")).unwrap());
    let rt = Arc::new(common::FakeRuntime::default());
    let m = SessionManager::new(db, rt as Arc<dyn Runtime>)
        .with_external_finished_ttl(Duration::from_secs(finished_secs as u64));
    m.enable_hook_checkpoints(home);
    m
}

#[test]
fn expire_arms_and_the_local_seen_anchor_are_pinned() {
    let now = clock::now_secs();
    let day = 86_400;

    // —— arm 1：external FINISHED，时钟 = ended_at；ended_at 缺失的旧库行退回 status_since ——
    let (m, db) = expire_mgr(day, day);
    let ended = register_external_row(&m, "arm1-ended", Origin::External);
    m.apply_hook(&ended, 1, &[AgoraEvent::SessionEnded(Some("other".into()))])
        .unwrap();
    // ended_at 25 h 前、status_since 刚刚：到期读 ended_at，删。
    set_ended_at(&db, &ended, Some(&clock::format_utc_secs(now - 25 * 3600)));
    // 旧库行（本次改动之前入库的 FINISHED external）：ended_at 空，只能拿 status_since 算。
    let fallback = register_external_row(&m, "arm1-fallback", Origin::External);
    m.apply_hook_at(
        &fallback,
        1,
        &[AgoraEvent::SessionEnded(Some("other".into()))],
        Some(now - 25 * 3600),
    )
    .unwrap();
    set_ended_at(&db, &fallback, None);
    // 对照：刚结束的行不动（ended_at 是现在）。
    let fresh = register_external_row(&m, "arm1-fresh", Origin::External);
    m.apply_hook(&fresh, 1, &[AgoraEvent::SessionEnded(Some("other".into()))])
        .unwrap();
    let mut removed = m.expire_external_finished(now).unwrap();
    removed.sort();
    let mut want = vec![ended.clone(), fallback.clone()];
    want.sort();
    assert_eq!(removed, want, "arm 1 按 ended_at，缺失时退回 status_since");
    assert!(m.get(&fresh).is_ok(), "刚结束的行不该到期");

    // —— arm 2：external UNKNOWN `hooks_silent; no process handle`，时钟 = status_since ——
    let (m, _db) = expire_mgr(day, day);
    let silent = register_external_row(&m, "arm2-silent", Origin::External);
    // 事件 30 h 前：越过沉默阈值（默认 2 h）那一刻也早过 24 h。
    m.apply_hook_at(
        &silent,
        1,
        &[AgoraEvent::TurnEnded(Some("done".into()))],
        Some(now - 30 * 3600),
    )
    .unwrap();
    let v = m.get(&silent).unwrap();
    assert_eq!(v.assessment.status, Status::Unknown, "{:?}", v.assessment);
    assert_eq!(
        v.assessment.unknown_cause,
        Some(UnknownCause::HooksSilentNoHandle),
        "{:?}",
        v.assessment
    );
    // 不认的一档：UNKNOWN「没有观测」不归 arm 2（status_since 是刚刚，也不到时）。
    let unhooked = register_external_row(&m, "arm2-unhooked", Origin::External);
    assert_eq!(m.get(&unhooked).unwrap().assessment.status, Status::Unknown);
    assert_eq!(
        m.expire_external_finished(now).unwrap(),
        vec![silent.clone()]
    );
    assert!(m.get(&unhooked).is_ok(), "没有观测的 UNKNOWN 不归 arm 2");

    // —— arm 3：headless 不论状态，按 finished_ttl；UNKNOWN 那档在 unknown_ttl=0 时由它兜住 ——
    let (m, _db) = expire_mgr(day, 0);
    let head_busy = register_external_row(&m, "arm3-head-turn-done", Origin::Headless);
    m.apply_hook_at(
        &head_busy,
        1,
        &[AgoraEvent::TurnEnded(Some("done".into()))],
        Some(now - 30 * 3600),
    )
    .unwrap();
    let head_silent = register_external_row(&m, "arm3-head-silent", Origin::Headless);
    m.apply_hook_at(
        &head_silent,
        1,
        &[AgoraEvent::TurnEnded(Some("done".into()))],
        Some(now - 30 * 3600),
    )
    .unwrap();
    m.get(&head_silent).unwrap();
    // 对照：external 的 TURN_DONE 与 UNKNOWN 在 unknown_ttl=0 时没有出口，不许被 arm 3 顺手删。
    let ext_busy = register_external_row(&m, "arm3-ext-turn-done", Origin::External);
    m.apply_hook_at(
        &ext_busy,
        1,
        &[AgoraEvent::TurnEnded(Some("done".into()))],
        Some(now - 30 * 3600),
    )
    .unwrap();
    let ext_silent = register_external_row(&m, "arm3-ext-silent", Origin::External);
    m.apply_hook_at(
        &ext_silent,
        1,
        &[AgoraEvent::TurnEnded(Some("done".into()))],
        Some(now - 30 * 3600),
    )
    .unwrap();
    m.get(&ext_silent).unwrap();
    let mut removed = m.expire_external_finished(now).unwrap();
    removed.sort();
    let mut want = vec![head_busy.clone(), head_silent.clone()];
    want.sort();
    assert_eq!(removed, want, "arm 3 只吃 headless，不看状态");
    assert!(m.get(&ext_busy).is_ok(), "external 的 TURN_DONE 没有出口");
    assert!(
        m.get(&ext_silent).is_ok(),
        "unknown_ttl=0 时 external 的沉默 UNKNOWN 也不归 arm 3"
    );

    // —— 两个 ttl 全关：sweep 的门直接早退，什么都不删（agora-j4w.3 的门 / agora-8bfm）——
    let (m, _db) = expire_mgr(0, 0);
    let off = register_external_row(&m, "ttl-off", Origin::External);
    m.apply_hook_at(
        &off,
        1,
        &[AgoraEvent::SessionEnded(Some("other".into()))],
        Some(now - 25 * 3600),
    )
    .unwrap();
    assert!(m.expire_external_finished(now).unwrap().is_empty());
    assert!(m.get(&off).is_ok(), "ttl 关了就不该删");

    // —— 91vy：`local_seen_at` 锚的存在 / 缺失两态，决定 arm 1 看不看得见这一行 ——
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new("sleep").arg("300").spawn().unwrap();
    let id = {
        let m = manager_with_checkpoints(home.path(), day);
        let id = register_external_row(&m, "anchor-row", Origin::External);
        // 登记信封报来的号：锚 = daemon 本地第一次见到它的时刻（本机钟），91vy。
        m.note_external_pid(&id, child.id(), clock::now_secs() * 1000);
        m.apply_hook(&id, 1, &[AgoraEvent::TurnEnded(Some("done".into()))])
            .unwrap();
        id
    };
    let path = checkpoint_path(home.path(), &id);
    let cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let started = cp["agent_process"]["started_at"]
        .as_i64()
        .expect("本机能读到进程启动时刻");
    assert!(
        cp["agent_process"]["local_seen_at"].is_i64(),
        "登记时写下本机钟锚: {cp}"
    );

    // 态 A（升级前的检查点：没有锚）：跳过"号被复用"判据 → 号还在 → 行留在 TURN_DONE × alive，
    // 没有 ended_at，arm 1 够不着它。
    let mut cp = cp.clone();
    cp["agent_process"]
        .as_object_mut()
        .unwrap()
        .remove("local_seen_at");
    std::fs::write(&path, cp.to_string()).unwrap();
    let m = manager_with_checkpoints(home.path(), day);
    m.restore_hook_checkpoints().unwrap();
    let v = m.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::TurnDone, Source::Hook),
        "{:?}",
        v.assessment
    );
    assert_eq!(v.process, ProcessState::Alive, "{:?}", v.assessment);
    assert!(v.record.ended_at.is_none(), "活着的行不许有结束时刻");
    assert!(
        m.expire_external_finished(clock::now_secs() + 25 * 3600)
            .unwrap()
            .is_empty(),
        "arm 1 看不见活着的行"
    );

    // 态 B（锚在、号比锚新 = 号被复用）：探活判死 → FINISHED process_gone / 近似 ended_at →
    // arm 1 删它。
    let mut cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cp["agent_process"]["local_seen_at"] = serde_json::json!(started - 60);
    std::fs::write(&path, cp.to_string()).unwrap();
    let m = manager_with_checkpoints(home.path(), day);
    m.restore_hook_checkpoints().unwrap();
    let v = m.get(&id).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    assert_eq!(
        v.assessment.end_cause,
        Some(EndCause::ProcessGone),
        "{:?}",
        v.assessment
    );
    assert!(v.record.ended_at.is_some(), "探到号的复用要写近似 ended_at");
    assert_eq!(
        m.expire_external_finished(clock::now_secs() + 25 * 3600)
            .unwrap(),
        vec![id.clone()],
        "arm 1 看见判死的行并按 ended_at 删"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}
