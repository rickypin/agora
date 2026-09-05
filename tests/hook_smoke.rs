//! 版本漂移守卫（ADR-002 D10 第 4 条；agora-3la.2）：对本机装着的每个有 hook 的 agent 跑一轮
//! 无头最小交互（`AgentIdentity::headless_args`），经真实 `agora hook --record` 拿到事件，事件名
//! 集合与每个事件的顶层键集合必须与 `testdata/<host>/<version>/hooks/headless.jsonl` 一致——
//! agent 升级改了事件名或字段，这里变红并给出录制命令，而不是线上永远 RUNNING。agent 版本不在
//! 版本表、或版本目录里没有 fixture，同样红。没装的 agent 跳过：GitHub 的 runner 上一个都没有。
//!
//! 三个冒烟是 `#[ignore]`：CI 用 `cargo test --test hook_smoke -- --ignored` 显式跑；开发机上
//! 普通 `cargo test` 不该每次都真起三个 agent 花 token 等半分钟。
//!
//! 隔离：agent 的 HOME 是临时目录，用户自己的配置一个字节不动，hook 装进临时 HOME，事件投到
//! 临时 AGORA_HOME。只借用登录凭据（不借就是 "Not logged in"）：Claude 复制 `~/.claude.json`，
//! macOS 上再把 `~/Library` 软链进临时 HOME——keychain 是按 HOME 找的，2026-09-05 实测不链就
//! 找不到凭据；Linux 上复制 `~/.claude/.credentials.json`。Grok / Codex 各复制自己的 auth.json。
//!
//! 录新 fixture：`AGORA_SMOKE_RECORD=1 cargo test --test hook_smoke <host> -- --ignored --nocapture`
//! ——fixture 不存在就写到位，存在就写到旁边的 `.new`，人核对后替换并补 `expect` 行
//! （`testdata/README.md`）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use agora::adapter::{self, VersionProbe};
use agora::hook::install::Installer;
use agora::hook::record;

const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");
const PROMPT: &str = "Reply with exactly: pong";
const SCENARIO: &str = "headless";
/// 一轮 "pong" 连模型延迟带 hook 落盘，实测 5–20 s；给足余量。
const AGENT_TIMEOUT: Duration = Duration::from_secs(180);

static N: AtomicU32 = AtomicU32::new(0);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_path(host: &str, version: &str) -> PathBuf {
    root()
        .join("testdata")
        .join(host)
        .join(version)
        .join("hooks")
        .join(format!("{SCENARIO}.jsonl"))
}

/// 事件名 → 顶层键集合。同名事件出现多次取并集：Stop 有没有 `last_assistant_message` 之类
/// 看那一轮说没说话，不是版本差异。
fn key_sets(text: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for raw in text.lines() {
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let line: serde_json::Value =
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        let Some(payload) = line.get("payload").and_then(|p| p.as_object()) else {
            continue;
        };
        let event = payload
            .get("hook_event_name")
            .or_else(|| payload.get("hookEventName"))
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_owned();
        out.entry(event)
            .or_default()
            .extend(payload.keys().cloned());
    }
    out
}

/// `SessionStart` / `session_start` 归一成 `sessionstart`。
fn norm(event: &str) -> String {
    event
        .chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn copy_if_exists(from: &Path, to: &Path) {
    if from.exists() {
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::copy(from, to).unwrap_or_else(|e| panic!("复制 {} 失败: {e}", from.display()));
    }
}

/// 只借凭据，不借配置。
fn seed_credentials(host: &str, real: &Path, user: &Path) {
    match host {
        "claude" => {
            copy_if_exists(&real.join(".claude.json"), &user.join(".claude.json"));
            copy_if_exists(
                &real.join(".claude/.credentials.json"),
                &user.join(".claude/.credentials.json"),
            );
            if cfg!(target_os = "macos") {
                let lib = real.join("Library");
                if lib.exists() {
                    std::os::unix::fs::symlink(&lib, user.join("Library")).unwrap();
                }
            }
        }
        "grok" => copy_if_exists(&real.join(".grok/auth.json"), &user.join(".grok/auth.json")),
        "codex" => copy_if_exists(
            &real.join(".codex/auth.json"),
            &user.join(".codex/auth.json"),
        ),
        other => panic!("{other} 没有凭据借用规则：新 adapter 要在这里加一条"),
    }
}

fn run_with_timeout(cmd: &mut Command, limit: Duration) -> (Option<i32>, String, String) {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("起 agent 失败: {e}"));
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    (status, out.join().unwrap(), err.join().unwrap())
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn smoke(host: &str) {
    let a = adapter::find(host).unwrap_or_else(|| panic!("{host} 不是内置 adapter"));
    let hooks = a
        .hooks()
        .unwrap_or_else(|| panic!("{host} 没有 hook，冒烟无从谈起"));
    let cmd = a.default_command();
    let version = match adapter::probe(a, cmd, Duration::from_secs(15)) {
        VersionProbe::Missing => {
            eprintln!("{host}: `{cmd}` 不在 PATH，跳过");
            return;
        }
        VersionProbe::Unparsable(why) => panic!(
            "{host}: `{cmd} --version` 不可解析或不在版本表（{why}）——先在 adapter 的版本表加一行，再录 fixture"
        ),
        VersionProbe::Available(v) => v,
    };
    let Some(args) = a.headless_args(PROMPT) else {
        eprintln!("{host}: 没有无头模式，跳过");
        return;
    };
    let fixture = fixture_path(host, &version.to_string());
    let record_mode = std::env::var_os("AGORA_SMOKE_RECORD").is_some();
    let rerecord = format!(
        "录新 fixture：AGORA_SMOKE_RECORD=1 cargo test --test hook_smoke {host} -- --ignored --nocapture，核对后补 expect（testdata/README.md）"
    );
    assert!(
        fixture.exists() || record_mode,
        "{host} {version}: 没有 {}——`{cmd}` 升级了？{rerecord}",
        fixture.display()
    );

    // 隔离环境：AGORA_HOME 要短路径（socket 路径上限），用户 HOME 随便。
    let real_home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let n = N.fetch_add(1, Ordering::SeqCst);
    let agora_home = PathBuf::from(format!("/tmp/agsm-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&agora_home);
    agora::local::ensure_home(&agora_home).unwrap();
    let user = tempfile::tempdir().unwrap();
    let user_home = user.path().to_path_buf();
    let work = user_home.join("work");
    fs::create_dir_all(&work).unwrap();
    seed_credentials(host, &real_home, &user_home);
    let inst = Installer {
        agora_home: agora_home.clone(),
        user_home: user_home.clone(),
    };
    inst.ensure_bin_link(Path::new(AGORA_BIN)).unwrap();
    let plan = inst.plan_install(hooks).unwrap();
    inst.write(&plan).unwrap();
    let rec = agora_home.join(format!("{SCENARIO}.jsonl"));

    let mut command = Command::new(cmd);
    command
        .args(&args)
        .current_dir(&work)
        .env("HOME", &user_home)
        .env(record::ENV, &rec)
        .env_remove("AGORA_HOME")
        .env_remove("AGORA_SESSION_ID")
        .env_remove("AGORA_EPOCH")
        .env_remove("GROK_SESSION_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME");
    let started = Instant::now();
    let (status, stdout, stderr) = run_with_timeout(&mut command, AGENT_TIMEOUT);
    let context = format!(
        "agent 退出码 {status:?}，用时 {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        started.elapsed(),
        tail(&stdout, 10),
        tail(&stderr, 10)
    );
    let text = fs::read_to_string(&rec).unwrap_or_else(|_| {
        panic!("{host} {version}: 一条 hook 事件都没收到——hook 装进 {} 了但 `{cmd}` 没跑它？\n{context}",
            plan.file.display())
    });
    let got = key_sets(&text);
    let events: BTreeSet<String> = got.keys().map(|e| norm(e)).collect();
    assert!(
        events.contains("sessionstart") && events.contains("stop"),
        "{host} {version}: 无头一轮没拿到 SessionStart + Stop，只有 {:?}\n{context}",
        got.keys().collect::<Vec<_>>()
    );

    if record_mode {
        let target = if fixture.exists() {
            fixture.with_extension("jsonl.new")
        } else {
            fixture.clone()
        };
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let header = format!(
            "# {host} {version} 无头一轮（`{cmd} {}`），tests/hook_smoke.rs 以 AGORA_SMOKE_RECORD=1 录于 {}；expect 待人核对后补。\n",
            args.join(" "),
            agora::hook::inbox::local_time_string()
        );
        fs::write(&target, header + &text).unwrap();
        eprintln!("{host}: 已录到 {}", target.display());
        if target != fixture {
            eprintln!("{host}: fixture 已存在，请核对 .new 后替换");
        }
    }

    let want = key_sets(&fs::read_to_string(&fixture).unwrap_or_default());
    let mut diff = Vec::new();
    for (event, keys) in &want {
        match got.get(event) {
            None => diff.push(format!("  {event}: 这次没收到")),
            Some(g) if g != keys => {
                let missing: Vec<_> = keys.difference(g).collect();
                let extra: Vec<_> = g.difference(keys).collect();
                diff.push(format!("  {event}: 少了 {missing:?}，多了 {extra:?}"));
            }
            Some(_) => {}
        }
    }
    for event in got.keys() {
        if !want.contains_key(event) {
            diff.push(format!("  {event}: fixture 里没有这个事件"));
        }
    }
    assert!(
        diff.is_empty(),
        "{host} {version} 的无头一轮与 {} 的键集合不一致：\n{}\n{rerecord}\n{context}",
        fixture.display(),
        diff.join("\n")
    );
    let _ = fs::remove_dir_all(&agora_home);
    eprintln!(
        "{host} {version}: {} 个事件的键集合与 fixture 一致（{:?}）",
        got.len(),
        started.elapsed()
    );
}

#[test]
#[ignore = "真起本机的 agent；CI 用 cargo test --test hook_smoke -- --ignored 显式跑"]
fn claude() {
    smoke("claude");
}

#[test]
#[ignore = "真起本机的 agent；CI 用 cargo test --test hook_smoke -- --ignored 显式跑"]
fn codex() {
    smoke("codex");
}

#[test]
#[ignore = "真起本机的 agent；CI 用 cargo test --test hook_smoke -- --ignored 显式跑"]
fn grok() {
    smoke("grok");
}

/// 新 adapter 有 hook 就得有自己的冒烟函数与凭据借用规则；这一条不 ignore，CI 每次跑。
#[test]
fn every_hooked_agent_has_a_smoke() {
    assert_eq!(adapter::hosts(), vec!["claude", "codex", "grok"]);
}

/// 每个版本目录的无头 fixture 都是合法的、含 SessionStart + Stop 的录制——不 ignore，CI 每次跑，
/// 没有真实 agent 也守得住"fixture 本身没坏"。
#[test]
fn headless_fixtures_carry_session_start_and_stop() {
    let mut seen = 0;
    for host in adapter::hosts() {
        let Ok(versions) = fs::read_dir(root().join("testdata").join(host)) else {
            continue;
        };
        for v in versions.flatten() {
            let f = v.path().join("hooks").join(format!("{SCENARIO}.jsonl"));
            if !f.exists() {
                continue;
            }
            let events: BTreeSet<String> = key_sets(&fs::read_to_string(&f).unwrap())
                .keys()
                .map(|e| norm(e))
                .collect();
            assert!(
                events.contains("sessionstart") && events.contains("stop"),
                "{}: {events:?}",
                f.display()
            );
            seen += 1;
        }
    }
    assert!(seen > 0, "没有任何无头 fixture");
}
