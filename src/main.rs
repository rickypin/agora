//! agora 入口：daemon 与本机 CLI 共用一个 binary。
//!
//! ```text
//! agora [serve]                 起 daemon（明文监听器 + unix socket）
//! agora url                     经 socket 铸造一条配对链接并打印
//! agora open                    同上，并用系统默认浏览器打开
//! agora auth devices            已配对设备列表（直接读 SQLite，不需要 daemon）
//! agora auth revoke <id>|--all  吊销设备，即时生效
//! agora hook --host <h> --home <dir> [--record <file>]
//!                               agent 的 hook 命令：落盘、唤醒、必要时挂起（ADR-002 D3）；
//!                               --record（或 AGORA_HOOK_RECORD）顺手录成脱敏 fixture（D10）
//! agora peer token create <name> [--rotate] | list | revoke <name>
//!                               被访问节点签发 / 列出 / 吊销某个 peer 的机器 token（ADR-003 D3）
//! agora tls fingerprint         本节点 TLS 证书的 SPKI 指纹（peer 的 cert_fingerprint 填它；ADR-003 D4）
//! agora tls rotate-key          自签模式换钥重签；daemon 热加载，peer 需更新指纹
//! agora upgrade --from <新二进制> [--no-restart]
//!                               一条命令升级本节点：按哈希放进 versions/、probe 新版本、重指 bin/agora、
//!                               重启 daemon（A39；agora-7ku.8）
//! agora upgrade --probe         新二进制自报 {"schema_version","api_version"}（stdout JSON，给 upgrade 读）
//! ```
//!
//! 配置来自 `AGORA_HOME/config.yaml`（docs/spec/config.md），缺文件全走默认；明文监听器
//! 只接受 loopback（ADR-003 D5）。这里是组装根：选运行时、把它的配置子段交给它解析。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agora::api::{self, AppState};
use agora::auth::{Auth, PairedVia};
use agora::config::{self, Config, Settings};
use agora::local::{self, Request, Response, SOCKET_FILE};
use agora::peer::backoff::CONNECT_TIMEOUT;
use agora::peer::registry::PeerRegistry;
use agora::peer::transport::HttpsTransport;
use agora::runtime::exec::{exec, ExecOptions};
use agora::runtime::tmux::{TmuxConfig, TmuxRuntime, TmuxSection};
use agora::runtime::{env_probe, Runtime};
use agora::session::{Db, SessionManager};
use agora::tls::{self, Mode, TlsFiles};

/// V1 唯一的运行时；配置里 `runtime.kind` 缺省就是它。
const RUNTIME_KIND: &str = "tmux";

const USAGE: &str = "用法: agora [serve | url | open | auth devices | auth revoke <id>|--all | hook --host <h> --home <dir> [--record <file>] | hooks install|uninstall <agent> | peer token create <name> [--rotate]|list|revoke <name> | tls fingerprint|rotate-key | upgrade --from <新二进制> [--no-restart] | upgrade --probe | fake-agent <script>|-e <inline>]";

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
        ["hook", rest @ ..] => agora::hook::cmd::run(rest).await,
        ["hooks", rest @ ..] => agora::hook::install::run(rest),
        ["peer", rest @ ..] => agora::cli::peer::run(rest),
        ["tls", rest @ ..] => tls_cmd(rest),
        // --probe 不读配置、不碰 AGORA_HOME：被问的是这份二进制本身（agora-7ku.8）。
        ["upgrade", "--probe"] => agora::cli::upgrade::probe(),
        ["upgrade", rest @ ..] => upgrade_cmd(rest),
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

    // 两个监听器最先绑（agora-apr，2026-09-05）：同一 AGORA_HOME 的第二个实例要在碰运行时 /
    // 库 / reconcile / 投递箱之前就退出，且不能动活实例的任何东西。TCP 在前——端口占用是最常见
    // 的死法；unix socket 的 bind 自己会先 connect 探活，活的就报 AlreadyRunning、不 unlink。
    // 退出时只删自己绑的那个 socket 文件（socket_cleanup 的 Drop 比对 inode），别人的不动。
    let http = match api::bind(addr).await {
        Ok(l) => l,
        Err(err) => {
            report_bind_failure(&home, addr, &err).await;
            return 1;
        }
    };
    // TLS 监听器（ADR-003 D5）：配了 tls_listen 才有；证书按 tls.mode 来（D4）。
    let tls = match bind_tls(&home, &settings).await {
        Ok(t) => t,
        Err(code) => return code,
    };
    let socket_path = home.join(SOCKET_FILE);
    let (sock, socket_cleanup) = match local::bind(&socket_path).await {
        Ok(b) => b,
        Err(local::SocketError::AlreadyRunning(path)) => {
            eprintln!("已有实例在跑：{path} 有 daemon 在监听；要重启请先停掉它");
            return 1;
        }
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    // 三个监听器都绑上了才算"这个实例在跑"：pid 文件写在这里，退出（含 SIGTERM 收尾）时由 Drop 删。
    // 只给 `agora upgrade` 与人看（agora-7ku.8）；单实例判定仍是上面的 bind，不读它。
    let _pid_file = match agora::cli::upgrade::PidFile::write(&home) {
        Ok(f) => Some(f),
        Err(err) => {
            tracing::warn!(component = "main", %err, "写不了 agora.pid；`agora upgrade` 将找不到这个 daemon");
            None
        }
    };

    // hooks 装过、二进制却搬了家或 AGORA_HOME 被重置过：链接悬空时 hook 被条目里的 `[ -x ]` 守卫
    // 静默跳过，用户只会看到 hooks_unheard。这里说一句怎么修；链接根本不存在不提——装 hooks 之前
    // 它本来就不存在（agora-vkt）。
    if let Some((link, target)) = agora::hook::install::dangling_bin_link(&home) {
        tracing::warn!(
            component = "hook",
            link = %link.display(),
            target = %target.display(),
            "指向的二进制不存在，跑一次 `agora hooks install <agent>` 重指"
        );
    }

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
    // 只降级不退出（ADR-001 D7）。启动时探一次版本给出初始结论；之后每次读运行时的成败
    // 都由同一个 observe 更新它，所以 server 换代之后不必重启 daemon 就能自愈。
    let version_probe = runtime.check_version();
    if let Err(err) = &version_probe {
        tracing::warn!(component = "main", %err, "运行时降级");
    }

    let db = open_db(&home);
    let sessions = Arc::new(
        SessionManager::with_prefix(
            db.clone(),
            runtime.clone() as Arc<dyn Runtime>,
            &section.prefix,
        )
        .with_status_config(agora::status::MachineConfig {
            idle_after: settings.idle_after,
            silence_after: settings.hook_silence_after,
            unheard_after: settings.hook_unheard_after,
            tick: settings.detector_interval,
            ..agora::status::MachineConfig::default()
        }),
    );
    sessions.runtime_status().observe(&version_probe);
    // reconcile 会起子进程：放 blocking 线程，不占 tokio worker。
    let sessions_for_reconcile = sessions.clone();
    match tokio::task::spawn_blocking(move || sessions_for_reconcile.reconcile()).await {
        Ok(Ok(_report)) => {}
        Ok(Err(err)) => tracing::warn!(component = "main", %err, "reconcile 失败，继续启动"),
        Err(err) => tracing::warn!(component = "main", %err, "reconcile 任务异常"),
    }

    // 投递箱重放（MISSION §3.4）：daemon 不在时落下的 hook 事件在 reconcile 之后补上。
    // 权限过宽是拒绝读而不是退出：agent 照跑，只是状态退成 UNKNOWN，日志说明原因。
    let hooks = Arc::new(agora::hook::Receiver::new(&home, sessions.clone()));
    let replay_hooks = hooks.clone();
    match tokio::task::spawn_blocking(move || replay_hooks.replay()).await {
        Ok(Ok(n)) if n > 0 => tracing::info!(component = "hook", replayed = n, "投递箱重放完成"),
        Ok(Ok(_)) => {}
        Ok(Err(err)) => {
            tracing::error!(component = "hook", %err, "投递箱不可用，hook 事件不会被读取")
        }
        Err(err) => tracing::error!(component = "hook", %err, "投递箱重放任务失败"),
    }
    tokio::spawn(hooks.clone().run_sweeper(std::time::Duration::from_secs(5)));

    let auth = Arc::new(Auth::new(db, settings.auth.clone()));
    let paired_devices = auth
        .list_devices()
        .map(|d| d.iter().filter(|d| d.revoked_at.is_none()).count())
        .unwrap_or(0);
    if paired_devices == 0 {
        tracing::info!(
            component = "main",
            "还没有已配对设备：在本机运行 `agora open` 打开浏览器"
        );
    }
    let tls_listener = tls.map(|setup| {
        // 零凭据只警告不拒绝（ADR-003 D5）。机器 token 的计数等 agora-7ku.2 把 peer_tokens 表接进
        // Auth 后从那里取，现在按 0 算——所以此刻它在"只有 token 没有设备"的节点上会多叫一声，
        // 接入时把这个 0 换掉。
        if let Some(w) = tls::zero_credentials_warning(paired_devices, 0) {
            tracing::warn!(component = "tls", "{w}");
        }
        // 证书文件热加载（rotate-key / 续期后不必重启）；external 还按 renew_before 调 renew_command。
        let renew = match setup.mode {
            Mode::External => tls::reload::Renew::from_config(
                settings.raw.tls.external.renew_command.as_deref(),
                config::parse_duration(
                    "tls.external.renew_before",
                    &settings.raw.tls.external.renew_before,
                )
                .unwrap_or(std::time::Duration::from_secs(720 * 3600)),
            ),
            Mode::SelfSigned => None,
        };
        let (_watcher, _events) = tls::reload::spawn(
            setup.acceptor,
            tls::reload::WatchConfig {
                files: setup.files,
                interval: tls::reload::DEFAULT_INTERVAL,
                renew,
            },
        );
        setup.listener
    });

    // unix socket：CLI 经它铸造配对链接；origin 是 daemon 自己的监听地址。
    let origin = format!("http://{addr}");
    let auth_for_socket = auth.clone();
    let hooks_for_socket = hooks.clone();
    let handler: local::Handler = Arc::new(move |req| {
        let auth = auth_for_socket.clone();
        let hooks = hooks_for_socket.clone();
        let origin = origin.clone();
        Box::pin(async move {
            match req {
                Request::Ping => Response::Pong,
                Request::Pair { origin: o } => match auth.mint_pair_token(PairedVia::Socket) {
                    Ok(token) => Response::Pair {
                        url: Auth::pair_link(o.as_deref().unwrap_or(&origin), &token),
                    },
                    Err(err) => Response::Error {
                        message: err.to_string(),
                    },
                },
                Request::Hook { path } => hooks.wake(Path::new(&path)).await,
            }
        })
    });
    let socket_task = tokio::spawn(sock.serve(handler));

    let mut state = AppState::new(auth, sessions.clone(), &settings.node_id);
    state.agents = Arc::new(settings.raw.agents.clone());
    state.projects = Arc::new(
        agora::project::Projects::new(sessions.db_handle(), settings.raw.project_roots.clone())
            .with_worktree_root(settings.raw.worktree_root.clone()),
    );
    state.runtime_path_source = path_source;
    // 配置里的 peer 从第一秒起就在 health / Header 里（离线、没见过），不等连上才出现（MISSION §10.3）。
    // 同时装节点名 → transport 的表（agora-7ku.11）：peer 客户端（7ku.5）与一跳转发（7ku.7）都从
    // state.registry 查。名字重复或与本机 node.id 同名是配置错误，与其他配置错误一样退出码 2；
    // url / 指纹 / token_file 的形态错误不在这里拦——HttpsTransport 到真要拨号时才报 Config，
    // 显示为「配置错误」而不是拒绝启动（docs/spec/config.md「机器 token 文件」）。
    let mut registry = PeerRegistry::new(&settings.node_id);
    for p in &settings.raw.peers {
        state.peers.register(&p.name);
        let transport = HttpsTransport::new(p.clone(), CONNECT_TIMEOUT);
        if let Err(err) = registry.insert(Arc::new(transport)) {
            tracing::error!(component = "main", %err, "peers 配置被拒绝");
            return 2;
        }
    }
    state.registry = Arc::new(registry);
    // 每个 peer 一个客户端任务（agora-7ku.5）：比版本、先流后快照并入视图、断线退避重连；
    // 视图与状态都写进 state 里的共享把手，这里 clone 出去的 state 看到的是同一份。
    agora::peer::client::spawn_all(&state);
    hooks.attach_events(state.events.clone(), state.node.clone());
    state.hooks = Some(hooks);
    // 状态变化没有人来通知：轮询求差发 /api/events。
    tokio::spawn(agora::events::watch(
        sessions,
        state.events.clone(),
        state.node.clone(),
        settings.detector_interval,
        settings.raw.notifications.enabled,
    ));
    // 两个 listening 都已打出，这一行才算数（agora-apr 的验收看日志顺序）。
    tracing::info!(component = "main", node = %settings.node_id, "daemon 就绪");

    let served = tokio::select! {
        r = api::serve_on(http, state.clone()) => r.map_err(|e| e.to_string()),
        r = serve_tls(tls_listener, state) => r.map_err(|e| e.to_string()),
        r = socket_task => match r {
            Ok(Err(e)) => Err(e.to_string()),
            Ok(Ok(())) => Err("socket 服务意外结束".into()),
            Err(e) => Err(e.to_string()),
        },
    };
    // 只删自己绑的那个 socket 文件；路径上若已是别的实例的文件则不动（agora-apr）。
    drop(socket_cleanup);
    match served {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!(component = "main", %err, "daemon 退出");
            1
        }
    }
}

/// TLS 监听器绑好之后留给 serve 阶段的东西。
struct TlsSetup {
    listener: api::BoundTls,
    acceptor: tls::server::Acceptor,
    files: TlsFiles,
    mode: Mode,
}

/// TLS 监听器（ADR-003 D4 / D5）：`tls.mode` 决定证书从哪来——self-signed 首次开时生成到
/// `<AGORA_HOME>/tls/`、之后复用；external 读 cert_file / key_file。与明文监听器一样最先绑。
/// 没配 `server.tls_listen` → `Ok(None)`；配置或证书不可用 → 退出码（配置 2，端口 1）。
async fn bind_tls(home: &Path, settings: &Settings) -> Result<Option<TlsSetup>, i32> {
    let Some(addr) = settings.tls_listen else {
        return Ok(None);
    };
    let (mode, files) = TlsFiles::from_config(home, &settings.raw.tls).map_err(|err| {
        tracing::error!(component = "tls", %err, "配置被拒绝");
        2
    })?;
    let identity = match mode {
        Mode::SelfSigned => tls::load_or_generate_self_signed(home, agora::clock::now_secs()),
        Mode::External => tls::Identity::from_files(&files),
    }
    .map_err(|err| {
        tracing::error!(component = "tls", %err, "TLS 证书不可用");
        2
    })?;
    let acceptor = tls::server::Acceptor::new(&identity).map_err(|err| {
        tracing::error!(component = "tls", %err, "TLS 证书装不上");
        2
    })?;
    let listener = api::bind_tls(addr, acceptor.clone()).await.map_err(|err| {
        eprintln!("{err}；端口被别的程序占用？改 config.yaml 的 server.tls_listen");
        1
    })?;
    tracing::info!(
        component = "tls",
        mode = mode.as_str(),
        fingerprint = %identity.fingerprint(),
        "TLS 证书就绪；别的节点把它配成 peer 时 cert_fingerprint 填这个值"
    );
    Ok(Some(TlsSetup {
        listener,
        acceptor,
        files,
        mode,
    }))
}

/// `agora tls fingerprint | rotate-key`：home 与 tls 段用 daemon 同一套解析，CLI 与 daemon 看到的
/// 永远是同一对证书文件（agora-7ku.10）。
fn tls_cmd(args: &[&str]) -> i32 {
    let home = home_or_exit();
    let settings = settings_or_exit(&home);
    agora::cli::tls::run(args, &home, &settings.raw.tls)
}

/// `agora upgrade --from …`：home 与 `server.listen` 用 daemon 同一套解析——health 轮询打的就是
/// daemon 绑的那个地址（agora-7ku.8）。
fn upgrade_cmd(args: &[&str]) -> i32 {
    let home = home_or_exit();
    let settings = settings_or_exit(&home);
    agora::cli::upgrade::run(args, &home, settings.listen)
}

/// 没开 TLS 监听器时这一支永远不返回，让 `select!` 只看另外两支。
async fn serve_tls(
    listener: Option<api::BoundTls>,
    state: AppState,
) -> Result<(), api::ServeError> {
    match listener {
        Some(l) => api::serve_tls_on(l, state).await,
        None => std::future::pending().await,
    }
}

/// TCP 绑不上：先问问自己的 agora.sock 有没有活 daemon——有就是"用户又敲了一次 agora"或
/// launchd/systemd 与手工各起一份（agora-apr），说人话；没有才是端口被别的程序占了。
async fn report_bind_failure(home: &Path, addr: std::net::SocketAddr, err: &api::ServeError) {
    let socket = home.join(SOCKET_FILE);
    match local::request(&socket, &Request::Ping).await {
        Ok(Response::Pong) => eprintln!(
            "已有实例在跑：{} 有 daemon 应答，{addr} 也已被占用；要重启请先停掉它",
            socket.display()
        ),
        _ => eprintln!("{err}；端口被别的程序占用？改 config.yaml 的 server.listen"),
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
