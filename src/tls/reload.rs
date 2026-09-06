//! 证书文件的热加载与 external 续期（ADR-003 D4："文件变化即热加载；SPKI 变了则日志警告
//! peer 需更新指纹；到期前 `renew_before` 调用 `renew_command`"）。self-signed 模式也跑同一个
//! watcher（不带 renew）：`agora tls rotate-key` 写完文件，daemon 不重启就换上新证书。
//!
//! 轮询而不引 notify：两个小文件、几十秒读一次没有成本；而 notify 在 macOS（FSEvents）与 Linux
//! （inotify）上对"编辑器原子写 / mv 覆盖 / `tailscale cert` 直写"的事件形态各不相同，正是热加载
//! 最容易漏的地方（2026-09-06 的取舍）。比对的是文件**内容**的哈希而不是 mtime：HFS+ 的 mtime
//! 只有秒级、连着写两次看不出变化，而两张 P-256 证书长度还常常相同。

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use super::server::Acceptor;
use super::{Fingerprint, Identity, TlsFiles};
use crate::runtime::exec::{exec, ExecOptions};

/// 生产的轮询间隔。
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);
/// 续期命令失败后至少隔这么久再试：`tailscale cert` 出错通常是网络或 Let's Encrypt 限速，
/// 秒级重试只会撞上限速。
pub const RENEW_RETRY: Duration = Duration::from_secs(3600);
/// 续期命令的运行上限（ACME 一轮几十秒是正常的）。
pub const RENEW_TIMEOUT: Duration = Duration::from_secs(120);

/// external 的续期设置。
#[derive(Debug, Clone)]
pub struct Renew {
    /// argv，直传不经 shell（`runtime::exec` 的规则）。
    pub command: Vec<String>,
    /// 距 notAfter 不到这么久就调命令。
    pub before: Duration,
    /// 两次尝试之间的最小间隔（失败重试也受它管）。
    pub retry: Duration,
}

impl Renew {
    /// `tls.external.renew_command` 没配或为空 → 不续期（只热加载）。
    pub fn from_config(command: Option<&[String]>, before: Duration) -> Option<Renew> {
        let command = command?.to_vec();
        if command.is_empty() {
            return None;
        }
        Some(Renew {
            command,
            before,
            retry: RENEW_RETRY,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub files: TlsFiles,
    pub interval: Duration,
    pub renew: Option<Renew>,
}

/// watcher 对外的事件；日志之外的观察者（测试）用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadEvent {
    /// 文件变了且新证书已装上。`spki_changed` 为 true 时每个把本节点配成 peer 的节点都要改指纹。
    Reloaded {
        fingerprint: Fingerprint,
        spki_changed: bool,
    },
    /// 文件变了但装不上（半写、钥证不配、不是 PEM）：旧证书继续服务，文件再变时再试。
    Rejected,
    /// 跑了一次 `renew_command`；`ok` 是退出码 0。命令产出的新文件由下一轮热加载装上。
    Renewed { ok: bool },
}

/// 起 watcher 任务。事件用 broadcast 发：没人收也不阻塞、不报错。
pub fn spawn(
    acceptor: Acceptor,
    cfg: WatchConfig,
) -> (
    tokio::task::JoinHandle<()>,
    broadcast::Receiver<ReloadEvent>,
) {
    let (tx, rx) = broadcast::channel(16);
    let handle = tokio::spawn(run(acceptor, cfg, tx, crate::clock::now_secs));
    (handle, rx)
}

async fn run(
    acceptor: Acceptor,
    cfg: WatchConfig,
    tx: broadcast::Sender<ReloadEvent>,
    now: fn() -> i64,
) {
    let mut stamp = stamp_of(&cfg.files);
    let mut last_renew: Option<Instant> = None;
    let mut ticker = tokio::time::interval(cfg.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // interval 的第一拍立即到：跳过——启动时装的就是当前文件。
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let current = stamp_of(&cfg.files);
        // 文件暂时读不到（正在被替换）不算变化，等它回来。
        if current.is_some() && current != stamp {
            stamp = current;
            let _ = tx.send(reload(&acceptor, &cfg.files));
        }
        if let Some(renew) = &cfg.renew {
            let due = acceptor.not_after() - now() <= renew.before.as_secs() as i64;
            let cooled = last_renew.is_none_or(|t| t.elapsed() >= renew.retry);
            if due && cooled {
                last_renew = Some(Instant::now());
                let ok = run_renew(renew).await;
                let _ = tx.send(ReloadEvent::Renewed { ok });
            }
        }
    }
}

fn reload(acceptor: &Acceptor, files: &TlsFiles) -> ReloadEvent {
    let identity = match Identity::from_files(files) {
        Ok(i) => i,
        Err(err) => {
            tracing::warn!(component = "tls", %err, "证书文件变了但读不出来，继续用旧证书");
            return ReloadEvent::Rejected;
        }
    };
    match acceptor.swap(&identity) {
        Ok(old) => {
            let fingerprint = identity.fingerprint();
            let spki_changed = old != fingerprint;
            if spki_changed {
                tracing::warn!(
                    component = "tls",
                    old = %old,
                    new = %fingerprint,
                    "证书公钥变了：每个把本节点配成 peer 的节点都要更新 peers[].cert_fingerprint"
                );
            } else {
                tracing::info!(component = "tls", %fingerprint, "证书已热加载（公钥未变，peer 不用动）");
            }
            ReloadEvent::Reloaded {
                fingerprint,
                spki_changed,
            }
        }
        Err(err) => {
            tracing::warn!(component = "tls", %err, "证书文件变了但装不上（钥证不配？），继续用旧证书");
            ReloadEvent::Rejected
        }
    }
}

/// 两个文件内容的联合哈希；任一读不到 → None。
fn stamp_of(files: &TlsFiles) -> Option<[u8; 32]> {
    let cert = std::fs::read(&files.cert_file).ok()?;
    let key = std::fs::read(&files.key_file).ok()?;
    let mut h = Sha256::new();
    h.update((cert.len() as u64).to_le_bytes());
    h.update(&cert);
    h.update(&key);
    Some(h.finalize().into())
}

async fn run_renew(renew: &Renew) -> bool {
    let argv = renew.command.clone();
    let program = argv.first().cloned().unwrap_or_default();
    let opts = ExecOptions {
        timeout: Some(RENEW_TIMEOUT),
        ..ExecOptions::default()
    };
    // 子进程在 blocking 线程上跑（runtime::exec 的约定）。
    match tokio::task::spawn_blocking(move || exec(&argv, &opts)).await {
        Ok(Ok(out)) if out.status.success() => {
            tracing::info!(component = "tls", %program, "renew_command 成功；新证书文件由下一轮热加载装上");
            true
        }
        Ok(Ok(out)) => {
            tracing::warn!(
                component = "tls",
                %program,
                status = ?out.status,
                stderr = %String::from_utf8_lossy(&out.stderr_tail),
                "renew_command 失败，{:?} 后重试",
                renew.retry
            );
            false
        }
        Ok(Err(err)) => {
            tracing::warn!(component = "tls", %program, %err, "renew_command 起不来，{:?} 后重试", renew.retry);
            false
        }
        Err(err) => {
            tracing::warn!(component = "tls", %program, %err, "renew_command 任务异常");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renew_config_needs_a_non_empty_command() {
        let before = Duration::from_secs(720 * 3600);
        assert!(Renew::from_config(None, before).is_none());
        assert!(Renew::from_config(Some(&[]), before).is_none());
        let r = Renew::from_config(Some(&["true".to_owned()]), before).unwrap();
        assert_eq!(r.command, ["true"]);
        assert_eq!(r.before, before);
        assert_eq!(r.retry, RENEW_RETRY);
    }

    #[test]
    fn stamp_changes_with_content_not_with_time() {
        let dir = tempfile::tempdir().unwrap();
        let files = TlsFiles::self_signed(dir.path());
        assert!(stamp_of(&files).is_none(), "缺文件 → None");
        std::fs::create_dir_all(dir.path().join(super::super::DIR)).unwrap();
        std::fs::write(&files.cert_file, "a").unwrap();
        std::fs::write(&files.key_file, "b").unwrap();
        let s1 = stamp_of(&files).unwrap();
        std::fs::write(&files.cert_file, "a").unwrap(); // 同内容再写一次：mtime 变、stamp 不变
        assert_eq!(stamp_of(&files).unwrap(), s1);
        std::fs::write(&files.key_file, "c").unwrap();
        assert_ne!(stamp_of(&files).unwrap(), s1);
    }
}
