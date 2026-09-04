//! 投递箱文件（ADR-002 D3）：信封 + 原始 payload 一个文件一条事件。
//!
//! 文件名 `<unix 毫秒 13 位>-<seq>.json`，定宽时间戳让字典序 = 时间序，重放按它排；`seq` 是
//! hook 进程 pid，同一毫秒两个 hook 进程也不撞名（同一进程只写一条）。先写 `.part` 再
//! rename：daemon 读到的文件一定是完整的，半截文件只会以 `.part` 结尾被跳过。

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::HookError;

pub const INBOX_DIR: &str = "hooks/inbox";
pub const DONE_DIR: &str = "hooks/done";
/// `done/` 里的文件保留多久供排障（ADR-002 D3）。
pub const DONE_RETENTION: Duration = Duration::from_secs(24 * 3600);

/// 信封：hook 进程从自己的环境与身份里带回的东西（ADR-002 D3 ①）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub host: String,
    /// agora 起的会话经 `LaunchSpec.env` 带回的身份；外部会话没有。
    pub agora_session_id: Option<String>,
    pub agora_epoch: Option<i64>,
    /// agent 自报的对话 id（payload 里取）；没有就是 `unknown`，目录名要用它。
    pub agent_session_id: String,
    /// `CLAUDE_PID` / `GROK_*` / `CODEX_*` 之类 agent 自己的环境变量。
    pub agent_env: BTreeMap<String, String>,
    /// 定位 pane 的运行时环境变量（名单在 `runtime::PANE_ENV_VARS`）：外部会话采纳的线索。
    pub runtime_env: BTreeMap<String, String>,
    /// hook 进程的父进程——外部会话存活判断的依据之一（ADR-002 D4）。
    pub ppid: u32,
    /// hook 进程的本地时间（带时区偏移），给排障看；机器比较用 `received_unix_ms`。
    pub received_at: String,
    pub received_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub envelope: Envelope,
    pub payload: Value,
}

/// 一份投递箱：`<home>/hooks/{inbox,done}`。
#[derive(Debug, Clone)]
pub struct Inbox {
    home: PathBuf,
}

impl Inbox {
    pub fn new(home: &Path) -> Self {
        Inbox {
            home: home.to_owned(),
        }
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.home.join(INBOX_DIR)
    }

    pub fn done_dir(&self) -> PathBuf {
        self.home.join(DONE_DIR)
    }

    fn hooks_dir(&self) -> PathBuf {
        self.home.join("hooks")
    }

    /// 写一条。目录不存在就以 0700 建（hook 进程可能先于 daemon 第一次启动就跑）。
    pub fn write(&self, delivery: &Delivery) -> Result<PathBuf, HookError> {
        let env = &delivery.envelope;
        let dir = self
            .inbox_dir()
            .join(&env.host)
            .join(safe_component(&env.agent_session_id));
        create_private_dirs(&self.hooks_dir(), &dir)?;
        let name = format!("{:013}-{}.json", env.received_unix_ms, std::process::id());
        let final_path = dir.join(&name);
        let part = dir.join(format!("{name}.part"));
        let io = |p: &Path| {
            let p = p.display().to_string();
            move |source| HookError::Io { path: p, source }
        };
        let body = serde_json::to_vec(delivery).map_err(|e| HookError::Io {
            path: final_path.display().to_string(),
            source: std::io::Error::other(e),
        })?;
        std::fs::write(&part, body).map_err(io(&part))?;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o600))
            .map_err(io(&part))?;
        std::fs::rename(&part, &final_path).map_err(io(&final_path))?;
        Ok(final_path)
    }

    /// `hooks/` 及其下目录必须属于自己且 group / other 没有任何位：过宽拒绝读（ADR-002
    /// "什么会让它变危险"）。目录不存在视为空投递箱，不算错。
    pub fn check_permissions(&self) -> Result<(), HookError> {
        let root = self.hooks_dir();
        if !root.exists() {
            return Ok(());
        }
        // SAFETY: getuid 没有前置条件、不会失败。
        let me = unsafe { libc::getuid() };
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let display = dir.display().to_string();
            let meta = std::fs::metadata(&dir).map_err(|source| HookError::Io {
                path: display.clone(),
                source,
            })?;
            if meta.uid() != me {
                return Err(HookError::WrongOwner {
                    path: display,
                    owner: meta.uid(),
                    me,
                });
            }
            let mode = meta.mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(HookError::TooOpen {
                    path: display,
                    mode,
                });
            }
            for entry in std::fs::read_dir(&dir).map_err(|source| HookError::Io {
                path: display.clone(),
                source,
            })? {
                let entry = entry.map_err(|source| HookError::Io {
                    path: display.clone(),
                    source,
                })?;
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    stack.push(entry.path());
                }
            }
        }
        Ok(())
    }

    /// 待重放的文件，按文件名（= 时间）排序，跨宿主跨会话合在一起。`.part` 跳过。
    pub fn pending(&self) -> Result<Vec<PathBuf>, HookError> {
        let mut out = Vec::new();
        let root = self.inbox_dir();
        if !root.exists() {
            return Ok(out);
        }
        for host in list_dir(&root)? {
            for session in list_dir(&host)? {
                for file in list_dir(&session)? {
                    if file.extension().is_some_and(|e| e == "json") {
                        out.push(file);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(out)
    }

    /// 只接受投递箱里的路径：socket 上送来的路径是提示不是真相。
    pub fn contains(&self, path: &Path) -> bool {
        let Ok(root) = self.inbox_dir().canonicalize() else {
            return false;
        };
        path.canonicalize()
            .map(|p| p.starts_with(root))
            .unwrap_or(false)
    }

    pub fn read(&self, path: &Path) -> Result<Delivery, HookError> {
        let display = path.display().to_string();
        let bytes = std::fs::read(path).map_err(|source| HookError::Io {
            path: display.clone(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| HookError::Malformed {
            path: display,
            source,
        })
    }

    /// 应用完移到 `done/`，保持 `<host>/<session>/<file>` 的相对路径。
    pub fn done(&self, path: &Path) -> Result<(), HookError> {
        let rel = path
            .strip_prefix(self.inbox_dir())
            .map_err(|_| HookError::Outside(path.display().to_string()))?;
        let target = self.done_dir().join(rel);
        if let Some(parent) = target.parent() {
            create_private_dirs(&self.hooks_dir(), parent)?;
        }
        std::fs::rename(path, &target).map_err(|source| HookError::Io {
            path: target.display().to_string(),
            source,
        })
    }

    /// 删掉 `done/` 里超过保留期的文件；空目录随手删。错误只记日志——排障文件不值得让 daemon 退出。
    pub fn prune_done(&self, retention: Duration) {
        let root = self.done_dir();
        let Ok(hosts) = list_dir(&root) else {
            return;
        };
        let cutoff = SystemTime::now().checked_sub(retention);
        for host in hosts {
            for session in list_dir(&host).unwrap_or_default() {
                for file in list_dir(&session).unwrap_or_default() {
                    let old = std::fs::metadata(&file)
                        .and_then(|m| m.modified())
                        .map(|m| cutoff.is_some_and(|c| m < c))
                        .unwrap_or(false);
                    if old {
                        let _ = std::fs::remove_file(&file);
                    }
                }
                let _ = std::fs::remove_dir(&session);
            }
            let _ = std::fs::remove_dir(&host);
        }
    }
}

fn list_dir(dir: &Path) -> Result<Vec<PathBuf>, HookError> {
    let display = dir.display().to_string();
    let rd = std::fs::read_dir(dir).map_err(|source| HookError::Io {
        path: display.clone(),
        source,
    })?;
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|source| HookError::Io {
            path: display.clone(),
            source,
        })?;
        out.push(entry.path());
    }
    Ok(out)
}

/// 逐级创建目录，每一级都 0700。hook 进程的 umask 是 agent 的，不一定是 077，所以显式 chmod。
/// 并行工具调用会同时起好几个 hook 进程抢着建同一个目录（2026-09-04 实测 8 个里丢 2 个）：
/// AlreadyExists 是别人赢了，不是错。
fn create_private_dirs(root: &Path, dir: &Path) -> Result<(), HookError> {
    let mut chain: Vec<&Path> = dir
        .ancestors()
        .take_while(|p| p.starts_with(root))
        .collect();
    chain.reverse();
    for p in chain {
        if !p.exists() {
            match std::fs::create_dir(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(HookError::Io {
                        path: p.display().to_string(),
                        source,
                    })
                }
            }
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700)).map_err(
                |source| HookError::Io {
                    path: p.display().to_string(),
                    source,
                },
            )?;
        }
    }
    Ok(())
}

/// agent 自报的 id 进目录名：只留 `[A-Za-z0-9._-]`，别的换成 `_`，空的叫 `unknown`。
fn safe_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "unknown".into()
    } else {
        cleaned
    }
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `2026-09-04T10:22:33+0800`——hook 进程的本地时间，给人看。
pub fn local_time_string() -> String {
    let now = now_unix_ms() / 1000;
    let t = now as libc::time_t;
    // SAFETY: tm 是纯数据结构，零值合法；localtime_r 把结果写进我们给的缓冲区。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    let off = tm.tm_gmtoff;
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        off / 3600,
        (off % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery(ms: u64, sid: &str) -> Delivery {
        Delivery {
            envelope: Envelope {
                host: "h".into(),
                agora_session_id: None,
                agora_epoch: None,
                agent_session_id: sid.into(),
                agent_env: BTreeMap::new(),
                runtime_env: BTreeMap::new(),
                ppid: 1,
                received_at: String::new(),
                received_unix_ms: ms,
            },
            payload: serde_json::json!({ "n": ms }),
        }
    }

    #[test]
    fn files_sort_by_time_across_sessions_and_move_to_done() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(dir.path());
        let b = inbox.write(&delivery(2, "s2")).unwrap();
        let a = inbox.write(&delivery(1, "s1")).unwrap();
        let c = inbox.write(&delivery(3, "s1")).unwrap();
        assert_eq!(
            inbox.pending().unwrap(),
            vec![a.clone(), b.clone(), c.clone()]
        );
        assert!(inbox.contains(&a));
        assert_eq!(inbox.read(&a).unwrap().payload["n"], 1);
        assert_eq!(
            std::fs::metadata(inbox.inbox_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        inbox.done(&a).unwrap();
        assert_eq!(inbox.pending().unwrap(), vec![b, c]);
        assert!(inbox
            .done_dir()
            .join("h/s1")
            .join(a.file_name().unwrap())
            .exists());
        inbox.check_permissions().unwrap();
        // 保留期 0 → 立刻清光。
        inbox.prune_done(Duration::ZERO);
        assert!(!inbox.done_dir().join("h").exists());
    }

    #[test]
    fn foreign_paths_are_not_inside() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(dir.path());
        inbox.write(&delivery(1, "s")).unwrap();
        let outside = dir.path().join("agora.db");
        std::fs::write(&outside, b"x").unwrap();
        assert!(!inbox.contains(&outside));
        assert!(inbox.done(&outside).is_err());
    }

    #[test]
    fn session_ids_become_safe_directory_names() {
        assert_eq!(safe_component("../x/y"), ".._x_y");
        assert_eq!(safe_component(""), "unknown");
        assert_eq!(safe_component(".."), "unknown");
        assert_eq!(safe_component("abc-DEF_1.2"), "abc-DEF_1.2");
    }
}
