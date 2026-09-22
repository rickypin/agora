//! ADR-001 D7 / agora-xqa.4：运行时版本过低、或 client / server 协议不匹配时，agora 对那个
//! socket 失明——此时 `/api/health` 的 runtime 报 degraded 并给出原因，已有会话报 UNKNOWN，
//! daemon 照常应答；运行时恢复后自己转回 ok，不必重启 daemon。
//!
//! 用假 tmux 二进制模拟，不依赖本机装了什么版本：一个 mode 文件切换它的行为
//! （`ok` / `mismatch` / `old`），socket 自己 bind 一个，好让 `server_running` 成立。

#[path = "common/isolate.rs"]
mod isolate;

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
use agora::session::{AdoptSession, Db, NewSession, SessionManager};
use agora::status::{ProcessState, Status, UnknownCause};

const HOST: &str = "127.0.0.1:7680";

struct Fake {
    state: AppState,
    sessions: Arc<SessionManager>,
    /// 同一个运行时 / 同一个库：`reconcile` 的守卫要拿它们再造一个 manager（模拟 daemon 重启）。
    rt: Arc<TmuxRuntime>,
    db: Arc<Db>,
    auth: Arc<Auth>,
    mode: std::path::PathBuf,
    name_file: std::path::PathBuf,
    socket: String,
    /// 采纳 socket（`Some` 时 `adopt_sockets` 里有它）；它的假 tmux 行为由 `adopt_fail` 控制。
    adopt_socket: Option<String>,
    /// 存在就让假 tmux 对采纳 socket 的 `list-panes` 退出非 0（server 仍在应答）。
    adopt_fail: std::path::PathBuf,
    _listener: UnixListener,
    _adopt_listener: Option<UnixListener>,
    _dir: tempfile::TempDir,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(socket_path(&self.socket));
        if let Some(s) = &self.adopt_socket {
            // 采纳 socket 也是自己 bind 的：留下死文件就是把名字留给下一个进程（AGENTS.md 隔离规矩）。
            let _ = std::fs::remove_file(socket_path(s));
        }
    }
}

fn fake(tag: &str, mode0: &str) -> Fake {
    fake_with(tag, mode0, None)
}

/// `adopt` 为 `Some(名字)` 时，这个 fixture 的运行时还采纳那个 socket（假 server 同样 bind 一个，
/// 好让「server 在、但它对 list-panes 退出非 0」这个状态成立——那正是"没扫到"与"会话没了"
/// 在返回形态上分不开的那一格，agora-dkv3）。
fn fake_with(tag: &str, mode0: &str, adopt: Option<&str>) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("tmux");
    let mode = dir.path().join("mode");
    let name_file = dir.path().join("name");
    let adopt_name = dir.path().join("adopt-name");
    let adopt_fail = dir.path().join("adopt-fail");
    std::fs::write(&mode, mode0).unwrap();
    std::fs::write(&name_file, "ag-none").unwrap();
    std::fs::write(&adopt_name, "mywork").unwrap();

    // 与 src/runtime/tmux/mod.rs 的 SEP 一致；13 段（第 10 段是 window_activity），最后一段是 title。
    let sep = "|#|";
    let tail = format!(
        "{sep}{}",
        ["4242", "0", "", "", "", "", "0", "160", "48", "0", "fakehost", "/tmp", ""].join(sep)
    );
    let body = format!(
        r#"#!/bin/sh
# 假 tmux（tests/runtime_degraded.rs）：主 socket 由 mode 文件驱动；
# 采纳 socket（参数里的 -L <名字> 对上）由 adopt-fail 文件驱动。
mode=$(cat '{m}')
NAME=$(cat '{n}')
ANAME=$(cat '{an}')
sock=""
prev=""
for a in "$@"; do
  [ "$prev" = "-L" ] && sock="$a"
  prev="$a"
done
case "$*" in
  *list-panes*)
    if [ "$sock" = '{a}' ]; then
      if [ -f '{af}' ]; then
        echo 'no servers found on the adopted socket' >&2
        exit 1
      fi
      printf '%s\n' "$ANAME{t}"
    else
      if [ "$mode" = mismatch ]; then
        echo 'protocol version mismatch (client 8, server 7)' >&2
        exit 1
      fi
      printf '%s\n' "$NAME{t}"
    fi ;;
  -V)
    if [ "$mode" = old ]; then echo 'tmux 3.0'; else echo 'tmux 3.7c'; fi ;;
  *) exit 0 ;;
esac
"#,
        m = mode.display(),
        n = name_file.display(),
        an = adopt_name.display(),
        a = adopt.unwrap_or("__no_adopt_socket__"),
        af = adopt_fail.display(),
        t = tail,
    );
    // 要 exec 的假 tmux 走 isolate::exec_script（裸 fs::write + chmod 造出来的脚本会让下一次
    // exec 按概率撞 ETXTBSY，agora-pmm2）；下面 bind_fake_server 给 sockets 目录补的 0700
    // 不是要 exec 的 fixture，不经 helper。
    isolate::exec_script(&script, &body);

    // `server_running` 靠能不能 connect 上 socket 判断：自己 bind 一个，好让"server 在、
    // 但它拒绝应答"这个状态成立——这正是协议不匹配时的样子。
    let socket = isolate::socket_name(&format!("deg-{tag}"), isolate::nth());
    let listener = bind_fake_server(&socket);
    let adopt_listener = adopt.map(bind_fake_server);
    let adopt_socket = adopt.map(str::to_owned);

    let rt = Arc::new(
        TmuxRuntime::new(TmuxConfig {
            bin: script.to_string_lossy().into_owned(),
            socket: socket.clone(),
            adopt_sockets: adopt_socket.clone().into_iter().collect(),
            conf_path: dir.path().join("tmux.conf"),
            // 假 tmux 是 `#!/bin/sh` 脚本（sh 再加两次 `$(cat …)` 子进程）。运行时的默认子进程
            // 超时是 5 s（ADR-001 D6，daemon 上是对的：一次 hang 住的调用不得拖住所有会话），
            // 但机器满载时 fork+exec 一个 sh 就可能超过 5 s：2026-09-06 两个 worktree 并行
            // `cargo test --all-targets`（8 核、CARGO_BUILD_JOBS=4 ×2）时，下面那次
            // `check_version()` 就返回了 `RuntimeError::Timeout`，`degrades_runtime()` 为
            // true，三条测试的起点已经 degraded——reason 是"运行时调用超时: …/tmux"而不是
            // "3.0 / 下限"（agora-74s）。这些测试钉的是 mode 文件（ok / mismatch / old）驱动的
            // 降级与自愈，与超时无关；把超时放宽到测试里等不到的长度，超时就不再是噪声。
            // 把这行去掉、或改回 `Duration::from_millis(1)`，三条测试会以同样的文本失败。
            // 别去改 DEFAULT_TIMEOUT、也别让 Timeout 不算降级——那是 D6 / D7 的设计。
            exec_timeout: std::time::Duration::from_secs(60),
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
        rt,
        db,
        auth,
        mode,
        name_file,
        socket,
        adopt_socket,
        adopt_fail,
        _listener: listener,
        _adopt_listener: adopt_listener,
        _dir: dir,
    }
}

/// bind 一个假 tmux server：只为让 `server_running`（一次 connect）成立，不读任何字节。
fn bind_fake_server(socket: &str) -> UnixListener {
    let path = socket_path(socket);
    let sockets_dir = path.parent().unwrap();
    if !sockets_dir.exists() {
        std::fs::create_dir_all(sockets_dir).unwrap();
        std::fs::set_permissions(sockets_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path).unwrap()
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

#[tokio::test]
async fn unscanned_adopt_socket_keeps_adopted_rows_unknown() {
    // agora-dkv3 的守卫：采纳 socket 这一次没扫成（假 tmux 对它的 list-panes 退出非 0，而
    // server 还在应答），那个 socket 上的 adopted 行只是"没看见"，不得读成"会话没了"：
    // 不得判 FINISHED、不得写 ended_at，要落 UNKNOWN `runtime unavailable`。
    // 把 view 里的「没扫到」改回当成「会话没了」（去掉 `unreadable` 那半条），这一条先红。
    let adopt = isolate::socket_name("deg-adopt", isolate::nth());
    let f = fake_with("unscanned", "ok", Some(&adopt));
    let mine = f.sessions.create(&spec("主 socket 的行")).unwrap();
    f.announce(
        mine.record
            .runtime_ref
            .as_deref()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap(),
    );

    // 采纳 socket 上的行：ref 的形态归运行时，测试不拼字符串，从 unregistered() 里拿。
    let foreign_ref = f
        .sessions
        .unregistered()
        .unwrap()
        .into_iter()
        .find(|u| u.session.name == "mywork")
        .expect("采纳 socket 上的会话应当可见")
        .session
        .r#ref;
    let adopted = f
        .sessions
        .adopt(&AdoptSession {
            runtime_ref: foreign_ref.0.clone(),
            display_name: None,
            agent_type: None,
            working_directory: None,
        })
        .unwrap();
    let before = adopted.assessment.status;
    assert!(adopted.alive, "起点：adopted 行是活的");
    assert!(adopted.record.ended_at.is_none());

    // 用户自己的 tmux 坏了（升级后 client / server 协议不匹配是现实里最常见的一种）。
    std::fs::write(&f.adopt_fail, "1").unwrap();
    let views = f.sessions.list().unwrap();
    let a = view_of(&views, &adopted.record.id);
    assert_eq!(
        a.assessment.status,
        Status::Unknown,
        "没扫到只能报说不清：{:?} {}",
        a.assessment.status,
        a.assessment.reason.as_deref().unwrap_or_default()
    );
    assert_eq!(
        a.assessment.unknown_cause,
        Some(UnknownCause::RuntimeUnavailable),
        "这一格要说清说不清的原因（封闭词表里的一项）: {:?}",
        a.assessment.unknown_cause
    );
    let reason = a.assessment.reason.clone().unwrap_or_default();
    assert!(
        reason.contains("runtime unavailable") && reason.contains(&adopt),
        "原因要带得上是哪个 socket 没扫到: {reason}"
    );
    assert!(
        a.record.ended_at.is_none(),
        "绝不能因为没扫到就给 adopted 行写 ended_at"
    );
    assert_eq!(
        a.process,
        ProcessState::Unknown,
        "同一个道理不能从 process 上漏出一条假的 gone"
    );

    // 主 socket 上的行不受影响（现有 warn 语义：用户 socket 不得拖垮 agora 自己的会话列表）。
    let m = view_of(&views, &mine.record.id);
    assert!(m.alive, "用户 socket 出问题不得拖垮 agora 自己的会话");
    assert!(m.record.ended_at.is_none());
    let h = f.health().await;
    assert_eq!(
        h["runtime"]["status"], "ok",
        "采纳 socket 失明不等于 agora 自己的运行时降级: {:?}",
        h["runtime"]
    );

    // 恢复：那一次扫成了，行就回到原来的那一格，不必重启 daemon，也不留下任何结束痕迹。
    std::fs::remove_file(&f.adopt_fail).unwrap();
    let back = f.sessions.get(&adopted.record.id).unwrap();
    assert!(back.alive);
    assert_eq!(back.assessment.status, before, "恢复后回到原来那一格");
    assert!(back.record.ended_at.is_none());
}

#[test]
fn reconcile_skips_rows_on_an_unscanned_adopt_socket() {
    // reconcile 的 missing 分支同一形状（daemon 重启时把"没扫到"当"会话消失"，早就在写近似
    // ended_at，agora-dkv3）。reconcile 只在启动时跑，所以这里拿同一个库 + 同一个运行时再造一个
    // manager，等价于 daemon 重启。
    let adopt = isolate::socket_name("deg-rec", isolate::nth());
    let f = fake_with("reconcile", "ok", Some(&adopt));
    let mine = f.sessions.create(&spec("重启还在的行")).unwrap();
    f.announce(
        mine.record
            .runtime_ref
            .as_deref()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap(),
    );
    let foreign_ref = f
        .sessions
        .unregistered()
        .unwrap()
        .into_iter()
        .find(|u| u.session.name == "mywork")
        .expect("采纳 socket 上的会话应当可见")
        .session
        .r#ref;
    let adopted = f
        .sessions
        .adopt(&AdoptSession {
            runtime_ref: foreign_ref.0.clone(),
            display_name: None,
            agent_type: None,
            working_directory: None,
        })
        .unwrap();

    std::fs::write(&f.adopt_fail, "1").unwrap();
    let restarted = SessionManager::new(f.db.clone(), f.rt.clone() as Arc<dyn Runtime>);
    let report = restarted.reconcile().unwrap();
    assert!(
        report.known_missing.is_empty(),
        "没扫到的 socket 上的行不得进 missing: {:?}",
        report.known_missing
    );
    assert_eq!(report.known_unreadable, vec![adopted.record.id.clone()]);
    assert_eq!(report.known_alive, vec![mine.record.id.clone()]);

    let v = restarted.get(&adopted.record.id).unwrap();
    assert!(
        v.record.ended_at.is_none(),
        "reconcile 不得为「没扫到」写 ended_at"
    );
    assert_eq!(v.assessment.status, Status::Unknown);
}

fn view_of<'a>(
    views: &'a [agora::session::SessionView],
    id: &str,
) -> &'a agora::session::SessionView {
    views.iter().find(|v| v.record.id == id).unwrap_or_else(|| {
        panic!(
            "列表里没有 {id}: {:?}",
            views.iter().map(|v| &v.record.id).collect::<Vec<_>>()
        )
    })
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
