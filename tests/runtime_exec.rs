//! `runtime::exec` 的三条守卫（ADR-001 D2 子进程规则）。

use std::time::{Duration, Instant};

use agora::runtime::exec::{exec, ExecError, ExecOptions, STDERR_TAIL};

#[test]
fn hung_child_times_out_in_5s() {
    // 用 1 s 超时证明机制；生产默认 5 s（DEFAULT_TIMEOUT）。
    let opts = ExecOptions {
        timeout: Some(Duration::from_secs(1)),
        ..Default::default()
    };
    let t0 = Instant::now();
    let err = exec(&["sleep", "30"], &opts).unwrap_err();
    assert!(matches!(err, ExecError::Timeout { .. }), "{err}");
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "took {:?}",
        t0.elapsed()
    );
}

#[test]
fn stderr_flood_does_not_deadlock() {
    // 2 MB stderr 远超 pipe 缓冲；不排空就会卡死。
    let script =
        "i=0; while [ $i -lt 20000 ]; do printf '%0100d\\n' $i >&2; i=$((i+1)); done; echo done";
    let out = exec(&["sh", "-c", script], &ExecOptions::default()).unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
    assert!(out.stderr_tail.len() <= STDERR_TAIL);
    assert!(String::from_utf8_lossy(&out.stderr_tail).ends_with("19999\n"));
}

#[test]
fn argv_is_never_shell_interpolated() {
    // zsh 会把 `=name:` 展开、`$HOME` 替换；argv 直传两者都原样到达。
    let out = exec(
        &["printf", "%s|%s", "=name:", "$HOME"],
        &ExecOptions::default(),
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "=name:|$HOME");
}

#[test]
fn spawn_failure_is_classified_not_found() {
    let err = exec(&["/nonexistent/agora-bin"], &ExecOptions::default()).unwrap_err();
    assert!(err.is_not_found(), "{err}");
}
