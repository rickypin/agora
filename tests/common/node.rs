//! 单进程多节点 fixture（agora-3la；ADR-001 输入 ①）：每个节点自己的 AGORA_HOME、tmux
//! socket、SQLite、AppState 与监听器，同一个测试进程里起几个互不干扰。
//!
//! AGORA_HOME 用短路径 `/tmp/agt-<pid>-<n>`：macOS unix socket 路径上限 104 字节，
//! tempfile 的默认目录太深会让 agora.sock 绑定失败（agora-3la 注记）。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agora::api::AppState;
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::runtime::tmux::{TmuxConfig, TmuxRuntime};
use agora::runtime::{Runtime, Size};
use agora::session::{Db, NewSession, SessionManager, SessionView};

// 轮询 tmux 不能太密：每次 inspect/capture 都要新起一个 tmux client 进程，密集轮询会和
// server 收集子进程退出状态抢 SIGCHLD，pane 会死得"没有退出码"（agora-tc4；实测 2026-09-03
// 3.2a 上密集轮询丢 5/6、200 ms 轮询丢 1/6）。生产的 status.detector_interval 是 2 s。
const POLL: Duration = Duration::from_millis(200);

static N: AtomicU32 = AtomicU32::new(0);

/// 测试二进制旁边的 agora，本身就是 fake-agent（`agora fake-agent ...`）。
pub const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");

pub struct TmuxNode {
    pub name: String,
    pub home: PathBuf,
    pub socket: String,
    pub db: Arc<Db>,
    pub rt: Arc<TmuxRuntime>,
    pub sessions: Arc<SessionManager>,
    pub auth: Arc<Auth>,
    pub state: AppState,
    /// `127.0.0.1:port`；`serve()` 之后才有。
    pub addr: Option<String>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl TmuxNode {
    pub fn new() -> Self {
        Self::with_runtime("tmux", Duration::from_secs(5))
    }

    /// 换掉运行时二进制与超时：不变量 5 用一个"永远不返回"的假 tmux 造坏节点。
    pub fn with_runtime(bin: &str, exec_timeout: Duration) -> Self {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let name = format!("n{n}");
        let home = PathBuf::from(format!("/tmp/agt-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&home);
        agora::local::ensure_home(&home).unwrap();
        let socket = format!("agora-t-{pid}-{n}");
        let rt = Arc::new(
            TmuxRuntime::new(TmuxConfig {
                bin: bin.to_owned(),
                socket: socket.clone(),
                adopt_sockets: vec![],
                conf_path: home.join("tmux.conf"),
                exec_timeout,
                ..Default::default()
            })
            .unwrap(),
        );
        if bin == "tmux" {
            rt.check_version().unwrap();
        }
        let db = Arc::new(Db::open(&home.join("agora.db")).unwrap());
        let sessions = Arc::new(SessionManager::new(
            db.clone(),
            rt.clone() as Arc<dyn Runtime>,
        ));
        let auth = Arc::new(Auth::new(db.clone(), AuthConfig::default()));
        let state = AppState::new(auth.clone(), sessions.clone(), &name);
        TmuxNode {
            name,
            home,
            socket,
            db,
            rt,
            sessions,
            auth,
            state,
            addr: None,
            server: None,
        }
    }

    /// 在 127.0.0.1:0 上起 HTTP/WS 与事件轮询；返回地址。
    pub async fn serve(&mut self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let app = agora::api::router(self.state.clone());
        let watch = tokio::spawn(agora::events::watch(
            self.sessions.clone(),
            self.state.events.clone(),
            self.state.node.clone(),
            Duration::from_millis(200),
        ));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
            watch.abort();
        });
        self.server = Some(server);
        self.addr = Some(addr.clone());
        addr
    }

    /// 模拟 daemon 死掉：监听器与轮询任务全部取消。运行时与 agent 不受影响（不变量 3）。
    pub fn crash(&mut self) {
        if let Some(s) = self.server.take() {
            s.abort();
        }
        self.addr = None;
    }

    /// 同一 AGORA_HOME 与 socket 上重建对象（daemon 重启）：新 Session Manager + reconcile。
    pub fn rebuild(&self) -> SessionManager {
        let m = SessionManager::new(self.db.clone(), self.rt.clone() as Arc<dyn Runtime>);
        m.reconcile().unwrap();
        m
    }

    /// 库整个丢掉（磁盘坏、用户删了 AGORA_HOME/agora.db）之后的 daemon 重启：新库是空的，
    /// 会话只能从运行时重新发现（不变量 7）。
    pub fn rebuild_after_db_loss(&self) -> SessionManager {
        std::fs::remove_file(self.home.join("agora.db")).unwrap();
        let db = Arc::new(Db::open(&self.home.join("agora.db")).unwrap());
        let m = SessionManager::new(db, self.rt.clone() as Arc<dyn Runtime>);
        m.reconcile().unwrap();
        m
    }

    pub fn cookie(&self) -> String {
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

    /// 起一个 fake-agent 会话；返回本机 id。
    pub fn create_fake(&self, name: &str, script: &str) -> SessionView {
        self.sessions
            .create(&NewSession {
                display_name: name.into(),
                agent_type: "fake".into(),
                working_directory: std::env::temp_dir(),
                worktree: None,
                task_ref: None,
                command: format!("{AGORA_BIN} fake-agent -e \"{script}\""),
                env: vec![],
                size: Size::default(),
            })
            .unwrap()
    }

    pub fn gid(&self, id: &str) -> String {
        format!("{}:{id}", self.name)
    }

    pub fn wait(&self, id: &str, pred: impl Fn(&SessionView) -> bool) -> SessionView {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let v = self.sessions.get(id).unwrap();
            if pred(&v) {
                return v;
            }
            assert!(Instant::now() < deadline, "timeout: {v:?}");
            std::thread::sleep(POLL);
        }
    }

    /// 直接问 tmux：pane 进程还在不在（不经 agora，作为独立证人）。
    pub fn pane_alive(&self, view: &SessionView) -> bool {
        let r = view.record.runtime_ref.clone().unwrap();
        let session_name = r.rsplit(':').next().unwrap().to_owned();
        let out = Command::new("tmux")
            .args([
                "-L",
                &self.socket,
                "list-panes",
                "-t",
                &format!("={session_name}"),
                "-F",
                "#{pane_dead}",
            ])
            .output()
            .unwrap();
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "0"
    }

    pub fn tail(&self, view: &SessionView) -> String {
        let r = agora::runtime::RuntimeRef(view.record.runtime_ref.clone().unwrap());
        String::from_utf8_lossy(&self.rt.capture_tail(&r, 100).unwrap()).into_owned()
    }
}

impl Drop for TmuxNode {
    fn drop(&mut self) {
        self.crash();
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}
