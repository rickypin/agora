//! ADR-001 D7：PATH 探测失败不得静默吞掉。

use std::time::Duration;

use agora::runtime::env_probe::{probe_path, PathSource};

#[test]
fn false_shell_falls_back_to_daemon_path_with_reason() {
    let p = probe_path(Some("/bin/false"), Duration::from_secs(5));
    assert_eq!(p.source, PathSource::Daemon);
    assert!(p.reason.as_deref().is_some_and(|r| !r.is_empty()));
    assert_eq!(p.path, std::env::var("PATH").unwrap_or_default());
}

#[test]
fn real_sh_yields_shell_path() {
    let p = probe_path(Some("/bin/sh"), Duration::from_secs(5));
    assert_eq!(p.source, PathSource::Shell, "{:?}", p.reason);
    assert!(!p.path.is_empty());
}

#[test]
fn missing_shell_binary_reports_reason() {
    let p = probe_path(Some("/nonexistent/shell"), Duration::from_secs(5));
    assert_eq!(p.source, PathSource::Daemon);
    assert!(p.reason.is_some());
}
