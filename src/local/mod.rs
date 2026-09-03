//! 本机通道（ADR-003 D6）：`AGORA_HOME` 目录自检与 `agora.sock`。
//!
//! 信任锚是 OS 用户身份：目录 0700、socket 仅属主可访问，再加每个连接的对端 uid 校验
//! （文件权限挡正常情况，peer_cred 挡权限被改错的情况）。socket 上跑的是一行 JSON 请求、
//! 一行 JSON 应答：配对链接铸造（`agora open` / `agora url`）；hook 唤醒与挂起随 ADR-002 D3 接入。

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
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
    Pair { origin: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Pair { url: String },
    Error { message: String },
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

pub type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

/// 绑定并一直服务。已有活的 daemon → `AlreadyRunning`；只剩下残留文件 → 删掉重绑。
/// 权限：进程 umask 077 使文件生来 0600，再显式 set_permissions 兜一层。
pub async fn serve(path: &Path, handler: Handler) -> Result<(), SocketError> {
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
    tracing::info!(component = "local", socket = %path.display(), "listening");
    loop {
        let (stream, _) = listener.accept().await.map_err(io)?;
        let handler = handler.clone();
        tokio::spawn(async move {
            if let Err(err) = handle(stream, handler).await {
                tracing::debug!(component = "local", %err, "连接结束");
            }
        });
    }
}

async fn handle(stream: UnixStream, handler: Handler) -> std::io::Result<()> {
    // 对端 uid ≠ 自己 → 一个字节都不读就关（ADR-003 D6）。
    let cred = stream.peer_cred()?;
    // SAFETY: 同 ensure_home。
    let me = unsafe { libc::getuid() };
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
            Ok(req) => handler(req),
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
