//! Session Manager 的生命周期映射与 reconcile 六种情况（ADR-001 D4），用内存里的假运行时。

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agora::runtime::{Exit, Runtime, RuntimeRef, Size};
use agora::session::{Db, NewSession, Origin, SessionError, SessionManager};
use agora::status::{EndCause, RuntimeGone, Status};
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
    assert!(
        rt.list().unwrap().sessions.is_empty(),
        "运行时会话必须被回滚掉"
    );
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
    assert_eq!(
        rt.list().unwrap().sessions.len(),
        1,
        "Kill 不销毁运行时会话"
    );
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

/// 预设参数（agora-prdg.3）：落库时 `command` 仍是裸名（ADR-001 D7）、参数在 `launch_args`，
/// Restart 把覆盖命令（API 层算的 resume 形态）与参数一起交给运行时——两条分支共用一个 spec，
/// 不是只在 respawn 那一条拼。
#[test]
fn preset_launch_args_live_in_their_own_column_and_are_replayed_on_restart() {
    let (m, rt, db) = mgr();
    let v = m
        .create_with_prompt(
            &new_session("preset"),
            None,
            Some("--model opus --continue"),
        )
        .unwrap();
    assert_eq!(
        v.record.command.as_deref(),
        Some("sleep 300"),
        "command 不塞参数（ADR-001 D7）"
    );
    assert_eq!(
        v.record.launch_args.as_deref(),
        Some("--model opus --continue"),
        "参数单独一列"
    );
    // 库里的原文也一样（视图来自同一行）。
    let stored: Option<String> = db
        .conn()
        .query_row(
            "SELECT launch_args FROM sessions WHERE id = ?1",
            [&v.record.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_deref(), Some("--model opus --continue"));

    let r = m
        .restart_with(&v.record.id, &[], Some("sleep 300 --resume ctx-1"))
        .unwrap();
    assert_eq!(r.record.epoch, 2);
    let respawns = rt.respawns.lock().unwrap();
    assert_eq!(
        respawns.last().map(String::as_str),
        Some("sleep 300 --resume ctx-1 --model opus --continue"),
        "restart 的这一代命令行 = 覆盖命令 + 参数"
    );
    // 参数被复制进会话行，不引用 preset：之后改预设不影响这一行的重放。
    assert_eq!(
        r.record.launch_args.as_deref(),
        Some("--model opus --continue")
    );
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
    // 拨过 2 s 的 STARTING 窗口：窗口内的行"列表里还没有它"只说明运行时没来得及报到，
    // 不许当成运行时会话没了（见 `starting_window_exempts_a_row_that_was_just_created`）。
    backdate_spawn(&_db, &v.record.id);
    assert!(matches!(
        m.cleanup(&v.record.id),
        Err(SessionError::StillAlive(_))
    ));
    rt.set_dead(v.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));
    m.cleanup(&v.record.id).unwrap();
    assert_eq!(rt.list().unwrap().sessions.len(), 0);
    let after = m.get(&v.record.id).unwrap();
    assert!(after.record.ended_at.is_some());
    assert_eq!(
        after.assessment.status,
        Status::Finished,
        "运行时会话没了、metadata 还在：这是一条结束的事实，不是永远看不懂的 UNKNOWN（agora-u5p）"
    );
    assert!(
        after
            .assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("runtime session gone"),
        "{:?}",
        after.assessment
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
    // missing 那一行拨过 STARTING 窗口：本代进程刚起 2 s 内"没看见"不算"没了"
    // （`starting_window_exempts_a_row_that_is_still_starting` 单独守那一条）。
    backdate_spawn(&db, &missing.record.id);
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
    assert_eq!(
        mv.assessment.status,
        Status::Finished,
        "ref 不在列表里 = 运行时会话没了，不是看不清（agora-u5p；ADR-001 D4）"
    );
    assert_eq!(mv.assessment.source, agora::status::Source::Process);
    assert_eq!(mv.assessment.confidence, 0.8, "拿不到退出码，不给满分");
    assert_eq!(
        mv.assessment.reason.as_deref(),
        Some("runtime session gone (session gone; no exit status)"),
        "server 还在应答（fake 默认），所以是 session gone"
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
fn missing_runtime_session_finishes_the_row_and_writes_ended_at_once() {
    // agora-u5p（Mac 2026-09-18 现场 A2）：有 runtime_ref、运行时应答正常却不在列表里 ——
    // 会话连同 pane 都没了，pane 进程那时收 SIGHUP，agent 确定不在。这是结束的事实，不是
    // "看不清"：报 FINISHED（source process、conf 0.8、reason runtime session gone），
    // 不再钉在 UNKNOWN。ended_at 同时补上（reconcile 只在启动时跑一次，运行中 server 死等不
    // 到下一次重启）。
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("gone")).unwrap();
    backdate_spawn(&_db, &v.record.id);
    let r = v.record.runtime_ref.as_deref().unwrap().to_owned();
    assert_eq!(
        m.get(&v.record.id).unwrap().assessment.status,
        Status::Running
    );

    rt.kill_session(&r);
    let after = m.get(&v.record.id).unwrap();
    assert_eq!(after.assessment.status, Status::Finished);
    assert_eq!(after.assessment.source, agora::status::Source::Process);
    assert_eq!(
        after.assessment.confidence, 0.8,
        "拿不到退出码，又不排除 socket 被误删的假阳性，不给满分"
    );
    assert_eq!(
        after.assessment.reason.as_deref(),
        Some("runtime session gone (session gone; no exit status)")
    );
    assert!(!after.alive, "行上同时说清：运行时会话不在");
    // 与 agora-5gg.18 的进程三态接缝（rebase 到 22afc70 之后多出来的一格）：状态是 FINISHED 就一律
    // `gone`（Q4 裁决：对话结束即不再谈进程），不是 `unknown`——这一行没有 pane 可连是事实。
    // 旧 `alive` 只是它的投影 false；去掉 derive 的 FINISHED 提前返回，这里会退回 liveness 的编码。
    assert_eq!(after.process, agora::status::ProcessState::Gone);
    assert!(!after.would_kill(), "FINISHED 行 Restart / Kill 不再要确认");
    let ended = after
        .record
        .ended_at
        .clone()
        .expect("运行时会话没了：view 首次观察到就补 ended_at");
    assert!(
        after.record.ended_at_approximate,
        "谁也不知道它何时死的，只能近似"
    );
    assert!(
        agora::clock::age_secs(&ended).unwrap_or(u64::MAX) < 5,
        "记的是当下的时刻: {ended}"
    );

    // 幂等：再读一轮不得把 ended_at 刷成更晚的"今天"（否则一行的结束时刻会随轮询一路往前漂）。
    let again = m.get(&v.record.id).unwrap();
    assert_eq!(again.record.ended_at, after.record.ended_at);
    assert_eq!(again.assessment.reason, after.assessment.reason);
}

#[test]
fn a_restart_rediscovering_a_long_dead_row_keeps_its_end_as_status_since() {
    // agora-9q1x（2026-10-08 zuan 现场，用户报"我明明没有操作，但自动跳到需要我的状态"）：
    // 进程层在 daemon 重启后重新观测到"运行时早就没了"——那是**旧事实的新观测**，状态机是新建的
    // （检查点里没有这条进程事实），`set()` 于是把它当成一次新迁移。起点若记成观测时刻：① 9 月 20 日
    // 结束的行在 10 月 8 日重启后 `status_since` 变成重启那一刻，TURN_DONE/FINISHED 段按完成时刻倒序
    // 时它跳到「需要我」顶部；② 「看过」的键是 `<id>@<status_since>`（MISSION §4.6 证据 ①），假时刻
    // 让已经看过、已经收进 Finished 区的行重新冒回需要我。现场两行：`zuan:b4027c`/`zuan:799dc4`
    // （`ended_at` = 09-20T08:30:30Z，重启后 `since` = 10-08T03:50:57）。修法：起点用库里的
    // `ended_at`（`Observation::at`）。
    let (m, rt, db) = mgr();
    let v = m.create(&new_session("old")).unwrap();
    backdate_spawn(&db, &v.record.id);
    let r = v.record.runtime_ref.clone().unwrap();
    rt.kill_session(&r);
    let first = m.get(&v.record.id).unwrap();
    assert_eq!(first.assessment.status, Status::Finished);

    // 把 ended_at 挪到很久以前（这一行早就结束了），再像一个重启那样新建 SessionManager：
    // 状态机重建、库里的事实还在——这正是现场的形状。
    db.conn()
        .execute(
            "UPDATE sessions SET ended_at = '2020-09-13T12:26:40Z' WHERE id = ?1",
            [&v.record.id],
        )
        .unwrap();
    let m2 = SessionManager::new(Arc::clone(&db), rt.clone() as Arc<dyn Runtime>);
    let after = m2.get(&v.record.id).unwrap();
    assert_eq!(after.assessment.status, Status::Finished);
    assert_eq!(
        after.status_since, 1_600_000_000,
        "起点必须是库里记的结束时刻（2020-09-13T12:26:40Z），不是这次观测的时刻"
    );
    assert_eq!(
        after.record.ended_at.as_deref(),
        Some("2020-09-13T12:26:40Z"),
        "ended_at 也照旧是它自己"
    );
}

/// 一次 tick 的全部行，按 id 取。
fn by_id(m: &SessionManager) -> HashMap<String, agora::session::SessionView> {
    m.list()
        .unwrap()
        .into_iter()
        .map(|v| (v.record.id.clone(), v))
        .collect()
}

#[test]
fn a_row_wrongly_judged_gone_takes_its_ended_at_back_when_the_session_shows_up_again() {
    // agora-psj0（承 agora-u5p 的结论、agora-dkv3 的另一半）：「运行时列表里没有它」写下的结束
    // 时刻是**可以错的**——socket 文件被 systemd-tmpfiles / tmpreap 之类扫掉而 tmux server 还
    // 活着时（tmux 靠 SIGUSR1 重建 socket），`list_socket` 连不上 socket 就返回空列表（ADR-001
    // D4：没有会话不是故障，所以它既不报错、也进不了 dkv3 的 `RuntimeScan::unreadable`），那几轮
    // 里这个 socket 上的每一行都被判 FINISHED 并盖上近似 ended_at。socket 回来之后会话原样还在，
    // 而在这条修复之前，那个结束时刻只有 Restart 清得掉——要人动手，还白换一代进程。
    let (m, rt, db) = mgr();
    let back = m.create(&new_session("back")).unwrap(); // 只是看不见，会回来
    let exited = m.create(&new_session("exited")).unwrap(); // 会回来，但 agent 真在失明期间退了
    let gone = m.create(&new_session("gone")).unwrap(); // 真被 kill-session 了，不会回来
    for id in [&back.record.id, &exited.record.id, &gone.record.id] {
        backdate_spawn(&db, id);
    }
    rt.kill_session(gone.record.runtime_ref.as_deref().unwrap());

    // 第一轮：socket 文件没了。三行在列表上长得一模一样——这正是问题所在。
    rt.blind_socket("agora");
    let first = by_id(&m);
    for id in [&back.record.id, &exited.record.id, &gone.record.id] {
        let v = &first[id];
        assert_eq!(
            v.assessment.status,
            Status::Finished,
            "{id}: {:?}",
            v.assessment
        );
        assert!(v.record.ended_at.is_some(), "{id}: 首次观察到就补结束时刻");
        assert!(v.record.ended_at_approximate, "{id}: 没人报得出退出时刻");
        assert!(
            v.record.ended_at_from_missing,
            "{id}: 这一档是「没在列表里」猜出来的，必须带可撤回的记号，否则下一轮分不出该不该收回"
        );
    }

    // 第二轮：socket 回来了，会话一个没少。"pane 随会话收 SIGHUP、agent 确定不在"这句话
    // 对 back 不成立 —— 结束时刻得自己收回去，不等 Restart、不用人动手。
    rt.set_dead(exited.record.runtime_ref.as_deref().unwrap(), Exit::Code(0));
    rt.socket_back("agora");
    let second = by_id(&m);
    let b = &second[&back.record.id];
    assert!(
        matches!(b.assessment.status, Status::Running | Status::Idle),
        "状态回到实际值: {:?}",
        b.assessment
    );
    assert!(b.alive);
    assert!(
        b.record.ended_at.is_none(),
        "会话还在，它没结束过: {:?}",
        b.record
    );
    assert!(!b.record.ended_at_approximate);
    assert!(!b.record.ended_at_from_missing);
    assert!(
        b.assessment.end_cause.is_none(),
        "没结束就不带 end_cause: {:?}",
        b.assessment
    );

    // 对照一：会话回到了列表里，pane 却是死的——失明期间 agent 真的退了。行确实结束了，
    // 那个猜出来的时刻是它仅有的结束时刻（运行时报不出 pane_dead_time），不许撤回。
    let e = &second[&exited.record.id];
    assert_eq!(e.assessment.status, Status::Finished);
    assert_eq!(e.assessment.end_cause, Some(EndCause::ExitCode(0)));
    assert_eq!(
        e.record.ended_at, first[&exited.record.id].record.ended_at,
        "回写只认活着的会话：pane 在而进程没了，结束是事实"
    );

    // 对照二：真的没了的那一行一个字都不许动。区别只有一个——它没回到列表里。
    let g = &second[&gone.record.id];
    assert_eq!(g.assessment.status, Status::Finished);
    assert_eq!(
        g.record.ended_at, first[&gone.record.id].record.ended_at,
        "真结束的行不得被这条回写碰到"
    );
    assert!(
        g.record.ended_at_from_missing,
        "它的结束时刻同样是猜的：哪天真扫到它还活着，一样要能撤回"
    );
    assert_eq!(
        g.assessment.end_cause,
        Some(EndCause::RuntimeGone(RuntimeGone::Session)),
        "server 回来了，没了的只有这一个会话"
    );

    // 清的是库不是内存里那份副本：换一个 manager（= daemon 重启）再看一眼。
    let m2 = SessionManager::new(db, rt.clone() as Arc<dyn Runtime>);
    let report = m2.reconcile().unwrap();
    assert_eq!(report.known_alive, vec![back.record.id.clone()]);
    assert!(m2.get(&back.record.id).unwrap().record.ended_at.is_none());
    assert!(m2.get(&gone.record.id).unwrap().record.ended_at.is_some());
}

#[test]
fn an_approximate_ended_at_that_was_not_guessed_survives_a_live_pane() {
    // 上一条回写只认 `ended_at_from_missing` 这一个记号，不认 `ended_at_approximate`
    // （agora-psj0 验收 ③）：近似只说"时刻是 daemon 的表补的"，Kill / cleanup / reconcile 的
    // dead pane 都会写出近似值，而那些行是确定结束了的，绝不能因为运行时那边还看得见一个活着的
    // pane 就把结束时刻抹掉。今天没有哪条路会写出"近似 + pane 活着"，所以这一行直接用 SQL 造：
    // 守的是规则本身——日后谁再添一条写近似 ended_at 的路，也不会被这条回写顺手清掉。
    let (m, _rt, db) = mgr();
    let v = m.create(&new_session("approx")).unwrap();
    backdate_spawn(&db, &v.record.id);
    db.conn()
        .execute(
            "UPDATE sessions SET ended_at = '2026-09-21T00:00:00Z', ended_at_approximate = TRUE,
                ended_at_from_missing = FALSE WHERE id = ?1",
            [&v.record.id],
        )
        .unwrap();
    let after = m.get(&v.record.id).unwrap();
    assert_eq!(
        after.record.ended_at.as_deref(),
        Some("2026-09-21T00:00:00Z"),
        "不是猜出来的结束时刻，pane 活着也不撤回"
    );
    assert!(after.record.ended_at_approximate);
}

#[test]
fn starting_window_exempts_a_row_that_is_still_starting() {
    // "列表里没有它"要过 STARTING 窗口才算"运行时会话没了"。反例是自家路径：`create_with_prompt`
    // 与 `restart_with` 在运行时刚返回的那一刻就 `get()`，那一 tick 的列表里可能还没有这个 pane
    // （`list_socket` 对解析不了的 pane 行也是跳过、不报错）。少了这条守卫，每起一次会话都会先给
    // 自己写一个 ended_at、报一次 FINISHED，再把行推回 STARTING（`tests/runtime_degraded.rs` 的
    // 「绝不能因为读不到就写上 ended_at」同一件事，2026-09-19 实测）。
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("fresh")).unwrap();
    let r = v.record.runtime_ref.as_deref().unwrap().to_owned();
    rt.kill_session(&r);
    let after = m.get(&v.record.id).unwrap();
    assert_eq!(
        after.assessment.reason.as_deref(),
        Some("runtime session missing"),
        "窗口内仍是「没有运行时事实可给」，不抢着下结论"
    );
    assert!(after.record.ended_at.is_none(), "更不能写 ended_at");
    assert_eq!(after.assessment.status, Status::Unknown);
}

#[test]
fn server_gone_and_session_gone_are_told_apart_in_the_reason() {
    // 同一个结论（FINISHED / process / 0.8），reason 分两档：整个 server 连不上，还是只有
    // 这一个会话没了。排障时这一句就是"为什么一屋子行同时结束"的答案（issue notes ③）。
    let (m, rt, db) = mgr();
    let a = m.create(&new_session("a")).unwrap();
    let b = m.create(&new_session("b")).unwrap();
    backdate_spawn(&db, &a.record.id);
    backdate_spawn(&db, &b.record.id);
    rt.kill_server("agora");
    let views: HashMap<String, _> = m
        .list()
        .unwrap()
        .into_iter()
        .map(|v| (v.record.id.clone(), v))
        .collect();
    for id in [&a.record.id, &b.record.id] {
        let v = &views[id];
        assert_eq!(
            v.assessment.status,
            Status::Finished,
            "{id}: {:?}",
            v.assessment
        );
        let reason = v.assessment.reason.as_deref().unwrap_or_default();
        assert!(
            reason.contains("runtime session gone") && reason.contains("server gone"),
            "一屋子的行一起没了，要说得上是 server 的事: {reason}"
        );
        assert!(v.record.ended_at.is_some(), "{id} 要补上结束时刻");
    }
    // 对照：server 还在应答、只删一个会话，就说 session gone，不推到 server 头上。
    let (m2, rt2, db2) = mgr();
    let c = m2.create(&new_session("c")).unwrap();
    let d = m2.create(&new_session("d")).unwrap();
    backdate_spawn(&db2, &c.record.id);
    rt2.kill_session(c.record.runtime_ref.as_deref().unwrap());
    let cv = m2.get(&c.record.id).unwrap();
    assert!(
        cv.assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("session gone; no exit status"),
        "{:?}",
        cv.assessment
    );
    assert!(
        !cv.assessment
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("server gone"),
        "server 在，不要把话说大"
    );
    assert!(
        matches!(
            m2.get(&d.record.id).unwrap().assessment.status,
            Status::Starting | Status::Running
        ),
        "server 在、会话也在：只删一个会话不应把别的算成结束"
    );
}

#[test]
fn runtime_gone_of_a_killed_row_still_says_killed_by_user() {
    // 库里有 killed_at（用户在 Dashboard 按过 Kill）而运行时会话后来也没了（Mac 现场 b226b5：
    // killed_at 09-09、状态却说看不清）：按 killed_by_user 说，不弹通知——那是他自己干的。
    let (m, rt, db) = mgr();
    let v = m.create(&new_session("k")).unwrap();
    backdate_spawn(&db, &v.record.id);
    m.kill(&v.record.id).unwrap();
    rt.kill_session(v.record.runtime_ref.as_deref().unwrap());
    let after = m.get(&v.record.id).unwrap();
    let reason = after.assessment.reason.as_deref().unwrap_or_default();
    assert_eq!(after.assessment.status, Status::Finished);
    assert!(
        reason.starts_with("killed by user") && reason.contains("runtime session gone"),
        "既要留下 Kill 的事实，也要说清运行时会话不在了: {reason}"
    );
}

#[test]
fn degraded_runtime_never_becomes_runtime_gone() {
    // ADR-001 D7 的降级路径一个字不改：server 在、但应答不了（协议不匹配）→ UNKNOWN
    // runtime unavailable，而且绝不写 ended_at（"读不到"不等于"已经死了"）。
    let (m, rt, db) = mgr();
    let v = m.create(&new_session("deg")).unwrap();
    backdate_spawn(&db, &v.record.id);
    *rt.list_error.lock().unwrap() =
        Some("protocol version mismatch (client 8, server 7)".to_owned());
    let views = m.list().unwrap();
    let got = views.iter().find(|x| x.record.id == v.record.id).unwrap();
    assert_eq!(got.assessment.status, Status::Unknown);
    let reason = got.assessment.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("runtime unavailable") && !reason.contains("runtime session gone"),
        "{reason}"
    );
    assert!(
        got.record.ended_at.is_none(),
        "降级期间不得写 ended_at（与 tests/runtime_degraded.rs 同一条纪律）"
    );
    // 同一次「读不到」在进程三态上必须是 `unknown`：上半段给状态机编码成的 Dead 不能原样导出，
    // 否则 API 上就多出一条假的 gone（agora-5gg.18 的 derive 第三个参数，ADR-001 D7）。
    assert_eq!(got.process, agora::status::ProcessState::Unknown);
    // 恢复：server 换代后下一次读就转回活着，不必重启 daemon。
    *rt.list_error.lock().unwrap() = None;
    let back = m.get(&v.record.id).unwrap();
    assert!(back.alive);
}

#[test]
fn runtime_gone_notifies_once_and_obeys_killed_by_user() {
    // 验收：RUNNING → 这条 FINISHED 像 process gone 一样通知一次（求差器第一轮只建基线，
    // 所以 daemon 重启时发现 server 不在不会弹——只有运行中才响一次）。
    use agora::events::{Differ, Event};
    let (m, rt, db) = mgr();
    let gone = m.create(&new_session("gone")).unwrap();
    let killed = m.create(&new_session("killed")).unwrap();
    backdate_spawn(&db, &gone.record.id);
    backdate_spawn(&db, &killed.record.id);
    let mut differ = Differ::default();
    assert!(differ.step("n", &m.list().unwrap()).is_empty());

    rt.kill_session(gone.record.runtime_ref.as_deref().unwrap());
    m.kill(&killed.record.id).unwrap();
    rt.kill_session(killed.record.runtime_ref.as_deref().unwrap());
    let events = differ.step("n", &m.list().unwrap());
    let notes: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Notification { id, title, .. } => {
                Some((id.clone().unwrap_or_default(), title.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        notes,
        vec![(
            format!("n:{}", gone.record.id),
            "Shell / gone @ n finished".to_owned()
        )],
        "用户自己 Kill 的不弹；运行中会话消失的弹一条: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::StatusChanged {
                status: Status::Finished,
                ref reason,
                ..
            } if reason.as_deref().unwrap_or_default().contains("runtime session gone")
        )),
        "{events:?}"
    );
    // 下一轮 nothing changed：不重复弹。
    assert!(differ.step("n", &m.list().unwrap()).is_empty());
}

/// 把运行时里的会话标成"在 `exited` 那一刻退出了"（FakeRuntime 的 set_dead 不带时刻）。
fn set_dead_at(rt: &FakeRuntime, r#ref: &str, exit: Exit, exited: SystemTime) {
    let mut m = rt.sessions.lock().unwrap();
    let s = m.get_mut(r#ref).unwrap();
    s.alive = false;
    s.exit = Some(exit);
    s.exited_at = Some(exited);
}

fn utc(t: SystemTime) -> String {
    agora::clock::format_utc_secs(t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64)
}

#[test]
fn ended_at_from_runtime_exit_time_after_daemon_restart() {
    // A42（agora-h1k.4；M3 剧本第 4 步）：停 daemon，agent 在 tmux 里退出，一小时后起 daemon →
    // reconcile 补的 ended_at 是运行时报的退出时刻（pane_dead_time），不是 daemon 起来的"今天"；
    // 运行时会话已经不在、谁也不知道它何时死的，才记 reconcile 时刻并标 approximate。
    let (m, rt, db) = mgr();
    let exact = m.create(&new_session("exact")).unwrap();
    let gone = m.create(&new_session("gone")).unwrap();
    assert!(exact.record.ended_at.is_none());
    drop(m); // daemon 停了

    let exited = SystemTime::now() - Duration::from_secs(3600);
    set_dead_at(
        &rt,
        exact.record.runtime_ref.as_deref().unwrap(),
        Exit::Code(0),
        exited,
    );
    rt.forget(gone.record.runtime_ref.as_deref().unwrap());

    let m2 = SessionManager::new(db, rt.clone() as Arc<dyn Runtime>);
    let report = m2.reconcile().unwrap();
    assert_eq!(report.known_dead, vec![exact.record.id.clone()]);
    assert_eq!(report.known_missing, vec![gone.record.id.clone()]);

    let e = m2.get(&exact.record.id).unwrap();
    assert_eq!(e.record.ended_at.as_deref(), Some(utc(exited).as_str()));
    assert!(!e.record.ended_at_approximate, "运行时报的时刻是准的");
    assert_eq!(e.assessment.status, Status::Finished);

    let g = m2.get(&gone.record.id).unwrap();
    let age = agora::clock::age_secs(g.record.ended_at.as_deref().unwrap()).unwrap();
    assert!(age < 5, "运行时会话不在了：记 reconcile 时刻，age={age}");
    assert!(g.record.ended_at_approximate, "并且标 approximate");

    // 再重启一次：准确值不动，近似值也不会被又一次 reconcile 刷成更晚的"今天"。
    let m3 = SessionManager::new(m2.db_handle(), rt.clone() as Arc<dyn Runtime>);
    m3.reconcile().unwrap();
    assert_eq!(
        m3.get(&exact.record.id).unwrap().record.ended_at,
        e.record.ended_at
    );
    assert_eq!(
        m3.get(&gone.record.id).unwrap().record.ended_at,
        g.record.ended_at
    );
}

#[test]
fn approximate_ended_at_is_corrected_by_the_runtime_exit_time() {
    // Kill 那一刻运行时多半还没收集到退出时刻：先记近似的现在；下次 reconcile 拿到 pane_dead_time
    // 就用准确值覆盖近似值。反过来准确值绝不被近似值覆盖。Restart 把两者一起清掉。
    let (m, rt, db) = mgr();
    let v = m.create(&new_session("k")).unwrap();
    let r = v.record.runtime_ref.clone().unwrap();
    let killed = m.kill(&v.record.id).unwrap();
    assert!(killed.record.ended_at.is_some());
    assert!(
        killed.record.ended_at_approximate,
        "FakeRuntime 不报退出时刻 → 近似"
    );
    // 运行时随后收集到了退出时刻（比 Kill 早几秒也无妨——它才是事实）。
    let exited = SystemTime::now() - Duration::from_secs(3);
    set_dead_at(&rt, &r, Exit::Signal("TERM".into()), exited);
    let m2 = SessionManager::new(db, rt.clone() as Arc<dyn Runtime>);
    m2.reconcile().unwrap();
    let after = m2.get(&v.record.id).unwrap();
    assert_eq!(after.record.ended_at.as_deref(), Some(utc(exited).as_str()));
    assert!(!after.record.ended_at_approximate);
    assert_eq!(
        after.assessment.status,
        Status::Finished,
        "killed by user 不变"
    );

    // 准确值到手后，cleanup（运行时已无从再报）不把它改回近似的现在。
    m2.cleanup(&v.record.id).unwrap();
    let cleaned = m2.get(&v.record.id).unwrap();
    assert_eq!(cleaned.record.ended_at, after.record.ended_at);
    assert!(!cleaned.record.ended_at_approximate);

    // Restart：新一代进程，ended_at 与 approximate 一起清空。
    let restarted = m2.restart(&v.record.id, &[]).unwrap();
    assert!(restarted.record.ended_at.is_none());
    assert!(!restarted.record.ended_at_approximate);
}

#[test]
fn cleanup_takes_the_exit_time_before_removing_the_runtime_session() {
    // 会话昨天退出、用户今天才点清理：remove 之后运行时再也不知道它何时死的，所以先看再删。
    let (m, rt, _db) = mgr();
    let v = m.create(&new_session("c")).unwrap();
    let exited = SystemTime::now() - Duration::from_secs(86_400);
    set_dead_at(
        &rt,
        v.record.runtime_ref.as_deref().unwrap(),
        Exit::Code(0),
        exited,
    );
    m.cleanup(&v.record.id).unwrap();
    let after = m.get(&v.record.id).unwrap();
    assert_eq!(after.record.ended_at.as_deref(), Some(utc(exited).as_str()));
    assert!(!after.record.ended_at_approximate);
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

#[test]
fn restart_of_an_adopted_session_without_a_command_is_a_typed_error_not_an_empty_shell() {
    // agora-vto：adopted 会话的 command 是 NULL；空命令 respawn 会让 tmux 起一个交互 shell，
    // 看起来像"重启成功"其实 agent 没了。要的是明确的错误类型。
    let (m, rt, _db) = mgr();
    rt.insert("fake:agora:manual", true, None, true);
    let v = m
        .adopt(&agora::session::AdoptSession {
            runtime_ref: "fake:agora:manual".into(),
            display_name: Some("manual".into()),
            agent_type: Some("shell".into()),
            working_directory: None,
        })
        .unwrap();
    assert_eq!(v.record.origin, Origin::Adopted);
    assert!(v.record.command.is_none());
    let err = m.restart(&v.record.id, &[]).unwrap_err();
    assert!(matches!(err, SessionError::NoCommand(_)), "{err}");
    // 覆盖命令也救不了：它本来就是从库里的命令算出来的。
    let err = m.restart_with(&v.record.id, &[], Some("")).unwrap_err();
    assert!(matches!(err, SessionError::NoCommand(_)), "{err}");
    assert!(rt.respawns.lock().unwrap().is_empty(), "不该有任何 respawn");
}

#[test]
fn respond_via_follows_the_held_hook_host_not_the_declared_agent_type() {
    // agora-1dr：custom / fake 类型的会话里跑着包了 claude 的脚本，hook 照样挂起等答复。
    // 挂起按收到的事件登记，与声明类型无关；respond_via 有挂起时看挂起那条的宿主。
    use agora::session::PendingDecision;
    let (m, _rt, _db) = mgr();
    let mut spec = new_session("wrapped");
    spec.agent_type = "fake".into();
    let v = m.create(&spec).unwrap();
    assert_eq!(
        v.respond_via, "terminal",
        "没有挂起：fake 没有 hook，只能开终端"
    );
    assert_eq!(v.respond_within_secs, None);

    m.add_pending_decision(
        &v.record.id,
        PendingDecision {
            request_id: "req-1".into(),
            summary: "Bash: rm x".into(),
            epoch: v.record.epoch,
            host: "claude".into(),
        },
    );
    let held = m.get(&v.record.id).unwrap();
    assert_eq!(held.respond_via, "hook");
    assert_eq!(
        held.respond_within_secs,
        Some(
            agora::adapter::for_host("claude")
                .unwrap()
                .hold_timeout()
                .as_secs()
        ),
        "挂起上限也取挂起宿主的"
    );
    assert_eq!(
        held.pending_decision.as_ref().map(|p| p.host.as_str()),
        Some("claude")
    );

    m.remove_pending_decision(&v.record.id, "req-1");
    assert_eq!(m.get(&v.record.id).unwrap().respond_via, "terminal");
}
