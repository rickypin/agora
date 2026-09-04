//! 任务标签层（MISSION §6.3；ADR-002 D8；A23；不变量 12 的零写入守卫，agora-dvh.10）。
//!
//! 用一个假 `bd` 脚本把每次调用的 argv 录下来：agora 对 beads 只准 `show`。关掉 `READ_ONLY`
//! 的限制、或在别处起 `bd`，这里与 `tests/arch_boundary.rs::beads_is_read_only_and_lives_in_task`
//! 就红。

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agora::runtime::Runtime;
use agora::session::{Db, NewSession, SessionManager};
use agora::status::AgoraEvent;
use agora::task::{TaskIndex, READ_ONLY};
use common::FakeRuntime;

/// 一个假 `bd`：把 argv 追加到 `<dir>/calls.log`，`show <id> --json` 按 id 回答，
/// 其它 id 报错退出 1（模拟"没这个 issue"）。
fn fake_bd(dir: &Path) -> PathBuf {
    let bin = dir.join("bd");
    let log = dir.join("calls.log");
    let script = format!(
        r#"#!/bin/sh
echo "$@" >> "{log}"
case "$2" in
  agora-dvh.10) printf '%s' '[{{"id":"agora-dvh.10","title":"Attention Dashboard","priority":1,"status":"in_progress"}}]';;
  agora-9nv) printf '%s' '[{{"id":"agora-9nv","title":"Kill 宽限期提示","priority":3,"status":"open"}}]';;
  *) echo "Error: no issue found" >&2; exit 1;;
esac
"#,
        log = log.display()
    );
    std::fs::write(&bin, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn calls(dir: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(dir.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.split_whitespace().map(str::to_owned).collect())
        .collect()
}

#[test]
fn only_read_only_subcommands_ever_reach_bd() {
    // 不变量 12：agora 对 beads 零写入。命中、未命中、非 id 的 task_ref 三条路都走一遍，
    // 录下来的每一次调用都只能是 READ_ONLY 里的子命令。
    let dir = tempfile::tempdir().unwrap();
    let bd = fake_bd(dir.path());
    let index = Arc::new(TaskIndex::new(bd.to_str().unwrap()).synchronous());

    let hit = index.get(dir.path(), "agora-dvh.10").unwrap();
    assert_eq!(
        (hit.title.as_str(), hit.priority),
        ("Attention Dashboard", 1)
    );
    assert!(index.get(dir.path(), "agora-nope").is_none());
    assert!(index.get(dir.path(), "把 sidebar 换成可折叠的").is_none());

    let recorded = calls(dir.path());
    assert_eq!(
        recorded.len(),
        2,
        "非 id 的 task_ref 不该敲 bd: {recorded:?}"
    );
    for argv in &recorded {
        assert!(
            READ_ONLY.contains(&argv[0].as_str()),
            "对 beads 只准读: {argv:?}"
        );
        assert_eq!(argv.last().map(String::as_str), Some("--json"));
    }
    assert_eq!(
        READ_ONLY,
        &["show"],
        "改 READ_ONLY 等于改不变量 12，先改 MISSION"
    );
}

#[test]
fn answers_including_misses_are_cached() {
    // list() 每 2 s 一轮：同一个 issue 不能每轮都敲 Dolt；"没这个 issue"也要记住。
    let dir = tempfile::tempdir().unwrap();
    let bd = fake_bd(dir.path());
    let index = Arc::new(
        TaskIndex::new(bd.to_str().unwrap())
            .synchronous()
            .with_ttl(Duration::from_secs(300)),
    );
    for _ in 0..3 {
        assert!(index.get(dir.path(), "agora-dvh.10").is_some());
        assert!(index.get(dir.path(), "agora-nope").is_none());
    }
    assert_eq!(calls(dir.path()).len(), 2);
}

fn mgr(index: Arc<TaskIndex>) -> (SessionManager, Arc<FakeRuntime>) {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(db, rt.clone() as Arc<dyn Runtime>).with_task_index(index);
    (m, rt)
}

fn session(agent: &str, cwd: &Path, task_ref: Option<&str>) -> NewSession {
    NewSession {
        display_name: "s".into(),
        agent_type: agent.into(),
        working_directory: cwd.to_path_buf(),
        worktree: None,
        task_ref: task_ref.map(str::to_owned),
        command: "sleep 300".into(),
        env: vec![],
        size: Default::default(),
    }
}

#[test]
fn a_session_with_an_issue_id_carries_title_and_priority() {
    // A23：每行显示任务（issue id + 标题），同分按 bd 优先级排——优先级得先到前端手里。
    let dir = tempfile::tempdir().unwrap();
    let bd = fake_bd(dir.path());
    let (m, _rt) = mgr(Arc::new(TaskIndex::new(bd.to_str().unwrap()).synchronous()));
    let v = m
        .create(&session("shell", dir.path(), Some("agora-9nv")))
        .unwrap();
    let task = v.task.expect("像 issue id 的 task_ref 查得到标签");
    assert_eq!(
        (task.id.as_str(), task.title.as_str(), task.priority),
        ("agora-9nv", "Kill 宽限期提示", 3)
    );
    let listed = m.list().unwrap();
    assert_eq!(listed[0].task.as_ref().map(|t| t.priority), Some(3));
}

#[test]
fn first_prompt_becomes_the_task_summary_but_never_overwrites_an_explicit_ref() {
    // ADR-002 D8：task_ref 摘要 = 首条 prompt 的首行。只补空的；对话框里填的不动。
    let dir = tempfile::tempdir().unwrap();
    let (m, _rt) = mgr(Arc::new(TaskIndex::new("/nonexistent/bd").synchronous()));
    let blank = m.create(&session("claude", dir.path(), None)).unwrap();
    let explicit = m
        .create(&session("claude", dir.path(), Some("agora-dvh.10")))
        .unwrap();
    for v in [&blank, &explicit] {
        m.apply_hook(
            &v.record.id,
            v.record.epoch,
            &[AgoraEvent::PromptSubmitted(
                "\n  把 sidebar 的 workspace chip 换成可折叠的\n第二行不算".into(),
            )],
        )
        .unwrap();
        m.apply_hook(
            &v.record.id,
            v.record.epoch,
            &[AgoraEvent::PromptSubmitted("第二条 prompt".into())],
        )
        .unwrap();
    }
    assert_eq!(
        m.get(&blank.record.id).unwrap().record.task_ref.as_deref(),
        Some("把 sidebar 的 workspace chip 换成可折叠的")
    );
    assert_eq!(
        m.get(&explicit.record.id)
            .unwrap()
            .record
            .task_ref
            .as_deref(),
        Some("agora-dvh.10")
    );
}

#[test]
fn two_lines_come_from_hooks_and_pane_preview_only_for_hookless_sessions() {
    // MISSION §6.3：❯ / ↳ 读自 hook 事件，不读 pane；没有 hook 的会话保持一行 pane preview。
    let dir = tempfile::tempdir().unwrap();
    let (m, rt) = mgr(Arc::new(TaskIndex::new("/nonexistent/bd").synchronous()));
    let hooked = m.create(&session("claude", dir.path(), None)).unwrap();
    let plain = m.create(&session("shell", dir.path(), None)).unwrap();
    for v in [&hooked, &plain] {
        rt.tails.lock().unwrap().insert(
            v.record.runtime_ref.clone().unwrap(),
            "$ cargo test\n\x1b[32mtest result: ok\x1b[0m. 12 passed\n\n".into(),
        );
    }
    m.apply_hook(
        &hooked.record.id,
        hooked.record.epoch,
        &[
            AgoraEvent::PromptSubmitted("把 sidebar 换掉".into()),
            AgoraEvent::Activity("Edit web/src/Sidebar.tsx".into()),
        ],
    )
    .unwrap();

    let h = m.get(&hooked.record.id).unwrap();
    assert_eq!(h.prompt.as_deref(), Some("把 sidebar 换掉"));
    assert_eq!(h.progress.as_deref(), Some("Edit web/src/Sidebar.tsx"));
    assert_eq!(h.preview, None, "有 hook 的会话不读 pane");

    let p = m.get(&plain.record.id).unwrap();
    assert_eq!((p.prompt.as_deref(), p.progress.as_deref()), (None, None));
    assert_eq!(
        p.preview.as_deref(),
        Some("test result: ok. 12 passed"),
        "最后一个非空行，ANSI 已剥掉"
    );

    // turn.ended 的最后一条回复替换 ↳；下一条 prompt 又把它清掉（那是上一轮的话）。
    m.apply_hook(
        &hooked.record.id,
        hooked.record.epoch,
        &[AgoraEvent::TurnEnded(Some(
            "改完了，144 个 e2e 全绿".into(),
        ))],
    )
    .unwrap();
    assert_eq!(
        m.get(&hooked.record.id).unwrap().progress.as_deref(),
        Some("改完了，144 个 e2e 全绿")
    );
    m.apply_hook(
        &hooked.record.id,
        hooked.record.epoch,
        &[AgoraEvent::PromptSubmitted("push 吧".into())],
    )
    .unwrap();
    let h = m.get(&hooked.record.id).unwrap();
    assert_eq!((h.prompt.as_deref(), h.progress), (Some("push 吧"), None));
}
