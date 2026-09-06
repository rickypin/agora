//! 任务的验收标准只读不存（MISSION §6.3 看结果；A40；不变量 12；agora-h1k.3）。
//!
//! `bd show --json` 的 acceptance_criteria 随 `task` 字段进 API，但 agora 的库里只有 task_ref：
//! 关掉 `parse_show` 里对 acceptance_criteria 的读取、或把它写进 sessions 表的任何一列，这里就红。

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agora::runtime::{Runtime, Size};
use agora::session::{Db, NewSession, SessionManager};
use agora::task::TaskIndex;
use common::FakeRuntime;

const ACCEPTANCE: &str =
    "tests/task_info.rs::acceptance_is_read_not_stored（库里无该字段、API 有）；\n前端 vitest：展开显示与折叠。";

/// 一个假 `bd`：`show agora-h1k.3 --json` 带验收标准；`show agora-9nv --json` 没写验收标准。
fn fake_bd(dir: &Path) -> PathBuf {
    let bin = dir.join("bd");
    let acceptance = ACCEPTANCE.replace('\n', "\\n");
    let script = format!(
        r#"#!/bin/sh
case "$2" in
  agora-h1k.3) printf '%s' '[{{"id":"agora-h1k.3","title":"验收标准","priority":2,"status":"in_progress","acceptance_criteria":"{acceptance}"}}]';;
  agora-9nv) printf '%s' '[{{"id":"agora-9nv","title":"Kill 宽限期提示","priority":3,"status":"open","acceptance_criteria":""}}]';;
  *) echo "Error: no issue found" >&2; exit 1;;
esac
"#
    );
    std::fs::write(&bin, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn session(cwd: &Path, task_ref: &str) -> NewSession {
    NewSession {
        display_name: "s".into(),
        agent_type: "shell".into(),
        working_directory: cwd.to_path_buf(),
        worktree: None,
        task_ref: Some(task_ref.into()),
        command: "sleep 300".into(),
        env: vec![],
        size: Size::default(),
    }
}

#[test]
fn acceptance_is_read_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let bd = fake_bd(dir.path());
    let db = Arc::new(Db::open_in_memory().unwrap());
    let rt = Arc::new(FakeRuntime::default());
    let m = SessionManager::new(db.clone(), rt as Arc<dyn Runtime>)
        .with_task_index(Arc::new(TaskIndex::new(bd.to_str().unwrap()).synchronous()));

    // API 有：task 里带 acceptance 全文，多行原样。
    let v = m.create(&session(dir.path(), "agora-h1k.3")).unwrap();
    let task = v.task.clone().expect("像 issue id 的 task_ref 查得到");
    assert_eq!(task.acceptance.as_deref(), Some(ACCEPTANCE));
    let wire = serde_json::to_value(&v).unwrap();
    assert_eq!(wire["task"]["acceptance"], ACCEPTANCE);
    assert_eq!(wire["task"]["id"], "agora-h1k.3");
    // 没写验收标准 → null，前端据此不占位。
    let none = m.create(&session(dir.path(), "agora-9nv")).unwrap();
    assert_eq!(none.task.as_ref().unwrap().acceptance, None);
    assert!(serde_json::to_value(&none).unwrap()["task"]["acceptance"].is_null());

    // 库里无：sessions 表没有任何一列叫 acceptance / title / priority 之类的任务字段……
    let conn = db.conn();
    let mut stmt = conn.prepare("PRAGMA table_info(sessions)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for forbidden in ["acceptance", "title", "priority", "task_status"] {
        assert!(
            !cols.iter().any(|c| c.contains(forbidden)),
            "sessions 长出了任务内容列 {forbidden}: {cols:?}（不变量 12：只存 task_ref）"
        );
    }
    // ……任何一列的值里也没有验收标准的文字；task_ref 就是那个 id。
    let mut all = conn
        .prepare(&format!(
            "SELECT {} FROM sessions WHERE id = ?1",
            cols.join(", ")
        ))
        .unwrap();
    let values: Vec<Option<String>> = all
        .query_row([&v.record.id], |r| {
            (0..cols.len())
                .map(|i| {
                    r.get::<_, rusqlite::types::Value>(i).map(|x| match x {
                        rusqlite::types::Value::Text(s) => Some(s),
                        rusqlite::types::Value::Integer(n) => Some(n.to_string()),
                        rusqlite::types::Value::Real(f) => Some(f.to_string()),
                        _ => None,
                    })
                })
                .collect()
        })
        .unwrap();
    for (col, val) in cols.iter().zip(&values) {
        if let Some(val) = val {
            assert!(
                !val.contains("acceptance_is_read_not_stored") && !val.contains("展开显示"),
                "验收标准的文字被复制进了 sessions.{col}: {val:?}"
            );
        }
    }
    assert_eq!(v.record.task_ref.as_deref(), Some("agora-h1k.3"));
    // 其它表也没有：整个库里只有 task_ref 这一处关联。
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        !tables.iter().any(|t| t.contains("task")),
        "多出了任务表: {tables:?}"
    );
}
