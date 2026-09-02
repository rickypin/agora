//! 启动时探测一次用户 shell 的 PATH 与 locale（ADR-001 D7）。
//!
//! pane 进程继承运行时 server 的环境而不是客户端的；launchd / systemd 起的 daemon
//! 只有 `/usr/bin:/bin`。所以问一次 `$SHELL -l -i -c`，拿哨兵行后面的 PATH。
//! 失败不吞：退回 daemon PATH 并把原因带到 health（`tests/env_probe.rs`）。

use std::time::Duration;

use super::exec::{exec, ExecOptions};

const SENTINEL: &str = "__AGORA_PATH__";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathSource {
    Shell,
    Daemon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbe {
    pub path: String,
    pub source: PathSource,
    /// `source == Daemon` 时必有：为什么没拿到 shell 的 PATH。
    pub reason: Option<String>,
}

/// `shell` 为 `None` 时读 `$SHELL`。
pub fn probe_path(shell: Option<&str>, timeout: Duration) -> PathProbe {
    let daemon_path = std::env::var("PATH").unwrap_or_default();
    let fallback = |reason: String| PathProbe {
        path: daemon_path.clone(),
        source: PathSource::Daemon,
        reason: Some(reason),
    };

    let shell = match shell
        .map(str::to_owned)
        .or_else(|| std::env::var("SHELL").ok())
    {
        Some(s) if !s.is_empty() => s,
        _ => return fallback("SHELL 未设置".into()),
    };

    // TERM=dumb 会让 .zshrc 提前退出（实测 2026-09-02），所以给一个真实的 TERM。
    let opts = ExecOptions {
        timeout: Some(timeout),
        env: vec![("TERM".into(), "xterm-256color".into())],
        ..Default::default()
    };
    let script = format!("printf '\\n{SENTINEL}%s\\n' \"$PATH\"");
    let argv = [shell.as_str(), "-l", "-i", "-c", script.as_str()];

    match exec(&argv, &opts) {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            match stdout
                .lines()
                .rev()
                .find_map(|l| l.strip_prefix(SENTINEL))
                .filter(|p| !p.is_empty())
            {
                Some(path) => PathProbe {
                    path: path.to_owned(),
                    source: PathSource::Shell,
                    reason: None,
                },
                None => fallback(format!("{shell} 的输出里没有哨兵行")),
            }
        }
        Ok(out) => fallback(format!(
            "{shell} 退出 {:?}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr_tail).trim()
        )),
        Err(err) => fallback(err.to_string()),
    }
}

/// daemon 环境缺 UTF-8 locale 时要注入的变量（devcenter 的 CJK 乱码教训）。
pub fn locale_injection() -> Option<(String, String)> {
    let is_utf8 = |k: &str| {
        std::env::var(k)
            .map(|v| {
                v.to_ascii_lowercase().contains("utf-8") || v.to_ascii_lowercase().contains("utf8")
            })
            .unwrap_or(false)
    };
    if is_utf8("LC_ALL") || is_utf8("LANG") {
        None
    } else {
        Some(("LANG".into(), "C.UTF-8".into()))
    }
}
