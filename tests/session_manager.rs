//! Session Manager 的生命周期映射与 reconcile 六种情况（ADR-001 D4），用内存里的假运行时。

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agora::runtime::{Exit, Runtime, RuntimeRef, Size};
use agora::session::{Db, NewSession, Origin, SessionError, SessionManager};
use agora::status::Status;
use common::FakeRuntime;

fn mgr() -> (SessionManager, Arc<FakeRuntime>, Arc<Db>) {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(Arc::clone(&db), rt.clone() as Arc<dyn Runtime>);
    (m, rt, db)
}

fn new_session(name: &str) -> NewSession {
    NewSession {
        display_name: name.into(),
        agent_type: "shell".into(),
        working_directory: PathBuf::from("/tmp"),
        worktree: None,
        task_ref: None,
        command: "sleep 300".into(),
        env: vec![],
        size: Size::default(),
    }
}

#[test]
fn create_writes_metadata_after_runtime_and_reports_starting_then_running() {
    let (m, _rt, _db) = mgr();
    let v = m.create(&new_session("one")).unwrap();
    assert_eq!(v.record.origin, Origin::Agora);
    assert_eq!(v.record.epoch, 1);
    assert!(v
        .record
        .runtime_ref
        .as_deref()
        .unwrap()
        .starts_with("fake:agora:ag-"));
    assert!(v.alive);
    // 刚创建、尚无活动：STARTING（进程状态层的窗口）。
    assert_eq!(v.assessment.status, Status::Starting);
}

/// 把本代进程的起始时刻拨回过去，越过 2 s 的 STARTING 窗口而不真等。
fn backdate_spawn(db: &Db, id: &str) {
    db.conn()
        .execute(
            "UPDATE sessions SET spawned_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            [id],
        )
        .unwrap();
}

#[test]
fn starting_window_follows_spawned_at_not_updated_at() {
    // agora-xqa.15：STARTING 只看本代进程起始时刻；rename 刷新 updated_at 不得让它回到 STARTING。
    let (m, _rt, db) = mgr();
    let v = m.create(&new_session("s")).unwrap();
    assert!(v.record.spawned_at.is_some());
    assert_eq!(v.assessment.status, Status::Starting);
    backdate_spawn(&db, &v.record.id);
    assert_eq!(
        m.get(&v.record.id).unwrap().assessment.status,
        Status::Running
    );
    let renamed = m.rename(&v.record.id, "renamed").unwrap();
    assert_ne!(renamed.record.updated_at, "2020-01-01T00:00:00Z");
    assert_eq!(renamed.assessment.status, Status::Running, "改名不是新进程");
}

#[test]
fn restart_opens_a_fresh_starting_window() {
    let (m, _rt, db) = mgr();
    let v = m.create(&new_session("s")).unwrap();
    backdate_spawn(&db, &v.record.id);
    m.kill(&v.record.id).unwrap();
    let r = m.restart(&v.record.id, &[]).unwrap();
    assert_eq!(
        r.assessment.status,
        Status::Starting,
        "respawn 是新一代进程"
    );
}

#[test]
fn killed_by_user_survives_daemon_restart() {
    // agora-xqa.16：killed_at 落库，新 manager（模拟 daemon 重启）仍报 FINISHED（killed by user）。
    let (m, rt, db) = mgr();
    let v = m.create(&new_session("k")).unwrap();
    let killed = m.kill(&v.record.id).unwrap();
    assert!(killed.record.killed_at.is_some());
    drop(m);

    let m2 = SessionManager::new(Arc::clone(&db), rt.clone() as Arc<dyn Runtime>);
    let report = m2.reconcile().unwrap();
    assert_eq!(report.known_dead, vec![v.record.id.clone()]);
    let after = m2.get(&v.record.id).unwrap();
    assert_eq!(after.assessment.status, Status::Finished);
    assert!(after
        .assessment
        .reason
        .as_deref()
        .unwrap()
        .contains("killed by user"));

    let restarted = m2.restart(&v.record.id, &[]).unwrap();
    assert!(
        restarted.record.killed_at.is_none(),
        "Restart 清掉 killed_at"
    );
    // 新一代被别人用信号杀掉 → FAILED，不再沾上一代的 Kill。
    rt.set_dead(
        restarted.record.runtime_ref.as_deref().unwrap(),
        Exit::Signal("KILL".into()),
    );
    assert_eq!(
        m2.get(&v.record.id).unwrap().assessment.status,
        Status::Failed
    );
}

#[test]
fn db_failure_rolls_back_runtime_session() {
    let (m, rt, db) = mgr();
    db.conn()
        .execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
    let err = m.create(&new_session("x")).unwrap_err();
    assert!(matches!(err, SessionError::Db(_)), "{err}");
    assert!(rt.list().unwrap().is_empty(), "运行时会话必须被回滚掉");
    assert_eq!(rt.removed.lock().unwrap().len(), 1);
}

#[test]
fn rename_to_same_string_still_locks_and_title_only_wins_unlocked() {
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("plain")).unwrap();
    let r = v.record.runtime_ref.clone().unwrap();
    rt.set_title(&r, "agent-set-title");
    assert_eq!(m.get(&v.record.id).unwrap().name, "agent-set-title");
    let v2 = m.rename(&v.record.id, "plain").unwrap();
    assert!(v2.record.name_locked);
    assert_eq!(v2.name, "plain", "改成同名字符串也落锁，title 从此不赢");
}

#[test]
fn kill_keeps_dead_pane_and_counts_as_finished_by_user() {
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("k")).unwrap();
    let killed = m.kill(&v.record.id).unwrap();
    assert!(!killed.alive);
    assert_eq!(killed.exit, Some(Exit::Signal("TERM".into())));
    assert_eq!(killed.assessment.status, Status::Finished);
    assert!(killed
        .assessment
        .reason
        .as_deref()
        .unwrap()
        .contains("killed by user"));
    assert!(killed.record.ended_at.is_some());
    assert_eq!(rt.list().unwrap().len(), 1, "Kill 不销毁运行时会话");
}

#[test]
fn exit_codes_map_to_finished_and_failed() {
    let (m, rt, _db) = mgr();
    let a = m.create(&new_session("a")).unwrap();
    let b = m.create(&new_session("b")).unwrap();
    rt.set_dead(a.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));
    rt.set_dead(b.record.runtime_ref.as_deref().unwrap(), Exit::Code(7));
    assert_eq!(
        m.get(&a.record.id).unwrap().assessment.status,
        Status::Finished
    );
    let bv = m.get(&b.record.id).unwrap();
    assert_eq!(bv.assessment.status, Status::Failed);
    assert_eq!(bv.assessment.reason.as_deref(), Some("exit code 7"));
    // 别人杀的（不是 agora terminate）按信号退出 → FAILED。
    rt.set_dead(
        a.record.runtime_ref.as_deref().unwrap(),
        Exit::Signal("KILL".into()),
    );
    assert_eq!(
        m.get(&a.record.id).unwrap().assessment.status,
        Status::Failed
    );
}

#[test]
fn restart_bumps_epoch_and_clears_ended_at() {
    let (m, _rt, _db) = mgr();
    let v = m.create(&new_session("r")).unwrap();
    m.kill(&v.record.id).unwrap();
    let r = m.restart(&v.record.id, &[]).unwrap();
    assert_eq!(r.record.epoch, 2);
    assert!(r.record.ended_at.is_none());
    assert!(r.alive);
    assert_eq!(r.record.runtime_ref, v.record.runtime_ref, "同一运行时会话");
}

#[test]
fn delete_metadata_leaves_alive_session_and_removes_dead_one() {
    let (m, rt, _db) = mgr();
    let alive = m.create(&new_session("alive")).unwrap();
    let dead = m.create(&new_session("dead")).unwrap();
    rt.set_dead(dead.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));

    m.delete_metadata(&alive.record.id).unwrap();
    assert!(matches!(
        m.get(&alive.record.id),
        Err(SessionError::NotFound(_))
    ));
    assert!(
        rt.inspect(&RuntimeRef(alive.record.runtime_ref.clone().unwrap()))
            .is_ok(),
        "Delete ≠ kill"
    );

    m.delete_metadata(&dead.record.id).unwrap();
    assert!(
        rt.inspect(&RuntimeRef(dead.record.runtime_ref.clone().unwrap()))
            .is_err(),
        "已死的顺手 remove"
    );
}

#[test]
fn cleanup_refuses_alive_and_removes_dead() {
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("c")).unwrap();
    assert!(matches!(
        m.cleanup(&v.record.id),
        Err(SessionError::StillAlive(_))
    ));
    rt.set_dead(v.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));
    m.cleanup(&v.record.id).unwrap();
    assert_eq!(rt.list().unwrap().len(), 0);
    let after = m.get(&v.record.id).unwrap();
    assert!(after.record.ended_at.is_some());
    assert_eq!(
        after.assessment.status,
        Status::Unknown,
        "运行时会话没了，metadata 还在"
    );
}

#[test]
fn reconcile_covers_all_six_cases() {
    let (m, rt, db) = mgr();
    let alive = m.create(&new_session("alive")).unwrap();
    let dead = m.create(&new_session("dead")).unwrap();
    let missing = m.create(&new_session("missing")).unwrap();
    rt.set_dead(dead.record.runtime_ref.as_deref().unwrap(), Exit::Code(3));
    rt.forget(missing.record.runtime_ref.as_deref().unwrap());
    rt.insert("fake:agora:ag-orphan", true, None, true);
    rt.insert("fake:default:mywork", true, None, false);
    db.conn()
        .execute(
            "INSERT INTO sessions (id, runtime_ref, display_name, agent_type, created_at, updated_at, origin)
             VALUES ('ext001', NULL, 'hook-only', 'shell', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'external')",
            [],
        )
        .unwrap();

    // 模拟 daemon 重启：同一个库、同一个运行时，新的 manager。
    let m2 = SessionManager::new(db.clone(), rt.clone() as Arc<dyn Runtime>);
    let report = m2.reconcile().unwrap();
    assert_eq!(report.known_alive, vec![alive.record.id.clone()]);
    assert_eq!(report.known_dead, vec![dead.record.id.clone()]);
    assert_eq!(report.known_missing, vec![missing.record.id.clone()]);
    assert_eq!(
        report.unregistered_managed,
        vec![RuntimeRef("fake:agora:ag-orphan".into())]
    );
    assert_eq!(
        report.unregistered_adoptable,
        vec![RuntimeRef("fake:default:mywork".into())]
    );
    assert_eq!(report.external, vec!["ext001".to_string()]);

    let views: HashMap<String, _> = m2
        .list()
        .unwrap()
        .into_iter()
        .map(|v| (v.record.id.clone(), v))
        .collect();
    // 刚建不到 2 s 仍在 STARTING 窗口内；两者都算"活着"。
    assert!(matches!(
        views[&alive.record.id].assessment.status,
        Status::Starting | Status::Running
    ));
    assert_eq!(views[&dead.record.id].assessment.status, Status::Failed);
    assert!(views[&dead.record.id].record.ended_at.is_some());
    let mv = &views[&missing.record.id];
    assert_eq!(mv.assessment.status, Status::Unknown);
    assert_eq!(
        mv.assessment.reason.as_deref(),
        Some("runtime session missing")
    );
    assert!(
        mv.record.ended_at.is_some(),
        "known ref 不在 → ended_at 补今天"
    );
    assert_eq!(views["ext001"].assessment.status, Status::Unknown);

    // missing 的 Restart 退化为同名 create，无 scrollback。
    let back = m2.restart(&missing.record.id, &[]).unwrap();
    assert!(back.alive);
    assert_eq!(back.record.epoch, 2);
}

#[test]
fn kill_on_already_dead_session_leaves_failed_and_no_killed_at() {
    // agora-1a0：被别人 kill -9 的 FAILED 会话，Dashboard 点 Kill 是空操作，不能冒充"用户杀的"。
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("dead")).unwrap();
    rt.set_dead(
        v.record.runtime_ref.as_deref().unwrap(),
        Exit::Signal("KILL".into()),
    );
    let after = m.kill(&v.record.id).unwrap();
    assert!(
        after.record.killed_at.is_none(),
        "已死的会话 Kill 不写 killed_at"
    );
    assert_eq!(after.assessment.status, Status::Failed);
    assert_eq!(after.assessment.reason.as_deref(), Some("signal KILL"));
    assert!(after.record.ended_at.is_some(), "空操作也把结束时刻补上");
}

#[test]
fn user_kill_answered_with_shell_signal_exit_code_counts_as_finished() {
    // agora-3ib：Claude 收到 SIGTERM 后自己以 143 退出，tmux 报的是 Code(143) 不是 Signal。
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("c")).unwrap();
    let r = v.record.runtime_ref.clone().unwrap();
    let killed = m.kill(&v.record.id).unwrap();
    assert!(killed.record.killed_at.is_some());
    rt.set_dead(&r, Exit::Code(143));
    let view = m.get(&v.record.id).unwrap();
    assert_eq!(view.assessment.status, Status::Finished);
    assert_eq!(
        view.assessment.reason.as_deref(),
        Some("killed by user (exit code 143)")
    );
    // 用户杀了但 agent 以别的非零码退出（真崩了）→ 仍 FAILED。
    rt.set_dead(&r, Exit::Code(1));
    assert_eq!(
        m.get(&v.record.id).unwrap().assessment.status,
        Status::Failed
    );
    // 没人按 Kill 的 143 → FAILED。
    let w = m.create(&new_session("w")).unwrap();
    rt.set_dead(w.record.runtime_ref.as_deref().unwrap(), Exit::Code(143));
    assert_eq!(
        m.get(&w.record.id).unwrap().assessment.status,
        Status::Failed
    );
}

#[test]
fn conversation_id_change_resends_the_row() {
    // agora-dvh.13：/clear 后 agent 自报新对话 id，是 metadata 不是状态——求差器要整行重发
    // session_updated，Settings 里"当前对话"才跟着变（2026-09-04 真实 Claude 代验时发现漏掉）。
    use agora::events::{Differ, Event};
    let (m, _rt, db) = mgr();
    let v = m.create(&new_session("c")).unwrap();
    let mut differ = Differ::default();
    assert!(
        differ.step("n", &m.list().unwrap()).is_empty(),
        "第一轮只建基线"
    );
    db.conn()
        .execute(
            "UPDATE sessions SET agent_session_id = 'conv-2' WHERE id = ?1",
            [&v.record.id],
        )
        .unwrap();
    let events = differ.step("n", &m.list().unwrap());
    assert!(
        matches!(&events[..], [Event::SessionUpdated { session, .. }] if session["agent_session_id"] == "conv-2"),
        "{events:?}"
    );
}

#[test]
fn differ_notifies_once_per_transition_out_of_running_and_obeys_the_switch() {
    // agora-dvh.11（A18）：进程退出 → RUNNING → FINISHED / FAILED 各发一条 notification，
    // 跟在 status_changed 后面；同一状态再来一轮不重复；notifications.enabled=false 只静音通知。
    use agora::events::{Differ, Event};
    let (m, rt, db) = mgr();
    let ok = m.create(&new_session("ok")).unwrap();
    let bad = m.create(&new_session("bad")).unwrap();
    backdate_spawn(&db, &ok.record.id);
    backdate_spawn(&db, &bad.record.id);
    let mut differ = Differ::default();
    assert!(differ.step("n", &m.list().unwrap()).is_empty());
    rt.set_dead(ok.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));
    rt.set_dead(bad.record.runtime_ref.as_deref().unwrap(), Exit::Code(1));
    let events = differ.step("n", &m.list().unwrap());
    let mut notes: Vec<(String, String, String)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Notification {
                id, title, status, ..
            } => Some((
                id.clone().unwrap(),
                title.clone(),
                format!("{:?}", status.unwrap()),
            )),
            _ => None,
        })
        .collect();
    notes.sort();
    let mut want = vec![
        (
            format!("n:{}", bad.record.id),
            "Shell / bad @ n failed".to_owned(),
            "Failed".to_owned(),
        ),
        (
            format!("n:{}", ok.record.id),
            "Shell / ok @ n finished".to_owned(),
            "Finished".to_owned(),
        ),
    ];
    want.sort();
    assert_eq!(notes, want, "{events:?}");
    // 通知紧跟在同一会话的 status_changed 之后（客户端先改行再弹）。
    let idx = |pred: &dyn Fn(&Event) -> bool| events.iter().position(pred).unwrap();
    let ok_gid = format!("n:{}", ok.record.id);
    assert!(
        idx(&|e| matches!(e, Event::StatusChanged { id, .. } if *id == ok_gid))
            < idx(&|e| matches!(e, Event::Notification { id: Some(id), .. } if *id == ok_gid))
    );
    // 抖动：下一轮什么都没变 → 无事件。
    assert!(differ.step("n", &m.list().unwrap()).is_empty());

    // 用户自己 Kill 的（RUNNING → FINISHED，reason killed by user）不通知：是他自己干的。
    let k = m.create(&new_session("killed")).unwrap();
    backdate_spawn(&db, &k.record.id);
    differ.step("n", &m.list().unwrap());
    m.kill(&k.record.id).unwrap();
    rt.set_dead(k.record.runtime_ref.as_deref().unwrap(), Exit::Code(143));
    let events = differ.step("n", &m.list().unwrap());
    assert!(events.iter().any(|e| matches!(
        e,
        Event::StatusChanged {
            status: Status::Finished,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Notification { .. })),
        "{events:?}"
    );

    // 开关关掉：状态事件照发，通知一条没有。
    let (m2, rt2, db2) = mgr();
    let v = m2.create(&new_session("quiet")).unwrap();
    backdate_spawn(&db2, &v.record.id);
    let mut muted = Differ::new(false);
    muted.step("n", &m2.list().unwrap());
    rt2.set_dead(v.record.runtime_ref.as_deref().unwrap(), Exit::Code(1));
    let events = muted.step("n", &m2.list().unwrap());
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::StatusChanged { .. })));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Notification { .. })),
        "{events:?}"
    );
}
