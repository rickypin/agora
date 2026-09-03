//! Terminal Gateway：`xterm.js ↔ WS ↔ PTY ↔ 运行时 attach ↔ Agent`（MISSION §3.2；ADR-001 D5）。
//!
//! 本模块只拥有 **attach 客户端进程**，绝不碰运行时会话与其中的 agent（不变量 1–4）。
//! 两条施工约束在这里落地（ADR-001 D8）：
//! - PTY 读、写、等退出各占一个专用 OS 线程，经有界 channel 与 async 侧相接；只有一种
//!   并发模型（tokio），线程只是 channel 另一头的阻塞 I/O，没有第二个事件循环。
//!   **不用 `spawn_blocking`**：它的线程归 runtime 管，runtime 关闭时会等它们结束，而 PTY
//!   read 只在 attach 退出或 master 关闭后返回——任何一条没 detach 干净的流都会让 daemon
//!   退出与测试 runtime 卡死（2026-09-03 实测：tests/gateway.rs 整个二进制挂住）。
//!   `runtime::exec` 出于同样的原因也用 `std::thread`。
//! - 释放顺序：WS 断开 → 给 attach 进程 SIGHUP → **确认它退出后**才释放 PTY writer。
//!   portable-pty 的 `UnixMasterWriter` 在 drop 时写 `"\n" + VEOF(^D)`；attach 若还活着，
//!   这个 ^D 会作为按键送进 pane，agent 读到 EOF 就退出，整个会话陪葬（devcenter 实锤，
//!   2026-09-03 复核 portable-pty 0.9.0 `unix.rs::UnixMasterWriter::drop` 仍如此）。
//!   所以 writer 归一个专职线程持有，**默认泄漏**：只有收到明确的"已确认退出"指令才 drop。
//!   守卫：tests/gateway.rs::never_writes_eof_to_live_attach。
//!
//! 与具体运行时无关：只认 [`AttachSpec`]（argv + env），具体运行时的名字不在这里出现。

use std::io::{Read, Write};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::runtime::{AttachSpec, Exit, Size};

/// 终端流协议（docs/spec/api.md）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Output {
        data: String,
    },
    Status {
        status: &'static str,
    },
    /// attach 客户端退出（形态与 `RuntimeSession::exit` 同，ADR-001）。这只说明 **这一条流**
    /// 结束了——会话可能还活着（detach、被另一个客户端接管、运行时 server 重启）。
    Exit {
        exit: Exit,
    },
    Pong,
}

/// keepalive：服务端每 `PING_INTERVAL` 发一个 WS Ping；`IDLE_TIMEOUT` 内没有任何入站帧
/// 就当对端死了（半开的隧道），断掉 **这一条** attach，会话不受影响。
pub const PING_INTERVAL: Duration = Duration::from_secs(20);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(65);
/// SIGHUP 之后等 attach 退出的上限；超过就泄漏 writer（宁可泄漏 fd）。
pub const DETACH_GRACE: Duration = Duration::from_secs(3);
/// PTY 输出 channel 的容量（每格一次 read，≤ 8 KB）；读线程写满就阻塞，反压到 PTY。
const OUTPUT_CAPACITY: usize = 64;
const READ_CHUNK: usize = 8192;
/// 我们向 attach 客户端自称的终端类型；xterm.js 实现的就是这套控制序列。
const TERM: &str = "xterm-256color";

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("打开 PTY 失败: {0}")]
    OpenPty(#[source] std::io::Error),
    #[error("在 PTY 里启动 attach 失败: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("attach 规格为空：运行时没有给出可执行的 argv")]
    EmptyArgv,
}

/// 把任意切块的字节流解码成 UTF-8：一次 PTY read 可能停在多字节字符中间，逐块解码会把
/// 那个字符永久变成 U+FFFD（写进终端 buffer，随 scrollback 存活；devcenter 实测约每 12 KB
/// 框线输出坏一个字）。不完整的尾巴留到下一块再拼；真正的坏字节仍替换成 U+FFFD，
/// 终端不能为一个永远不来的字节停住。
#[derive(Default)]
pub struct Utf8Stream {
    /// 最长 3 字节：UTF-8 序列最长 4，`from_utf8` 只对严格短于整字符的前缀报"不完整"。
    carry: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(bytes);
        let mut out = String::with_capacity(buf.len());
        let mut rest = &buf[..];
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    break;
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                    match err.error_len() {
                        Some(len) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            rest = &rest[valid + len..];
                        }
                        None => {
                            self.carry.extend_from_slice(&rest[valid..]);
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

/// 写线程收的指令。channel 关闭而没收到 `Release` → 泄漏 writer（安全默认）。
enum WriteCmd {
    Data(Vec<u8>),
    /// 调用方已确认 attach 进程退出，可以 drop writer（此时 ^D 无处可去）。
    Release,
}

/// 一个 PTY 里跑着的 attach 客户端。
pub struct AttachedPty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    writer: mpsc::Sender<WriteCmd>,
    output: mpsc::Receiver<Vec<u8>>,
    exit: ExitSignal,
    pid: Option<u32>,
}

/// attach 进程的退出信号，可从 [`AttachedPty`] 克隆出来单独等（select 里与 `read` 并列）。
#[derive(Clone)]
pub struct ExitSignal(watch::Receiver<Option<Exit>>);

impl ExitSignal {
    pub async fn wait(&mut self) -> Exit {
        match self.0.wait_for(|e| e.is_some()).await {
            Ok(v) => v.clone().unwrap_or(Exit::Signal("unknown".into())),
            // 发送端没了（等退出的线程异常结束）：当作未知信号退出。
            Err(_) => Exit::Signal("unknown".into()),
        }
    }

    pub fn exited(&self) -> Option<Exit> {
        self.0.borrow().clone()
    }
}

impl AttachedPty {
    /// 打开 PTY、在里面 spawn `spec`，起三个专用线程（读 / 写 / 等退出）。
    pub fn spawn(spec: &AttachSpec, size: Size) -> Result<Self, GatewayError> {
        let (program, args) = spec.argv.split_first().ok_or(GatewayError::EmptyArgv)?;
        let pair = native_pty_system()
            .openpty(pty_size(size))
            .map_err(|e| GatewayError::OpenPty(io_err(e)))?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.env("TERM", TERM);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| GatewayError::Spawn(io_err(e)))?;
        // slave 端只属于子进程；父进程留着它会让 master 读不到 EOF。
        drop(pair.slave);
        let killer = child.clone_killer();
        let pid = child.process_id();
        let master = pair.master;
        let raw_writer = master
            .take_writer()
            .map_err(|e| GatewayError::OpenPty(io_err(e)))?;
        let mut reader = master
            .try_clone_reader()
            .map_err(|e| GatewayError::OpenPty(io_err(e)))?;

        // 读：blocking 线程 → 有界 channel。
        let (out_tx, output) = mpsc::channel::<Vec<u8>>(OUTPUT_CAPACITY);
        std::thread::spawn(move || {
            let mut buf = [0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    // EOF / EIO：attach 客户端退出，或 master 已关。
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        // 写：writer 由这个线程独占；见模块文档的释放顺序。
        let (writer, mut write_rx) = mpsc::channel::<WriteCmd>(OUTPUT_CAPACITY);
        std::thread::spawn(move || {
            let mut w = raw_writer;
            loop {
                match write_rx.blocking_recv() {
                    Some(WriteCmd::Data(bytes)) => {
                        if w.write_all(&bytes).and_then(|_| w.flush()).is_err() {
                            break;
                        }
                    }
                    Some(WriteCmd::Release) => {
                        drop(w);
                        return;
                    }
                    None => break,
                }
            }
            // 没有明确的 Release 就不 drop：drop 会写 ^D。
            std::mem::forget(w);
        });

        // 等退出：`wait` 是阻塞调用。
        let (exit_tx, exit_rx) = watch::channel::<Option<Exit>>(None);
        std::thread::spawn(move || {
            let exit = match child.wait() {
                Ok(st) if st.success() => Exit::Code(0),
                Ok(st) => match st.signal() {
                    Some(sig) => Exit::Signal(sig.to_lowercase()),
                    None => Exit::Code(st.exit_code() as i32),
                },
                Err(_) => Exit::Signal("unknown".into()),
            };
            let _ = exit_tx.send(Some(exit));
        });

        Ok(AttachedPty {
            master,
            killer,
            writer,
            output,
            exit: ExitSignal(exit_rx),
            pid,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// 下一块原始输出；`None` 表示读端已结束。
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.output.recv().await
    }

    /// 送进 PTY；写线程已停（attach 退出后）就静默丢弃。
    /// 返回的 future 不借用 `self`：master 不是 `Sync`，借着它跨 await 会让上层 future 失去 `Send`。
    pub fn write(&self, bytes: Vec<u8>) -> impl std::future::Future<Output = ()> + Send + 'static {
        let tx = self.writer.clone();
        async move {
            let _ = tx.send(WriteCmd::Data(bytes)).await;
        }
    }

    pub fn resize(&self, size: Size) -> bool {
        self.master.resize(pty_size(size)).is_ok()
    }

    /// 退出信号的克隆：在 select 里与 [`Self::read`] 并列等。
    pub fn exit_signal(&self) -> ExitSignal {
        self.exit.clone()
    }

    pub fn has_exited(&self) -> bool {
        self.exit.exited().is_some()
    }

    /// 释放这一条 attach：SIGHUP → 等退出（上限 [`DETACH_GRACE`]）→ 确认退出才释放 writer。
    /// 返回是否确认了退出；`false` 表示 writer 被故意泄漏。
    pub async fn detach(mut self) -> bool {
        if !self.has_exited() {
            let _ = self.killer.kill();
            match tokio::time::timeout(DETACH_GRACE, self.exit.wait()).await {
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        component = "gateway",
                        pid = self.pid,
                        "attach 进程收到 SIGHUP 后未退出，泄漏 PTY writer 以免写入 ^D"
                    );
                    // writer sender 随 self drop → 写线程 forget(writer)。
                    return false;
                }
            }
        }
        let _ = self.writer.send(WriteCmd::Release).await;
        true
    }
}

/// 没走 [`AttachedPty::detach`] 就被丢弃（桥接任务被取消、runtime 关闭）：仍要把 attach
/// 客户端收走，否则它挂在 PTY 上永远不退出；writer 走线程里的默认路径泄漏，不写 ^D。
impl Drop for AttachedPty {
    fn drop(&mut self) {
        if !self.has_exited() {
            let _ = self.killer.kill();
        }
    }
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// portable-pty 用的是万能错误类型（本仓库源码里禁止出现它的名字，tests/arch_boundary.rs）；
/// 在这一处压平成 io::Error，模块边界上仍是 [`GatewayError`] 枚举。
fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_reads_survives() {
        let bytes = "框".as_bytes();
        let mut s = Utf8Stream::default();
        let mut out = s.push(&bytes[..1]);
        out.push_str(&s.push(&bytes[1..]));
        assert_eq!(out, "框");
    }

    #[test]
    fn a_character_split_three_ways_survives() {
        let bytes = "😀".as_bytes();
        let mut s = Utf8Stream::default();
        let mut out = String::new();
        for b in bytes {
            out.push_str(&s.push(std::slice::from_ref(b)));
        }
        assert_eq!(out, "😀");
    }

    #[test]
    fn garbage_becomes_replacement_and_does_not_stall() {
        let mut s = Utf8Stream::default();
        assert_eq!(s.push(&[b'a', 0xff, b'b']), "a\u{FFFD}b");
        assert!(s.carry.is_empty());
    }

    #[test]
    fn carry_never_exceeds_three_bytes() {
        let mut s = Utf8Stream::default();
        for _ in 0..100 {
            s.push(&[0xf0]);
            assert!(s.carry.len() <= 3);
        }
    }

    #[test]
    fn protocol_shapes_match_spec() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"resize","cols":100,"rows":30}"#).unwrap();
        assert!(matches!(
            m,
            ClientMessage::Resize {
                cols: 100,
                rows: 30
            }
        ));
        let s = serde_json::to_string(&ServerMessage::Exit {
            exit: Exit::Code(0),
        })
        .unwrap();
        assert_eq!(s, r#"{"type":"exit","exit":{"kind":"code","value":0}}"#);
    }
}
