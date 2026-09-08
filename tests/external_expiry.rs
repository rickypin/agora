//! agora-j4w.3：external FINISHED 行的自动过期。侧栏 58 行里 39 行是 external 且 FINISHED
//! （2026-09-08 现场），它们没有运行时会话与输出，只剩 agora 的两行记录；结束超过
//! `sessions.external_finished_ttl` 就删 metadata（MISSION §4.6）。agora / adopted 行有 scrollback，不动。

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use std::collections::BTreeMap;

use agora::clock::now_secs;
use agora::events::{Differ, Event};
use agora::hook::{Delivery, Envelope, Inbox, Receiver};
use agora::runtime::{Exit, Runtime, Size};
use agora::session::{Db, ExternalSession, NewSession, Origin, SessionManager};
use agora::status::{AgoraEvent, Source, Status};
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
    // 30 分钟后：second 早就过了 24 h（status_since 是现在，sweep 的表在 25.5 h 后），但周期未满，不扫。
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
    // 永远到不了 24h。守卫：重启后 status_since 仍是 SessionEnd 那一刻（检查点里的 set_at），ttl 到期照删。
    // 关掉 machine.rs observe 里 process_fact_is_no_better 的判断 → status_since 断言红、sweep 不删。
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
            json!({"hook_event_name":"SessionStart","session_id":"ext-ec4","cwd":"/work/agora","source":"startup"}),
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
        inbox.prune_done(Duration::ZERO);
        id
    };
    // 把检查点里的结束时刻拨回两小时前（真实世界里是 daemon 停了两小时），再让进程退出。
    let path = checkpoint_path(home.path(), &id);
    let mut cp: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let ended = cp["set_at"].as_i64().expect("检查点带 set_at") - 2 * 3600;
    cp["set_at"] = json!(ended);
    std::fs::write(&path, cp.to_string()).unwrap();
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
    assert_eq!(
        s.sweep(now_secs()).unwrap(),
        vec![id.clone()],
        "两小时前结束、ttl 一小时：第一次 sweep 就删"
    );
    assert!(s.get(&id).is_err(), "行已删");
}
