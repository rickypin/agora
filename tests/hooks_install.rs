//! `agora hooks install / uninstall`（ADR-002 D4；agora-dvh.1）。
//! 真实 agent 的热加载由人按剧本第 1 步验；这里验文件层面的承诺。

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agora::adapter;
use agora::hook::install::{command, run_with, Installer, Plan};
use agora::hook::HOLD_TIMEOUT;
use serde_json::{json, Value};

fn installer(tmp: &Path) -> Installer {
    Installer {
        agora_home: tmp.join("agora"),
        user_home: tmp.join("home"),
    }
}

/// tmp 里的一个假"当前二进制"：`run_with` 只拿它的路径建链接，内容无所谓。
fn fake_exe(tmp: &Path, name: &str) -> PathBuf {
    let exe = tmp.join(name);
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();
    exe
}

/// 跑一次 `agora hooks <action> claude`，AGORA_HOME / HOME 都落在 tmp 下（与 `installer` 同一套
/// 路径），返回它对用户说的全部话。
fn hooks_cmd(tmp: &Path, action: &str, exe: &Path, dry_run: bool) -> String {
    let inst = installer(tmp);
    let agora_home = inst.agora_home.to_str().unwrap();
    let user_home = inst.user_home.to_str().unwrap();
    let mut argv = vec![
        action,
        "claude",
        "--home",
        agora_home,
        "--user-home",
        user_home,
    ];
    if dry_run {
        argv.push("--dry-run");
    }
    let mut out = Vec::new();
    run_with(&argv, exe, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn ours(v: &Value, home: &Path) -> usize {
    let marker = format!("{}/bin/agora hook ", home.display());
    v.to_string().matches(&marker).count()
}

#[test]
fn timeout_exceeds_hold() {
    // 安装到 agent 配置里的每个 timeout 必须大于该宿主的挂起上限（默认 55 min，Codex 秒级）加
    // 客户端余量，至少 PermissionRequest 如此；其余事件不挂起，只要 ≥ 1 s 够落盘。
    for host in adapter::hosts() {
        let hooks = adapter::for_host(host).unwrap();
        assert!(hooks.hold_timeout() <= HOLD_TIMEOUT, "{host}");
        for h in hooks.install_spec() {
            if h.event == "PermissionRequest" {
                assert!(
                    h.timeout > hooks.hold_timeout() + Duration::from_secs(30),
                    "{host}"
                );
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
fn install_repairs_a_missing_bin_link_even_when_the_config_is_a_noop() {
    // agora-vkt（2026-09-05 实测）：清空 ~/.agora 之后三家条目都还在、链接没了，install 只说
    // "无需改动"，hook 被 `[ -x ]` 守卫静默跳过。链接的检查不能挂在"配置要不要改"上。
    let tmp = tempfile::tempdir().unwrap();
    let exe = fake_exe(tmp.path(), "agora-current");
    let link = installer(tmp.path()).agora_home.join("bin/agora");
    let first = hooks_cmd(tmp.path(), "install", &exe, false);
    assert!(
        first.contains("已写入") && first.contains("已建立"),
        "{first}"
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);

    std::fs::remove_file(&link).unwrap();
    let again = hooks_cmd(tmp.path(), "install", &exe, false);
    assert!(again.contains("无需改动"), "{again}");
    let arrow = format!("{} -> {}", link.display(), exe.display());
    assert!(again.contains(&arrow), "{again}");
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    // 链接那行在配置那行之前：条目生效时守卫已经通得过。
    assert!(again.find(&arrow).unwrap() < again.find("无需改动").unwrap());

    // 什么都对的时候也说一句，让人看得见链接指到了哪。
    let third = hooks_cmd(tmp.path(), "install", &exe, false);
    assert!(
        third.contains(&arrow) && third.contains("已指向"),
        "{third}"
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
}

#[test]
fn install_repoints_a_stale_bin_link() {
    let tmp = tempfile::tempdir().unwrap();
    let old = fake_exe(tmp.path(), "agora-old");
    let exe = fake_exe(tmp.path(), "agora-current");
    let link = installer(tmp.path()).agora_home.join("bin/agora");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    // 指向旧二进制（还在）→ 重指，并说出原来指向哪。
    symlink(&old, &link).unwrap();
    let out = hooks_cmd(tmp.path(), "install", &exe, false);
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    assert!(
        out.contains("重指") && out.contains(&old.display().to_string()),
        "{out}"
    );
    // 悬空（旧二进制已删）→ 重指，说明目标已不存在。
    std::fs::remove_file(&old).unwrap();
    std::fs::remove_file(&link).unwrap();
    symlink(&old, &link).unwrap();
    let out = hooks_cmd(tmp.path(), "install", &exe, false);
    assert!(out.contains("重指") && out.contains("已不存在"), "{out}");
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    // 普通文件占位 → 换成链接。
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, "not a link").unwrap();
    let out = hooks_cmd(tmp.path(), "install", &exe, false);
    assert!(out.contains("重指") && out.contains("普通文件"), "{out}");
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
}

#[test]
fn dry_run_reports_the_link_but_does_not_create_it() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = fake_exe(tmp.path(), "agora-current");
    let link = installer(tmp.path()).agora_home.join("bin/agora");
    hooks_cmd(tmp.path(), "install", &exe, false);
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_dir(link.parent().unwrap()).unwrap();

    // 配置 noop + 链接缺失 + --dry-run：说出会建什么，但连 bin/ 目录都不建。
    let out = hooks_cmd(tmp.path(), "install", &exe, true);
    assert!(out.contains("无需改动"), "{out}");
    assert!(
        out.contains("将建立") && out.contains(&link.display().to_string()),
        "{out}"
    );
    assert!(link.symlink_metadata().is_err(), "--dry-run 不能建链接");
    assert!(!link.parent().unwrap().exists(), "--dry-run 不能建目录");

    // 指错了 + --dry-run：说"将重指"，链接原样。
    let old = fake_exe(tmp.path(), "agora-old");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&old, &link).unwrap();
    let out = hooks_cmd(tmp.path(), "install", &exe, true);
    assert!(out.contains("将重指"), "{out}");
    assert_eq!(std::fs::read_link(&link).unwrap(), old);
}

#[test]
fn uninstall_leaves_the_bin_link_alone() {
    // 别的 agent 的条目可能还在用这条链接；换一个"当前二进制"去卸载也不重指。
    let tmp = tempfile::tempdir().unwrap();
    let exe = fake_exe(tmp.path(), "agora-current");
    let link = installer(tmp.path()).agora_home.join("bin/agora");
    hooks_cmd(tmp.path(), "install", &exe, false);
    let other = fake_exe(tmp.path(), "agora-other");
    let out = hooks_cmd(tmp.path(), "uninstall", &other, false);
    assert!(out.contains("已写入") && !out.contains(" -> "), "{out}");
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
    // 链接也只报告（真跑用 current_exe，路径形态随平台，不在这里比对）。
    assert!(
        err.contains("将建立") && err.contains(&agora_home.join("bin/agora").display().to_string()),
        "{err}"
    );
    assert!(!user_home.join(".claude/settings.json").exists());
    assert!(!agora_home.join("bin/agora").exists());
    assert!(!agora_home.exists(), "--dry-run 连 AGORA_HOME 都不该建");

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

#[test]
fn codex_installs_into_hooks_json_with_a_short_hold_and_a_trust_hint() {
    // ADR-002 D4 + 附录 A（2026-09-05 实测）：Codex 读 ~/.codex/hooks.json，形态与 Claude 相同；
    // 条目内容（命令、timeout）就是信任哈希的输入，所以重复装必须是 noop——否则用户每次都要重新
    // /hooks；SessionEnd / Interrupt 超过 3 s 会被 clamp 并每次启动警告。
    let tmp = tempfile::tempdir().unwrap();
    let inst = installer(tmp.path());
    let hooks = adapter::for_host("codex").unwrap();
    let plan = inst.plan_install(hooks).unwrap();
    assert_eq!(plan.file, inst.user_home.join(".codex/hooks.json"));
    inst.write(&plan).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&plan.file).unwrap()).unwrap();
    let pr = &v["hooks"]["PermissionRequest"][0]["hooks"][0];
    assert!(pr["command"].as_str().unwrap().contains("exec "));
    assert!(pr["timeout"].as_u64().unwrap() > hooks.hold_timeout().as_secs() + 30);
    assert!(
        v["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"]
            .as_u64()
            .unwrap()
            <= 3
    );
    assert!(
        v["hooks"]["Interrupt"][0]["hooks"][0]["timeout"]
            .as_u64()
            .unwrap()
            <= 3
    );
    assert!(v["hooks"].get("Notification").is_none());
    assert_eq!(ours(&v, &inst.agora_home), hooks.install_spec().len());
    assert!(inst.plan_install(hooks).unwrap().is_noop());
    assert!(hooks.install_hint().unwrap().contains("/hooks"));
    let un = inst.plan_uninstall(hooks).unwrap();
    inst.write(&un).unwrap();
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&plan.file).unwrap()).unwrap();
    assert_eq!(ours(&after, &inst.agora_home), 0);
}
