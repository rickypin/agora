//! 单进程多节点 fixture（agora-3la；ADR-001 输入 ①）：每个节点自己的 AGORA_HOME、tmux
//! socket、SQLite、AppState 与监听器，同一个测试进程里起几个互不干扰。
//!
//! AGORA_HOME 用短路径 `/tmp/agt-<pid>-<n>`：macOS unix socket 路径上限 104 字节，
//! tempfile 的默认目录太深会让 agora.sock 绑定失败（agora-3la 注记）。

use std::net::SocketAddr;
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
use agora::tls::{self, Fingerprint};

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
    /// TLS 监听器的地址；`serve_tls()` 之后才有，`crash()` 清掉。
    pub tls_addr: Option<SocketAddr>,
    /// 自签证书的 SPKI 指纹（`<home>/tls/`，随 home 存在，跨 `crash()` 不变）；`serve_tls()` 之后才有。
    pub fingerprint: Option<Fingerprint>,
    server: Option<tokio::task::JoinHandle<()>>,
    /// TLS 监听器、它的每条连接与事件轮询所在的运行时（见 `serve_tls_at`）。
    tls: Option<tokio::runtime::Runtime>,
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
            tls_addr: None,
            fingerprint: None,
            server: None,
            tls: None,
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
            true,
        ));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
            watch.abort();
        });
        self.server = Some(server);
        self.addr = Some(addr.clone());
        addr
    }

    /// TLS 监听器（ADR-003 D5 的第二个监听器；agora-7ku.9）：自签证书在 `<home>/tls/`（同一 home
    /// 重建时复用，指纹不变），`api::bind_tls` + `api::serve_tls_router`——生产的 accept 循环，每个
    /// 请求盖 `TlsListener` 标记。`port` 为 0 让 OS 挑；daemon "重启"时传上次的端口——生产的
    /// `server.tls_listen` 是固定的，peer 配置里的 url 才指得回来。返回地址与指纹。
    ///
    /// 监听器、它的每条连接与事件轮询都跑在**节点自己的 tokio 运行时**上，不在测试的运行时里：
    /// `crash()` 关掉这个运行时，已升级的 WS（peer 对 `/api/events` 的订阅）随之断掉，对端立刻
    /// 看到 EOF——像真进程死了一样。只 abort accept 循环做不到：axum / hyper 把每条连接放在独立
    /// 任务里，对端要等 65 s keepalive 才发现（2026-09-06 实测：invariant_8 等 stale 直接超时）。
    /// 事件轮询只在明文 `serve()` 没在跑时才起——两个监听器共用一个 daemon，轮询只有一份；
    /// 先 `serve_tls()` 再 `serve()` 会多一份轮询（事件重复），两条测试都没这么用。
    ///
    /// 监听器拿到的 `state` 是此刻的克隆：之后 `state.registry` 被换掉（测试里 link peer）对它不
    /// 可见；`peers` / `peer_views` / `events` 是共享把手，照常可见。要经监听器做一跳转发的测试
    /// 先 link 再 serve。
    pub async fn serve_tls_at(&mut self, port: u16) -> (SocketAddr, Fingerprint) {
        assert!(self.tls.is_none(), "TLS 监听器已在跑；先 crash()");
        let identity =
            tls::load_or_generate_self_signed(&self.home, agora::clock::now_secs()).unwrap();
        let fingerprint = identity.fingerprint();
        let acceptor = tls::server::Acceptor::new(&identity).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name(format!("{}-tls", self.name))
            .build()
            .unwrap();
        let app = agora::api::router(self.state.clone());
        let want: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<SocketAddr, String>>();
        rt.spawn(async move {
            // 重启复用端口：上一个运行时的 worker 线程放掉监听 socket 要几毫秒（shutdown_background
            // 不等它们），bind 失败就等一等再试，上限 2 s。
            let mut last = None;
            for _ in 0..40 {
                match agora::api::bind_tls(want, acceptor.clone()).await {
                    Ok(bound) => {
                        let _ = tx.send(bound.local_addr().map_err(|e| e.to_string()));
                        let _ = agora::api::serve_tls_router(bound, app).await;
                        return;
                    }
                    Err(err) => {
                        last = Some(err.to_string());
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
            let _ = tx.send(Err(last.unwrap_or_default()));
        });
        if self.server.is_none() {
            rt.spawn(agora::events::watch(
                self.sessions.clone(),
                self.state.events.clone(),
                self.state.node.clone(),
                Duration::from_millis(200),
                true,
            ));
        }
        let addr = rx
            .await
            .expect("TLS 监听任务没回地址就退出了")
            .unwrap_or_else(|err| panic!("绑定 {want} 失败: {err}"));
        self.tls = Some(rt);
        self.tls_addr = Some(addr);
        self.fingerprint = Some(fingerprint);
        (addr, fingerprint)
    }

    /// `serve_tls_at(0)`：OS 挑端口。
    pub async fn serve_tls(&mut self) -> (SocketAddr, Fingerprint) {
        self.serve_tls_at(0).await
    }

    /// 模拟 daemon 死掉：明文监听器与它的轮询任务取消；TLS 那一侧的运行时整个关掉（监听器、
    /// 已升级的连接、轮询一起没了，见 `serve_tls_at`）。运行时与 agent 不受影响（不变量 3）。
    pub fn crash(&mut self) {
        if let Some(s) = self.server.take() {
            s.abort();
        }
        self.addr = None;
        // 在 async 上下文里 drop 一个 Runtime 会 panic（"Cannot drop a runtime in a context where
        // blocking is not allowed"）；shutdown_background 不等 worker 收尾，任务照样被取消并 drop。
        if let Some(rt) = self.tls.take() {
            rt.shutdown_background();
        }
        self.tls_addr = None;
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
