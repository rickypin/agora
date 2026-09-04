//! `agora hook --host <宿主> --home <AGORA_HOME>`（ADR-002 D3/D4/D5）；宿主名单在 `adapter::hooks::HOSTS`。
//!
//! 在 agent 进程里跑，stdin 是 agent 的 hook payload。原则是**永远不拖累 agent**：
//! 任何失败（目录写不了、daemon 不在、socket 断、超时、超上限）都 exit 0 不输出——TUI 的
//! 提示还在，人照样能在终端答。只有用法错误（宿主名不认识、缺参数）才 exit 2 报错，
//! 那是安装写错了，应该被看见。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::adapter::hooks;
use crate::local::{self, Request, Response, SOCKET_FILE};

use super::inbox::{self, Delivery, Envelope, Inbox};
use super::HOLD_TIMEOUT;

fn usage() -> String {
    format!(
        "用法: agora hook --host <{}> --home <AGORA_HOME>",
        hooks::HOSTS.join("|")
    )
}

/// 不挂起的事件等 daemon 读完文件再退：SessionEnd 只有 1.5 s 预算（ADR-002 D4），别等太久。
const ACK_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Args {
    pub host: String,
    pub home: PathBuf,
}

pub fn parse_args(argv: &[&str]) -> Result<Args, String> {
    let mut host = None;
    let mut home = None;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match *a {
            "--host" => host = it.next().map(|s| s.to_string()),
            "--home" => home = it.next().map(PathBuf::from),
            other => return Err(format!("未知参数 {other}\n{}", usage())),
        }
    }
    let host = host.ok_or_else(|| format!("缺 --host\n{}", usage()))?;
    if !hooks::is_host(&host) {
        return Err(format!("不认识的宿主 {host}\n{}", usage()));
    }
    // --home 缺省用环境变量：外部会话没有 AGORA_HOME，安装写入的命令总是显式带 --home。
    let home = home.unwrap_or_else(local::resolve_home);
    Ok(Args { host, home })
}

/// 入口。读 stdin、落盘、唤醒、必要时挂起。
pub async fn run(argv: &[&str]) -> i32 {
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    if !hooks::host_matches_env(&args.host, std::env::var_os("GROK_SESSION_ID").is_some()) {
        return 0;
    }
    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_err() {
        return 0;
    }
    let payload = serde_json::from_slice(&raw)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&raw).into_owned()));
    let delivery = Delivery {
        envelope: envelope(&args.host, &payload),
        payload,
    };
    let path = match Inbox::new(&args.home).write(&delivery) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("agora hook: {err}");
            return 0;
        }
    };
    deliver(&args.home, &args.host, &delivery, path).await;
    0
}

fn envelope(host: &str, payload: &serde_json::Value) -> Envelope {
    let env = |k: &str| std::env::var(k).ok();
    let agent_env: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| hooks::agent_env_prefixes().iter().any(|p| k.starts_with(p)))
        .collect();
    Envelope {
        host: host.to_owned(),
        agora_session_id: env("AGORA_SESSION_ID"),
        agora_epoch: env("AGORA_EPOCH").and_then(|e| e.parse().ok()),
        agent_session_id: hooks::agent_session_id(payload).unwrap_or_else(|| "unknown".into()),
        agent_env,
        runtime_env: crate::runtime::PANE_ENV_VARS
            .iter()
            .filter_map(|k| env(k).map(|v| (k.to_string(), v)))
            .collect(),
        // SAFETY: getppid 没有前置条件、不会失败。
        ppid: unsafe { libc::getppid() } as u32,
        received_at: inbox::local_time_string(),
        received_unix_ms: inbox::now_unix_ms(),
    }
}

/// 连 socket 唤醒；挂起的等决定，决定有就写 stdout。所有失败路径静默返回。
async fn deliver(home: &Path, host: &str, delivery: &Delivery, path: PathBuf) {
    let socket = home.join(SOCKET_FILE);
    let hold = hooks::hold_key(host, &delivery.payload).is_some();
    // daemon 侧 55 min 自己会解除；客户端再宽 30 s 只是防 daemon 卡死。
    let wait = if hold {
        HOLD_TIMEOUT + Duration::from_secs(30)
    } else {
        ACK_TIMEOUT
    };
    let req = Request::Hook {
        path: path.display().to_string(),
    };
    let reply = tokio::time::timeout(wait, local::request(&socket, &req)).await;
    if let Ok(Ok(Response::Hook { decision })) = reply {
        if let Some(out) = hooks::decision_output(host, &decision) {
            println!("{out}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_require_a_known_host() {
        assert!(parse_args(&["--host", hooks::HOSTS[0], "--home", "/x"]).is_ok());
        assert!(parse_args(&["--host", "nope"]).is_err());
        assert!(parse_args(&["--home", "/x"]).is_err());
        assert!(parse_args(&["--host", hooks::HOSTS[1], "--bogus"]).is_err());
    }
}
