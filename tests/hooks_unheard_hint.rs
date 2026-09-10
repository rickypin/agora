//! agora-rip：「hook 没接上」的提示按事实分两档——配置文件里根本没有 agora 的条目时，
//! 教人去 TUI 里"信任 agora 的条目"是让人做一件做不到的事。
//!
//! 2026-09-08 现场：zuan 上从没跑过 `agora hooks install codex`，`~/.codex/hooks.json` 不存在，
//! 三个 codex 会话钉在 working（声明了 hook 的会话活动层不产生 IDLE），侧栏提示却是 Codex 的
//! `install_hint`（进 `/hooks` 按 t 信任）——条目不存在，照着做没有任何效果。
mod common;
use agora::{
    hook::install::Installer,
    session::{Db, SessionManager},
};
use std::sync::Arc;

fn manager(home: &std::path::Path) -> Arc<SessionManager> {
    let db = Arc::new(Db::open(&home.join("agora.db")).unwrap());
    Arc::new(SessionManager::new(
        db,
        Arc::new(common::FakeRuntime::default()),
    ))
}

#[test]
fn a_node_without_the_hooks_config_file_is_told_to_install_not_to_trust() {
    let agora_home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let s = manager(agora_home.path());
    s.set_hook_install_homes(agora_home.path(), user_home.path());

    // 用户 HOME 是空的：~/.codex/hooks.json 不存在，正是 zuan 当时的样子。
    let hint = s.unheard_hint("codex", 1000);
    assert!(
        hint.contains("没装 codex 的 hooks") && hint.contains("agora hooks install codex"),
        "文件不存在时要说没装、给安装命令，实际：{hint}"
    );
    assert!(
        !hint.contains("/hooks"),
        "条目都不存在，不该教人去 TUI 里信任它：{hint}"
    );
}

#[test]
fn a_config_file_without_our_entries_counts_as_not_installed() {
    let agora_home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let s = manager(agora_home.path());
    s.set_hook_install_homes(agora_home.path(), user_home.path());

    // 文件在，但里面是别人的条目（装过又卸了，或那是另一个 agora 装的）。
    let file = user_home.path().join(".codex/hooks.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"/opt/other/thing"}]}]}}"#,
    )
    .unwrap();

    let hint = s.unheard_hint("codex", 1000);
    assert!(
        hint.contains("没装 codex 的 hooks"),
        "文件在但没有我们的条目，仍然算没装，实际：{hint}"
    );
}

#[test]
fn an_installed_agent_still_gets_the_hosts_own_hint() {
    let agora_home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let s = manager(agora_home.path());
    s.set_hook_install_homes(agora_home.path(), user_home.path());

    // 真的按 `agora hooks install codex` 写一遍（口径与安装同源）。
    let installer = Installer {
        agora_home: agora_home.path().to_owned(),
        user_home: user_home.path().to_owned(),
    };
    let hooks = agora::adapter::for_host("codex").unwrap();
    let plan = installer.plan_install(hooks).unwrap();
    installer.write(&plan).unwrap();

    let hint = s.unheard_hint("codex", 1000);
    assert!(
        hint.contains("/hooks"),
        "条目装上了却没声音，才轮到宿主自己的 install_hint（Codex 的信任步骤），实际：{hint}"
    );
    assert!(
        !hint.contains("没装 codex 的 hooks"),
        "装了就别说没装：{hint}"
    );
}
