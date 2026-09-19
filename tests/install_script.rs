//! `scripts/install.sh` 的机械守卫（A26 的机械半边；agora-7ku.1）。
//! zuan 实机的安装、`systemctl --user is-enabled`、`sudo reboot` 后自起归实机专场；这里钉住
//! 脚本对文件系统的承诺：目录布局、链接目标、config.yaml 能被 daemon 自己的加载器接受、
//! 单元文件写了 LANG、重跑不动已有文件、dry-run 一个字节都不写、tmux 低于下限拒绝且不留痕。
//! 两个 CI OS 都跑，不需要 root：--home / --unit-dir 都落在 tempdir，--no-service --skip-tmux。
//!
//! macOS 分支（launchd 单元 + bootstrap）靠 `AGORA_INSTALL_OS=Darwin` 加一个假 `launchctl` 在
//! Linux 上跑（agora-5gg.15）：CI 虽有 macos runner，可旧有的守卫一律带 `--no-service`，
//! `launchctl` 那一半（print / bootstrap / bootout）在任何机器上都没被执行过。真 Mac 上
//! "bootstrap 后杀掉 daemon 能自动拉起"仍归人眼验收。
//! 假 launchctl 只钉住脚本**怎么叫** launchctl（print → bootstrap / bootout 的顺序与参数），
//! 不假装验证 launchd 本身的语义。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use agora::config::Config;
use sha2::{Digest, Sha256};

const AGORA_BIN: &str = env!("CARGO_BIN_EXE_agora");
const LISTEN: &str = "127.0.0.1:7761";

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh")
}

/// 跑一次安装脚本；`path` 非空时替换 PATH（放假 tmux、假 launchctl 用）。
fn run(args: &[&str], path: Option<&str>) -> Output {
    run_env(args, path, &[])
}

/// 同 `run`，但要额外设环境变量（扮演系统、假 launchctl 的日志与状态文件）。
fn run_env(args: &[&str], path: Option<&str>, envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(script());
    cmd.args(args);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// 除 --no-service 外的公共参数（launchd 守卫要跑真服务分支，靠假 launchctl 接住）。
fn install_args<'a>(home: &'a Path, units: &'a Path) -> Vec<&'a str> {
    vec![
        "--binary",
        AGORA_BIN,
        "--home",
        home.to_str().unwrap(),
        "--unit-dir",
        units.to_str().unwrap(),
        "--listen",
        LISTEN,
        "--skip-tmux",
    ]
}

/// 假 launchctl：每次调用把 argv 记进 `log`，用 `state` 文件存在与否表示"单元已加载"。
/// `FAKE_BOOTSTRAP_RC` 非 0 时模拟 `Bootstrap failed: 5: Input/output error`（gui 域不在）。
/// 返回（要前插到 PATH 的目录、日志路径、状态文件路径）。只记命令形状，不模拟 launchd 行为。
fn fake_launchctl(tmp: &Path) -> (String, PathBuf, PathBuf) {
    let fake_bin = tmp.join("fakebin");
    fs::create_dir_all(&fake_bin).unwrap();
    let script = fake_bin.join("launchctl");
    fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$LAUNCHCTL_LOG"
case "$1" in
  print) [ -f "$LAUNCHD_STATE" ] ;;
  bootstrap)
    rc=${FAKE_BOOTSTRAP_RC:-0}
    if [ "$rc" != 0 ]; then echo "Bootstrap failed: $rc: Input/output error" >&2; exit "$rc"; fi
    : > "$LAUNCHD_STATE" ;;
  bootout) rm -f "$LAUNCHD_STATE" ;;
  *) : ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (
        format!(
            "{}:{}",
            fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
        tmp.join("launchctl.log"),
        tmp.join("launchd_loaded"),
    )
}

fn launchctl_calls(log: &Path) -> Vec<String> {
    if !log.exists() {
        return Vec::new();
    }
    fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

/// plist 的键与值之间只差空白，抹掉空白再断言结构（模板缩进动一下不必把测试弄红）。
fn flat(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 取 plist 里 `<key>KEY</key>` 之后第一个 `<string>…</string>` 的内容。模板里每个键都这么用；
/// 不为此引一个 XML 解析器依赖（结构错了上面那些 flat() 断言会红）。
fn plist_string(text: &str, key: &str) -> String {
    let after = text
        .split(&format!("<key>{key}</key>"))
        .nth(1)
        .unwrap_or_else(|| panic!("单元文件里没 <key>{key}</key>:\n{text}"));
    let start = after
        .find("<string>")
        .unwrap_or_else(|| panic!("<key>{key}</key> 后没 <string>:\n{text}"))
        + "<string>".len();
    let end = start + after[start..].find("</string>").unwrap();
    after[start..end].to_string()
}

fn sha256_hex(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mtime(path: &Path) -> SystemTime {
    fs::symlink_metadata(path).unwrap().modified().unwrap()
}

fn unit_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "dev.agora.daemon.plist"
    } else {
        "agora.service"
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn install_writes_config_link_and_unit_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let args = [
        "--binary",
        AGORA_BIN,
        "--home",
        home.to_str().unwrap(),
        "--unit-dir",
        units.to_str().unwrap(),
        "--listen",
        LISTEN,
        "--no-service",
        "--skip-tmux",
    ];

    let out = run(&args, None);
    assert!(out.status.success(), "{}", stderr(&out));
    // 给人看的话全在 stderr，stdout 留给机器（MISSION §2.3 规则 10）。
    assert!(out.stdout.is_empty(), "stdout 应为空: {:?}", out.stdout);

    // versions/<sha 前 12 位>/agora：内容与源二进制逐字节相同，0755。
    let sha = sha256_hex(Path::new(AGORA_BIN));
    let versioned = home.join("versions").join(&sha[..12]).join("agora");
    assert!(versioned.is_file(), "{}", versioned.display());
    assert_eq!(sha256_hex(&versioned), sha);
    assert_eq!(
        fs::metadata(&versioned).unwrap().permissions().mode() & 0o777,
        0o755
    );

    // bin/agora 是指向它的符号链接，目标是绝对路径（canonicalize 后相等：macOS 的 /var 是
    // /private/var 的链接，脚本走 pwd -P 得到的就是真实路径）。
    let link = home.join("bin/agora");
    let target = fs::read_link(&link).unwrap();
    assert!(target.is_absolute(), "{}", target.display());
    assert_eq!(
        target.canonicalize().unwrap(),
        versioned.canonicalize().unwrap()
    );

    // config.yaml 用 daemon 自己的加载器读：未知键会在这里炸（docs/spec/config.md）。
    let config = home.join("config.yaml");
    let settings = Config::load(&home, "tmux").unwrap();
    assert_eq!(settings.listen.to_string(), LISTEN);
    assert!(!settings.node_id.is_empty());
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );

    // 单元文件：LANG=C.UTF-8（ADR-001 D7）与 bin/agora 的路径。
    let unit = units.join(unit_name());
    let text = fs::read_to_string(&unit).unwrap();
    assert!(text.contains("C.UTF-8"), "{text}");
    assert!(text.contains("LANG"), "{text}");
    // systemd 默认 KillMode=control-group 会在 stop / restart 时把 daemon 起的 tmux server 与全部
    // agent 一起杀（2026-09-07 zuan 实测，agora-x1z；不变量 3）。这一行没了就是 A39 升级杀光会话。
    if cfg!(target_os = "linux") {
        assert!(text.contains("KillMode=process"), "{text}");
    }
    let link_real = link.parent().unwrap().canonicalize().unwrap().join("agora");
    assert!(
        text.contains(&format!("{} serve", link_real.display()))
            || text.contains(&format!("<string>{}</string>", link_real.display())),
        "{text}"
    );

    // 重跑：退出 0，三个文件的 mtime 与内容都不变，链接也不动。
    let before = [
        (config.clone(), mtime(&config), fs::read(&config).unwrap()),
        (
            versioned.clone(),
            mtime(&versioned),
            fs::read(&versioned).unwrap(),
        ),
        (unit.clone(), mtime(&unit), fs::read(&unit).unwrap()),
    ];
    let link_mtime = mtime(&link);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let again = run(&args, None);
    assert!(again.status.success(), "{}", stderr(&again));
    for (path, m, content) in &before {
        assert_eq!(&mtime(path), m, "{} 被重写", path.display());
        assert_eq!(
            &fs::read(path).unwrap(),
            content,
            "{} 内容变了",
            path.display()
        );
    }
    assert_eq!(mtime(&link), link_mtime, "链接被重做");
    assert_eq!(fs::read_link(&link).unwrap(), target);
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let out = run(
        &[
            "--binary",
            AGORA_BIN,
            "--home",
            home.to_str().unwrap(),
            "--unit-dir",
            units.to_str().unwrap(),
            "--listen",
            LISTEN,
            "--no-service",
            "--skip-tmux",
            "--dry-run",
        ],
        None,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    let left: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
    assert!(left.is_empty(), "dry-run 写了东西: {left:?}");
}

#[test]
fn refuses_tmux_below_minimum() {
    let tmp = tempfile::tempdir().unwrap();
    // PATH 最前面放一个假 tmux：`tmux -V` 报 3.1，低于 3.2。只测版本判断，不真装。
    let fake_bin = tmp.path().join("fakebin");
    fs::create_dir(&fake_bin).unwrap();
    let fake_tmux = fake_bin.join("tmux");
    fs::write(&fake_tmux, "#!/bin/sh\necho \"tmux 3.1\"\n").unwrap();
    fs::set_permissions(&fake_tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let out = run(
        &[
            "--binary",
            AGORA_BIN,
            "--home",
            home.to_str().unwrap(),
            "--unit-dir",
            units.to_str().unwrap(),
            "--listen",
            LISTEN,
            "--no-service",
            "--skip-tmux",
        ],
        Some(&path),
    );
    assert!(
        !out.status.success(),
        "3.1 的 tmux 应被拒绝: {}",
        stderr(&out)
    );
    assert!(out.stdout.is_empty());
    assert!(!home.exists(), "拒绝时不该建 AGORA_HOME");
    assert!(!units.exists(), "拒绝时不该写单元文件");
    // tempdir 里只剩我们自己放的假 tmux。
    let names: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("fakebin")]);
}

// ---------- macOS 分支（launchd） ----------
// 下面几条靠 AGORA_INSTALL_OS=Darwin + 假 launchctl 在 Linux 上跑真服务分支（agora-5gg.15）。
// 钉的是脚本对 launchctl 的调用序列与单元内容；真 Mac 上 launchd 能把进程拉起、bootstrap
// 在 ssh 会话里会不会撞 gui 域，仍归人眼（见 docs/spec/config.md「安装」）。

/// 公共环境：假 launchctl 的日志与状态文件。
fn launchd_env<'a>(log: &'a Path, state: &'a Path) -> [(&'a str, &'a str); 3] {
    [
        ("AGORA_INSTALL_OS", "Darwin"),
        ("LAUNCHCTL_LOG", log.to_str().unwrap()),
        ("LAUNCHD_STATE", state.to_str().unwrap()),
    ]
}

#[test]
fn macos_branch_writes_launchd_unit_and_bootstraps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let (path, log, state) = fake_launchctl(tmp.path());

    let out = run_env(
        &install_args(&home, &units),
        Some(&path),
        &launchd_env(&log, &state),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());

    // 单元目录里只有 launchd 的 plist，没有 systemd 的单元。
    let plist = units.join("dev.agora.daemon.plist");
    assert!(plist.is_file(), "{}", plist.display());
    assert!(
        !units.join("agora.service").exists(),
        "扮演 Darwin 时不该写 systemd 单元"
    );

    let text = fs::read_to_string(&plist).unwrap();
    let f = flat(&text);
    assert!(
        f.contains("<key>Label</key><string>dev.agora.daemon</string>"),
        "{text}"
    );
    // ProgramArguments 走 <home>/bin/agora 这条稳定路径：升级只换链接目标，单元不用重做。
    let link_real = home.join("bin").canonicalize().unwrap().join("agora");
    // 脚本把 UNIT_DIR / HOME_DIR 都按 `pwd -P` 解过符号链接（macOS 的 tempdir 在 /private/var 下），
    // 断言里的路径要跟它同一份形式，否则 macOS CI 上只是差一个 /private 前缀就红。
    let plist_real = units.canonicalize().unwrap().join("dev.agora.daemon.plist");
    assert!(
        f.contains(&format!(
            "<key>ProgramArguments</key><array><string>{}</string><string>serve</string></array>",
            link_real.display()
        )),
        "{text}"
    );
    assert!(f.contains("<key>RunAtLoad</key><true/>"), "{text}");
    // KeepAlive 退化成裸 <true/> 就是「daemon 自己收工也被 launchd 永动拉起」，
    // 与 agora upgrade 的正常退出相冲（等价物是 systemd 的 Restart=on-failure）。
    assert!(
        f.contains("<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>"),
        "{text}"
    );
    assert!(
        f.contains("<key>LANG</key><string>C.UTF-8</string>"),
        "{text}"
    );
    // 占位符必须全替掉：漏一个 @PATH@ 进单元，launchd 起的进程找不到 tmux。
    assert!(!text.contains('@'), "占位符没替换干净:\n{text}");
    assert_eq!(
        Path::new(&plist_string(&text, "AGORA_HOME"))
            .canonicalize()
            .unwrap(),
        home.canonicalize().unwrap(),
        "AGORA_HOME 指错目录，单元起的是另一个实例"
    );
    let unit_path = plist_string(&text, "PATH");
    assert!(
        unit_path.contains("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"),
        "PATH 不是 macOS 那一套（homebrew 装 tmux 就在这些目录里）: {unit_path}"
    );

    // 写了还要 load：print 探状态 → bootstrap 装载。只写文件不装载 = Mac 上的 daemon 还是没人管，
    // 就是本任务要修的 D1（手工起的 daemon 停了 3.5 天没人知道）。
    let calls = launchctl_calls(&log);
    let uid = uid_hint(&calls);
    assert_eq!(
        calls,
        vec![
            format!("print gui/{uid}/dev.agora.daemon"),
            format!("bootstrap gui/{uid} {}", plist_real.display())
        ],
        "{calls:?}"
    );
    assert!(state.exists(), "bootstrap 没落地");
    assert!(stderr(&out).contains("已 bootstrap"), "{}", stderr(&out));
}

/// 断言里不写死 uid（不同机器不一样）也不写死 tempdir 的真实路径（macOS 的 /var 是
/// /private/var 的链接，脚本走 `pwd -P` 拿的是后者）：uid 从第一行 print 里反推。
fn uid_hint(calls: &[String]) -> String {
    calls
        .first()
        .and_then(|c| c.strip_prefix("print gui/"))
        .and_then(|rest| rest.split('/').next())
        .map(|uid| uid.to_string())
        .unwrap_or_else(|| panic!("假 launchctl 没收到 print: {calls:?}"))
}

#[test]
fn macos_rerun_with_unchanged_unit_leaves_daemon_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let (path, log, state) = fake_launchctl(tmp.path());
    let envs = launchd_env(&log, &state);
    let args = install_args(&home, &units);

    let first = run_env(&args, Some(&path), &envs);
    assert!(first.status.success(), "{}", stderr(&first));
    let plist = units.join("dev.agora.daemon.plist");
    let before = (mtime(&plist), fs::read(&plist).unwrap());

    // 只留第二次的调用：这一轮单元没变、服务已加载，launchctl 应该只被问一次状态。
    fs::write(&log, "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let again = run_env(&args, Some(&path), &envs);
    assert!(again.status.success(), "{}", stderr(&again));
    let calls = launchctl_calls(&log);
    assert_eq!(calls.len(), 1, "重跑不该动服务: {calls:?}");
    assert!(
        calls[0].starts_with("print gui/") && calls[0].ends_with("/dev.agora.daemon"),
        "{calls:?}"
    );
    assert_eq!(mtime(&plist), before.0, "plist 被重写");
    assert_eq!(fs::read(&plist).unwrap(), before.1);
    assert!(
        stderr(&again).contains("不重启 daemon"),
        "{}",
        stderr(&again)
    );
}

#[test]
fn macos_changed_unit_is_reloaded_with_bootout_then_bootstrap() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let (path, log, state) = fake_launchctl(tmp.path());
    let envs = launchd_env(&log, &state);
    let args = install_args(&home, &units);

    let first = run_env(&args, Some(&path), &envs);
    assert!(first.status.success(), "{}", stderr(&first));
    let plist = units.join("dev.agora.daemon.plist");
    let plist_real = units.canonicalize().unwrap().join("dev.agora.daemon.plist");
    let fresh = fs::read_to_string(&plist).unwrap();

    // 模拟磁盘上躺着旧版单元（上一次安装写的，这一版模板改了：PATH、KeepAlive 都可能不一样）。
    fs::write(&plist, format!("{fresh}<!-- 旧版单元 -->\n")).unwrap();
    fs::write(&log, "").unwrap();

    let out = run_env(&args, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = launchctl_calls(&log);
    // 内容变了就要 bootout + bootstrap：kickstart -k 只把已加载的那份定义重启，不重读 plist，
    // 拿它当「让它读新单元」会让人以为新版已生效，实际跑的还是旧 PATH。
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(calls[0].starts_with("print gui/"), "{calls:?}");
    assert_eq!(
        calls[1],
        format!("bootout gui/{}/dev.agora.daemon", uid_hint(&calls))
    );
    assert!(
        calls[2].starts_with("bootstrap gui/")
            && calls[2].ends_with(&format!("{}", plist_real.display())),
        "{calls:?}"
    );
    assert_eq!(fs::read_to_string(&plist).unwrap(), fresh, "新单元没写回");
}

#[test]
fn macos_bootstrap_failure_prints_the_manual_command() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let (path, log, state) = fake_launchctl(tmp.path());
    let mut envs = launchd_env(&log, &state).to_vec();
    envs.push(("FAKE_BOOTSTRAP_RC", "5"));

    let out = run_env(&install_args(&home, &units), Some(&path), &envs);
    assert!(
        !out.status.success(),
        "bootstrap 失败不能当成装好了: {}",
        stderr(&out)
    );
    assert!(out.stdout.is_empty());
    let err = stderr(&out);
    // 裸一个 “Bootstrap failed: 5” 不告诉人为什么（从 ssh 装时 gui 域常常不在）：要给可执行命令。
    assert!(err.contains("launchctl asuser"), "{err}");
    assert!(err.contains("launchctl bootstrap gui/"), "{err}");
    // 单元文件照写（人只要补一句 bootstrap 就生效），但不能假装已加载。
    assert!(units.join("dev.agora.daemon.plist").is_file(), "{err}");
    assert!(!state.exists(), "bootstrap 没成功却写了状态文件: {err}");
}

#[test]
fn macos_dry_run_and_no_service_never_call_launchctl() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let (path, log, state) = fake_launchctl(tmp.path());
    let envs = launchd_env(&log, &state);

    // --dry-run 连 --no-service 都不给也不该碰 launchctl：在真 Mac 上误跑一次就把在服务的
    // daemon 抽掉一轮（[dry-run] 那行只是打印计划，不得走真命令）。
    let mut dry = install_args(&home, &units);
    dry.push("--dry-run");
    let out = run_env(&dry, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !log.exists(),
        "dry-run 叫了 launchctl: {:?}",
        launchctl_calls(&log)
    );
    assert!(
        !home.exists() && !units.exists(),
        "dry-run 写了东西: {} / {}",
        home.display(),
        units.display()
    );

    // --no-service：单元文件照写（开发机验证、指到临时目录的安装都靠这一支），但不 bootstrap。
    let mut nosvc = install_args(&home, &units);
    nosvc.push("--no-service");
    let out = run_env(&nosvc, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(units.join("dev.agora.daemon.plist").is_file());
    assert!(
        !log.exists(),
        "--no-service 不该叫 launchctl: {:?}",
        launchctl_calls(&log)
    );
    assert!(stderr(&out).contains("--no-service"), "{}", stderr(&out));
}

#[test]
fn macos_warns_when_an_unmanaged_daemon_holds_the_home() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agora");
    let units = tmp.path().join("units");
    let (path, log, state) = fake_launchctl(tmp.path());
    let envs = launchd_env(&log, &state);
    let args = install_args(&home, &units);

    // 本任务要修的就是 Mac 上那台手工起的 daemon（bd memories mac-agora-daemon-manual-restart）。
    // 装 launchd 单元时它还活着：agora.pid 分不清是谁起的 daemon（launchd 起手的也写同一个文件），
    // 但"单元没装载 + pid 活着"两件事同时成立就是确证。用测试进程自己的 pid——它在这一刻必然活着。
    fs::create_dir_all(&home).unwrap();
    let pid = std::process::id();
    fs::write(home.join("agora.pid"), format!("{pid}\n")).unwrap();

    let out = run_env(&args, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    let err = stderr(&out);
    assert!(err.contains("单元没装载"), "{err}");
    assert!(err.contains("手工 daemon"), "{err}");
    // 光说"有个 daemon 在跑"没有可执行性：要把 kill 与复核命令一起给。
    assert!(err.contains(&format!("kill {pid}")), "{err}");
    assert!(err.contains("launchctl print gui/"), "{err}");
    // 只警告不拒装：人把旧进程停掉之后，launchd 的节流重试自己会把它拉起来。
    let calls = launchctl_calls(&log);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[1].starts_with("bootstrap gui/"), "{calls:?}");

    // 反向对照一：pid 文件不在（干净机器、或上一代 daemon 正常退出删掉了它）就一句警告都不该有，
    // 否则每次正常安装都白挨一吓。清掉假 launchd 的状态文件，重新走"单元没装载"那一支。
    fs::remove_file(home.join("agora.pid")).unwrap();
    fs::remove_file(&state).unwrap();
    fs::write(&log, "").unwrap();
    let out = run_env(&args, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(!err.contains("单元没装载"), "没有 pid 文件却警告了: {err}");
    assert_eq!(
        launchctl_calls(&log).len(),
        2,
        "这一轮也该 bootstrap: {err}"
    );

    // 反向对照二：pid 文件内容不是数字（半文件、被人写过）时不去 kill -0 一个不存在的东西。
    fs::write(home.join("agora.pid"), "not-a-pid\n").unwrap();
    fs::remove_file(&state).unwrap();
    fs::write(&log, "").unwrap();
    let out = run_env(&args, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("单元没装载"),
        "pid 内容不是数字时不该警告: {}",
        stderr(&out)
    );

    // 反向对照三：pid 文件留着但进程早没了（被 kill -9、没走到 Drop）。拿一个自己回收掉的子进程
    // 的 pid——wait 之后槽位就空了。理论上这个空槽可能被下一次 fork（就是安装脚本本身）占回去而
    // 假红，本机 pid_max=4194304、概率约 1/在用车位数，2026-09-19 跑 20 次未复现。
    fs::write(home.join("agora.pid"), format!("{}\n", dead_pid())).unwrap();
    fs::remove_file(&state).unwrap();
    fs::write(&log, "").unwrap();
    let out = run_env(&args, Some(&path), &envs);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("单元没装载"),
        "pid 已经不活着却警告了: {}",
        stderr(&out)
    );
}

/// 一个当前不存在的 pid：spawn 一个 sleep 再 kill + wait 回收，槽位就空了（两个平台都这么拿）。
fn dead_pid() -> u32 {
    let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let _ = child.kill();
    child.wait().unwrap();
    pid
}
