//! 各 API / Session Manager 测试共用的内存假运行时与 AppState 组装。

#![allow(dead_code)]

pub mod isolate;
pub mod node;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agora::adapter::{Version, VersionProbe};
use agora::api::AppState;
use agora::auth::{Auth, AuthConfig, PairedVia};
use agora::runtime::{
    AttachSpec, Exit, LaunchSpec, Runtime, RuntimeError, RuntimeRef, RuntimeSession, Size,
    TerminateSignal,
};
use agora::session::{Db, SessionManager};

pub const HOST: &str = "127.0.0.1:7680";
pub const NODE: &str = "testnode";

#[derive(Default)]
pub struct FakeRuntime {
    pub sessions: Mutex<HashMap<String, RuntimeSession>>,
    pub removed: Mutex<Vec<String>>,
    /// `attach` 返回的 argv；网关测试用它塞一个真实的假 attach（`sh -c ...`）。
    pub attach_argv: Mutex<Vec<String>>,
    /// `send_input` 写过的 `(ref, data)`。
    pub inputs: Mutex<Vec<(String, String)>>,
    /// `locate` 的答案：TMUX_PANE 值 → ref（dvh.12 外部会话定位）。
    pub panes: Mutex<HashMap<String, String>>,
    /// `capture_tail` 的答案：ref → 屏幕文本（dvh.10 pane 预览兜底）。
    pub tails: Mutex<HashMap<String, String>>,
    /// `respawn` 收到的命令，按顺序（dvh.13：Restart 的 resume 命令要到运行时才算数）。
    pub respawns: Mutex<Vec<String>>,
}

impl FakeRuntime {
    pub fn socket_of(r: &str) -> &str {
        r.split(':').nth(1).unwrap_or("")
    }

    pub fn insert(&self, r#ref: &str, alive: bool, exit: Option<Exit>, managed: bool) {
        let name = r#ref.rsplit(':').next().unwrap().to_owned();
        self.sessions.lock().unwrap().insert(
            r#ref.to_owned(),
            RuntimeSession {
                r#ref: RuntimeRef(r#ref.to_owned()),
                name,
                pid: Some(4242),
                alive,
                exit,
                exited_at: None,
                title: String::new(),
                cwd: PathBuf::from("/"),
                attached: false,
                size: Size::default(),
                managed,
                output_at: None,
            },
        );
    }

    pub fn set_dead(&self, r#ref: &str, exit: Exit) {
        let mut m = self.sessions.lock().unwrap();
        let s = m.get_mut(r#ref).unwrap();
        s.alive = false;
        s.exit = Some(exit);
    }

    pub fn set_title(&self, r#ref: &str, title: &str) {
        self.sessions.lock().unwrap().get_mut(r#ref).unwrap().title = title.into();
    }

    pub fn forget(&self, r#ref: &str) {
        self.sessions.lock().unwrap().remove(r#ref);
    }
}

impl Runtime for FakeRuntime {
    fn kind(&self) -> &'static str {
        "fake"
    }
    fn create(&self, spec: &LaunchSpec) -> Result<RuntimeRef, RuntimeError> {
        let r = format!("fake:agora:{}", spec.name);
        self.insert(&r, true, None, true);
        Ok(RuntimeRef(r))
    }
    fn list(&self) -> Result<Vec<RuntimeSession>, RuntimeError> {
        Ok(self.sessions.lock().unwrap().values().cloned().collect())
    }
    fn inspect(&self, r: &RuntimeRef) -> Result<RuntimeSession, RuntimeError> {
        self.sessions
            .lock()
            .unwrap()
            .get(&r.0)
            .cloned()
            .ok_or_else(|| RuntimeError::NotFound(r.clone()))
    }
    fn attach(&self, r: &RuntimeRef, _s: Size) -> Result<AttachSpec, RuntimeError> {
        if !self.sessions.lock().unwrap().contains_key(&r.0) {
            return Err(RuntimeError::NotFound(r.clone()));
        }
        Ok(AttachSpec {
            argv: self.attach_argv.lock().unwrap().clone(),
            env: vec![],
        })
    }
    fn capture_tail(&self, r: &RuntimeRef, _n: u32) -> Result<Vec<u8>, RuntimeError> {
        Ok(self
            .tails
            .lock()
            .unwrap()
            .get(&r.0)
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default())
    }
    fn terminate(&self, r: &RuntimeRef, _g: Duration) -> Result<(), RuntimeError> {
        if Self::socket_of(&r.0) != "agora" {
            return Err(RuntimeError::ReadOnly(r.clone()));
        }
        self.inspect(r)?;
        self.set_dead(&r.0, Exit::Signal("TERM".into()));
        Ok(())
    }
    /// fake 里的进程收到什么信号就按什么信号死——没有"忽略 TERM"的 fake；要那个用真 tmux
    /// （tests/session_tmux.rs / tests/invariants_peer.rs 的 `ignore-term`，agora-284）。
    fn signal(&self, r: &RuntimeRef, sig: TerminateSignal) -> Result<(), RuntimeError> {
        if Self::socket_of(&r.0) != "agora" {
            return Err(RuntimeError::ReadOnly(r.clone()));
        }
        if !self.inspect(r)?.alive {
            return Ok(());
        }
        let name = match sig {
            TerminateSignal::Term => "TERM",
            TerminateSignal::Kill => "KILL",
        };
        self.set_dead(&r.0, Exit::Signal(name.into()));
        Ok(())
    }
    fn respawn(&self, r: &RuntimeRef, spec: &LaunchSpec) -> Result<(), RuntimeError> {
        let mut m = self.sessions.lock().unwrap();
        let s = m
            .get_mut(&r.0)
            .ok_or_else(|| RuntimeError::NotFound(r.clone()))?;
        s.alive = true;
        s.exit = None;
        self.respawns.lock().unwrap().push(spec.command.clone());
        Ok(())
    }
    fn locate(
        &self,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<RuntimeRef>, RuntimeError> {
        Ok(env
            .get("TMUX_PANE")
            .and_then(|p| self.panes.lock().unwrap().get(p).cloned())
            .map(RuntimeRef))
    }
    fn send_input(&self, r: &RuntimeRef, data: &str) -> Result<(), RuntimeError> {
        self.inspect(r)?;
        self.inputs
            .lock()
            .unwrap()
            .push((r.0.clone(), data.to_owned()));
        Ok(())
    }
    fn remove(&self, r: &RuntimeRef) -> Result<(), RuntimeError> {
        let s = self.inspect(r)?;
        if s.alive {
            return Err(RuntimeError::StillAlive(r.clone()));
        }
        self.forget(&r.0);
        self.removed.lock().unwrap().push(r.0.clone());
        Ok(())
    }
}

/// 一套 API 测试用的组装：内存库 + 假运行时 + 节点 id。
pub struct Fx {
    pub state: AppState,
    pub auth: Arc<Auth>,
    pub db: Arc<Db>,
    pub rt: Arc<FakeRuntime>,
    pub sessions: Arc<SessionManager>,
}

impl Fx {
    pub fn new() -> Self {
        Self::with(AuthConfig::default())
    }

    pub fn with(cfg: AuthConfig) -> Self {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let auth = Arc::new(Auth::new(db.clone(), cfg));
        let rt = Arc::new(FakeRuntime::default());
        let sessions = Arc::new(SessionManager::new(
            db.clone(),
            rt.clone() as Arc<dyn Runtime>,
        ));
        let state = AppState::new(auth.clone(), sessions.clone(), NODE);
        // 内置 agent 一律当"不在 PATH"：测试不能真跑本机的 claude --version（慢且随机器变）。
        // 要走 resume / pin 路径的测试用 `probe_available` 显式塞版本。
        {
            let mut probes = state.probes.lock().unwrap();
            for a in agora::adapter::ADAPTERS {
                probes.insert(
                    a.default_command().to_owned(),
                    (VersionProbe::Missing, std::time::Instant::now()),
                );
            }
        }
        Fx {
            state,
            auth,
            db,
            rt,
            sessions,
        }
    }

    /// 让 `<program> --version` 探测答"可用 v"（不跑子进程）。
    pub fn probe_available(&self, program: &str, v: Version) {
        self.state.probes.lock().unwrap().insert(
            program.to_owned(),
            (VersionProbe::Available(v), std::time::Instant::now()),
        );
    }

    /// 让探测答"不可解析"。
    pub fn probe_unparsable(&self, program: &str, why: &str) {
        self.state.probes.lock().unwrap().insert(
            program.to_owned(),
            (
                VersionProbe::Unparsable(why.to_owned()),
                std::time::Instant::now(),
            ),
        );
    }

    pub fn app(&self) -> axum::Router {
        agora::api::router(self.state.clone())
    }

    /// 直接从 Auth 兑换一条 socket 铸造的链接，返回 `agora_session=<token>`。
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
}

// ---------- WS 关闭码（agora-0jt） ----------

pub type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 等服务端主动发来带 `code` 的关闭帧（最多 `within`）：业务帧 / Ping 跳过；流在关闭帧之前结束或
/// 出错都算失败——RST 会把关闭码一起冲掉，那正是 `close_handshake` 要防的（src/api/terminal.rs）。
/// 返回关闭帧的 reason。
pub async fn expect_close_code(ws: &mut Ws, code: u16, within: Duration) -> String {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg = tokio::time::timeout(left, ws.next())
            .await
            .unwrap_or_else(|_| panic!("{within:?} 内应收到关闭码 {code}"));
        match msg {
            Some(Ok(Message::Close(Some(frame)))) => {
                assert_eq!(u16::from(frame.code), code, "关闭码不对: {frame:?}");
                return frame.reason.to_string();
            }
            Some(Ok(Message::Close(None))) => panic!("收到不带码的关闭帧，期待 {code}"),
            Some(Ok(_)) => continue,
            None => panic!("流在关闭帧之前结束（期待关闭码 {code}）"),
            Some(Err(err)) => panic!("流在关闭帧之前出错（期待关闭码 {code}）: {err}"),
        }
    }
}

/// 复查间隔调短到测试能等的量级；缺省 5 s 是线上的（`api::REVOKE_CHECK_INTERVAL`）。
pub const FAST_REVOKE_CHECK: Duration = Duration::from_millis(100);
