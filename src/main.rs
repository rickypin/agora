//! agora 入口：daemon 与本机 CLI 共用一个 binary。
//!
//! ```text
//! agora [serve]                 起 daemon（明文监听器 + unix socket）
//! agora url                     经 socket 铸造一条配对链接并打印
//! agora open                    同上，并用系统默认浏览器打开
//! agora auth devices            已配对设备列表（直接读 SQLite，不需要 daemon）
//! agora auth revoke <id>|--all  吊销设备，即时生效
//! ```
//!
//! 配置来自 `AGORA_HOME/config.yaml`（docs/spec/config.md），缺文件全走默认；明文监听器
//! 只接受 loopback（ADR-003 D5）。这里是组装根：选运行时、把它的配置子段交给它解析。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agora::api::{self, AppState, RuntimeHealth};
use agora::auth::{Auth, PairedVia};
use agora::config::{self, Config, Settings};
use agora::local::{self, Request, Response, SOCKET_FILE};
use agora::runtime::exec::{exec, ExecOptions};
use agora::runtime::tmux::{TmuxConfig, TmuxRuntime, TmuxSection};
use agora::runtime::{env_probe, Runtime};
use agora::session::{Db, SessionManager};

/// V1 唯一的运行时；配置里 `runtime.kind` 缺省就是它。
const RUNTIME_KIND: &str = "tmux";

const USAGE: &str =
    "用法: agora [serve | url | open | auth devices | auth revoke <id>|--all | fake-agent <script>|-e <inline>]";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match argv.as_slice() {
        [] | ["serve"] => serve().await,
        ["url"] => pair_link(false).await,
        ["open"] => pair_link(true).await,
        ["auth", "devices"] => auth_devices(),
        ["auth", "revoke", target] => auth_revoke(target),
        // 测试用假 agent（agora-3la）：藏在子命令里，不占第二个 binary。
        ["fake-agent", rest @ ..] => agora::fake_agent::run(rest),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// 目录自检失败 → 打印 chmod 命令并退出（ADR-003 D6）。
fn home_or_exit() -> PathBuf {
    let home = local::resolve_home();
    if let Err(err) = local::ensure_home(&home) {
        eprintln!("{err}");
        std::process::exit(2);
    }
    home
}

/// 配置文件不合法 → 打印原因退出；CLI 子命令也用它拿 auth 段。
fn settings_or_exit(home: &Path) -> Settings {
    match Config::load(home, RUNTIME_KIND) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("配置被拒绝: {err}");
            std::process::exit(2);
        }
    }
}

fn open_db(home: &Path) -> Arc<Db> {
    match Db::open(&home.join("agora.db")) {
        Ok(db) => Arc::new(db),
        Err(err) => {
            eprintln!("打开 metadata 库失败: {err}");
            std::process::exit(1);
        }
    }
}

// ---------- daemon ----------

async fn serve() -> i32 {
    agora::telemetry::init();
    // daemon 生出的每个文件（socket、库、tmux 配置）都只属主可访问（ADR-003 D6）。
    // SAFETY: umask 没有前置条件；在起任何线程之前调用。
    unsafe { libc::umask(0o077) };

    let home = home_or_exit();
    let settings = match Config::load(&home, RUNTIME_KIND) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(component = "main", %err, "配置被拒绝");
            return 2;
        }
    };
    let addr = settings.listen;
    if settings.raw.runtime.kind != RUNTIME_KIND {
        tracing::error!(component = "main", kind = %settings.raw.runtime.kind, "未知的 runtime.kind（V1 只有 tmux）");
        return 2;
    }
    // 运行时子段只有这里解析：core 层对它不透明（ADR-001 D2）。
    let section: TmuxSection =
        match serde_yaml_ng::from_value(settings.raw.runtime.section(RUNTIME_KIND)) {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(component = "main", %err, "runtime.tmux 段不合法");
                return 2;
            }
        };
    let exec_timeout =
        match config::parse_duration("runtime.tmux.exec_timeout", &section.exec_timeout) {
            Ok(d) => d,
            Err(err) => {
                tracing::error!(component = "main", %err, "配置被拒绝");
                return 2;
            }
        };

    let probe = env_probe::probe_path(None, std::time::Duration::from_secs(5));
    tracing::info!(component = "main", source = ?probe.source, reason = ?probe.reason, "PATH 探测");
    let path_source = if probe.source == env_probe::PathSource::Shell {
        "shell"
    } else {
        "daemon"
    };

    let runtime = match TmuxRuntime::new(TmuxConfig {
        conf_path: home.join("tmux.conf"),
        path: (probe.source == env_probe::PathSource::Shell).then(|| probe.path.clone()),
        ..TmuxConfig::from_section(&section, exec_timeout)
    }) {
        Ok(rt) => Arc::new(rt),
        Err(err) => {
            tracing::error!(component = "main", %err, "运行时初始化失败");
            return 1;
        }
    };
    // 只降级不退出（ADR-001 D7）；实时探测与恢复归 agora-xqa.4，现在是启动时一次结论。
    let runtime_health = match runtime.check_version() {
        Ok(_) => RuntimeHealth {
            status: "ok",
            reason: None,
            path_source,
        },
        Err(err) => {
            tracing::warn!(component = "main", %err, "运行时降级");
            RuntimeHealth {
                status: "degraded",
                reason: Some(err.to_string()),
                path_source,
            }
        }
    };

    let db = open_db(&home);
    let sessions = Arc::new(SessionManager::with_prefix(
        db.clone(),
        runtime.clone() as Arc<dyn Runtime>,
        &section.prefix,
    ));
    // reconcile 会起子进程：放 blocking 线程，不占 tokio worker。
    let sessions_for_reconcile = sessions.clone();
    match tokio::task::spawn_blocking(move || sessions_for_reconcile.reconcile()).await {
        Ok(Ok(_report)) => {}
        Ok(Err(err)) => tracing::warn!(component = "main", %err, "reconcile 失败，继续启动"),
        Err(err) => tracing::warn!(component = "main", %err, "reconcile 任务异常"),
    }

    let auth = Arc::new(Auth::new(db, settings.auth.clone()));
    if auth.list_devices().map(|d| d.is_empty()).unwrap_or(true) {
        tracing::info!(
            component = "main",
            "还没有已配对设备：在本机运行 `agora open` 打开浏览器"
        );
    }

    // unix socket：CLI 经它铸造配对链接；origin 是 daemon 自己的监听地址。
    let socket_path = home.join(SOCKET_FILE);
    let origin = format!("http://{addr}");
    let auth_for_socket = auth.clone();
    let handler: local::Handler = Arc::new(move |req| match req {
        Request::Ping => Response::Pong,
        Request::Pair { origin: o } => match auth_for_socket.mint_pair_token(PairedVia::Socket) {
            Ok(token) => Response::Pair {
                url: Auth::pair_link(o.as_deref().unwrap_or(&origin), &token),
            },
            Err(err) => Response::Error {
                message: err.to_string(),
            },
        },
    });
    let socket_task = tokio::spawn(async move { local::serve(&socket_path, handler).await });

    let mut state = AppState::new(auth, sessions.clone(), &settings.node_id);
    state.agents = Arc::new(settings.raw.agents.clone());
    state.projects = Arc::new(agora::project::Projects::new(
        sessions.db_handle(),
        settings.raw.project_roots.clone(),
    ));
    state.runtime_health = Arc::new(runtime_health);
    // 状态变化没有人来通知：轮询求差发 /api/events。
    tokio::spawn(agora::events::watch(
        sessions,
        state.events.clone(),
        state.node.clone(),
        settings.detector_interval,
    ));
    tracing::info!(component = "main", node = %settings.node_id, "daemon 就绪");

    let served = tokio::select! {
        r = api::serve(addr, state) => r.map_err(|e| e.to_string()),
        r = socket_task => match r {
            Ok(Err(e)) => Err(e.to_string()),
            Ok(Ok(())) => Err("socket 服务意外结束".into()),
            Err(e) => Err(e.to_string()),
        },
    };
    let _ = std::fs::remove_file(home.join(SOCKET_FILE));
    match served {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!(component = "main", %err, "daemon 退出");
            1
        }
    }
}

// ---------- CLI ----------

async fn pair_link(open: bool) -> i32 {
    let home = home_or_exit();
    let req = Request::Pair { origin: None };
    match local::request(&home.join(SOCKET_FILE), &req).await {
        Ok(Response::Pair { url }) => {
            println!("{url}");
            if open {
                // 打开默认浏览器；失败只提示，链接已经打印出来了。
                let opener = if cfg!(target_os = "macos") {
                    "open"
                } else {
                    "xdg-open"
                };
                if let Err(err) = exec(&[opener, url.as_str()], &ExecOptions::default()) {
                    eprintln!("无法打开浏览器（{err}）；请手动打开上面的链接");
                }
            }
            0
        }
        Ok(Response::Error { message }) => {
            eprintln!("{message}");
            1
        }
        Ok(other) => {
            eprintln!("daemon 应答异常: {other:?}");
            1
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn auth_devices() -> i32 {
    let home = home_or_exit();
    let auth = Auth::new(open_db(&home), settings_or_exit(&home).auth);
    match auth.list_devices() {
        Ok(devices) if devices.is_empty() => {
            println!("没有已配对设备；运行 `agora open` 配对本机浏览器");
            0
        }
        Ok(devices) => {
            let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str| {
                println!("{a:<10} {b:<22} {c:<8} {d:<20} {e:<20} {f}");
            };
            row("ID", "NAME", "VIA", "PAIRED", "LAST_SEEN", "STATE");
            for d in devices {
                let state = if d.revoked_at.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                row(
                    &d.id,
                    &d.name,
                    &d.paired_via,
                    &d.created_at,
                    &d.last_seen_at,
                    state,
                );
            }
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn auth_revoke(target: &str) -> i32 {
    let home = home_or_exit();
    let auth = Auth::new(open_db(&home), settings_or_exit(&home).auth);
    let result = if target == "--all" {
        auth.revoke_all().map(|n| println!("已吊销 {n} 台设备"))
    } else {
        auth.revoke(target).map(|_| println!("已吊销 {target}"))
    };
    match result {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
