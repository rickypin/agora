//! external 行的自动过期。侧栏 58 行里 39 行是 external 且 FINISHED（2026-09-08 现场），它们
//! 没有运行时会话与输出，只剩 agora 的两行记录；结束超过 `sessions.external_finished_ttl` 就删
//! metadata（MISSION §4.6，agora-j4w.3）。agora / adopted 行有 scrollback，不动。
//!
//! agora-e08 补第二条出口：无可信进程号的 external 行沉默 2 h 落 UNKNOWN
//! `hooks silent; no process handle` 之后，只有下一条 hook 事件能让它离开，而进程多半早不在。
//! 一行 agora 既观察不到也无法操作、还沉默了一天，对使用者价值为零，所以这一格 UNKNOWN 必须是
//! 暂态：持续 ≥ `sessions.external_unknown_ttl` 走同一条 DELETE 路径自动删。

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::collections::BTreeMap;

use agora::clock::{self, now_secs};
use agora::events::{Differ, Event};
use agora::hook::{Delivery, Envelope, Inbox, Receiver};
use agora::runtime::{Exit, Runtime, Size};
use agora::session::{Db, ExternalSession, NewSession, Origin, SessionManager};
use agora::status::{AgoraEvent, MachineConfig, Source, Status};
use common::FakeRuntime;
use serde_json::json;

const DAY: i64 = 86_400;

fn mgr(ttl: Duration) -> (SessionManager, Arc<FakeRuntime>, Arc<Db>) {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(Arc::clone(&db), rt.clone() as Arc<dyn Runtime>)
        .with_external_finished_ttl(ttl);
    (m, rt, db)
}

/// 无句柄的 external 行（Terminal.app 里裸跑的 agent，只有 hook 看得见）。
fn external(m: &SessionManager, agent_session: &str) -> String {
    m.register_external(&ExternalSession {
        agent_type: "claude".into(),
        agent_session_id: agent_session.into(),
        runtime_ref: None,
        working_directory: Some(PathBuf::from("/work/agora")),
        created_at: None,
        origin: Origin::External,
    })
    .unwrap()
}

/// agora 自己起的行，进程已退出 → FINISHED(process)，有运行时会话与 scrollback。
fn agora_finished(m: &SessionManager, rt: &FakeRuntime, db: &Db, name: &str) -> String {
    let v = m
        .create(&NewSession {
            display_name: name.into(),
            agent_type: "shell".into(),
            working_directory: PathBuf::from("/tmp"),
            worktree: None,
            task_ref: None,
            command: "true".into(),
            env: vec![],
            size: Size::default(),
        })
        .unwrap();
    db.conn()
        .execute(
            "UPDATE sessions SET spawned_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            [&v.record.id],
        )
        .unwrap();
    rt.set_dead(v.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));
    v.record.id
}

/// 一个已经退出的进程号：external 行"进程消失"那一档的 FINISHED。
fn gone_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn status_of(m: &SessionManager, id: &str) -> Status {
    m.get(id).unwrap().assessment.status
}

#[test]
fn expired_external_finished_rows_are_deleted_and_the_rest_stay() {
    // 守卫：关掉 expire_external_finished 里的 origin 判断 → agora 行也被删；关掉 status 判断 →
    // TURN_DONE 行也被删；ttl 比较写反 → 同龄的行全删或全留。
    let (m, rt, db) = mgr(Duration::from_secs(DAY as u64));
    let by_hook = external(&m, "conv-hook");
    m.apply_hook(
        &by_hook,
        1,
        &[AgoraEvent::SessionEnded(Some("prompt_input_exit".into()))],
    )
    .unwrap();
    let superseded = external(&m, "conv-old");
    m.apply_hook(&superseded, 1, &[AgoraEvent::Superseded])
        .unwrap();
    let process_gone = external(&m, "conv-gone");
    m.note_external_pid(&process_gone, gone_pid(), now_secs());
    let turn_done = external(&m, "conv-busy");
    m.apply_hook(&turn_done, 1, &[AgoraEvent::TurnEnded(Some("done".into()))])
        .unwrap();
    let agora_row = agora_finished(&m, &rt, &db, "mine");

    assert_eq!(status_of(&m, &by_hook), Status::Finished);
    assert_eq!(status_of(&m, &superseded), Status::Finished);
    assert_eq!(status_of(&m, &process_gone), Status::Finished);
    assert_eq!(status_of(&m, &turn_done), Status::TurnDone);
    assert_eq!(status_of(&m, &agora_row), Status::Finished);
    assert_eq!(m.get(&agora_row).unwrap().record.origin, Origin::Agora);

    // 三种结束各写各的 `ended_at`（agora-5gg.3：Mac 2026-09-18 实测库里 35 行 FINISHED 的
    // external 行 ended_at 全空，按结束时间的保留 / 折叠 / 淘汰没有依据）。过期扫描从这里开始
    // 拿它当时钟，所以这三行不写就等于这三行永不到期（只会每轮回退到 status_since）。
    for id in [&by_hook, &superseded, &process_gone] {
        let rec = m.record(id).unwrap();
        assert!(
            rec.ended_at.is_some(),
            "{id} 结束了却没写 ended_at：{rec:?}"
        );
    }
    assert!(
        m.record(&turn_done).unwrap().ended_at.is_none(),
        "还在干活的行没有结束时刻"
    );

    let mut differ = Differ::default();
    assert!(
        differ.step("n", &m.list().unwrap()).is_empty(),
        "第一轮只建基线"
    );

    // 还没到期：一小时后什么都不删。
    assert!(m.sweep(now_secs() + 3600).unwrap().is_empty());
    assert_eq!(m.list().unwrap().len(), 5);

    // 25 小时后：三条 external FINISHED 行（hook 结束 / superseded / 进程消失）都到期。
    let mut removed = m.expire_external_finished(now_secs() + 25 * 3600).unwrap();
    removed.sort();
    let mut expected = vec![by_hook.clone(), superseded.clone(), process_gone.clone()];
    expected.sort();
    assert_eq!(removed, expected);

    let left: Vec<String> = m.list().unwrap().into_iter().map(|v| v.record.id).collect();
    assert!(
        left.contains(&turn_done),
        "external 但没结束的行不动：{left:?}"
    );
    assert!(
        left.contains(&agora_row),
        "agora 起的 FINISHED 行不动：{left:?}"
    );
    assert_eq!(left.len(), 2, "{left:?}");

    // 求差器在下一轮发 session_removed——与 DELETE /api/sessions/:id 之后客户端看到的同一条事件。
    let events = differ.step("n", &m.list().unwrap());
    let mut gone: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::SessionRemoved { id } => Some(id.clone()),
            _ => None,
        })
        .collect();
    gone.sort();
    let mut expected: Vec<String> = expected.iter().map(|id| format!("n:{id}")).collect();
    expected.sort();
    assert_eq!(gone, expected, "{events:?}");
}

#[test]
fn ttl_zero_turns_expiry_off() {
    let (m, _rt, _db) = mgr(Duration::ZERO);
    let id = external(&m, "conv-hook");
    m.apply_hook(&id, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();
    assert_eq!(status_of(&m, &id), Status::Finished);
    assert!(m.sweep(now_secs() + 400 * DAY).unwrap().is_empty());
    assert!(m
        .expire_external_finished(now_secs() + 400 * DAY)
        .unwrap()
        .is_empty());
    assert_eq!(m.list().unwrap().len(), 1);
}

#[test]
fn sweep_scans_at_most_once_per_period() {
    // 轮询每 2 s 一 tick，扫描一小时一次（ttl 24h）；ttl 更短就按 ttl 扫。守卫：去掉 Throttle →
    // 第二次 sweep 把刚到期的行删了，第二段断言红。
    let (m, _rt, _db) = mgr(Duration::from_secs(DAY as u64));
    let first = external(&m, "conv-1");
    m.apply_hook(&first, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();
    let t0 = now_secs() + 25 * 3600;
    assert_eq!(m.sweep(t0).unwrap(), vec![first.clone()]);

    let second = external(&m, "conv-2");
    m.apply_hook(&second, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();
    // 30 分钟后：second 早就过了 24 h（ended_at 是现在，sweep 的表在 25.5 h 后），但周期未满，不扫。
    assert!(m.sweep(t0 + 1800).unwrap().is_empty(), "周期未满不扫第二次");
    assert_eq!(m.list().unwrap().len(), 1, "行还在");
    assert_eq!(
        m.sweep(t0 + 3600).unwrap(),
        vec![second.clone()],
        "满一小时再扫"
    );

    // ttl = 1m：周期跟着缩到 1 分钟（代检把 ttl 调成 1m 时"不到两个周期就消失"才成立）。
    let (m, _rt, _db) = mgr(Duration::from_secs(60));
    let id = external(&m, "conv-short");
    m.apply_hook(&id, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();
    let t0 = now_secs();
    assert!(m.sweep(t0).unwrap().is_empty(), "刚结束");
    assert!(m.sweep(t0 + 59).unwrap().is_empty(), "周期未满");
    assert_eq!(m.sweep(t0 + 60).unwrap(), vec![id]);
}

#[test]
fn expiry_takes_the_delete_metadata_path_and_drops_the_hook_checkpoint() {
    // 与 DELETE /api/sessions/:id 同一条路径：hook 检查点一起删，hooks/state/ 里不留没人读的孤儿文件。
    let home = tempfile::tempdir().unwrap();
    let (m, _rt, _db) = mgr(Duration::from_secs(DAY as u64));
    m.enable_hook_checkpoints(home.path());
    let id = external(&m, "conv-ckpt");
    m.apply_hook(&id, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();
    assert!(m.has_hook_checkpoint(&id));
    assert_eq!(m.sweep(now_secs() + 25 * 3600).unwrap(), vec![id.clone()]);
    assert!(!m.has_hook_checkpoint(&id), "检查点随 metadata 一起删");
    assert!(m.get(&id).is_err());
}

/// hook 报来的 external 投递（Terminal.app 里裸跑的 claude，CLAUDE_PID 是它的进程号）。
fn external_delivery(
    agent_session: &str,
    pid: u32,
    ms: u64,
    payload: serde_json::Value,
) -> Delivery {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    Delivery {
        envelope: Envelope {
            host: "claude".into(),
            agora_session_id: None,
            agora_epoch: None,
            agent_session_id: agent_session.into(),
            agent_env: BTreeMap::from([("CLAUDE_PID".to_owned(), pid.to_string())]),
            runtime_env: BTreeMap::new(),
            ppid: 1,
            received_at: String::new(),
            received_unix_ms: now_ms + ms,
        },
        payload,
    }
}

fn checkpoint_path(home: &std::path::Path, id: &str) -> std::path::PathBuf {
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    home.join("hooks/state").join(format!("{key}.json"))
}

#[test]
fn hook_finished_external_row_keeps_its_end_time_across_daemon_restart_and_expires() {
    // agora-ec4（2026-09-08 对抗审查）：SessionEnd 之后 claude 进程立刻退出，几乎所有 external FINISHED
    // 行都是「hook 说结束、进程也没了」。daemon 重启重建时进程层报 (Finished, Process) 若盖掉检查点里的
    // (Finished, Hook)，(status, source) 变了 set_at 就重置成重启时刻——开发机一天重启几次 daemon，这些行
    // 永远到不了 24h。守卫：重启后 status_since 仍是 SessionEnd 那一刻（检查点里的 set_at）。
    // 关掉 machine.rs observe 里 process_fact_is_no_better 的判断 → status_since 断言红。
    // agora-5gg.3 改了这条测试的后半：到期不再拿 status_since 当主时钟，拿库里的 ended_at（同一条
    // 纪律的落库版，不依赖检查点恢复）。所以拨表要两处一起拨——现实中两者都是 SessionEnd 那一刻，
    // 只拨检查点就是在测一个现实里到不了的局面。ended_at 单独的红绿见下面
    // `expiry_counts_from_ended_at_not_from_the_status_clock`。
    let home = tempfile::tempdir().unwrap();
    let rt = Arc::new(FakeRuntime::default());
    let ttl = Duration::from_secs(3600);
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let id = {
        let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
        let s = Arc::new(
            SessionManager::new(db, rt.clone() as Arc<dyn Runtime>).with_external_finished_ttl(ttl),
        );
        let r = Receiver::new(home.path(), s.clone());
        let inbox = Inbox::new(home.path());
        let events = [
            json!({"hook_event_name":"SessionStart","session_id":"ext-ec4","cwd":"/work/agora","source":"startup",
            "model": "claude-opus-4-8", "scratchpad_dir": "/tmp/claude-scratch"}),
            json!({"hook_event_name":"Stop","session_id":"ext-ec4","last_assistant_message":"done"}),
            json!({"hook_event_name":"SessionEnd","session_id":"ext-ec4","reason":"prompt_input_exit"}),
        ];
        let mut id = None;
        for (i, ev) in events.into_iter().enumerate() {
            let path = inbox
                .write(&external_delivery("ext-ec4", child.id(), i as u64 + 1, ev))
                .unwrap();
            if let Some(got) = r.ingest(&path).unwrap() {
                id = Some(got.session_key);
            }
        }
        let id = id.unwrap();
        let v = s.get(&id).unwrap();
        assert_eq!(
            (v.assessment.status, v.assessment.source),
            (Status::Finished, Source::Hook),
            "{:?}",
            v.assessment
        );
        // 结束时刻当场就落库（agora-5gg.3：以前这一步是缺的，库里那 35 行就是这么攒出来的）。
        assert!(
            v.record.ended_at.is_some(),
            "SessionEnd 应用完就有 ended_at：{:?}",
            v.record
        );
        inbox.prune_done(Duration::ZERO);
        id
    };
    // 把结束时刻拨回两小时前（真实世界里是 daemon 停了两小时）：检查点里的 set_at 与库里的
    // ended_at 一起拨，再让进程退出。
    let path = checkpoint_path(home.path(), &id);
    let mut cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let ended = cp["set_at"].as_i64().expect("检查点带 set_at") - 2 * 3600;
    cp["set_at"] = json!(ended);
    std::fs::write(&path, cp.to_string()).unwrap();
    let probe = Db::open(&home.path().join("agora.db")).unwrap();
    set_ended_at(&probe, &id, &clock::format_utc_secs(ended));
    drop(probe);
    child.kill().unwrap();
    child.wait().unwrap();

    // 重启：进程层此刻只知道「进程没了」（Finished, Process, 0.8），不比 hook 的 SessionEnd 更有把握，
    // 不许盖掉 hook 的结论，结束的起点仍是 SessionEnd 那一刻。
    let db = Arc::new(Db::open(&home.path().join("agora.db")).unwrap());
    let s =
        Arc::new(SessionManager::new(db, rt as Arc<dyn Runtime>).with_external_finished_ttl(ttl));
    Receiver::new(home.path(), s.clone()).replay().unwrap();
    let v = s.get(&id).unwrap();
    assert_eq!(v.assessment.status, Status::Finished, "{:?}", v.assessment);
    assert_eq!(
        v.status_since, ended,
        "重启后 status_since 必须还是 SessionEnd 那一刻，不是重启时刻"
    );
    // ended_at 是库里的值，本来就不靠检查点：重启不许把它洗成空、也不许被探活的 tick 盖掉
    //（它是准确的，近似值只能覆盖近似值）。
    assert_eq!(
        clock::parse_utc_secs(v.record.ended_at.as_deref().unwrap()),
        Some(ended),
        "重启后 ended_at 仍是 SessionEnd 那一刻：{:?}",
        v.record
    );
    assert!(!v.record.ended_at_approximate, "{:?}", v.record);
    assert_eq!(
        s.sweep(now_secs()).unwrap(),
        vec![id.clone()],
        "两小时前结束、ttl 一小时：第一次 sweep 就删（按 ended_at）"
    );
    assert!(s.get(&id).is_err(), "行已删");
}

#[test]
fn expiry_counts_from_ended_at_not_from_the_status_clock() {
    // agora-5gg.3：external 行的到期以库里的 `ended_at` 为准，`status_since` 退回兜底（本次改动
    // 之前入库的 FINISHED external 行 ended_at 是空的，只能拿旧时钟算）。
    // 现实中这两只表在 hook 结束的那一幕是同一个时刻，所以这里用 SQL 把它们拆开，钉的只是
    // "到期读哪一只"：
    //   ① ended_at 25 h 前、status_since 现在 → 该删。读 status_since → 不删，红。
    //      （现实中这一格是只靠探活结束的行：重启把 status_since 刷回重启时刻，ended_at 不动。）
    //   ② ended_at 现在、status_since 25 h 前 → 该留。读 status_since → 删了没结束多久，红。
    // 改坏：把 expire_external_finished 里的取值换成 status_since 优先 → 两条断言各红一次。
    let (m, _rt, db) = mgr(Duration::from_secs(DAY as u64));
    let now = now_secs();

    // ① 结束于 25 h 前，状态机却是刚刚才知道它结束的。
    let old_end = external(&m, "conv-old-end");
    m.apply_hook(&old_end, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();
    set_ended_at(&db, &old_end, &clock::format_utc_secs(now - 25 * 3600));
    // ② 那条 SessionEnd 是 25 h 前写下、现在才重放到的：状态起点在 25 h 前，结束时刻改成现在。
    let fresh_end = external(&m, "conv-fresh-end");
    m.apply_hook_at(
        &fresh_end,
        1,
        &[AgoraEvent::SessionEnded(None)],
        Some(now - 25 * 3600),
    )
    .unwrap();
    assert_eq!(
        m.get(&fresh_end).unwrap().status_since,
        now - 25 * 3600,
        "apply_hook_at 给的是事件那一刻"
    );
    set_ended_at(&db, &fresh_end, &clock::format_utc_secs(now));

    assert_eq!(status_of(&m, &old_end), Status::Finished);
    assert_eq!(status_of(&m, &fresh_end), Status::Finished);
    assert_eq!(
        m.sweep(now).unwrap(),
        vec![old_end.clone()],
        "到期以 ended_at 为准：① 删、② 留"
    );
    let left: Vec<String> = m.list().unwrap().into_iter().map(|v| v.record.id).collect();
    assert!(left.contains(&fresh_end), "{left:?}");
    assert!(!left.contains(&old_end), "{left:?}");
}

/// 直接改库里的 ended_at（把两只时钟拆开用）。
fn set_ended_at(db: &Db, id: &str, value: &str) {
    let n = db
        .conn()
        .execute(
            "UPDATE sessions SET ended_at = ?2 WHERE id = ?1",
            [&id.to_owned(), &value.to_owned()],
        )
        .unwrap();
    assert_eq!(n, 1, "改行命中：{id}");
}
// ---------- agora-e08：无句柄 external 行的 UNKNOWN 出口 ----------

/// 沉默阈值调成 0：无句柄 external 行只要被看一眼就落进 UNKNOWN
/// `hooks silent; no process handle`，测试不必真等默认的 2 h（`hooks.external_silent_after`）。
fn mgr_silent(unknown_ttl: Duration) -> SessionManager {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    SessionManager::new(db, rt as Arc<dyn Runtime>)
        .with_external_unknown_ttl(unknown_ttl)
        .with_status_config(MachineConfig {
            external_silent_after: Duration::ZERO,
            ..Default::default()
        })
}

/// 一行无句柄 external：hook 报过一轮结束（TURN_DONE），之后彻底沉默 → UNKNOWN。
fn silent_external(m: &SessionManager, agent_session: &str) -> String {
    let id = external(m, agent_session);
    m.apply_hook(&id, 1, &[AgoraEvent::TurnEnded(Some("done".into()))])
        .unwrap();
    let v = m.get(&id).unwrap();
    assert_eq!(
        (v.assessment.status, v.assessment.source),
        (Status::Unknown, Source::Hook),
        "external_silent_after = 0，看一眼就该落 UNKNOWN：{:?}",
        v.assessment
    );
    assert_eq!(
        v.assessment.reason.as_deref(),
        Some("hooks silent; no process handle")
    );
    id
}

/// 一个还活着的进程号（external 行「进程活着」那一档的 `Liveness::Alive`）。
fn alive_pid() -> (u32, std::process::Child) {
    let child = std::process::Command::new("sleep")
        .arg("120")
        .spawn()
        .unwrap();
    (child.id(), child)
}

#[test]
fn handleless_unknown_external_rows_expire_and_emit_session_removed() {
    // 守卫（agora-e08）：删掉 expire_external_finished 里的 `Status::Unknown` 那一 arm（或把
    // reason 判据写成匹配不上）→ 第二段 sweep 红，行永远留着；把 `unknown_ttl > 0` 门槛删了 →
    // `unknown_ttl_zero_turns_that_exit_off` 红（0 被当成「立刻到期」）。
    let home = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(db, rt as Arc<dyn Runtime>)
        // 两条出口同时开着：FINISHED 那条不许被这次改动带跑。
        .with_external_finished_ttl(Duration::from_secs(DAY as u64))
        .with_external_unknown_ttl(Duration::from_secs(DAY as u64))
        .with_status_config(MachineConfig {
            external_silent_after: Duration::ZERO,
            ..Default::default()
        });
    m.enable_hook_checkpoints(home.path());

    let silent = silent_external(&m, "conv-silent");
    // 同样的行、另一个对话 id：验证扫描逐行走，不是「这批全删」。
    let also_silent = silent_external(&m, "conv-silent-2");

    // 这一行 UNKNOWN 但不是 `no process handle`：从没收到过 hook 事件（source = none，
    // `external session: no runtime, hook only`）。它不在本条出口的范围里（真值表里那一格写着
    // 「持续出现 = 检查点丢了，属 bug」，归 agora-5gg.16），到期也不该被顺手删。
    let unhooked = external(&m, "conv-unhooked");
    assert_eq!(m.get(&unhooked).unwrap().assessment.status, Status::Unknown);
    assert_ne!(
        m.get(&unhooked)
            .unwrap()
            .assessment
            .reason
            .as_deref()
            .unwrap(),
        "hooks silent; no process handle"
    );

    // 进程号还活着的 external 行：沉默兜底对 `Liveness::Alive` 不生效，行停在 TURN_DONE
    // （人离开几小时再回来是正常的），两条 ttl 都不该碰它。
    let (pid, mut child) = alive_pid();
    let busy = external(&m, "conv-alive");
    // `seen_at` 是毫秒（检查点 v3）：拿秒传会被当成「进程比 hook 还新 = 号被复用」，当场判死。
    m.note_external_pid(&busy, pid, now_secs() * 1000);
    m.apply_hook(&busy, 1, &[AgoraEvent::TurnEnded(Some("done".into()))])
        .unwrap();
    assert_eq!(status_of(&m, &busy), Status::TurnDone);

    // 一条到期的 FINISHED 行：和 UNKNOWN 走同一条路径，但拿自己的时钟（ended_at）。
    let finished = external(&m, "conv-finished");
    m.apply_hook(&finished, 1, &[AgoraEvent::SessionEnded(None)])
        .unwrap();

    let now = now_secs();
    assert!(m.has_hook_checkpoint(&silent), "hook 应用过就有检查点");

    let mut differ = Differ::default();
    assert!(
        differ.step("n", &m.list().unwrap()).is_empty(),
        "第一轮只建基线"
    );

    // 一小时后：两条 ttl 都没到（UNKNOWN 只落了一小时）。
    assert!(m.sweep(now + 3600).unwrap().is_empty(), "未到 ttl 不删");
    assert_eq!(m.list().unwrap().len(), 5);

    // 25 h 后：两行 UNKNOWN 走 UNKNOWN 那条出口，FINISHED 行走 finished_ttl 那条。
    let mut removed = m.sweep(now + 25 * 3600).unwrap();
    removed.sort();
    let mut expected = vec![silent.clone(), also_silent.clone(), finished.clone()];
    expected.sort();
    assert_eq!(removed, expected, "{removed:?} vs {expected:?}");

    let left: Vec<String> = m.list().unwrap().into_iter().map(|v| v.record.id).collect();
    assert!(
        left.contains(&unhooked),
        "reason 不是 no process handle 的 UNKNOWN 不动：{left:?}"
    );
    assert!(left.contains(&busy), "进程号还活着的行不动：{left:?}");
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        !m.has_hook_checkpoint(&silent),
        "删走的是 DELETE 同一条路径，检查点一起清"
    );
    assert!(m.get(&silent).is_err());

    // 求差器在下一轮发 session_removed——与用户手工 DELETE 之后客户端看到的同一条事件。
    let events = differ.step("n", &m.list().unwrap());
    let mut gone: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::SessionRemoved { id } => Some(id.clone()),
            _ => None,
        })
        .collect();
    gone.sort();
    let mut expected: Vec<String> = expected.iter().map(|id| format!("n:{id}")).collect();
    expected.sort();
    assert_eq!(gone, expected, "{events:?}");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn unknown_ttl_zero_turns_that_exit_off() {
    // `sessions.external_unknown_ttl: "0"` = 关掉这条出口（与 finished 那条同一条语法）。
    let m = mgr_silent(Duration::ZERO);
    let id = silent_external(&m, "conv-off");
    assert!(m.sweep(now_secs() + 400 * DAY).unwrap().is_empty());
    assert!(m.get(&id).is_ok(), "关了就该留着");

    // 即使 FINISHED 那条开着也不许误删：那个 arm 只认 FINISHED。
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(db, rt as Arc<dyn Runtime>)
        .with_external_finished_ttl(Duration::from_secs(DAY as u64))
        .with_status_config(MachineConfig {
            external_silent_after: Duration::ZERO,
            ..Default::default()
        });
    let id = silent_external(&m, "conv-off-2");
    assert!(m.sweep(now_secs() + 400 * DAY).unwrap().is_empty());
    assert!(m.get(&id).is_ok(), "UNKNOWN 行不归 finished_ttl 管：{id}");
}

#[test]
fn the_shortest_open_ttl_sets_the_sweep_period() {
    // 两条出口共用一个节流（agora-j4w.3 的机制）：只开 unknown 那条、调成 1m，周期跟着缩到 1m。
    // 把 `reset_expiry_throttle` 改回只看 external_finished_ttl → 第三段断言红（行多留一小时）。
    let m = mgr_silent(Duration::from_secs(60));
    let id = silent_external(&m, "conv-short");
    let t0 = now_secs();
    assert!(m.sweep(t0).unwrap().is_empty(), "刚落 UNKNOWN，一分钟未到");
    assert!(m.sweep(t0 + 30).unwrap().is_empty(), "周期未满不扫第二次");
    assert_eq!(m.sweep(t0 + 60).unwrap(), vec![id.clone()]);
    assert!(m.get(&id).is_err(), "行已删");
}
