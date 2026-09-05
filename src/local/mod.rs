//! 本机通道（ADR-003 D6）：`AGORA_HOME` 目录自检与 `agora.sock`。
//!
//! 信任锚是 OS 用户身份：目录 0700、socket 仅属主可访问，再加每个连接的对端 uid 校验
//! （文件权限挡正常情况，peer_cred 挡权限被改错的情况）。socket 上跑的是一行 JSON 请求、
//! 一行 JSON 应答：配对链接铸造（`agora open` / `agora url`）；hook 唤醒与挂起（ADR-002 D3/D5）——
//! 挂起的请求要等 daemon 有了决定才应答，所以 handler 是异步的，一个连接占一个任务。

use std::future::Future;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub const SOCKET_FILE: &str = "agora.sock";

#[derive(Debug, thiserror::Error)]
pub enum HomeError {
    #[error("无法创建 AGORA_HOME {path}: {source}")]
    Create {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("无法读取 AGORA_HOME {path}: {source}")]
    Stat {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "AGORA_HOME {path} 不属于当前用户（uid {owner} ≠ {me}）；请换目录或 chown -R {me} {path}"
    )]
    WrongOwner { path: String, owner: u32, me: u32 },
    #[error("AGORA_HOME {path} 权限过宽（{mode:o}）：其他用户能读到凭据与 socket；请执行 chmod 700 {path}")]
    TooOpen { path: String, mode: u32 },
    /// unix socket 路径上限（macOS 104、Linux 108 字节，含结尾 NUL）：目录太长时 socket 根本绑不上，
    /// 底层错误只说 "path must be shorter than SUN_LEN"，用户看不出该怎么办（agora-kkd）。
    #[error("AGORA_HOME {path} 太长：socket 路径 {len} 字节超过系统上限 {max}；请用 AGORA_HOME 指向更短的目录")]
    PathTooLong {
        path: String,
        len: usize,
        max: usize,
    },
}

/// 本平台 unix socket 路径的最大长度（不含结尾 NUL）。
pub fn max_socket_path_len() -> usize {
    // SAFETY: sockaddr_un 是 plain C struct，全零是合法值；只读它的数组长度。
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_path.len() - 1
}

/// `$AGORA_HOME` | `~/.agora` | `./.agora`（ADR-003 D6）。
pub fn resolve_home() -> PathBuf {
    std::env::var_os("AGORA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".agora")))
        .unwrap_or_else(|| PathBuf::from(".agora"))
}

/// 目录不存在则以 0700 创建；存在则要求属于当前 uid 且 group / other 没有任何位。
/// 过宽时拒绝启动并把 chmod 命令写进错误——与 ssh 对 `~/.ssh` 的做法一致。
pub fn ensure_home(path: &Path) -> Result<(), HomeError> {
    let display = path.display().to_string();
    let socket_len = path.join(SOCKET_FILE).as_os_str().len();
    let max = max_socket_path_len();
    if socket_len > max {
        return Err(HomeError::PathTooLong {
            path: display,
            len: socket_len,
            max,
        });
    }
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|source| HomeError::Create {
            path: display.clone(),
            source,
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| HomeError::Create {
                path: display.clone(),
                source,
            },
        )?;
    }
    let meta = std::fs::metadata(path).map_err(|source| HomeError::Stat {
        path: display.clone(),
        source,
    })?;
    // SAFETY: getuid 没有前置条件、不会失败。
    let me = unsafe { libc::getuid() };
    if meta.uid() != me {
        return Err(HomeError::WrongOwner {
            path: display,
            owner: meta.uid(),
            me,
        });
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(HomeError::TooOpen {
            path: display,
            mode,
        });
    }
    Ok(())
}

// ---------- unix socket ----------

/// 一行 JSON。`origin` 只有远端配对（`agora pair`，V2-1）才会带；本机由 daemon 用自己的监听地址。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Pair {
        origin: Option<String>,
    },
    /// `agora hook` 落盘后的唤醒：只送路径，daemon 只读文件、不信 socket 上的内容（ADR-002 D3）。
    /// 要挂起的事件（PermissionRequest）由 daemon 从文件判定，应答要等到决定出来。
    Hook {
        path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Pair {
        url: String,
    },
    /// hook 唤醒的应答：不挂起的事件立即回 `None`；挂起的等 Dashboard / 终端 / 超时。
    Hook {
        decision: crate::adapter::hooks::Decision,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SocketError {
    #[error("另一个 daemon 已在 {0} 上监听")]
    AlreadyRunning(String),
    #[error("socket {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("daemon 未运行（{0} 连不上）；先启动 agora")]
    NotRunning(String),
    #[error("应答不是合法 JSON: {0}")]
    Protocol(String),
}

pub type Reply = Pin<Box<dyn Future<Output = Response> + Send>>;
pub type Handler = Arc<dyn Fn(Request) -> Reply + Send + Sync>;

/// 同步 handler 的便捷包装。
pub fn sync_handler(f: impl Fn(Request) -> Response + Send + Sync + 'static) -> Handler {
    Arc::new(move |req| {
        let resp = f(req);
        Box::pin(async move { resp })
    })
}

/// 绑定好、还没开始 accept 的 socket：daemon 启动时先把它（和 TCP 监听器）绑住，再去碰
/// 运行时 / 库 / reconcile——同一 AGORA_HOME 的第二个实例在这一步就退出，还什么都没动
/// （agora-apr）。绑定与 accept 之间来连的 hook 排在内核 backlog 里，accept 开始后照常应答。
pub struct BoundSocket {
    listener: UnixListener,
}

/// 退出时删 socket 文件的句柄，只删自己绑的那一个：记住绑定时的 (dev, inode)，删前比对，
/// 同路径若已换成别的实例绑的新文件就不动。2026-09-05 agora-apr 实测：第二个实例退出时
/// 无条件 `remove_file` 把活实例的 socket 删了，`agora url` / hook 唤醒全部失联而 HTTP 照常。
/// Drop 即删，所以起 daemon 的那一层把它留在自己的作用域里；`process::exit` 跳过 Drop 留下的
/// 残留文件无害——下次启动 connect 不上就当陈旧文件重绑。
#[derive(Debug)]
pub struct SocketCleanup {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl SocketCleanup {
    /// 路径上还是自己绑的那个文件 → 删掉并返回 true；已经没了或换成别人的 → 不动，false。
    pub fn remove(&self) -> bool {
        match std::fs::metadata(&self.path) {
            Ok(m) if m.dev() == self.dev && m.ino() == self.ino => {
                std::fs::remove_file(&self.path).is_ok()
            }
            _ => false,
        }
    }
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        self.remove();
    }
}

/// 绑定。已有活的 daemon → `AlreadyRunning`，一个字节不动；只剩下残留文件 → 删掉重绑。
/// 权限：进程 umask 077 使文件生来 0600，再显式 set_permissions 兜一层。
pub async fn bind(path: &Path) -> Result<(BoundSocket, SocketCleanup), SocketError> {
    let display = path.display().to_string();
    let io = |source| SocketError::Io {
        path: display.clone(),
        source,
    };
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(SocketError::AlreadyRunning(display));
        }
        std::fs::remove_file(path).map_err(io)?;
    }
    let listener = UnixListener::bind(path).map_err(io)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io)?;
    let meta = std::fs::metadata(path).map_err(io)?;
    tracing::info!(component = "local", socket = %path.display(), "listening");
    Ok((
        BoundSocket { listener },
        SocketCleanup {
            path: path.to_owned(),
            dev: meta.dev(),
            ino: meta.ino(),
        },
    ))
}

impl BoundSocket {
    /// 一直服务；对端必须是自己的 uid。
    pub async fn serve(self, handler: Handler) -> Result<(), SocketError> {
        // SAFETY: 同 ensure_home。
        let me = unsafe { libc::getuid() };
        self.serve_for_uid(handler, me).await
    }

    /// 同 [`Self::serve`]，但对端必须是 `uid`。生产永远传自己的 uid；测试用它演"其他用户来连"
    /// （单用户机器上造不出真的第二个 uid，只能把期望值换掉）。
    pub async fn serve_for_uid(self, handler: Handler, uid: u32) -> Result<(), SocketError> {
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|source| SocketError::Io {
                    path: self
                        .listener
                        .local_addr()
                        .ok()
                        .and_then(|a| a.as_pathname().map(|p| p.display().to_string()))
                        .unwrap_or_default(),
                    source,
                })?;
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Err(err) = handle(stream, handler, uid).await {
                    tracing::debug!(component = "local", %err, "连接结束");
                }
            });
        }
    }
}

/// 绑定并一直服务（[`bind`] + [`BoundSocket::serve`]）；测试用。未来被 drop / abort 时顺手删掉
/// 自己的 socket 文件。
pub async fn serve(path: &Path, handler: Handler) -> Result<(), SocketError> {
    let (sock, _cleanup) = bind(path).await?;
    sock.serve(handler).await
}

/// 同 [`serve`]，对端必须是 `uid`。
pub async fn serve_for_uid(path: &Path, handler: Handler, uid: u32) -> Result<(), SocketError> {
    let (sock, _cleanup) = bind(path).await?;
    sock.serve_for_uid(handler, uid).await
}

async fn handle(stream: UnixStream, handler: Handler, me: u32) -> std::io::Result<()> {
    // 对端 uid ≠ 自己 → 一个字节都不读就关（ADR-003 D6）。
    let cred = stream.peer_cred()?;
    if cred.uid() != me {
        tracing::warn!(
            component = "local",
            uid = cred.uid(),
            "拒绝其他用户的 socket 连接"
        );
        return Ok(());
    }
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    while let Some(line) = lines.next_line().await? {
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handler(req).await,
            Err(e) => Response::Error {
                message: format!("请求不是合法 JSON: {e}"),
            },
        };
        let mut out = serde_json::to_string(&resp).unwrap_or_default();
        out.push('\n');
        wr.write_all(out.as_bytes()).await?;
    }
    Ok(())
}

/// CLI 侧：一问一答。
pub async fn request(path: &Path, req: &Request) -> Result<Response, SocketError> {
    let display = path.display().to_string();
    let mut stream = UnixStream::connect(path)
        .await
        .map_err(|_| SocketError::NotRunning(display.clone()))?;
    let mut line = serde_json::to_string(req).unwrap_or_default();
    line.push('\n');
    let io = |source| SocketError::Io {
        path: display.clone(),
        source,
    };
    stream.write_all(line.as_bytes()).await.map_err(io)?;
    let (rd, _wr) = stream.split();
    let mut reply = String::new();
    BufReader::new(rd).read_line(&mut reply).await.map_err(io)?;
    serde_json::from_str(reply.trim()).map_err(|e| SocketError::Protocol(e.to_string()))
}
