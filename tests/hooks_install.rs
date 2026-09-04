//! `agora hooks install / uninstall`（ADR-002 D4；agora-dvh.1）。
//! 真实 agent 的热加载由人按剧本第 1 步验；这里验文件层面的承诺。

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use agora::adapter;
use agora::hook::install::{command, Installer, Plan};
use agora::hook::HOLD_TIMEOUT;
use serde_json::{json, Value};

fn installer(tmp: &Path) -> Installer {
    Installer {
        agora_home: tmp.join("agora"),
        user_home: tmp.join("home"),
    }
}

fn ours(v: &Value, home: &Path) -> usize {
    let marker = format!("{}/bin/agora hook ", home.display());
    v.to_string().matches(&marker).count()
}

#[test]
fn timeout_exceeds_hold() {
    // 安装到 agent 配置里的每个 timeout 必须大于 hooks.hold_timeout（55 min）加客户端余量，
    // 至少 PermissionRequest 如此；其余事件不挂起，只要 ≥ 1 s 够落盘。
    for host in adapter::hosts() {
        let spec = adapter::for_host(host).unwrap().install_spec();
        for h in spec {
            if h.event == "PermissionRequest" {
                assert!(h.timeout > HOLD_TIMEOUT + Duration::from_secs(30), "{host}");
            }
            assert!(h.timeout >= Duration::from_secs(1), "{host} {}", h.event);
        }
    }
}

#[test]
fn install_is_idempotent_and_uninstall_leaves_others_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let inst = installer(tmp.path());
    let hooks = adapter::for_host("claude").unwrap();
    let file = inst.user_home.join(".claude/settings.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    // 用户原有的配置：别人的 hook、别的键。
    let original = json!({
        "model": "opus",
        "hooks": {
            "Stop": [{ "hooks": [{ "type": "command", "command": "/usr/bin/say done", "timeout": 5 }] }],
            "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "/x/lint" }] }]
        }
    });
    std::fs::write(&file, original.to_string()).unwrap();

    let plan = inst.plan_install(hooks).unwrap();
    assert!(!plan.is_noop());
    assert!(plan.diff().contains("+ "), "装前要有 diff");
    inst.write(&plan).unwrap();
    let once: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let n = ours(&once, &inst.agora_home);
    assert_eq!(n, hooks.install_spec().len());
    assert_eq!(once["model"], "opus");
    assert_eq!(
        once["hooks"]["Stop"][0]["hooks"][0]["command"],
        "/usr/bin/say done"
    );
    assert_eq!(once["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    let pr = &once["hooks"]["PermissionRequest"][0]["hooks"][0];
    assert_eq!(pr["timeout"], 3600);
    assert_eq!(pr["command"], command(&inst.agora_home, "claude"));

    // 重复 install：不重复。
    let again = inst.plan_install(hooks).unwrap();
    assert!(again.is_noop(), "{}", again.diff());
    inst.write(&again).unwrap();
    let twice: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(ours(&twice, &inst.agora_home), n);

    // uninstall：只删自己的，用户原有 hooks 原样。
    let un = inst.plan_uninstall(hooks).unwrap();
    inst.write(&un).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(ours(&after, &inst.agora_home), 0);
    assert_eq!(after, original);
    // 再卸一次：无事发生。
    assert!(inst.plan_uninstall(hooks).unwrap().is_noop());
}

#[test]
fn install_creates_the_file_and_the_bin_link_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let inst = installer(tmp.path());
    let hooks = adapter::for_host("claude").unwrap();
    let plan = inst.plan_install(hooks).unwrap();
    inst.write(&plan).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&plan.file).unwrap()).unwrap();
    assert!(v["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"] == 1);
    let exe = std::env::current_exe().unwrap();
    let link = inst.ensure_bin_link(&exe).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    // 重指：先指错再修。
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("/bin/false", &link).unwrap();
    inst.ensure_bin_link(&exe).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
}

#[test]
fn dry_run_shows_the_diff_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let agora_home = tmp.path().join("agora");
    let user_home = tmp.path().join("home");
    let out = Command::new(env!("CARGO_BIN_EXE_agora"))
        .args([
            "hooks",
            "install",
            "claude",
            "--dry-run",
            "--home",
            agora_home.to_str().unwrap(),
            "--user-home",
            user_home.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("+ ") && err.contains("PermissionRequest"),
        "{err}"
    );
    assert!(!user_home.join(".claude/settings.json").exists());
    assert!(!agora_home.join("bin/agora").exists());

    // 不认识的 agent / 没有 hook 的 agent：用法错误 2。
    let out = Command::new(env!("CARGO_BIN_EXE_agora"))
        .args(["hooks", "install", "shell"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let _ = Plan {
        file: user_home,
        before: json!({}),
        after: json!({}),
    };
}

#[test]
fn grok_installs_into_its_own_global_hooks_file() {
    // ADR-002 D4：Grok 写 ~/.grok/hooks/agora.json（全局目录永远受信）；没有 PermissionRequest
    // 可装；命令 exec 进 agora，hook 的 ppid 才是 grok 本体（agent_pid 的依据）。
    let tmp = tempfile::tempdir().unwrap();
    let inst = installer(tmp.path());
    let hooks = adapter::for_host("grok").unwrap();
    let plan = inst.plan_install(hooks).unwrap();
    assert_eq!(plan.file, inst.user_home.join(".grok/hooks/agora.json"));
    inst.write(&plan).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&plan.file).unwrap()).unwrap();
    assert!(v["hooks"].get("PermissionRequest").is_none());
    assert_eq!(
        v["hooks"]["Notification"][0]["matcher"],
        "permission_prompt|idle_prompt"
    );
    assert!(v["hooks"]["StopCancelled"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("exec "));
    assert_eq!(ours(&v, &inst.agora_home), hooks.install_spec().len());
    assert!(inst.plan_install(hooks).unwrap().is_noop());
    let un = inst.plan_uninstall(hooks).unwrap();
    inst.write(&un).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&plan.file).unwrap()).unwrap();
    assert_eq!(ours(&after, &inst.agora_home), 0);
}
