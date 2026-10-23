//! 宿主注入 prompt 的真实形状（agora-5gg.13 / agora-tsgg；2026-10-24 本机取证）。
//!
//! Codex Desktop 的 ambient suggestion 任务以 `# Overview` 单独一行开头，2026-10-23 在本机
//! 真投递三件（`~/.agora/hooks/done/codex/01a1695c-…` 等，model=gpt-6-luna）。这份 payload 没有
//! 版本目录可放（`testdata/` 只允许七场景齐全或冒烟-only 两种形状，见 `tests/fixtures_replay.rs`），
//! 所以用 `adapter::replay` 的内联序列；回放器与 `testdata/` 是同一个。

mod common;

use std::sync::Arc;

use agora::adapter;
use agora::runtime::Runtime;
use agora::session::{Db, NewSession, SessionManager};
use agora::status::{AgoraEvent, Status};
use common::FakeRuntime;
use serde_json::json;

/// 2026-10-23 23:23:25 真投递的 prompt 开头；正文只留到第二段，后面是宿主的长指令。
const CODEX_DESKTOP_OVERVIEW: &str = concat!(
    "# Overview\n\n",
    "Generate 0 to 3 hyperpersonalized suggestions for what this user can do with Codex",
    " in this Projectless task\n\n\n",
    "Get an understanding of the user's intent and goals by deeply viewing their connected apps.",
);

fn prompt_payload() -> serde_json::Value {
    json!({
        "session_id": "01a1695c-07e2-7da2-88c8-6331b49f0bda",
        "turn_id": "01a1695c-0900-7ac0-9f45-60c1b269b94c",
        "transcript_path": null,
        "cwd": "/",
        "hook_event_name": "UserPromptSubmit",
        "model": "gpt-6-luna",
        "permission_mode": "bypassPermissions",
        "prompt": CODEX_DESKTOP_OVERVIEW,
    })
}

#[test]
fn codex_desktop_ambient_suggestion_replays_without_a_prompt() {
    let hooks = adapter::for_host("codex").expect("codex 有 hook adapter");
    let session_start = json!({
        "session_id": "01a1695c-07e2-7da2-88c8-6331b49f0bda",
        "transcript_path": null,
        "cwd": "/",
        "hook_event_name": "SessionStart",
        "model": "gpt-6-luna",
        "permission_mode": "bypassPermissions",
        "source": "startup",
    });
    let text = [
        "# 2026-10-23 本机真投递（脱敏后的键集合与开头，正文宿主长指令未抄全）".to_owned(),
        json!({
            "at": 0,
            "hold": false,
            "payload": session_start,
            "expect": {"status": "starting", "source": "hook", "min_confidence": 1.0},
        })
        .to_string(),
        json!({
            "at": 1,
            "hold": false,
            "payload": prompt_payload(),
            "expect": {"status": "running", "source": "hook", "prompt": null},
        })
        .to_string(),
    ]
    .join("\n");

    let states = adapter::replay::replay(hooks, &text).expect("真投递件要能回放");
    assert_eq!(states[1].status, Status::Running);
}

#[test]
fn codex_desktop_ambient_suggestion_never_becomes_task_ref_or_prompt() {
    let hooks = adapter::for_host("codex").expect("codex 有 hook adapter");
    let events = hooks.parse(&prompt_payload());
    assert_eq!(events, vec![AgoraEvent::PromptInjected]);

    let dir = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(
        Arc::new(Db::open_in_memory().unwrap()),
        Arc::new(FakeRuntime::default()) as Arc<dyn Runtime>,
    );
    let view = manager
        .create(&NewSession {
            display_name: "s".into(),
            agent_type: "codex".into(),
            working_directory: dir.path().to_path_buf(),
            worktree: None,
            task_ref: None,
            command: "sleep 300".into(),
            env: vec![],
            size: Default::default(),
        })
        .unwrap();
    manager
        .apply_hook(&view.record.id, view.record.epoch, &events)
        .unwrap();

    let got = manager.get(&view.record.id).unwrap();
    assert_eq!(got.record.task_ref, None, "注入的 # Overview 不当任务摘要");
    assert_eq!(got.prompt, None, "❯ 行也不拿注入的 prompt");
    assert_eq!(got.assessment.status, Status::Running, "一轮照常开始");
}
