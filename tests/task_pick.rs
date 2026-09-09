//! 从就绪任务起会话（MISSION §6.4「从就绪任务起会话」；A43；不变量 12；agora-h1k.2）。
//!
//! `GET /api/projects/tasks?path=` 读 `bd ready --json`，New Agent 对话框据此预填 task_ref /
//! Name / worktree 名 / 首条 prompt。**agora 对 beads 零写入**：用一个假 `bd` 把每次调用的 argv
//! 录下来，整条链路敲到 bd 的只能是 `ready --json`（与会话行标签的 `show <id> --json`），
//! 一次 update / close / create / claim 都没有——关掉 `READ_ONLY` 的限制、给 `ready` 加 `--claim`
//! 或在别处起 `bd`，这里与 `tests/arch_boundary.rs::beads_is_read_only_and_lives_in_task` 就红。
//!
//! 本批不改 tests/common（合并归 agora-7ku.9）：假 bd 与 argv 账本的辅助从 tests/task_beads.rs
//! 复制过来。

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agora::project::Projects;
use agora::runtime::{
    AttachSpec, LaunchSpec, Runtime, RuntimeError, RuntimeRef, RuntimeSession, Size,
    TerminateSignal,
};
use agora::session::SessionManager;
use agora::task::{TaskIndex, READ_ONLY};
use common::isolate;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use common::{FakeRuntime, Fx, HOST};

/// 一个假 `bd`：把 argv 追加到 `<dir>/calls.log`；`ready --json` 回一个 epic + 两个 task，
/// `show <id> --json` 按 id 回答（会话行的标签查询会走到它），别的子命令一律退出 2——
/// 真 bd 对 update / close 是会成功的，假的故意不成功，免得"写了但没人发现"。
fn fake_bd(dir: &Path) -> PathBuf {
    let bin = dir.join("bd");
    let log = dir.join("calls.log");
    let script = format!(
        r#"#!/bin/sh
echo "$@" >> "{log}"
case "$1" in
  ready) printf '%s' '[{{"id":"agora-h1k","title":"M3: 产出与起会话增强","priority":2,"issue_type":"epic","status":"open"}},{{"id":"agora-h1k.2","title":"从 bd ready 选任务起会话","priority":2,"issue_type":"task","status":"open"}},{{"id":"agora-q8x","title":"hook_recovery 假阴性","priority":1,"issue_type":"bug","status":"open"}}]';;
  show) printf '%s' '[{{"id":"agora-h1k.2","title":"从 bd ready 选任务起会话","priority":2,"status":"open"}}]';;
  *) echo "fake bd: refusing $1" >&2; exit 2;;
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

/// 一个只会失败的假 `bd`：`exit 1` 模拟"目录没有 beads"；退出 0 但吐非 JSON 模拟坏输出。
fn failing_bd(dir: &Path, name: &str, body: &str) -> PathBuf {
    let bin = dir.join(name);
    std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
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

/// 一套 API：`tmp/<repo>` 是已知项目（roots = tmp），会话管理器的 beads 入口指向给定的 bd。
fn fx_with_bd(tmp: &Path, bd: &Path) -> (Fx, String) {
    let mut fx = Fx::new();
    // 假 bd 是 `#!/bin/sh`：满载时 fork+exec 一次就可能超过 TaskIndex 默认的 10 s 超时，超时按
    // "没这个 issue"返回 None，断言就在别处假阴性（同一条病与理由见 tests/task_beads.rs 的
    // FAKE_BD_TIMEOUT；2026-09-09 那批红的是本文件三条，agora-pea）。别去改 TaskIndex 的默认值。
    let index = Arc::new(
        TaskIndex::new(bd.to_str().unwrap())
            .synchronous()
            .with_timeout(isolate::PROC),
    );
    let sessions = Arc::new(
        SessionManager::new(fx.db.clone(), fx.rt.clone() as Arc<dyn Runtime>)
            .with_task_index(index),
    );
    fx.sessions = sessions.clone();
    fx.state.sessions = sessions;
    fx.state.projects = Arc::new(Projects::new(fx.db.clone(), vec![tmp.to_path_buf()]));
    let cookie = fx.cookie();
    (fx, cookie)
}

async fn call(
    fx: &Fx,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::COOKIE, cookie);
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = fx.app().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn tasks_url(repo: &Path) -> String {
    format!(
        "/api/projects/tasks?path={}",
        urlencode(&repo.to_string_lossy())
    )
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn ready_tasks_listed_from_bd_ready_json() {
    // 假 bd 的 ready 输出含一个 epic + 两个 task：响应只有两个 task、字段齐、reason null。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let bd = fake_bd(tmp.path());
    let (fx, cookie) = fx_with_bd(tmp.path(), &bd);

    let (status, body) = call(&fx, &cookie, Method::GET, &tasks_url(&repo), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["reason"].is_null(), "{body}");
    assert_eq!(
        body["tasks"],
        json!([
            { "id": "agora-h1k.2", "title": "从 bd ready 选任务起会话", "priority": 2, "type": "task" },
            { "id": "agora-q8x", "title": "hook_recovery 假阴性", "priority": 1, "type": "bug" },
        ]),
        "epic 不是可起会话的任务，顺序照 bd 给的"
    );

    // 零写入守卫（不变量 12；A43）：整条 /api/projects/tasks 链路敲到 bd 的只有 `ready --json`。
    assert_eq!(
        calls(tmp.path()),
        vec![vec!["ready".to_owned(), "--json".to_owned()]],
        "只读端点带上 --claim 就会认领第一条"
    );

    // path 与 GET /api/projects/worktrees 同一"已知项目"校验：不是 → 400 bad_request，且不敲 bd。
    let elsewhere = tempfile::tempdir().unwrap();
    let (status, body) = call(
        &fx,
        &cookie,
        Method::GET,
        &tasks_url(elsewhere.path()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "bad_request");
    assert_eq!(calls(tmp.path()).len(), 1, "未知目录不该去敲 bd");
}

#[tokio::test]
async fn missing_bd_yields_empty_with_typed_reason() {
    // 没装 bd / 目录没有 beads / 输出不是 JSON：都是 200 + 空列表 + 类型化的 reason，
    // 前端按类型给文案（MISSION §2.3 规则 10），对话框退回一句话，会话照样能起。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    let cases: [(PathBuf, &str); 3] = [
        (PathBuf::from("/nonexistent/agora-no-such-bd"), "no_bd"),
        (
            failing_bd(
                tmp.path(),
                "bd-no-beads",
                "echo 'Error: no beads database found' >&2; exit 1",
            ),
            "no_beads",
        ),
        (
            failing_bd(tmp.path(), "bd-garbage", "echo 'not json at all'"),
            "bad_output",
        ),
    ];
    for (bd, reason) in &cases {
        let (fx, cookie) = fx_with_bd(tmp.path(), bd);
        let (status, body) = call(&fx, &cookie, Method::GET, &tasks_url(&repo), None).await;
        assert_eq!(status, StatusCode::OK, "{reason}: {body}");
        assert_eq!(body["tasks"], json!([]), "{reason}: {body}");
        assert_eq!(body["reason"], *reason, "{body}");
    }
}

/// 记下 `create` 收到的 `LaunchSpec.command`：common::FakeRuntime 只记 respawn 的命令，而首条
/// prompt 只在第一代的启动命令里出现（本批不改 tests/common，包一层）。
struct RecordingRuntime {
    inner: FakeRuntime,
    launches: Mutex<Vec<String>>,
}

impl Runtime for RecordingRuntime {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
    fn create(&self, spec: &LaunchSpec) -> Result<RuntimeRef, RuntimeError> {
        self.launches.lock().unwrap().push(spec.command.clone());
        self.inner.create(spec)
    }
    fn list(&self) -> Result<Vec<RuntimeSession>, RuntimeError> {
        self.inner.list()
    }
    fn inspect(&self, r: &RuntimeRef) -> Result<RuntimeSession, RuntimeError> {
        self.inner.inspect(r)
    }
    fn attach(&self, r: &RuntimeRef, s: Size) -> Result<AttachSpec, RuntimeError> {
        self.inner.attach(r, s)
    }
    fn capture_tail(&self, r: &RuntimeRef, n: u32) -> Result<Vec<u8>, RuntimeError> {
        self.inner.capture_tail(r, n)
    }
    fn terminate(&self, r: &RuntimeRef, g: Duration) -> Result<(), RuntimeError> {
        self.inner.terminate(r, g)
    }
    fn signal(&self, r: &RuntimeRef, sig: TerminateSignal) -> Result<(), RuntimeError> {
        self.inner.signal(r, sig)
    }
    fn respawn(&self, r: &RuntimeRef, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        self.inner.respawn(r, spec)
    }
    fn remove(&self, r: &RuntimeRef) -> Result<(), RuntimeError> {
        self.inner.remove(r)
    }
    fn send_input(&self, r: &RuntimeRef, d: &str) -> Result<(), RuntimeError> {
        self.inner.send_input(r, d)
    }
    fn locate(
        &self,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<RuntimeRef>, RuntimeError> {
        self.inner.locate(env)
    }
}

/// 对话框的 prompt 模板（原文在 docs/spec/ux.md；前端 `web/src/taskPrompt.ts`）——多行、反引号、
/// 括号，没有单引号，所以整段落在一对单引号里。
const PROMPT: &str = "任务 agora-h1k.2：从 bd ready 选任务起会话\n\n先 `bd update agora-h1k.2 --claim`，再 `bd show agora-h1k.2` 读任务书（description / notes / acceptance）。\n按 AGENTS.md 的任务纪律干活：commit subject 末尾带 (agora-h1k.2)；干活中发现的别的问题用 `bd create ... --deps discovered-from:agora-h1k.2` 另立。\n做完 `bd close agora-h1k.2 --reason \"<证据>\"` 写证据。";

#[tokio::test]
async fn session_created_from_task_prefills_ref_name_and_prompt() {
    // 对话框选了任务：task_ref = issue id、display_name = 标题、prompt = 模板。prompt 只进这一代的
    // 启动命令（单引号包住接在命令尾），库里的 command 不带它，Restart 也不重发。
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let bd = fake_bd(tmp.path());

    let mut fx = Fx::new();
    let rt = Arc::new(RecordingRuntime {
        inner: FakeRuntime::default(),
        launches: Mutex::new(Vec::new()),
    });
    // 假 bd 是 `#!/bin/sh`：满载时 fork+exec 一次就可能超过 TaskIndex 默认的 10 s 超时，超时按
    // "没这个 issue"返回 None，断言就在别处假阴性（同一条病与理由见 tests/task_beads.rs 的
    // FAKE_BD_TIMEOUT；2026-09-09 那批红的是本文件三条，agora-pea）。别去改 TaskIndex 的默认值。
    let index = Arc::new(
        TaskIndex::new(bd.to_str().unwrap())
            .synchronous()
            .with_timeout(isolate::PROC),
    );
    let sessions = Arc::new(
        SessionManager::new(fx.db.clone(), rt.clone() as Arc<dyn Runtime>).with_task_index(index),
    );
    fx.sessions = sessions.clone();
    fx.state.sessions = sessions;
    fx.state.projects = Arc::new(Projects::new(fx.db.clone(), vec![tmp.path().to_path_buf()]));
    let cookie = fx.cookie();

    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({
            "display_name": "从 bd ready 选任务起会话",
            "agent_type": "claude",
            "working_directory": repo.to_string_lossy(),
            "task_ref": "agora-h1k.2",
            "prompt": PROMPT,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let gid = body["id"].as_str().unwrap().to_owned();
    assert_eq!(body["task_ref"], "agora-h1k.2");
    assert_eq!(body["display_name"], "从 bd ready 选任务起会话");
    assert_eq!(
        body["task"]["title"], "从 bd ready 选任务起会话",
        "task_ref 像 issue id：标签照常经 bd show 补齐 {body}"
    );

    // 运行时收到的启动命令：原命令 + 单引号包住的 prompt。
    let launched = rt.launches.lock().unwrap().clone();
    assert_eq!(launched.len(), 1, "{launched:?}");
    assert_eq!(launched[0], format!("claude '{PROMPT}'"));

    // 库里的命令不带 prompt。
    let (status, body) = call(
        &fx,
        &cookie,
        Method::GET,
        &format!("/api/sessions/{gid}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["command"], "claude", "prompt 不进库 {body}");
    assert_eq!(body["task_ref"], "agora-h1k.2");

    // Restart 用库里的命令：重发 prompt 会让 agent 把任务从头再做一遍。
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        &format!("/api/sessions/{gid}/restart"),
        Some(json!({ "confirmed": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let respawns = rt.inner.respawns.lock().unwrap().clone();
    assert_eq!(respawns, vec!["claude".to_owned()], "Restart 不重发 prompt");
    assert_eq!(
        rt.launches.lock().unwrap().len(),
        1,
        "Restart 走 respawn，不是新建"
    );

    // 不接受首条 prompt 的 agent 类型（custom 没有 Adapter）→ 400 bad_request，什么都不起。
    let (status, body) = call(
        &fx,
        &cookie,
        Method::POST,
        "/api/sessions",
        Some(json!({
            "display_name": "x",
            "agent_type": "custom",
            "working_directory": repo.to_string_lossy(),
            "command": "my-agent",
            "prompt": PROMPT,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "bad_request");
    assert_eq!(rt.launches.lock().unwrap().len(), 1);

    // 零写入：整个流程敲到 bd 的只有 READ_ONLY 里的子命令（标签查询的 show），没有 claim。
    let recorded = calls(tmp.path());
    assert!(!recorded.is_empty(), "task_ref 像 issue id 应查过 bd show");
    for argv in &recorded {
        assert!(
            READ_ONLY.contains(&argv[0].as_str()),
            "对 beads 只准读: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.contains("claim")),
            "claim 是 agent 自己的事: {argv:?}"
        );
    }
}

#[test]
fn read_only_whitelist_is_exactly_show_and_ready() {
    // 白名单本身也钉死：多一个子命令就要先改 MISSION 不变量 12 / §6.4。
    assert_eq!(READ_ONLY, &["show", "ready"]);
}
