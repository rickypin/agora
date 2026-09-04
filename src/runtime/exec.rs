//! 子进程的唯一入口（ADR-001 D2 / D8 施工约束 2）。
//!
//! 规则：argv 直传不经 shell（zsh 会把 `=name:` 当等号展开，实测 2026-09-02）；
//! 每次调用有超时（默认 5 s，不变量 5）；在 blocking 线程上跑；stdout / stderr 都排空，
//! stderr 只保留尾部 4 KB（devcenter 两次管道死锁换来的规则）。
//! 其它模块**不得**直接碰 `std::process::Command` / `tokio::process`（`tests/arch_boundary.rs`）。

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// 默认子进程超时（ADR-001 D6）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// stderr 只保留尾部这么多字节。
pub const STDERR_TAIL: usize = 4 * 1024;

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub timeout: Option<Duration>,
    pub cwd: Option<PathBuf>,
    /// 追加/覆盖到子进程环境；`None` 表示继承 daemon 环境不动。
    pub env: Vec<(String, String)>,
    /// 清空继承的环境后只用 `env`（PATH 探测用）。
    pub env_clear: bool,
    /// 喂给子进程 stdin 的字节；`None` 是 `/dev/null`（fake-agent 走真实 `agora hook` 路径要它）。
    pub stdin: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    /// 尾部 ≤ 4 KB。
    pub stderr_tail: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
}

impl ExitStatus {
    pub fn success(self) -> bool {
        self == ExitStatus::Code(0)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("无法启动 {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{program} 超过 {timeout:?} 未退出，已 kill")]
    Timeout { program: String, timeout: Duration },
    #[error("读取 {program} 输出失败: {source}")]
    Io {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

impl ExecError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, ExecError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}

/// 同步跑一个子进程直到退出或超时。调用方必须已经在 blocking 线程上
/// （async 侧用 [`exec_async`]）。
pub fn exec<S: AsRef<OsStr>>(argv: &[S], opts: &ExecOptions) -> Result<Output, ExecError> {
    let program = argv
        .first()
        .map(|s| s.as_ref().to_string_lossy().into_owned())
        .unwrap_or_default();
    let timeout = opts.timeout.unwrap_or(DEFAULT_TIMEOUT);

    let mut cmd = Command::new(argv.first().map(AsRef::as_ref).unwrap_or_default());
    cmd.args(&argv[1..])
        .stdin(if opts.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    if opts.env_clear {
        cmd.env_clear();
    }
    cmd.envs(opts.env.iter().map(|(k, v)| (k, v)));

    let mut child = cmd.spawn().map_err(|source| ExecError::Spawn {
        program: program.clone(),
        source,
    })?;
    // stdin 另起线程写完就关：子进程要先读完 stdin 才会有输出，主线程不能在这里等它。
    if let (Some(bytes), Some(mut sin)) = (opts.stdin.clone(), child.stdin.take()) {
        thread::spawn(move || {
            let _ = sin.write_all(&bytes);
        });
    }
    // take() 之后 child 只剩 kill/wait 两个用途，可以塞进 Mutex 交给看门狗。
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));

    // 看门狗：到时就 kill，主线程随后在 stdout EOF 处醒来。
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let timed_out = Arc::new(Mutex::new(false));
    let watchdog = {
        let child = Arc::clone(&child);
        let timed_out = Arc::clone(&timed_out);
        thread::spawn(move || {
            if done_rx.recv_timeout(timeout).is_err() {
                *lock(&timed_out) = true;
                let _ = lock(&child).kill();
            }
        })
    };

    // stderr 单独线程排空，只留尾巴：不排空会在 pipe 满时把子进程卡死。
    let stderr_thread = thread::spawn(move || {
        let mut tail = Vec::with_capacity(STDERR_TAIL);
        let mut buf = [0u8; 4096];
        if let Some(mut err) = stderr.take() {
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > STDERR_TAIL {
                    let drop = tail.len() - STDERR_TAIL;
                    tail.drain(..drop);
                }
            }
        }
        tail
    });

    let mut out = Vec::new();
    let io_result = match stdout.take() {
        Some(mut o) => o.read_to_end(&mut out).map(|_| ()),
        None => Ok(()),
    };

    let wait = lock(&child).wait();
    let _ = done_tx.send(());
    let _ = watchdog.join();
    let stderr_tail = stderr_thread.join().unwrap_or_default();

    if *lock(&timed_out) {
        return Err(ExecError::Timeout { program, timeout });
    }
    io_result.map_err(|source| ExecError::Io {
        program: program.clone(),
        source,
    })?;
    let status = wait.map_err(|source| ExecError::Io {
        program: program.clone(),
        source,
    })?;

    Ok(Output {
        status: to_exit_status(status),
        stdout: out,
        stderr_tail,
    })
}

/// 在 tokio 的 blocking 线程池上跑 [`exec`]（唯一的并发模型，ADR-001 D8）。
pub async fn exec_async(argv: Vec<String>, opts: ExecOptions) -> Result<Output, ExecError> {
    let program = argv.first().cloned().unwrap_or_default();
    tokio::task::spawn_blocking(move || exec(&argv, &opts))
        .await
        .unwrap_or_else(|join_err| {
            Err(ExecError::Io {
                program,
                source: std::io::Error::other(join_err),
            })
        })
}

fn to_exit_status(status: std::process::ExitStatus) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return ExitStatus::Signal(sig);
        }
    }
    ExitStatus::Code(status.code().unwrap_or(-1))
}

/// 锁中毒时取回内层值继续用，而不是 expect 崩掉（ADR-001 D8 施工约束 4）。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
