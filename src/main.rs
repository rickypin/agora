//! agora daemon 入口。
//!
//! 现阶段（agora-xqa.6）只做三件事：起 tracing、在 loopback 起 axum、回答
//! `GET /api/health` 的公开子集。配置文件与 listen 校验归 agora-xqa.9，
//! 这里先把地址写死为 127.0.0.1:7680（docs/spec/config.md 的默认值）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use agora::api;
use agora::runtime::tmux::{TmuxConfig, TmuxRuntime};
use agora::runtime::{env_probe, Runtime};
use agora::session::{Db, SessionManager};

/// 配置文件落地前的默认监听地址（docs/spec/config.md：只允许 loopback）。
const DEFAULT_LISTEN: &str = "127.0.0.1:7680";

#[tokio::main]
async fn main() {
    agora::telemetry::init();

    let addr: SocketAddr = match DEFAULT_LISTEN.parse() {
        Ok(addr) => addr,
        Err(err) => {
            tracing::error!(component = "main", listen = DEFAULT_LISTEN, %err, "listen 地址无法解析");
            std::process::exit(2);
        }
    };

    // AGORA_HOME 的 0700 自检与配置加载归 agora-xqa.5 / xqa.9；这里只定位目录。
    let home = std::env::var_os("AGORA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".agora")))
        .unwrap_or_else(|| PathBuf::from(".agora"));
    if let Err(err) = std::fs::create_dir_all(&home) {
        tracing::error!(component = "main", home = %home.display(), %err, "无法创建 AGORA_HOME");
        std::process::exit(2);
    }

    let probe = env_probe::probe_path(None, std::time::Duration::from_secs(5));
    tracing::info!(component = "main", source = ?probe.source, reason = ?probe.reason, "PATH 探测");

    let runtime = match TmuxRuntime::new(TmuxConfig {
        conf_path: home.join("tmux.conf"),
        path: (probe.source == env_probe::PathSource::Shell).then(|| probe.path.clone()),
        ..Default::default()
    }) {
        Ok(rt) => Arc::new(rt),
        Err(err) => {
            tracing::error!(component = "main", %err, "运行时初始化失败");
            std::process::exit(1);
        }
    };
    if let Err(err) = runtime.check_version() {
        // 只降级不退出（ADR-001 D7）；health 呈现归 agora-xqa.4。
        tracing::warn!(component = "main", %err, "运行时降级");
    }

    let db = match Db::open(&home.join("agora.db")) {
        Ok(db) => Arc::new(db),
        Err(err) => {
            tracing::error!(component = "main", %err, "打开 metadata 库失败");
            std::process::exit(1);
        }
    };
    let sessions = Arc::new(SessionManager::new(db, runtime.clone() as Arc<dyn Runtime>));
    // reconcile 会起子进程：放 blocking 线程，不占 tokio worker。
    let sessions_for_reconcile = sessions.clone();
    match tokio::task::spawn_blocking(move || sessions_for_reconcile.reconcile()).await {
        Ok(Ok(_report)) => {}
        Ok(Err(err)) => tracing::warn!(component = "main", %err, "reconcile 失败，继续启动"),
        Err(err) => tracing::warn!(component = "main", %err, "reconcile 任务异常"),
    }

    if let Err(err) = api::serve(addr).await {
        tracing::error!(component = "main", %err, "daemon 退出");
        std::process::exit(1);
    }
}
