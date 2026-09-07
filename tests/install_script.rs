//! `scripts/install.sh` 的机械守卫（A26 的机械半边；agora-7ku.1）。
//! zuan 实机的安装、`systemctl --user is-enabled`、`sudo reboot` 后自起归实机专场；这里钉住
//! 脚本对文件系统的承诺：目录布局、链接目标、config.yaml 能被 daemon 自己的加载器接受、
//! 单元文件写了 LANG、重跑不动已有文件、dry-run 一个字节都不写、tmux 低于下限拒绝且不留痕。
//! 两个 CI OS 都跑，不需要 root：--home / --unit-dir 都落在 tempdir，--no-service --skip-tmux。

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

/// 跑一次安装脚本；`path` 非空时替换 PATH（放假 tmux 用）。
fn run(args: &[&str], path: Option<&str>) -> Output {
    let mut cmd = Command::new(script());
    cmd.args(args);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    cmd.output().unwrap()
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
