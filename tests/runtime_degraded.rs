//! ADR-001 D7 / agora-xqa.4：运行时版本过低、或 client / server 协议不匹配时，agora 对那个
//! socket 失明——此时 `/api/health` 的 runtime 报 degraded 并给出原因，已有会话报 UNKNOWN，
//! daemon 照常应答；运行时恢复后自己转回 ok，不必重启 daemon。
//!
//! 用假 tmux 二进制模拟，不依赖本机装了什么版本：一个 mode 文件切换它的行为
//! （`ok` / `mismatch` / `old`），socket 自己 bind 一个，好让 `server_running` 成立。

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use agora::api::AppState;
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::runtime::tmux::{socket_path, TmuxConfig, TmuxRuntime};
use agora::runtime::{Runtime, RuntimeError};
use agora::session::{Db, NewSession, SessionManager};
use agora::status::Status;

const HOST: &str = "127.0.0.1:7680";

struct Fake {
    state: AppState,
    sessions: Arc<SessionManager>,
    auth: Arc<Auth>,
    mode: std::path::PathBuf,
    name_file: std::path::PathBuf,
    socket: String,
    _listener: UnixListener,
    _dir: tempfile::TempDir,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(socket_path(&self.socket));
    }
}

fn fake(tag: &str, mode0: &str) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("tmux");
    let mode = dir.path().join("mode");
    let name_file = dir.path().join("name");
    std::fs::write(&mode, mode0).unwrap();
    std::fs::write(&name_file, "ag-none").unwrap();

    // 与 src/runtime/tmux/mod.rs 的 SEP 一致；12 段，最后一段是 title。
    let sep = "|#|";
    let pane = [
        "$NAME", "4242", "0", "", "", "", "0", "160", "48", "fakehost", "/tmp", "",
    ]
    .join(sep);
    let body = format!(
        "#!/bin/sh\n# 假 tmux（tests/runtime_degraded.rs）\n\
         mode=$(cat '{m}')\nNAME=$(cat '{n}')\ncase \"$*\" in\n  \
         *list-panes*)\n    \
         if [ \"$mode\" = mismatch ]; then\n      \
         echo 'protocol version mismatch (client 8, server 7)' >&2\n      exit 1\n    fi\n    \
         printf '%s\\n' \"{pane}\" ;;\n  \
         -V)\n    \
         if [ \"$mode\" = old ]; then echo 'tmux 3.0'; else echo 'tmux 3.7c'; fi ;;\n  \
         *) exit 0 ;;\nesac\n",
        m = mode.display(),
        n = name_file.display()
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // `server_running` 靠能不能 connect 上 socket 判断：自己 bind 一个，好让"server 在、
    // 但它拒绝应答"这个状态成立——这正是协议不匹配时的样子。
    let socket = format!("agora-degraded-{}-{tag}", std::process::id());
    let path = socket_path(&socket);
    let sockets_dir = path.parent().unwrap();
    if !sockets_dir.exists() {
        std::fs::create_dir_all(sockets_dir).unwrap();
        std::fs::set_permissions(sockets_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();

    let rt = Arc::new(
        TmuxRuntime::new(TmuxConfig {
            bin: script.to_string_lossy().into_owned(),
            socket: socket.clone(),
            adopt_sockets: vec![],
            conf_path: dir.path().join("tmux.conf"),
            ..Default::default()
        })
        .unwrap(),
    );
    let db = Arc::new(Db::open_in_memory().unwrap());
    let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
    let sessions = Arc::new(SessionManager::new(
        db.clone(),
        rt.clone() as Arc<dyn Runtime>,
    ));
    // daemon 启动时的一次版本探测：出错只降级，不退出（src/main.rs 走的是同一条 observe）。
    sessions.runtime_status().observe(&rt.check_version());
    let state = AppState::new(auth.clone(), sessions.clone(), "testnode");
    Fake {
        state,
        sessions,
        auth,
        mode,
        name_file,
        socket,
        _listener: listener,
        _dir: dir,
    }
}

impl Fake {
    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode, mode).unwrap();
    }

    /// 让假 tmux 的 list-panes 报出这个会话名。
    fn announce(&self, name: &str) {
        std::fs::write(&self.name_file, name).unwrap();
    }

    fn cookie(&self) -> String {
        let token = self.auth.mint_pair_token(PairedVia::Socket).unwrap();
        let (_, plain) = self
            .auth
            .redeem(
                &token,
                Some("Mozilla/5.0 (Macintosh) Chrome/1 Safari/1"),
                None,
            )
            .unwrap();
        format!("agora_session={plain}")
    }

    /// 完整 health 报告（要 principal）。
    async fn health(&self) -> serde_json::Value {
        let resp = agora::api::router(self.state.clone())
            .oneshot(
                Request::get("/api/health")
                    .header(header::HOST, HOST)
                    .header(header::COOKIE, self.cookie())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // daemon 存活：降级期间 health 仍然是 200，不是 5xx。
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
}

fn spec(name: &str) -> NewSession {
    NewSession {
        display_name: name.into(),
        agent_type: "shell".into(),
        working_directory: std::env::temp_dir(),
        worktree: None,
        task_ref: None,
        command: "sleep 300".into(),
        env: vec![],
        size: agora::runtime::Size::default(),
    }
}

#[tokio::test]
async fn protocol_mismatch_degrades_health_and_reports_unknown_then_recovers() {
    let f = fake("mismatch", "ok");
    let m = f.sessions.clone();
    let s = m.create(&spec("剧本-shell")).unwrap();
    let rt_ref = s.record.runtime_ref.clone().unwrap();
    f.announce(rt_ref.rsplit(':').next().unwrap());

    // 正常时：health ok，会话活着。
    let h = f.health().await;
    assert_eq!(h["runtime"]["status"], "ok");
    assert!(h["runtime"]["reason"].is_null());
    let v = m.get(&s.record.id).unwrap();
    assert!(v.alive);

    // server 在、但拒绝应答（client / server 协议不匹配）。
    f.set_mode("mismatch");
    let views = m.list().unwrap();
    assert_eq!(views.len(), 1, "失明不等于会话没了，列表照常给出全部会话");
    let v = &views[0];
    assert_eq!(v.assessment.status, Status::Unknown, "读不到只能报 UNKNOWN");
    let reason = v.assessment.reason.clone().unwrap_or_default();
    assert!(
        reason.contains("runtime unavailable") && reason.contains("protocol version mismatch"),
        "UNKNOWN 要带得上原因: {reason}"
    );
    assert!(
        v.record.ended_at.is_none(),
        "绝不能因为读不到就写上 ended_at"
    );

    let h = f.health().await;
    assert_eq!(h["runtime"]["status"], "degraded");
    assert!(
        h["runtime"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("protocol version mismatch"),
        "{:?}",
        h["runtime"]["reason"]
    );

    // 单个会话的读也一样：不报错，报 UNKNOWN。
    let one = m.get(&s.record.id).unwrap();
    assert_eq!(one.assessment.status, Status::Unknown);

    // server 换代之后自愈：不重启 daemon，下一次读就转回 ok。
    f.set_mode("ok");
    let v = m.get(&s.record.id).unwrap();
    assert!(v.alive);
    assert!(matches!(
        v.assessment.status,
        Status::Starting | Status::Running
    ));
    let h = f.health().await;
    assert_eq!(h["runtime"]["status"], "ok");
    assert!(h["runtime"]["reason"].is_null());
}

#[tokio::test]
async fn version_below_minimum_degrades_at_startup_without_exiting() {
    // tmux 3.0 < 下限 3.2：只降级，daemon 照常起、照常应答（ADR-001 D7）。
    let f = fake("old", "old");
    let h = f.health().await;
    assert_eq!(h["runtime"]["status"], "degraded");
    let reason = h["runtime"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("3.0") && reason.contains("下限"),
        "原因要说清是版本过低: {reason}"
    );
    // 公开子集不受影响：未认证仍然只看到 status ok（ADR-003 D1）。
    let resp = agora::api::router(f.state.clone())
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[test]
fn per_session_errors_do_not_degrade_the_runtime() {
    // NotFound / StillAlive / ReadOnly 是关于某个会话的回答，不是"运行时坏了"。
    let f = fake("per-session", "ok");
    let st = f.sessions.runtime_status();
    assert!(!st.is_degraded());
    for e in [
        RuntimeError::NotFound(agora::runtime::RuntimeRef("tmux:x:ag-1".into())),
        RuntimeError::StillAlive(agora::runtime::RuntimeRef("tmux:x:ag-1".into())),
        RuntimeError::ReadOnly(agora::runtime::RuntimeRef("tmux:x:ag-1".into())),
    ] {
        assert!(!e.degrades_runtime(), "{e}");
        st.observe(&Err::<(), _>(e));
        assert!(!st.is_degraded());
    }
    // 反过来，这三类是运行时整体不可信。
    for e in [
        RuntimeError::ServerUnavailable { reason: "x".into() },
        RuntimeError::VersionMismatch { reason: "x".into() },
        RuntimeError::Timeout("tmux".into()),
    ] {
        assert!(e.degrades_runtime(), "{e}");
    }
}

#[test]
fn daemon_startup_seeds_the_runtime_status() {
    // 上面两条测试自己调 observe，钉不住 daemon 有没有接线。这一条扫源码：启动时必须把
    // 版本探测的结论喂给 RuntimeStatus，否则"版本过低 → health degraded"在真 daemon 上
    // 根本不成立（照 tests/arch_boundary.rs 的做法，用文本守住一行接线）。
    let main =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs")).unwrap();
    assert!(
        main.contains("check_version()"),
        "daemon 启动要探一次运行时版本（ADR-001 D7）"
    );
    assert!(
        main.contains("runtime_status().observe("),
        "探测结论要喂给 RuntimeStatus，health 才看得见"
    );
    // 反面：探不到也只能降级，绝不退出。
    assert!(
        !main.contains("运行时降级\");\n        return 1"),
        "版本不满足只降级，不退 daemon"
    );
}
